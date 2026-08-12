use crate::coordination::outbound_queue::{
    OutboundPayload, OutboundReceiver, QueuedOutbound, TryReceiveError,
};
use crate::coordination::{CloseReason, ConnectionCloseSignal};
use crate::protocol::{DeliveryReportPayload, PlayerId, ServerMessage};
use axum::extract::ws::{Message, WebSocket};
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::server::EnhancedGameServer;

use super::connection::{record_outbound_probe_activity, PingProbeState};
use super::sending::{
    preflight_binary_fallback, send_single_message, send_single_message_ref,
    write_pending_unsupported_report, BinaryFallbackPreflight, SendAccounting, SendDisposition,
};
use super::{complete_before_optional_deadline, deadline_after};

/// Message batcher for WebSocket connections
/// Batches multiple messages together to reduce syscall overhead
pub(super) struct MessageBatcher {
    pending: VecDeque<QueuedOutbound>,
    #[cfg(test)]
    batch_size: usize,
    #[cfg(test)]
    batch_interval: Duration,
    #[cfg(test)]
    last_flush: Instant,
}

impl MessageBatcher {
    pub(super) fn new(_batch_size: usize, _batch_interval_ms: u64) -> Self {
        Self {
            // Do not eagerly allocate from operator-controlled configuration.
            // The queue grows only as messages actually arrive.
            pending: VecDeque::new(),
            #[cfg(test)]
            batch_size: _batch_size.min(crate::config::websocket::MAX_BATCH_SIZE),
            #[cfg(test)]
            batch_interval: Duration::from_millis(_batch_interval_ms),
            #[cfg(test)]
            last_flush: Instant::now(),
        }
    }

    /// Queue a message for batching
    pub(super) fn queue(&mut self, message: impl Into<QueuedOutbound>) {
        self.pending.push_back(message.into());
    }

    /// Check if batch should be flushed
    #[cfg(test)]
    pub(super) fn should_flush(&self) -> bool {
        // Flush if batch is full or time threshold exceeded
        self.pending.len() >= self.batch_size
            || (!self.pending.is_empty() && self.last_flush.elapsed() >= self.batch_interval)
    }

    /// Flush all pending messages at once (unit-test convenience; production
    /// sends drain incrementally via [`Self::pop_front`] so a cancelled write
    /// cannot lose the rest of the batch).
    #[cfg(test)]
    pub(super) fn flush(&mut self) -> Vec<QueuedOutbound> {
        self.last_flush = Instant::now();
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    /// Take the oldest pending message (FIFO), if any.
    pub(super) fn pop_front(&mut self) -> Option<QueuedOutbound> {
        self.pending.pop_front()
    }

    /// Get pending message count
    pub(super) fn len(&self) -> usize {
        self.pending.len()
    }

    /// Check if batch is empty
    pub(super) fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub(super) fn oldest_reliable_enqueued_at(&self) -> Option<Instant> {
        self.pending
            .iter()
            .filter(|message| message.class() == Some(crate::protocol::DeliveryClass::Reliable))
            .map(|message| message.enqueued_at)
            .min()
    }

    pub(super) fn count_by_class(&self) -> [(crate::protocol::DeliveryClass, u64); 3] {
        let mut counts = [
            (crate::protocol::DeliveryClass::Reliable, 0u64),
            (crate::protocol::DeliveryClass::Latest, 0u64),
            (crate::protocol::DeliveryClass::Volatile, 0u64),
        ];
        for class in self.pending.iter().filter_map(QueuedOutbound::class) {
            match class {
                crate::protocol::DeliveryClass::Reliable => {
                    counts[0].1 = counts[0].1.saturating_add(1)
                }
                crate::protocol::DeliveryClass::Latest => {
                    counts[1].1 = counts[1].1.saturating_add(1)
                }
                crate::protocol::DeliveryClass::Volatile => {
                    counts[2].1 = counts[2].1.saturating_add(1)
                }
            }
        }
        counts
    }
}

/// Helper function to send a batch of messages
///
/// Messages are popped one at a time rather than taken out wholesale, so that
/// cancellation (the send task's close-signal select racing an in-flight
/// socket write) leaves every unsent message inside the batcher where the
/// finalize path can still flush or count it. Only the single message
/// actively being written can be lost with the connection — it is already
/// (partially) on the wire.
pub(super) async fn send_batch(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    batcher: &mut MessageBatcher,
    receiver: &mut OutboundReceiver,
    player_id: &PlayerId,
    server: &Arc<EnhancedGameServer>,
    close_signal: &ConnectionCloseSignal,
    ping_probe_state: &watch::Sender<PingProbeState>,
    max_sojourn: Duration,
    write_phase: WritePhase,
) -> Result<(), QueueWriteError> {
    let mut batch_size = 0_usize;
    while !batcher.is_empty() {
        loop {
            match receiver.try_recv_control() {
                Ok(Some(control)) => {
                    send_queued(
                        sender,
                        control,
                        batcher.oldest_reliable_enqueued_at(),
                        receiver,
                        player_id,
                        server,
                        close_signal,
                        ping_probe_state,
                        max_sojourn,
                        write_phase,
                    )
                    .await?;
                }
                Ok(None) => break,
                Err(TryReceiveError::AccountabilityFailed) => {
                    #[cfg(feature = "trace-validation")]
                    close_signal.record_trace(
                        crate::trace_validation::DeliveryTraceAction::Unsupported,
                        None,
                        Some("writer-accountability-failed"),
                    );
                    if close_signal.request_close(CloseReason::SlowConsumer) {
                        server
                            .metrics()
                            .increment_websocket_slow_consumer_disconnects();
                    }
                    return Err(QueueWriteError::AccountabilityFailed);
                }
                Err(TryReceiveError::Empty | TryReceiveError::Disconnected) => break,
            }
        }

        let Some(message) = batcher.pop_front() else {
            break;
        };
        send_queued(
            sender,
            message,
            batcher.oldest_reliable_enqueued_at(),
            receiver,
            player_id,
            server,
            close_signal,
            ping_probe_state,
            max_sojourn,
            write_phase,
        )
        .await?;
        batch_size = batch_size.saturating_add(1);
    }
    // Only a real batch (2+ messages) is a "batch"; with batching off the writer
    // drains one message per call, which is a normal send, not a flush.
    if batch_size > 1 {
        tracing::trace!(%player_id, batch_size, "Flushed message batch");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueueWriteError {
    SocketClosed,
    SojournExpired,
    AccountabilityFailed,
}

/// Whether a queued item belongs to the live writer loop or the bounded final
/// flush after a close request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WritePhase {
    Live,
    CloseFlush,
}

fn queued_write_deadline(
    queued: &QueuedOutbound,
    oldest_reliable_batched: Option<Instant>,
    receiver: &OutboundReceiver,
    max_sojourn: Duration,
    write_started_at: Instant,
    known_unsupported: bool,
) -> Option<Instant> {
    if known_unsupported {
        // The reliable payload has already reached its terminal accounted-drop
        // path. Its exact report is control progress, not unresolved reliable
        // delivery, so unrelated reliable queue age must not expire it.
        return deadline_after(write_started_at, max_sojourn);
    }
    match queued.class() {
        Some(crate::protocol::DeliveryClass::Reliable) => deadline_after(
            receiver
                .oldest_reliable_enqueued_at()
                .into_iter()
                .chain(oldest_reliable_batched)
                .chain(std::iter::once(queued.enqueued_at))
                .min()
                .unwrap_or(write_started_at),
            max_sojourn,
        ),
        // Control traffic owns its queue-age deadline. In particular, a fresh
        // DeliveryReport must not inherit the age of stale lossy data.
        None => deadline_after(queued.enqueued_at, max_sojourn),
        // Latest/volatile queue age is resolved by their explicit loss policy.
        // Once selected, retain a bounded write-progress budget so a peer that
        // stops reading cannot wedge the sole socket writer forever.
        Some(crate::protocol::DeliveryClass::Latest | crate::protocol::DeliveryClass::Volatile) => {
            deadline_after(write_started_at, max_sojourn)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn complete_selected_write<F>(
    deadline: Option<Instant>,
    write: F,
    player_id: &PlayerId,
    server: &EnhancedGameServer,
    close_signal: &ConnectionCloseSignal,
    max_sojourn: Duration,
) -> Result<F::Output, QueueWriteError>
where
    F: Future,
{
    match complete_before_optional_deadline(deadline, write).await {
        Ok(result) => Ok(result),
        Err(_) => {
            #[cfg(feature = "trace-validation")]
            close_signal.record_trace(
                crate::trace_validation::DeliveryTraceAction::Unsupported,
                None,
                Some("writer-sojourn-expired"),
            );
            let initiated_close = close_signal.request_close(CloseReason::SlowConsumer);
            if initiated_close {
                server
                    .metrics()
                    .increment_websocket_slow_consumer_disconnects();
            }
            tracing::warn!(
                %player_id,
                max_sojourn_ms = u64::try_from(max_sojourn.as_millis()).unwrap_or(u64::MAX),
                initiated_close,
                "Outbound message exceeded the maximum queue sojourn; closing recipient"
            );
            Err(QueueWriteError::SojournExpired)
        }
    }
}

/// Materialize and write one queued delivery report, advancing its counter
/// frontier only after the sink's send/flush future succeeds.
///
/// Keeping prepare/write/confirm in one production seam makes cancellation
/// safe by construction: dropping this future while `write` is pending leaves
/// the frontier unchanged.
async fn write_queued_report<F, Fut>(
    receiver: &OutboundReceiver,
    mut report: DeliveryReportPayload,
    write: F,
) -> Result<SendDisposition, ()>
where
    F: FnOnce(Arc<ServerMessage>) -> Fut,
    Fut: Future<Output = Result<SendDisposition, ()>>,
{
    receiver.prepare_report_for_wire(&mut report);
    let per_class = report.per_class;
    let disposition = write(Arc::new(ServerMessage::DeliveryReport(Box::new(report)))).await?;
    if disposition == SendDisposition::Written {
        receiver.confirm_report_written(per_class);
    }
    Ok(disposition)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_queued(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    queued: QueuedOutbound,
    oldest_reliable_batched: Option<Instant>,
    receiver: &OutboundReceiver,
    player_id: &PlayerId,
    server: &Arc<EnhancedGameServer>,
    close_signal: &ConnectionCloseSignal,
    ping_probe_state: &watch::Sender<PingProbeState>,
    max_sojourn: Duration,
    write_phase: WritePhase,
) -> Result<(), QueueWriteError> {
    #[cfg(not(feature = "trace-validation"))]
    let _ = write_phase;
    #[cfg(feature = "trace-validation")]
    let trace_write = match &queued.payload {
        OutboundPayload::Message(message) => {
            close_signal.start_trace_write(message, write_phase == WritePhase::CloseFlush)
        }
        OutboundPayload::Data(delivery) => close_signal
            .start_trace_write(delivery.message(), write_phase == WritePhase::CloseFlush),
        OutboundPayload::DeliveryReport(_) => None,
    };
    let class = queued.class();
    let metadata = queued.metadata;
    let write_started_at = Instant::now();
    let recipient_format = receiver.game_data_format();
    let fallback_preflight = match &queued.payload {
        OutboundPayload::Message(message) => {
            preflight_binary_fallback(message, recipient_format, None)
        }
        OutboundPayload::Data(delivery) => preflight_binary_fallback(
            delivery.message(),
            recipient_format,
            delivery.relay_frame_cache(),
        ),
        OutboundPayload::DeliveryReport(_) => BinaryFallbackPreflight::NotNeeded,
    };
    let deadline = queued_write_deadline(
        &queued,
        oldest_reliable_batched,
        receiver,
        max_sojourn,
        write_started_at,
        fallback_preflight.is_unsupported(),
    );
    let carries_queued_report = matches!(queued.payload, OutboundPayload::DeliveryReport(_));
    // Exact accounting owns the head of the stream: a coalesced
    // unsupported-format report must reach this recipient before the next
    // delivered frame, because a recipient may not observe delivered data that
    // skips sequences it was never told about. Two items are handled
    // differently:
    //
    // - another undeliverable payload keeps coalescing instead, which is what
    //   collapses a burst into one report;
    // - a queued `DeliveryReport` is stamped against the wire frontier
    //   immediately before this write, so the flush follows it rather than
    //   preceding it: a flush in front would move a counter that the very next
    //   frame then reports as lower.
    let flush_before = !fallback_preflight.is_unsupported() && !carries_queued_report;
    let mut accounting = SendAccounting::new(receiver, server, ping_probe_state, *player_id, class);
    let recipient_supports_v3 = receiver.supports_v3();
    let recipient_format = receiver.game_data_format();
    let write = async {
        if flush_before && write_pending_unsupported_report(sender, receiver, player_id).await? {
            record_outbound_probe_activity(ping_probe_state, Instant::now());
        }
        let disposition = match queued.payload {
            OutboundPayload::Message(message) => {
                send_single_message(
                    sender,
                    message,
                    None,
                    player_id,
                    recipient_supports_v3,
                    recipient_format,
                    fallback_preflight,
                    metadata,
                    &mut accounting,
                )
                .await?
            }
            OutboundPayload::Data(delivery) => {
                send_single_message_ref(
                    sender,
                    delivery.message(),
                    delivery.relay_frame_cache(),
                    player_id,
                    recipient_supports_v3,
                    recipient_format,
                    fallback_preflight,
                    metadata,
                    &mut accounting,
                )
                .await?
            }
            OutboundPayload::DeliveryReport(report) => {
                let disposition = write_queued_report(receiver, report, |message| {
                    send_single_message(
                        sender,
                        message,
                        None,
                        player_id,
                        recipient_supports_v3,
                        recipient_format,
                        fallback_preflight,
                        metadata,
                        &mut accounting,
                    )
                })
                .await?;
                if write_pending_unsupported_report(sender, receiver, player_id).await? {
                    record_outbound_probe_activity(ping_probe_state, Instant::now());
                }
                disposition
            }
        };
        Ok(disposition)
    };
    let result = if max_sojourn.is_zero() {
        write.await.map_err(|()| QueueWriteError::SocketClosed)
    } else {
        complete_selected_write(
            deadline,
            write,
            player_id,
            server,
            close_signal,
            max_sojourn,
        )
        .await?
        .map_err(|()| QueueWriteError::SocketClosed)
    };
    let disposition = result?;
    if disposition == SendDisposition::Written {
        accounting.complete_written();
        record_outbound_probe_activity(ping_probe_state, Instant::now());
        #[cfg(feature = "trace-validation")]
        if let Some(delivery_id) = trace_write {
            close_signal.finish_trace_write(delivery_id, write_phase == WritePhase::CloseFlush);
        }
    } else {
        #[cfg(feature = "trace-validation")]
        close_signal.record_trace(
            crate::trace_validation::DeliveryTraceAction::Unsupported,
            None,
            Some("writer-accounted-drop"),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::coordination::outbound_queue::{channel, DataDeliveryMetadata, OutboundData};
    use crate::database::DatabaseConfig;
    use crate::protocol::{DeliveryClass, DeliveryCountersByClass, GameDataEncoding, RoomId};
    use crate::server::ServerConfig;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    async fn test_server() -> Arc<EnhancedGameServer> {
        EnhancedGameServer::new(
            ServerConfig::default(),
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            crate::config::SessionConfig::default(),
            crate::config::TurnConfig::default(),
            DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("construct selected-write test server")
    }

    fn data(class: DeliveryClass, seq: u64) -> OutboundData {
        let from_player = PlayerId::from_u128(1);
        let room_id = RoomId::from_u128(2);
        OutboundData::new(
            Arc::new(ServerMessage::GameData {
                from_player,
                data: json!({ "seq": seq }),
                seq: Some(seq),
                epoch: Some(1),
                class: Some(class),
                key: None,
            }),
            DataDeliveryMetadata {
                class,
                key: None,
                from_player,
                room_id,
                epoch: 1,
                seq,
            },
        )
    }

    fn unsupported_binary_data(seq: u64) -> OutboundData {
        let from_player = PlayerId::from_u128(1);
        let room_id = RoomId::from_u128(2);
        OutboundData::new(
            Arc::new(ServerMessage::GameDataBinary {
                from_player,
                encoding: GameDataEncoding::MessagePack,
                payload: bytes::Bytes::from_static(&[0xc1]),
                seq: Some(seq),
                epoch: Some(1),
            }),
            DataDeliveryMetadata {
                class: DeliveryClass::Reliable,
                key: None,
                from_player,
                room_id,
                epoch: 1,
                seq,
            },
        )
    }

    fn report_frontier(receiver: &OutboundReceiver) -> DeliveryCountersByClass {
        let mut probe = DeliveryReportPayload::default();
        receiver.prepare_report_for_wire(&mut probe);
        probe.per_class
    }

    #[tokio::test(start_paused = true)]
    async fn selected_write_expires_at_or_after_deadline_without_completing_accounting() {
        for (advance_by, context) in [
            (Duration::from_millis(10), "exact deadline"),
            (Duration::from_millis(11), "after deadline"),
        ] {
            let server = test_server().await;
            let player_id = PlayerId::from_u128(1);
            let (close, close_listener) = ConnectionCloseSignal::channel();
            let (release, gate) = tokio::sync::oneshot::channel();
            let completed = Arc::new(AtomicBool::new(false));
            let completed_by_write = Arc::clone(&completed);
            let write = async move {
                gate.await.expect("selected-write gate released");
                completed_by_write.store(true, Ordering::Release);
            };
            let deadline = Instant::now() + Duration::from_millis(10);
            let mut selected = Box::pin(complete_selected_write(
                Some(deadline),
                write,
                &player_id,
                &server,
                &close,
                Duration::from_millis(10),
            ));

            assert!(
                futures_util::poll!(&mut selected).is_pending(),
                "{context}: selected write must first be pending"
            );
            tokio::time::advance(advance_by).await;
            release.send(()).expect("make selected write ready");

            assert_eq!(
                selected.await,
                Err(QueueWriteError::SojournExpired),
                "{context}: selected write must fail with the production outcome"
            );
            assert!(
                !completed.load(Ordering::Acquire),
                "{context}: expired socket work must be cancelled before accounting can complete"
            );
            assert_eq!(
                close_listener.requested_reason(),
                Some(CloseReason::SlowConsumer),
                "{context}: selected-write expiry must own close 4002"
            );
            assert_eq!(
                server
                    .metrics()
                    .websocket_slow_consumer_disconnects
                    .load(Ordering::Relaxed),
                1,
                "{context}: the winning expiry must increment its metric exactly once"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn selected_write_completes_strictly_before_deadline() {
        let server = test_server().await;
        let player_id = PlayerId::from_u128(1);
        let (close, close_listener) = ConnectionCloseSignal::channel();
        let (release, gate) = tokio::sync::oneshot::channel();
        let deadline = Instant::now() + Duration::from_millis(10);
        let mut selected = Box::pin(complete_selected_write(
            Some(deadline),
            gate,
            &player_id,
            &server,
            &close,
            Duration::from_millis(10),
        ));

        assert!(
            futures_util::poll!(&mut selected).is_pending(),
            "selected write must first be pending"
        );
        tokio::time::advance(Duration::from_millis(9)).await;
        release.send(()).expect("make selected write ready");

        assert_eq!(selected.await, Ok(Ok(())));
        assert_eq!(
            close_listener.requested_reason(),
            None,
            "healthy completion must not request a close"
        );
        assert_eq!(
            server
                .metrics()
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unrepresentable_sojourn_deadline_does_not_expire_selected_write() {
        let server = test_server().await;
        let player_id = PlayerId::from_u128(1);
        let (close, close_listener) = ConnectionCloseSignal::channel();

        assert_eq!(
            complete_selected_write(
                None,
                async { 7 },
                &player_id,
                &server,
                &close,
                Duration::MAX,
            )
            .await,
            Ok(7),
            "deadline overflow must not turn a fresh write into slow-consumer expiry"
        );
        assert_eq!(close_listener.requested_reason(), None);
        assert_eq!(
            server
                .metrics()
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0
        );
    }

    /// Issue #218: exercise the production seam used by `send_queued` with a
    /// deterministically controlled socket-send future. The first poll proves
    /// preparation has happened while confirmation has not; success confirms,
    /// while both an explicit send error and cancellation leave the frontier
    /// untouched.
    #[tokio::test]
    async fn queued_report_frontier_confirms_only_after_send_flush_success() {
        let (_tx, receiver) = channel(1, 1);
        let mut baseline = DeliveryCountersByClass::default();
        baseline.latest.superseded = 7;
        receiver.confirm_report_written(baseline);

        let mut successful_report = DeliveryReportPayload::default();
        successful_report.per_class.latest.dropped_full = 3;
        let expected_success = DeliveryCountersByClass {
            latest: crate::protocol::LatestDeliveryCounters {
                superseded: 7,
                dropped_full: 3,
                ..Default::default()
            },
            ..Default::default()
        };
        let observed = Arc::new(Mutex::new(None));
        let observed_by_write = Arc::clone(&observed);
        let (release_send, await_send) = tokio::sync::oneshot::channel();
        let mut write = Box::pin(write_queued_report(
            &receiver,
            successful_report,
            move |message| {
                let observed = Arc::clone(&observed_by_write);
                async move {
                    let ServerMessage::DeliveryReport(report) = message.as_ref() else {
                        panic!("queued-report seam wrote a non-report message");
                    };
                    *observed.lock().expect("observed-report mutex poisoned") =
                        Some(report.per_class);
                    await_send.await.expect("test send gate released");
                    Ok(SendDisposition::Written)
                }
            },
        ));

        assert!(
            futures_util::poll!(&mut write).is_pending(),
            "the controlled socket send must remain in flight"
        );
        assert_eq!(
            *observed.lock().expect("observed-report mutex poisoned"),
            Some(expected_success),
            "the report must be prepared before its socket write starts"
        );
        assert_eq!(
            report_frontier(&receiver),
            baseline,
            "the frontier must not move while send/flush is pending"
        );

        release_send.send(()).expect("release controlled send");
        assert_eq!(write.await, Ok(SendDisposition::Written));
        assert_eq!(
            report_frontier(&receiver),
            expected_success,
            "a successful send/flush must confirm the prepared frontier"
        );

        let before_failure = report_frontier(&receiver);
        let mut failed_report = DeliveryReportPayload::default();
        failed_report.per_class.volatile.dropped = 2;
        assert_eq!(
            write_queued_report(&receiver, failed_report, |_| async { Err(()) }).await,
            Err(())
        );
        assert_eq!(
            report_frontier(&receiver),
            before_failure,
            "a failed socket send must not confirm its report"
        );

        let mut accounted_drop = DeliveryReportPayload::default();
        accounted_drop.per_class.volatile.dropped = 4;
        assert_eq!(
            write_queued_report(&receiver, accounted_drop, |_| async {
                Ok(SendDisposition::AccountedDrop)
            })
            .await,
            Ok(SendDisposition::AccountedDrop)
        );
        assert_eq!(
            report_frontier(&receiver),
            before_failure,
            "a non-written disposition must not confirm its report"
        );

        let mut cancelled_report = DeliveryReportPayload::default();
        cancelled_report.per_class.reliable.abandoned = 5;
        let mut cancelled_write =
            Box::pin(write_queued_report(&receiver, cancelled_report, |_| {
                std::future::pending()
            }));
        assert!(
            futures_util::poll!(&mut cancelled_write).is_pending(),
            "the cancellation boundary must be reached without timing"
        );
        drop(cancelled_write);
        assert_eq!(
            report_frontier(&receiver),
            before_failure,
            "cancelling an in-flight socket send must not confirm its report"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn writer_deadlines_are_partitioned_by_delivery_class() {
        let (tx, mut rx) = channel(4, 4);
        tx.set_protocol_version(3);
        tx.try_enqueue_data(data(DeliveryClass::Volatile, 1))
            .expect("enqueue stale volatile data");
        let stale_volatile_at = rx.oldest_enqueued_at().expect("volatile timestamp");
        tokio::time::advance(Duration::from_secs(10)).await;

        let control = QueuedOutbound::test_control(Arc::new(ServerMessage::RoomLeft));
        let control_started_at = Instant::now();
        assert_eq!(
            queued_write_deadline(
                &control,
                None,
                &rx,
                Duration::from_secs(15),
                control_started_at,
                false,
            ),
            Some(control.enqueued_at + Duration::from_secs(15)),
            "fresh control must not inherit stale volatile age"
        );

        assert_eq!(
            queued_write_deadline(
                &control,
                None,
                &rx,
                Duration::MAX,
                control_started_at,
                false,
            ),
            None,
            "an unrepresentable internal duration remains beyond the process lifetime"
        );

        let volatile = rx.recv().await.expect("queue open").expect("volatile data");
        let volatile_started_at = Instant::now();
        assert_eq!(volatile.enqueued_at, stale_volatile_at);
        assert_eq!(
            queued_write_deadline(
                &volatile,
                None,
                &rx,
                Duration::from_secs(15),
                volatile_started_at,
                false,
            ),
            Some(volatile_started_at + Duration::from_secs(15)),
            "lossy queue age must not trigger recipient eviction"
        );

        tx.try_enqueue_data(data(DeliveryClass::Reliable, 2))
            .expect("enqueue reliable data");
        let reliable = rx.recv().await.expect("queue open").expect("reliable data");
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            queued_write_deadline(
                &reliable,
                None,
                &rx,
                Duration::from_secs(15),
                Instant::now(),
                false,
            ),
            Some(reliable.enqueued_at + Duration::from_secs(15)),
            "reliable delivery must retain its end-to-end sojourn ceiling"
        );

        tx.try_enqueue_data(unsupported_binary_data(3))
            .expect("enqueue unsupported reliable binary data");
        let unsupported = rx
            .recv()
            .await
            .expect("queue open")
            .expect("unsupported data");
        tokio::time::advance(Duration::from_secs(10)).await;
        let report_write_started_at = Instant::now();
        assert_eq!(
            queued_write_deadline(
                &unsupported,
                None,
                &rx,
                Duration::from_secs(15),
                report_write_started_at,
                true,
            ),
            Some(report_write_started_at + Duration::from_secs(15)),
            "a deterministic unsupported outcome must use report write progress, not reliable queue age"
        );
    }

    #[test]
    fn test_message_batcher_new() {
        let batcher = MessageBatcher::new(10, 16);
        assert_eq!(batcher.len(), 0);
        assert!(batcher.is_empty());
        assert!(!batcher.should_flush());
    }

    #[test]
    fn extreme_batch_size_does_not_panic_during_construction() {
        let batcher = std::panic::catch_unwind(|| MessageBatcher::new(usize::MAX, 16));
        assert!(batcher.is_ok());
    }

    #[test]
    fn test_message_batcher_queue() {
        let mut batcher = MessageBatcher::new(10, 16);
        let message = Arc::new(ServerMessage::PlayerLeft {
            player_id: uuid::Uuid::new_v4(),
            epoch: None,
            final_seq: None,
        });

        batcher.queue(message);
        assert_eq!(batcher.len(), 1);
        assert!(!batcher.is_empty());
    }

    #[test]
    fn test_message_batcher_flush_on_size() {
        let mut batcher = MessageBatcher::new(3, 1000); // Large interval, flush on size

        // Add messages up to batch size
        for _ in 0..2 {
            let message = Arc::new(ServerMessage::PlayerLeft {
                player_id: uuid::Uuid::new_v4(),
                epoch: None,
                final_seq: None,
            });
            batcher.queue(message);
        }

        assert_eq!(batcher.len(), 2);
        assert!(!batcher.should_flush()); // Not full yet

        // Add one more to reach batch size
        let message = Arc::new(ServerMessage::PlayerLeft {
            player_id: uuid::Uuid::new_v4(),
            epoch: None,
            final_seq: None,
        });
        batcher.queue(message);

        assert_eq!(batcher.len(), 3);
        assert!(batcher.should_flush()); // Should flush now

        // Test flush
        let messages = batcher.flush();
        assert_eq!(messages.len(), 3);
        assert_eq!(batcher.len(), 0);
        assert!(batcher.is_empty());
    }

    // Deterministic under the paused-clock runtime: the flush timer reads
    // `tokio::time::Instant`, so `advance(..)` drives the interval exactly — no
    // `thread::sleep`, so nothing can overshoot or be descheduled. This removes
    // the FLAKE-002 wall-clock dependence at the root rather than band-aiding it
    // with large intervals; one batcher exercises both the "not yet" and
    // "elapsed" edges of the `>=` boundary.
    #[tokio::test(start_paused = true)]
    async fn test_message_batcher_flush_on_time() {
        let mut batcher = MessageBatcher::new(100, 50); // size 100 (unreachable), 50ms interval
        batcher.queue(Arc::new(ServerMessage::PlayerLeft {
            player_id: uuid::Uuid::new_v4(),
            epoch: None,
            final_seq: None,
        }));
        assert_eq!(batcher.len(), 1);
        assert!(
            !batcher.should_flush(),
            "a fresh batch must not flush before its interval elapses"
        );

        // Just under the interval: still not flushing (boundary is `>=`).
        tokio::time::advance(Duration::from_millis(49)).await;
        assert!(
            !batcher.should_flush(),
            "a batch just under its interval must not flush yet"
        );

        // Crossing the interval: flushes on time.
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(
            batcher.should_flush(),
            "the batch must flush once its interval has elapsed"
        );
        let messages = batcher.flush();
        assert_eq!(messages.len(), 1);
        assert_eq!(batcher.len(), 0);
    }

    #[test]
    fn test_message_batcher_multiple_flushes() {
        let mut batcher = MessageBatcher::new(2, 1000);

        // First batch
        for _ in 0..2 {
            let message = Arc::new(ServerMessage::PlayerLeft {
                player_id: uuid::Uuid::new_v4(),
                epoch: None,
                final_seq: None,
            });
            batcher.queue(message);
        }

        assert!(batcher.should_flush());
        let messages1 = batcher.flush();
        assert_eq!(messages1.len(), 2);
        assert_eq!(batcher.len(), 0);

        // Second batch
        for _ in 0..2 {
            let message = Arc::new(ServerMessage::PlayerLeft {
                player_id: uuid::Uuid::new_v4(),
                epoch: None,
                final_seq: None,
            });
            batcher.queue(message);
        }

        assert!(batcher.should_flush());
        let messages2 = batcher.flush();
        assert_eq!(messages2.len(), 2);
        assert_eq!(batcher.len(), 0);
    }

    #[test]
    fn test_message_batcher_empty_flush() {
        let mut batcher = MessageBatcher::new(10, 16);

        // Flush empty batcher
        let messages = batcher.flush();
        assert_eq!(messages.len(), 0);
        assert_eq!(batcher.len(), 0);
    }

    // Partial-batch (below `batch_size`) time flush, deterministic under the
    // paused clock — same `>=`-boundary pattern as
    // `test_message_batcher_flush_on_time` (fresh → just-under → crossing).
    #[tokio::test(start_paused = true)]
    async fn test_message_batcher_partial_batch() {
        let mut batcher = MessageBatcher::new(10, 50); // 3 < size 10, 50ms interval
        for _ in 0..3 {
            batcher.queue(Arc::new(ServerMessage::PlayerLeft {
                player_id: uuid::Uuid::new_v4(),
                epoch: None,
                final_seq: None,
            }));
        }
        assert_eq!(batcher.len(), 3);
        assert!(
            !batcher.should_flush(),
            "a partial batch must not flush before its interval elapses"
        );

        // Just under the interval: still not flushing (boundary is `>=`).
        tokio::time::advance(Duration::from_millis(49)).await;
        assert!(
            !batcher.should_flush(),
            "a partial batch just under its interval must not flush yet"
        );

        // Crossing the interval: flushes on time.
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(
            batcher.should_flush(),
            "a partial batch must flush once its interval has elapsed"
        );
        let messages = batcher.flush();
        assert_eq!(messages.len(), 3);
    }
}
