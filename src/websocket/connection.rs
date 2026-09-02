use crate::coordination::outbound_queue::{
    self, OutboundReceiver, OutboundSender, TryEnqueueError, TryReceiveError,
};
use crate::coordination::{
    deliver_or_disconnect, ClientDeliveryHandle, CloseReason, ConnectionCloseListener,
    ConnectionCloseSignal, DeliveryOutcome,
};
use crate::protocol::{
    ClientMessage, ErrorCode, GameDataEncoding, PlayerId, PlayerNameRulesPayload,
    ProtocolInfoPayload, RateLimitInfo, ServerMessage, Topology, Transport,
    PROTOCOL_INFO_TRANSPORT_WEBSOCKET, ROOM_OPERATION_IDS_CAPABILITY,
};
use crate::server::{EnhancedGameServer, NegotiatedProtocol, RegisterClientError};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use rand::RngExt;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch, RwLock};
use tokio::time::Instant;

use super::batching::{send_batch, send_queued, MessageBatcher, QueueWriteError, WritePhase};
use super::sending::{
    send_immediate_server_message, write_pending_unsupported_report, ImmediateSendError,
};
use super::token_binding::{parse_binary_message, parse_client_message, TokenBindingHandshake};
use super::{
    complete_before_deadline, deadline_after, CONNECTION_CLOSE_WRITE_TIMEOUT as CLOSE_WRITE_TIMEOUT,
};

const SERVER_PING_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

async fn send_outbound_too_large_close(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) {
    let reason = CloseReason::OutboundMessageTooLarge;
    let close_frame = Message::Close(Some(axum::extract::ws::CloseFrame {
        code: reason.websocket_close_code(),
        reason: reason.close_frame_reason().into(),
    }));
    let _ = tokio::time::timeout(CLOSE_WRITE_TIMEOUT, sender.send(close_frame)).await;
}

async fn handle_immediate_send_error(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    error: &ImmediateSendError,
) {
    if matches!(error, ImmediateSendError::MessageTooLarge { .. }) {
        send_outbound_too_large_close(sender).await;
    }
}

/// Refuse a WebSocket registration that failed admission (per-IP cap or the
/// server-wide ceiling): best-effort token-binding challenge, one `Error`
/// frame, then a bounded close. Shared by both admission-refusal variants so
/// their wire behavior stays identical.
async fn refuse_websocket_registration(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    token_binding: Option<&TokenBindingHandshake>,
    addr: SocketAddr,
    refusal_message: String,
    max_outbound_message_size: usize,
) {
    if !send_token_binding_challenge(sender, token_binding, addr, max_outbound_message_size).await {
        return;
    }
    let error_message = ServerMessage::Error {
        message: refusal_message,
        error_code: Some(ErrorCode::TooManyConnections),
    };
    match tokio::time::timeout(
        CLOSE_WRITE_TIMEOUT,
        send_immediate_server_message(sender, &error_message, max_outbound_message_size),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            handle_immediate_send_error(sender, &err).await;
            tracing::debug!(
                client_addr = %addr,
                error = %err,
                "Failed to send connection-limit error frame"
            );
        }
        Err(_elapsed) => {
            tracing::debug!(
                client_addr = %addr,
                "Timed out sending connection-limit error frame"
            );
        }
    }
    match tokio::time::timeout(CLOSE_WRITE_TIMEOUT, sender.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::debug!(
                client_addr = %addr,
                error = %err,
                "Failed to close connection-limited WebSocket registration"
            );
        }
        Err(_elapsed) => {
            tracing::debug!(
                client_addr = %addr,
                "Timed out closing connection-limited WebSocket registration"
            );
        }
    }
}

/// Convert a relative duration into an absolute deadline without changing an
/// overflow into an already-expired instant.
fn checked_deadline(start: Instant, duration: Duration) -> Instant {
    crate::deadline::saturating_after(start, duration)
}

fn random_ping_nonce() -> u64 {
    let mut rng = rand::rng();
    loop {
        let nonce = rng.random::<u64>();
        if nonce != 0 {
            return nonce;
        }
    }
}

async fn send_token_binding_challenge(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    binding: Option<&TokenBindingHandshake>,
    addr: SocketAddr,
    max_outbound_message_size: usize,
) -> bool {
    let Some(binding) = binding else {
        return true;
    };
    let challenge = serde_json::json!({
        "type": "TokenBindingChallenge",
        "data": binding.challenge,
    });
    let challenge = challenge.to_string();
    if challenge.len() > max_outbound_message_size {
        tracing::error!(
            client_addr = %addr,
            size = challenge.len(),
            max = max_outbound_message_size,
            "Token-binding challenge exceeds outbound message-size limit"
        );
        send_outbound_too_large_close(sender).await;
        return false;
    }
    match tokio::time::timeout(
        CLOSE_WRITE_TIMEOUT,
        sender.send(Message::Text(challenge.into())),
    )
    .await
    {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            tracing::debug!(client_addr = %addr, error = %err, "Failed to send token-binding challenge");
            false
        }
        Err(_elapsed) => {
            tracing::debug!(client_addr = %addr, "Timed out sending token-binding challenge");
            false
        }
    }
}

#[repr(u32)]
enum RegisteredConnectionCloseStep {
    FlushQueuedMessages,
    /// The coalesced unsupported-format report, written after the drain because
    /// the drain itself can discover further undeliverable payloads.
    FinalDeliveryReport,
    SemanticCloseFrame,
    SinkClose,
    Count,
}

pub(super) const REGISTERED_SHUTDOWN_CLOSE_WRITE_STEPS: u32 =
    RegisteredConnectionCloseStep::Count as u32;

struct ServerPingCommand {
    nonce: u64,
    baseline_generation: u64,
    baseline_outbound_generation: u64,
    write_outcome: oneshot::Sender<PingWriteOutcome>,
}

#[derive(Clone, Copy, Debug)]
struct PingWriteTiming {
    completed_at: Instant,
    outbound_generation: u64,
}

#[derive(Clone, Copy, Debug)]
enum PingWriteOutcome {
    Written(PingWriteTiming),
    SkippedActivity {
        inbound_generation: u64,
        outbound_generation: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PingProbeEvidence {
    MatchingPong { received_at: Instant },
    InboundActivity { received_at: Instant },
    OutboundActivity { completed_at: Instant },
}

#[derive(Clone, Copy, Debug)]
struct ActivePingProbe {
    nonce: u64,
    started_at: Instant,
    evidence: Option<PingProbeEvidence>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PingProbeState {
    inbound_generation: u64,
    outbound_generation: u64,
    inbound_processing: bool,
    active: Option<ActivePingProbe>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PingProbeResolution {
    MatchingPong(Duration),
    InboundActivity { generation: u64 },
    OutboundActivity { generation: u64 },
    TimedOut,
}

#[derive(Debug, PartialEq, Eq)]
enum InboundRead<T> {
    CloseRequested(Option<CloseReason>),
    DeadlineElapsed,
    Completed(T),
}

/// Read one inbound item while preserving the connection's lifecycle
/// precedence and the exclusive deadline contract.
///
/// A close already requested when this future is polled wins over both the
/// timer and a ready frame. With no deadline, only close or input can resolve.
async fn read_before_deadline<F>(
    close: &mut ConnectionCloseListener,
    deadline: Option<Instant>,
    read: F,
) -> InboundRead<F::Output>
where
    F: Future,
{
    let wait_for_deadline = async {
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(read);
    tokio::pin!(wait_for_deadline);
    tokio::select! {
        biased;
        reason = close.closed() => InboundRead::CloseRequested(reason),
        () = &mut wait_for_deadline => InboundRead::DeadlineElapsed,
        output = &mut read => InboundRead::Completed(output),
    }
}

async fn run_until_close<F>(
    close: &mut ConnectionCloseListener,
    operation: F,
) -> Option<Option<CloseReason>>
where
    F: Future<Output = ()>,
{
    tokio::pin!(operation);
    tokio::select! {
        biased;
        reason = close.closed() => Some(reason),
        () = &mut operation => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InboundDeadlineKind {
    Authentication,
    Idle,
}

impl InboundDeadlineKind {
    const fn close_reason(self) -> CloseReason {
        match self {
            Self::Authentication => CloseReason::AuthTimeout,
            Self::Idle => CloseReason::IdleTimeout,
        }
    }

    const fn error_code(self) -> ErrorCode {
        match self {
            Self::Authentication => ErrorCode::AuthenticationTimeout,
            Self::Idle => ErrorCode::ConnectionIdleTimeout,
        }
    }

    fn error_message(self, timeout_secs: u64) -> String {
        match self {
            Self::Authentication => {
                format!("Authentication timeout - must authenticate within {timeout_secs} seconds")
            }
            Self::Idle => {
                format!("Idle timeout - no messages received for {timeout_secs} seconds")
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct InboundDeadline {
    at: Option<Instant>,
    kind: InboundDeadlineKind,
    timeout_secs: u64,
}

impl InboundDeadline {
    fn for_connection(
        app_handshake_complete: bool,
        auth_deadline: Instant,
        auth_timeout_secs: u64,
        idle_timeout: Option<Duration>,
        idle_timeout_secs: u64,
    ) -> Self {
        if app_handshake_complete {
            Self {
                at: idle_timeout.and_then(|window| deadline_after(Instant::now(), window)),
                kind: InboundDeadlineKind::Idle,
                timeout_secs: idle_timeout_secs,
            }
        } else {
            Self {
                at: Some(auth_deadline),
                kind: InboundDeadlineKind::Authentication,
                timeout_secs: auth_timeout_secs,
            }
        }
    }

    async fn read<F>(self, close: &mut ConnectionCloseListener, read: F) -> InboundRead<F::Output>
    where
        F: Future,
    {
        read_before_deadline(close, self.at, read).await
    }

    fn expire(
        self,
        tx: &OutboundSender,
        close_signal: &ConnectionCloseSignal,
        player_id: &PlayerId,
    ) {
        enqueue_farewell_message(
            tx,
            close_signal,
            player_id,
            ServerMessage::Error {
                message: self.kind.error_message(self.timeout_secs),
                error_code: Some(self.kind.error_code()),
            },
            match self.kind {
                InboundDeadlineKind::Authentication => "authentication timeout",
                InboundDeadlineKind::Idle => "idle timeout error",
            },
        );
        close_signal.request_close(self.kind.close_reason());
    }
}

fn ping_write_timeout_policy(
    outbound_advanced: bool,
    max_sojourn: Duration,
    slow_consumer_timeout: Duration,
) -> (Duration, CloseReason) {
    if outbound_advanced {
        (
            max_sojourn.min(slow_consumer_timeout),
            CloseReason::SlowConsumer,
        )
    } else {
        (SERVER_PING_WRITE_TIMEOUT, CloseReason::ActivityTimeout)
    }
}

#[derive(Debug)]
enum PingWriteFailure<E> {
    Socket(E),
    DeadlineElapsed,
}

#[allow(clippy::too_many_arguments)]
async fn complete_ping_write<F, E>(
    write_started_at: Instant,
    timeout: Duration,
    timeout_reason: CloseReason,
    write: F,
    probe_state: &watch::Sender<PingProbeState>,
    nonce: u64,
    close_signal: &ConnectionCloseSignal,
    server: &EnhancedGameServer,
) -> Result<PingWriteTiming, PingWriteFailure<E>>
where
    F: Future<Output = Result<(), E>>,
{
    match complete_before_deadline(checked_deadline(write_started_at, timeout), write).await {
        Ok(Ok(())) => Ok(PingWriteTiming {
            completed_at: Instant::now(),
            outbound_generation: probe_state.borrow().outbound_generation,
        }),
        Ok(Err(error)) => {
            clear_ping_probe(probe_state, nonce);
            if close_signal.request_close(timeout_reason)
                && timeout_reason == CloseReason::SlowConsumer
            {
                server
                    .metrics()
                    .increment_websocket_slow_consumer_disconnects();
            }
            Err(PingWriteFailure::Socket(error))
        }
        Err(_) => {
            clear_ping_probe(probe_state, nonce);
            if close_signal.request_close(timeout_reason)
                && timeout_reason == CloseReason::SlowConsumer
            {
                server
                    .metrics()
                    .increment_websocket_slow_consumer_disconnects();
            }
            Err(PingWriteFailure::DeadlineElapsed)
        }
    }
}

fn begin_ping_probe(
    state: &watch::Sender<PingProbeState>,
    baseline_generation: u64,
    nonce: u64,
    started_at: Instant,
) -> Result<(), u64> {
    let mut result = Err(baseline_generation);
    state.send_modify(|current| {
        if current.inbound_generation == baseline_generation
            && !current.inbound_processing
            && current.active.is_none()
        {
            current.active = Some(ActivePingProbe {
                nonce,
                started_at,
                evidence: None,
            });
            result = Ok(());
        } else {
            result = Err(current.inbound_generation);
        }
    });
    result
}

fn record_inbound_probe_activity(state: &watch::Sender<PingProbeState>, received_at: Instant) {
    state.send_modify(|current| {
        current.inbound_generation = current.inbound_generation.wrapping_add(1);
        current.inbound_processing = true;
        if let Some(active) = current.active.as_mut() {
            if active.evidence.is_none() && received_at >= active.started_at {
                active.evidence = Some(PingProbeEvidence::InboundActivity { received_at });
            }
        }
    });
}

pub(super) fn record_outbound_probe_activity(
    state: &watch::Sender<PingProbeState>,
    completed_at: Instant,
) {
    state.send_modify(|current| {
        current.outbound_generation = current.outbound_generation.wrapping_add(1);
        if let Some(active) = current.active.as_mut() {
            if active.evidence.is_none() && completed_at >= active.started_at {
                active.evidence = Some(PingProbeEvidence::OutboundActivity { completed_at });
            }
        }
    });
}

struct InboundProbeActivityGuard {
    state: watch::Sender<PingProbeState>,
}

impl InboundProbeActivityGuard {
    fn begin(state: &watch::Sender<PingProbeState>, received_at: Instant) -> Self {
        record_inbound_probe_activity(state, received_at);
        Self {
            state: state.clone(),
        }
    }
}

impl Drop for InboundProbeActivityGuard {
    fn drop(&mut self) {
        self.state.send_if_modified(|current| {
            if current.inbound_processing {
                current.inbound_processing = false;
                true
            } else {
                false
            }
        });
    }
}

fn try_record_matching_pong(
    state: &watch::Sender<PingProbeState>,
    nonce: u64,
    received_at: Instant,
) -> bool {
    let mut recorded = false;
    state.send_if_modified(|current| {
        let Some(active) = current.active.as_mut() else {
            return false;
        };
        if active.nonce != nonce || active.evidence.is_some() || received_at < active.started_at {
            return false;
        }
        active.evidence = Some(PingProbeEvidence::MatchingPong { received_at });
        recorded = true;
        true
    });
    recorded
}

fn clear_ping_probe(state: &watch::Sender<PingProbeState>, nonce: u64) {
    state.send_if_modified(|current| {
        if current.active.is_some_and(|active| active.nonce == nonce) {
            current.active = None;
            true
        } else {
            false
        }
    });
}

fn resolve_ping_probe(
    state: &watch::Sender<PingProbeState>,
    nonce: u64,
    deadline_at: Instant,
    deadline_reached: bool,
) -> Option<PingProbeResolution> {
    let mut resolution = None;
    state.send_if_modified(|current| {
        let Some(active) = current.active else {
            return false;
        };
        if active.nonce != nonce {
            return false;
        }

        resolution = match active.evidence {
            Some(PingProbeEvidence::MatchingPong { received_at }) if received_at <= deadline_at => {
                Some(PingProbeResolution::MatchingPong(
                    received_at.duration_since(active.started_at),
                ))
            }
            Some(PingProbeEvidence::InboundActivity { received_at })
                if received_at <= deadline_at =>
            {
                Some(PingProbeResolution::InboundActivity {
                    generation: current.inbound_generation,
                })
            }
            Some(PingProbeEvidence::OutboundActivity { completed_at })
                if completed_at <= deadline_at =>
            {
                Some(PingProbeResolution::OutboundActivity {
                    generation: current.outbound_generation,
                })
            }
            _ if deadline_reached => Some(PingProbeResolution::TimedOut),
            _ => None,
        };
        if resolution.is_some() {
            current.active = None;
            true
        } else {
            false
        }
    });
    resolution
}

/// Enqueue a message on this connection's own outbound queue, honoring the
/// server-wide no-silent-drop contract: backpressure while the queue is
/// momentarily full, then a loud slow-consumer close if it never drains.
/// Used for control messages (auth responses, protocol info, timeout errors)
/// that are sent outside the message coordinator.
async fn enqueue_connection_message(
    tx: &OutboundSender,
    close_signal: &ConnectionCloseSignal,
    server: &Arc<EnhancedGameServer>,
    slow_consumer_timeout: Duration,
    player_id: &PlayerId,
    message: ServerMessage,
    context: &'static str,
) -> bool {
    let handle = ClientDeliveryHandle::classified(tx.clone(), close_signal.clone());
    let metrics = server.metrics();
    let outcome = deliver_or_disconnect(
        &metrics,
        slow_consumer_timeout,
        player_id,
        &handle,
        Arc::new(message),
    )
    .await;
    if outcome != DeliveryOutcome::Delivered {
        tracing::warn!(
            %player_id,
            ?outcome,
            context,
            "Connection control message was not delivered"
        );
    }
    outcome == DeliveryOutcome::Delivered
}

/// Best-effort enqueue of a pre-close farewell on this connection's own queue.
///
/// Never waits and never escalates to a slow-consumer close: the caller is
/// about to terminate the connection, and the close itself (with its
/// lifecycle reason) is the authoritative, loud signal — this frame is
/// advisory. The send task's final drain flushes it when the queue has room.
fn enqueue_farewell_message(
    tx: &OutboundSender,
    close_signal: &ConnectionCloseSignal,
    player_id: &PlayerId,
    message: ServerMessage,
    context: &'static str,
) {
    #[cfg(feature = "trace-validation")]
    close_signal.record_trace(
        crate::trace_validation::DeliveryTraceAction::Unsupported,
        None,
        Some("direct-v2-farewell-enqueue"),
    );
    #[cfg(not(feature = "trace-validation"))]
    let _ = close_signal;
    match tx.try_enqueue_control(Arc::new(message)) {
        Ok(_) => {}
        Err(TryEnqueueError::Full(_, _)) => {
            tracing::debug!(
                %player_id,
                context,
                "Farewell not enqueued: outbound queue full on a closing connection"
            );
        }
        Err(
            TryEnqueueError::Closed(_)
            | TryEnqueueError::AccountabilityUnavailable(_)
            | TryEnqueueError::InvalidMetadata(_),
        ) => {
            tracing::debug!(%player_id, context, "Farewell not enqueued: connection already closed");
        }
    }
}

fn resolve_final_close_reason(
    observed_reason: Option<CloseReason>,
    close_listener: &ConnectionCloseListener,
) -> Option<CloseReason> {
    match close_listener.requested_reason() {
        Some(CloseReason::Shutdown) => Some(CloseReason::Shutdown),
        current_reason => observed_reason.or(current_reason),
    }
}

fn close_frame_reason_for_server(
    reason: Option<CloseReason>,
    server: &EnhancedGameServer,
) -> CloseReason {
    if server.is_draining() {
        CloseReason::Shutdown
    } else {
        reason.unwrap_or(CloseReason::Unregistered)
    }
}

async fn registered_close_write_timeout<F, T>(
    _step: RegisteredConnectionCloseStep,
    operation: F,
) -> Result<T, tokio::time::error::Elapsed>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(CLOSE_WRITE_TIMEOUT, operation).await
}

/// Write whatever exact omission accounting is still coalesced for this
/// recipient, once no further frame will carry it.
///
/// Best-effort by nature — a wedged socket is one reason this path runs — but on
/// a healthy teardown it means the last burst of undeliverable data is still
/// reported rather than dying with the connection. Callers must invoke this
/// after the last queued write on their path, because each write can discover
/// further undeliverable payloads whose advisory (and therefore whose report
/// flush) the rate limiter may suppress.
async fn flush_pending_unsupported_report(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    rx: &OutboundReceiver,
    player_id: &PlayerId,
    max_outbound_message_size: usize,
) -> bool {
    // `DeliveryReport` is v3-only and `send_immediate_server_message` carries
    // no recipient version, so this final flush fail-closes like the live
    // write path (`websocket::sending::write_pending_unsupported_report`).
    // Pending ranges can only exist on a queue that negotiated v3 (record-time
    // accumulation gate plus the Authenticate-first rule), so the gate is
    // unreachable defense: a violation leaves the ranges pending rather than
    // leaking the frame onto a v2 wire.
    if !rx.supports_v3() {
        return false;
    }
    let Some(report) = rx.pending_unsupported_report() else {
        return false;
    };
    // Counted as its own step in `RegisteredConnectionCloseStep`, so the derived
    // shutdown settle timeout covers this budget too. The ranges are retired
    // only after the frame is written, so a timed-out or failed write leaves the
    // accounting recorded rather than silently dropped.
    match registered_close_write_timeout(
        RegisteredConnectionCloseStep::FinalDeliveryReport,
        send_immediate_server_message(
            sender,
            &ServerMessage::DeliveryReport(Box::new(report.clone())),
            max_outbound_message_size,
        ),
    )
    .await
    {
        Ok(Ok(())) => {
            rx.commit_pending_unsupported_report(&report);
            false
        }
        Ok(Err(ImmediateSendError::MessageTooLarge { size, max })) => {
            tracing::warn!(%player_id, size, max, "Final delivery report exceeds outbound message-size limit");
            true
        }
        Ok(Err(err)) => {
            tracing::debug!(%player_id, error = %err, "Failed to flush final delivery report");
            false
        }
        Err(_elapsed) => {
            tracing::debug!(%player_id, "Timed out flushing final delivery report");
            false
        }
    }
}

fn promote_normal_close_to_outbound_too_large(reason: &mut Option<CloseReason>) {
    if matches!(reason, None | Some(CloseReason::Unregistered)) {
        *reason = Some(CloseReason::OutboundMessageTooLarge);
    }
}

/// Final actions of the send task once a server-side close was requested.
///
/// - Slow consumer: the queue contents are abandoned **by design** (the
///   recipient proved unable to drain them); they are counted as dropped, and
///   a best-effort farewell error tells the client why it is being closed.
/// - Unregistration (or all delivery handles dropped): the recipient is
///   healthy — flush whatever is already queued (e.g. a final error emitted
///   just before unregistering), then close.
async fn finalize_closed_connection(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    rx: &mut OutboundReceiver,
    batcher: &mut MessageBatcher,
    reason: Option<CloseReason>,
    player_id: &PlayerId,
    server: &Arc<EnhancedGameServer>,
    close_signal: &ConnectionCloseSignal,
    ping_probe_state: &watch::Sender<PingProbeState>,
    max_sojourn: Duration,
) {
    let mut terminal_reason = reason;
    #[cfg(feature = "trace-validation")]
    if reason.is_none() {
        // Dropping every delivery handle closes the receiver without an
        // explicit watch reason. The trace abstraction makes that implicit
        // Open -> CloseRequested boundary visible before replaying teardown.
        close_signal.record_trace(
            crate::trace_validation::DeliveryTraceAction::LifecycleClose,
            None,
            Some("implicit-unregistration"),
        );
    }
    // Freeze the producer set before inspecting or draining queued state. A
    // stale routing snapshot may still hold a sender clone, but it can no
    // longer enqueue behind the teardown's final accounting snapshot.
    rx.close();
    #[cfg(feature = "trace-validation")]
    close_signal.record_trace_queue_closed();

    match reason {
        Some(CloseReason::SlowConsumer) => {
            // Nothing more will be written from the queue on this path — it is
            // abandoned below — so the coalesced omissions are flushed here,
            // before the counters that make the farewell terminal.
            flush_pending_unsupported_report(
                sender,
                rx,
                player_id,
                server.config().max_outbound_message_size,
            )
            .await;
            // `send_batch` pops messages one at a time, so a cancelled
            // in-flight write leaves everything unsent inside the batcher;
            // the count below misses at most the single message that was
            // actively (partially) on the wire when the close fired.
            let abandoned = rx.len().saturating_add(batcher.len());
            record_abandoned_by_class(rx, batcher);
            server
                .metrics()
                .add_websocket_messages_dropped(abandoned as u64);
            tracing::warn!(
                %player_id,
                abandoned_messages = abandoned,
                "Closing slow-consumer connection; abandoning its undeliverable queue"
            );

            let farewell = ServerMessage::Error {
                message: format!(
                    "Disconnected because outbound delivery could not make accountable progress \
                     (backpressure limit {} ms; maximum sojourn {} ms)",
                    server.config().websocket_config.slow_consumer_timeout_ms,
                    server.config().websocket_config.max_sojourn_ms,
                ),
                error_code: Some(ErrorCode::SlowConsumer),
            };
            match tokio::time::timeout(
                CLOSE_WRITE_TIMEOUT,
                send_immediate_server_message(
                    sender,
                    &farewell,
                    server.config().max_outbound_message_size,
                ),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::debug!(%player_id, error = %err, "Failed to write slow-consumer farewell frame");
                }
                Err(_elapsed) => {
                    tracing::debug!(%player_id, "Timed out writing slow-consumer farewell frame");
                }
            }
        }
        Some(CloseReason::OutboundMessageTooLarge) => {
            // The triggering application message was rejected before any
            // socket write. Later queued messages belong to state after a gap
            // the client cannot observe, so abandon them and close with 1009.
            let abandoned = rx.len().saturating_add(batcher.len());
            record_abandoned_by_class(rx, batcher);
            server
                .metrics()
                .add_websocket_messages_dropped(abandoned as u64);
            tracing::warn!(
                %player_id,
                abandoned_messages = abandoned,
                "Closing after oversized outbound message; abandoning later queued messages"
            );
            // An oversized queued report carrier dies before its own
            // post-write flush, so coalesced omissions can still be pending
            // and writable here. They remain exact and describe frames this
            // recipient already saw skipped; a report never advances the data
            // sequence, so flushing them cannot open a hole of its own.
            flush_pending_unsupported_report(
                sender,
                rx,
                player_id,
                server.config().max_outbound_message_size,
            )
            .await;
        }
        Some(
            CloseReason::Shutdown
            | CloseReason::AuthTimeout
            | CloseReason::ActivityTimeout
            | CloseReason::IdleTimeout
            | CloseReason::RoomInactive
            | CloseReason::Unregistered,
        )
        | None
            if rx.abandoned_in_flight_write() =>
        {
            // A queued payload was abandoned while a socket write owned it, so
            // its wire position is unknown (the sink may have taken the frame
            // into its own buffer before the close cancelled the write, or the
            // cancellation may have landed first). Everything still queued sits
            // BEHIND that payload, so flushing it here is exactly how a
            // recipient ends up observing a delivered sequence that skips one
            // it was never told about — an unexplained hole no `DeliveryReport`
            // covers. Abandon the remainder instead: a gap-free prefix that
            // stops early is a legal stream, a hole is not.
            let abandoned = rx.len().saturating_add(batcher.len());
            record_abandoned_by_class(rx, batcher);
            server
                .metrics()
                .add_websocket_messages_dropped(abandoned as u64);
            tracing::debug!(
                %player_id,
                abandoned_messages = abandoned,
                "Socket write was abandoned in flight while closing; abandoning the queue behind it \
                 rather than writing past an unaccountable sequence"
            );
            // The coalesced omissions are still exact and still describe frames
            // this recipient already saw skipped, so they are written after the
            // abandonment snapshot above — a report never advances the data
            // sequence and so can never open a hole of its own.
            flush_pending_unsupported_report(
                sender,
                rx,
                player_id,
                server.config().max_outbound_message_size,
            )
            .await;
        }
        Some(
            CloseReason::Shutdown
            | CloseReason::AuthTimeout
            | CloseReason::ActivityTimeout
            | CloseReason::IdleTimeout
            | CloseReason::RoomInactive
            | CloseReason::Unregistered,
        )
        | None => {
            // Drain whatever is already buffered and flush it, bounded by the
            // close-write budget: an unregistered connection is usually
            // healthy (flush completes in milliseconds), but if its socket is
            // wedged this must not pin the task — the whole point of the
            // close path is to reclaim the connection. Both terminal channel
            // states end the drain: `Empty` means the flush is complete, and
            // `Disconnected` means every delivery handle is gone AND the
            // buffer is empty — also a completed flush.
            let flush = registered_close_write_timeout(
                RegisteredConnectionCloseStep::FlushQueuedMessages,
                async {
                    if !batcher.is_empty() {
                        send_batch(
                            sender,
                            batcher,
                            rx,
                            player_id,
                            server,
                            close_signal,
                            ping_probe_state,
                            max_sojourn,
                            WritePhase::CloseFlush,
                        )
                        .await?;
                    }
                    // Not a `while let`: the repo-wide try_recv policy
                    // (tests/async_timeout_policy_scan.rs) requires both terminal
                    // channel states to be matched explicitly.
                    loop {
                        match rx.try_recv() {
                            Ok(message) => {
                                send_queued(
                                    sender,
                                    message,
                                    None,
                                    rx,
                                    player_id,
                                    server,
                                    close_signal,
                                    ping_probe_state,
                                    max_sojourn,
                                    WritePhase::CloseFlush,
                                )
                                .await?;
                            }
                            Err(TryReceiveError::Empty | TryReceiveError::Disconnected) => break,
                            Err(TryReceiveError::AccountabilityFailed) => {
                                return Err(QueueWriteError::AccountabilityFailed);
                            }
                        }
                    }
                    Ok(())
                },
            )
            .await;
            let flush_timed_out = flush.is_err();
            if matches!(&flush, Ok(Err(QueueWriteError::OutboundMessageTooLarge))) {
                promote_normal_close_to_outbound_too_large(&mut terminal_reason);
            }
            match flush {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => {
                    // Whatever the flush could not deliver dies with the
                    // connection. `SendAccounting::drop` already accounts for
                    // the one item owned by an interrupted socket write; this
                    // snapshot covers only the queued and batched remainder.
                    let abandoned = rx.len().saturating_add(batcher.len());
                    record_abandoned_by_class(rx, batcher);
                    server
                        .metrics()
                        .add_websocket_messages_dropped(abandoned as u64);
                    tracing::debug!(
                        %player_id,
                        abandoned_messages = abandoned,
                        flush_timed_out,
                        "Flush failed while closing unregistered connection; abandoning remainder"
                    );
                }
            }
            // After the drain, not before it: the drain itself writes queued
            // items and can discover further undeliverable payloads, and a
            // suppressed advisory leaves those coalesced. Flushing first would
            // leave exactly the tail this change exists to preserve unreported.
            if flush_pending_unsupported_report(
                sender,
                rx,
                player_id,
                server.config().max_outbound_message_size,
            )
            .await
            {
                promote_normal_close_to_outbound_too_large(&mut terminal_reason);
            }
        }
    }

    // Semantic close frame (issue #136, F1): the farewell `Error` above is
    // best-effort and may never survive the congested socket it escapes, but
    // the close frame's code travels in the closing handshake itself, so a
    // client that observes only the stream termination can still attribute
    // it (4000 shutdown, 4001 auth timeout, 4002 slow consumer, 4003
    // activity timeout, 4004 idle timeout, 4005 inactive-room cleanup, or
    // standard 1009 for an oversized outbound message; plain unregistration
    // closes with a normal 1000).
    // A `None` reason — every close signal clone dropped without an explicit
    // request, i.e. the connection was simply unregistered everywhere — is
    // the same normal closure, so the coded frame is TOTAL over server-side
    // teardowns: no path falls back to a bare, code-less close.
    let reason = close_frame_reason_for_server(terminal_reason, server);
    let close_frame = Message::Close(Some(axum::extract::ws::CloseFrame {
        code: reason.websocket_close_code(),
        reason: reason.close_frame_reason().into(),
    }));
    match registered_close_write_timeout(
        RegisteredConnectionCloseStep::SemanticCloseFrame,
        sender.send(close_frame),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::debug!(%player_id, error = %err, "Failed to write semantic close frame");
        }
        Err(_elapsed) => {
            tracing::debug!(%player_id, "Timed out writing semantic close frame");
        }
    }

    match registered_close_write_timeout(RegisteredConnectionCloseStep::SinkClose, sender.close())
        .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::debug!(%player_id, error = %err, "WebSocket close handshake failed");
        }
        Err(_elapsed) => {
            tracing::debug!(%player_id, "Timed out closing WebSocket sink");
        }
    }

    #[cfg(feature = "trace-validation")]
    close_signal.record_trace(
        crate::trace_validation::DeliveryTraceAction::CloseFinish,
        None,
        None,
    );
}

fn record_abandoned_by_class(rx: &OutboundReceiver, batcher: &MessageBatcher) {
    let queued = rx.count_by_class();
    let batched = batcher.count_by_class();
    for ((class, queued_count), (_, batched_count)) in queued.into_iter().zip(batched) {
        let count = queued_count.saturating_add(batched_count);
        if count > 0 {
            rx.record_abandoned(class, count);
        }
    }
}

/// Resolve the negotiated transport/topology capability sets for a connection.
///
/// Relay is the universal floor, so it is always present in both sets. A
/// connection negotiated below v3 is **relay-only** regardless of what it
/// advertised — peer-to-peer transports/topologies are a v3+ upgrade and must
/// never be recorded for a v2 peer. For v3+, the client's advertised sets are
/// used with `Relay` forced in (if the client omitted it) and duplicates
/// removed while preserving the client's stated preference order.
fn negotiate_capabilities(
    negotiated_version: u16,
    supported_transports: Option<Vec<Transport>>,
    supported_topologies: Option<Vec<Topology>>,
) -> (Vec<Transport>, Vec<Topology>) {
    if negotiated_version < 3 {
        return (vec![Transport::Relay], vec![Topology::Relay]);
    }

    let mut transports = supported_transports.unwrap_or_else(|| vec![Transport::Relay]);
    if !transports.contains(&Transport::Relay) {
        transports.push(Transport::Relay);
    }
    dedup_preserving_order(&mut transports);

    let mut topologies = supported_topologies.unwrap_or_else(|| vec![Topology::Relay]);
    if !topologies.contains(&Topology::Relay) {
        topologies.push(Topology::Relay);
    }
    dedup_preserving_order(&mut topologies);

    (transports, topologies)
}

/// Remove duplicate entries while preserving the first occurrence of each.
///
/// Unlike [`Vec::dedup`], which only collapses *consecutive* duplicates, this
/// drops every later repeat regardless of position. The lists it operates on
/// (negotiated transports/topologies) have at most three variants, so the
/// quadratic scan is trivially cheap and keeps the relative ordering the client
/// advertised (which conveys preference for the P3 selection logic).
fn dedup_preserving_order<T: PartialEq>(items: &mut Vec<T>) {
    let mut unique = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    *items = unique;
}

/// Merge SDK compatibility capabilities with negotiated wire extensions.
///
/// `room_operation_ids` is reserved for explicit connection negotiation. A
/// deployment cannot accidentally advertise it through the SDK compatibility
/// manifest, and a duplicate client request still produces one token.
fn protocol_info_capabilities(
    mut compatibility_capabilities: Vec<String>,
    room_operation_ids: bool,
) -> Vec<String> {
    compatibility_capabilities.retain(|capability| capability != ROOM_OPERATION_IDS_CAPABILITY);
    if room_operation_ids {
        compatibility_capabilities.push(ROOM_OPERATION_IDS_CAPABILITY.to_string());
    }
    compatibility_capabilities
}

// `default_protocol_version` is the fallback used when the client omits
// `Authenticate.protocol_version`: `2` for the `/v2/ws` path, `3` for `/v3/ws`.
pub(super) async fn handle_socket(
    socket: WebSocket,
    server: Arc<EnhancedGameServer>,
    addr: SocketAddr,
    token_binding: Option<TokenBindingHandshake>,
    default_protocol_version: u16,
) {
    let _socket_task_guard = server.track_socket_task();
    let (mut sender, mut receiver) = socket.split();
    // Validated >= 1 at startup; clamp anyway because `mpsc::channel` panics on 0.
    let queue_capacity = server.config().websocket_config.send_queue_capacity.max(1);
    // Validated >= 2 at startup: StartGame reserves GameStarting plus the
    // optional SessionPlan before its durable CAS. Clamp direct library
    // construction too so it cannot create a self-deadlocking one-slot queue.
    let control_queue_capacity = server
        .config()
        .websocket_config
        .control_queue_capacity
        .max(2);
    let slow_consumer_timeout = Duration::from_millis(
        server
            .config()
            .websocket_config
            .slow_consumer_timeout_ms
            .max(1),
    );
    let (tx, mut rx) = outbound_queue::channel_with_metrics(
        queue_capacity,
        control_queue_capacity,
        server.metrics(),
    );

    // One close signal per connection: the delivery layer requests closes
    // through its registered handle (slow consumer) or via unregistration;
    // both socket tasks listen and tear the connection down (as does the
    // optional RelayStats ticker spawned below).
    #[cfg(feature = "trace-validation")]
    let trace_output =
        std::env::var_os("SIGNAL_FISH_DELIVERY_TRACE_PATH").map(std::path::PathBuf::from);
    #[cfg(feature = "trace-validation")]
    let trace = if trace_output.is_some() {
        match crate::trace_validation::DeliveryTraceRecorder::new(
            crate::trace_validation::DeliveryTraceRecorder::next_trace_id("socket"),
            queue_capacity,
        ) {
            Ok(trace) => Some(Arc::new(trace)),
            Err(error) => {
                tracing::error!(%error, "Unable to initialize delivery trace recorder");
                return;
            }
        }
    } else {
        None
    };
    #[cfg(feature = "trace-validation")]
    let (close_signal, close_listener) = if let Some(trace) = &trace {
        ConnectionCloseSignal::channel_with_trace(Arc::clone(trace))
    } else {
        ConnectionCloseSignal::channel()
    };
    #[cfg(not(feature = "trace-validation"))]
    let (close_signal, close_listener) = ConnectionCloseSignal::channel();
    let mut send_task_close = close_listener.clone();
    let stats_task_close = close_listener.clone();
    let ping_task_close = close_listener.clone();
    let mut receive_task_close = close_listener;

    // Socket-internal ping commands are deliberately separate from both
    // application delivery lanes. At most one probe is outstanding, so one
    // slot is sufficient and cannot accumulate stale probes.
    let (ping_command_tx, mut ping_command_rx) = mpsc::channel::<ServerPingCommand>(1);
    // One coalescing O(1) state connects ticker, writer, and reader. It records
    // inbound activity before application handling can block, and retains only
    // the first evidence for the single active probe.
    let (ping_probe_state_tx, ping_probe_state_rx) = watch::channel(PingProbeState::default());
    let ping_probe_state_for_send = ping_probe_state_tx.clone();
    let server_ping_interval_secs = server.config().websocket_config.server_ping_interval_secs;
    let ping_probe_state_for_receive =
        (server_ping_interval_secs > 0).then(|| ping_probe_state_tx.clone());

    // Keep a clone of tx for sending auth responses
    let tx_clone = tx.clone();

    // Register client with server
    let player_id = match server
        .register_classified_client_with_close(tx.clone(), close_signal.clone(), addr)
        .await
    {
        Ok(player_id) => {
            tracing::info!(%player_id, client_addr = %addr, "WebSocket connection established");
            player_id
        }
        Err(
            err @ (RegisterClientError::IpLimitExceeded { .. }
            | RegisterClientError::CapacityExceeded { .. }),
        ) => {
            // The error's Display is the single source of the client-facing
            // refusal text for both admission refusals.
            refuse_websocket_registration(
                &mut sender,
                token_binding.as_ref(),
                addr,
                err.to_string(),
                server.config().max_outbound_message_size,
            )
            .await;
            return;
        }
        Err(RegisterClientError::ServerDraining) => {
            let close_frame = Message::Close(Some(axum::extract::ws::CloseFrame {
                code: CloseReason::Shutdown.websocket_close_code(),
                reason: CloseReason::Shutdown.close_frame_reason().into(),
            }));
            match tokio::time::timeout(CLOSE_WRITE_TIMEOUT, sender.send(close_frame)).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::debug!(
                        client_addr = %addr,
                        error = %err,
                        "Failed to send drain close frame for late WebSocket registration"
                    );
                }
                Err(_elapsed) => {
                    tracing::debug!(
                        client_addr = %addr,
                        "Timed out sending drain close frame for late WebSocket registration"
                    );
                }
            }
            match tokio::time::timeout(CLOSE_WRITE_TIMEOUT, sender.close()).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::debug!(
                        client_addr = %addr,
                        error = %err,
                        "Failed to close late shutdown WebSocket registration"
                    );
                }
                Err(_elapsed) => {
                    tracing::debug!(
                        client_addr = %addr,
                        "Timed out closing late shutdown WebSocket registration"
                    );
                }
            }
            return;
        }
    };
    // Registration remains the admission boundary, but a token-bound client
    // still receives the challenge before every application message or error.
    if !send_token_binding_challenge(
        &mut sender,
        token_binding.as_ref(),
        addr,
        server.config().max_outbound_message_size,
    )
    .await
    {
        server.unregister_client(&player_id).await;
        return;
    }
    let reconnection_identity = token_binding
        .as_ref()
        .filter(|binding| binding.verifier.require_fingerprint)
        .and_then(|binding| binding.fingerprint.as_ref())
        .map(|fingerprint| Arc::clone(&fingerprint.fingerprint));
    server.set_client_reconnection_identity(&player_id, reconnection_identity);

    // Open app-ID-policy endpoints normally use their path version immediately. A
    // deployment floor above that endpoint cannot be satisfied without lying
    // about the client's capabilities, so reject before starting socket tasks.
    if !server.config().app_id_allowlist_enabled
        && default_protocol_version < server.protocol_config().min_protocol_version
    {
        let minimum = server.protocol_config().min_protocol_version;
        let error = ServerMessage::AuthenticationError {
            error: format!(
                "Endpoint protocol version {default_protocol_version} is below the server minimum {minimum}"
            ),
            error_code: ErrorCode::UnsupportedProtocolVersion,
        };
        let send_result = tokio::time::timeout(
            CLOSE_WRITE_TIMEOUT,
            send_immediate_server_message(
                &mut sender,
                &error,
                server.config().max_outbound_message_size,
            ),
        )
        .await;
        if let Ok(Err(error)) = &send_result {
            handle_immediate_send_error(&mut sender, error).await;
        }
        let _ = tokio::time::timeout(CLOSE_WRITE_TIMEOUT, sender.close()).await;
        server.unregister_client(&player_id).await;
        return;
    }

    // Track the frozen wire handshake state; open policy starts complete.
    let mut app_handshake_complete = !server.config().app_id_allowlist_enabled;
    let mut authenticate_processed = false;
    let mut received_application_message = false;
    // Capability publication can precede the two handshake responses in the
    // open app-ID-policy endpoint-default path. Delivery advisories must wait until
    // both responses are queued; clients cannot interpret v3 accountability
    // before ProtocolInfo establishes the negotiated mode.
    let protocol_handshake_complete = Arc::new(AtomicBool::new(false));

    // With an open app-ID policy, legacy clients may skip Authenticate entirely. In
    // that mode the endpoint default still applies, so `/v3/ws` starts as v3
    // relay-only while `/v2/ws` remains pure v2. A later first Authenticate can
    // still refine transports/topologies.
    if app_handshake_complete {
        let cfg = server.protocol_config();
        let negotiated_version = cfg.negotiate_protocol_version(Some(default_protocol_version));
        let (negotiated_transports, negotiated_topologies) =
            negotiate_capabilities(negotiated_version, None, None);
        server.set_client_protocol(
            &player_id,
            NegotiatedProtocol {
                version: negotiated_version,
                transports: negotiated_transports,
                topologies: negotiated_topologies,
            },
        );
    }

    // Track connection time for authentication timeout
    let connection_start = Instant::now();
    let auth_timeout = Duration::from_secs(server.config().websocket_config.auth_timeout_secs);

    let effective_player_id = Arc::new(RwLock::new(player_id));
    let Some(connection_lifecycle) = server.client_lifecycle(&player_id) else {
        tracing::warn!(%player_id, "Registered connection disappeared before socket tasks started");
        return;
    };

    if server_ping_interval_secs > 0 {
        let pong_timeout = Duration::from_secs(server.config().websocket_config.pong_timeout_secs);
        let ping_commands = ping_command_tx.clone();
        let ping_close_signal = close_signal.clone();
        let mut ping_task_close = ping_task_close;
        let mut probe_states = ping_probe_state_rx;
        let probe_state_updates = ping_probe_state_tx.clone();
        let server_for_ping = server.clone();
        let effective_player_id_for_ping = Arc::clone(&effective_player_id);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(server_ping_interval_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let initial_state = *probe_states.borrow();
            let mut consumed_generation = initial_state.inbound_generation;
            let mut consumed_outbound_generation = initial_state.outbound_generation;
            // Start probing after one full configured interval.
            ticker.tick().await;
            loop {
                tokio::select! {
                    reason = ping_task_close.closed() => {
                        tracing::debug!(?reason, "Connection closing; ending WebSocket ping probes");
                        break;
                    }
                    _ = ticker.tick() => {}
                }

                let current_state = *probe_states.borrow_and_update();
                let observed_generation = current_state.inbound_generation;
                let observed_outbound_generation = current_state.outbound_generation;
                if current_state.inbound_processing || observed_generation != consumed_generation {
                    consumed_generation = observed_generation;
                    consumed_outbound_generation = observed_outbound_generation;
                    server_for_ping
                        .metrics()
                        .increment_websocket_ping_probes_skipped_activity();
                    continue;
                }

                let nonce = random_ping_nonce();
                let (write_outcome_tx, write_outcome_rx) = oneshot::channel();
                let command = ServerPingCommand {
                    nonce,
                    baseline_generation: consumed_generation,
                    baseline_outbound_generation: consumed_outbound_generation,
                    write_outcome: write_outcome_tx,
                };
                tokio::select! {
                    reason = ping_task_close.closed() => {
                        tracing::debug!(?reason, "Connection closing before WebSocket Ping write");
                        break;
                    }
                    result = ping_commands.send(command) => {
                        if result.is_err() {
                            break;
                        }
                    }
                }
                let write_outcome = tokio::select! {
                    reason = ping_task_close.closed() => {
                        tracing::debug!(?reason, "Connection closing during WebSocket Ping write");
                        break;
                    }
                    result = write_outcome_rx => {
                        let Ok(write_outcome) = result else {
                            break;
                        };
                        write_outcome
                    }
                };
                let write_timing = match write_outcome {
                    PingWriteOutcome::Written(timing) => timing,
                    PingWriteOutcome::SkippedActivity {
                        inbound_generation,
                        outbound_generation,
                    } => {
                        consumed_generation = inbound_generation;
                        consumed_outbound_generation = outbound_generation;
                        probe_states.borrow_and_update();
                        server_for_ping
                            .metrics()
                            .increment_websocket_ping_probes_skipped_activity();
                        continue;
                    }
                };
                if write_timing.outbound_generation != consumed_outbound_generation {
                    // The Ping was still written so a read-only client can
                    // return an automatic Pong and refresh the independent
                    // inbound-activity reaper. Do not enforce this particular
                    // Pong deadline: application output completed since the
                    // previous probe boundary and may still be ahead of the
                    // Ping on a constrained connection.
                    clear_ping_probe(&probe_state_updates, nonce);
                    consumed_outbound_generation = write_timing.outbound_generation;
                    probe_states.borrow_and_update();
                    server_for_ping
                        .metrics()
                        .increment_websocket_ping_probes_cancelled_activity();
                    continue;
                }

                let deadline_at = checked_deadline(write_timing.completed_at, pong_timeout);
                let deadline = tokio::time::sleep_until(deadline_at);
                tokio::pin!(deadline);
                let resolution = loop {
                    if let Some(resolution) =
                        resolve_ping_probe(&probe_state_updates, nonce, deadline_at, false)
                    {
                        break resolution;
                    }
                    tokio::select! {
                        reason = ping_task_close.closed() => {
                            tracing::debug!(?reason, "Connection closing while awaiting WebSocket Pong");
                            return;
                        }
                        changed = probe_states.changed() => {
                            if changed.is_err() {
                                return;
                            }
                        }
                        () = &mut deadline => {
                            break resolve_ping_probe(
                                &probe_state_updates,
                                nonce,
                                deadline_at,
                                true,
                            )
                            .unwrap_or(PingProbeResolution::TimedOut);
                        }
                    }
                };
                match resolution {
                    PingProbeResolution::MatchingPong(rtt) => {
                        server_for_ping
                            .metrics()
                            .record_websocket_ping_rtt(rtt)
                            .await;
                    }
                    PingProbeResolution::InboundActivity { generation } => {
                        consumed_generation = generation;
                        server_for_ping
                            .metrics()
                            .increment_websocket_ping_probes_cancelled_activity();
                    }
                    PingProbeResolution::OutboundActivity { generation } => {
                        consumed_outbound_generation = generation;
                        server_for_ping
                            .metrics()
                            .increment_websocket_ping_probes_cancelled_activity();
                    }
                    PingProbeResolution::TimedOut => {
                        if ping_close_signal.request_close(CloseReason::ActivityTimeout) {
                            let current_player_id = *effective_player_id_for_ping.read().await;
                            tracing::info!(
                                %current_player_id,
                                timeout_secs = pong_timeout.as_secs(),
                                "WebSocket Pong timeout - closing connection"
                            );
                            server_for_ping
                                .metrics()
                                .increment_websocket_ping_timeouts();
                        }
                        return;
                    }
                }
            }
        });
    } else {
        drop(ping_task_close);
        drop(ping_probe_state_rx);
    }

    // Periodic per-connection RelayStats emission (protocol v3, opt-in via
    // `websocket.delivery_stats_interval_secs`; default 0 = disabled, so no
    // task is spawned at all). The v3 gate is enforced at EMISSION on every
    // tick — a pre-v3 connection on a stats-enabled deployment never observes
    // the frame — and re-reads the effective player id so the ticker follows
    // a reconnection reassignment.
    let delivery_stats_interval_secs = server
        .config()
        .websocket_config
        .delivery_stats_interval_secs;
    if delivery_stats_interval_secs > 0 {
        let server_for_stats = server.clone();
        let stats_tx = tx_clone.clone();
        let effective_player_id_for_stats = Arc::clone(&effective_player_id);
        let protocol_handshake_complete_for_stats = Arc::clone(&protocol_handshake_complete);
        let mut stats_task_close = stats_task_close;
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(Duration::from_secs(delivery_stats_interval_secs));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // An interval's first tick resolves immediately; consume it so
            // frames arrive one full interval after the connection starts.
            ticker.tick().await;
            loop {
                tokio::select! {
                    reason = stats_task_close.closed() => {
                        tracing::debug!(?reason, "Connection closing; ending RelayStats emission");
                        break;
                    }
                    _ = ticker.tick() => {
                        if !protocol_handshake_complete_for_stats.load(Ordering::Acquire) {
                            continue;
                        }
                        let current_player_id = *effective_player_id_for_stats.read().await;
                        if !server_for_stats.client_supports_v3(&current_player_id) {
                            continue;
                        }
                        let Some(stats) = server_for_stats
                            .metrics()
                            .connection_delivery_stats(&current_player_id)
                        else {
                            continue;
                        };
                        let message = ServerMessage::RelayStats {
                            interval_ms: delivery_stats_interval_secs.saturating_mul(1_000),
                            sent_to_you: stats.sent_to_you.load(Ordering::Relaxed),
                            dropped_for_you: stats.dropped_for_you.load(Ordering::Relaxed),
                            backpressure_events: stats
                                .backpressure_events
                                .load(Ordering::Relaxed),
                        };
                        // Advisory frame on the connection's own queue: the
                        // counters are cumulative, so a frame skipped under
                        // load loses nothing — never wait on (or escalate
                        // over) a full queue, and never count the frame in
                        // the statistics it reports.
                        // Queue both atomically with the report trailing. With
                        // one available slot only the report is queued; with
                        // two, RelayStats precedes it. Exact future gaps can
                        // therefore append without an advisory causing a
                        // healthy connection to fail closed.
                        match stats_tx.try_enqueue_delivery_advisories(Arc::new(message)) {
                            Ok(true) => {}
                            Ok(false) => {
                                tracing::debug!(
                                    %current_player_id,
                                    "RelayStats skipped: preserving causal report capacity"
                                );
                            }
                            Err(TryEnqueueError::Full(_, _)) => {
                                tracing::debug!(
                                    %current_player_id,
                                    "Delivery advisories skipped: control queue full"
                                );
                            }
                            Err(
                                TryEnqueueError::Closed(_)
                                | TryEnqueueError::AccountabilityUnavailable(_)
                                | TryEnqueueError::InvalidMetadata(_),
                            ) => break,
                        }
                    }
                }
            }
        });
    } else {
        // No ticker: the pre-cloned listener is simply unused (a watch
        // receiver clone; dropping it here keeps intent explicit).
        drop(stats_task_close);
    }

    // Spawn task to handle outgoing messages
    let server_clone = server.clone();
    let effective_player_id_for_send = Arc::clone(&effective_player_id);
    let lifecycle_for_send = Arc::clone(&connection_lifecycle);
    let send_task_close_signal = close_signal.clone();
    #[cfg(feature = "trace-validation")]
    let trace_for_output = trace;
    #[cfg(feature = "trace-validation")]
    let trace_output_for_send = trace_output;
    let mut send_task = tokio::spawn(async move {
        let config = server_clone.config();
        let batching_enabled = config.websocket_config.enable_batching;
        let batch_size = config.websocket_config.batch_size;
        let batch_interval_ms = config.websocket_config.batch_interval_ms;
        let max_sojourn = Duration::from_millis(config.websocket_config.max_sojourn_ms);
        let slow_consumer_timeout =
            Duration::from_millis(config.websocket_config.slow_consumer_timeout_ms);
        let mut batcher = MessageBatcher::new(batch_size, batch_interval_ms);

        // The write loop runs INSIDE this select so a close request interrupts
        // it at ANY await point — including a socket write wedged against a
        // peer that stopped reading, which is precisely the slow-consumer
        // state that triggers the close. Handling the close as a sibling loop
        // arm would only observe it BETWEEN writes, and a wedged write never
        // finishes; the sink half would then never drop and the connection
        // would linger as a zombie socket.
        let close_request = run_until_close(&mut send_task_close, async {
            let batch_interval = Duration::from_millis(batch_interval_ms.max(1));
            loop {
                // Read outside the `select!`: the arm below only needs the
                // deadline value, and borrowing `rx` inside the select would
                // conflict with the `&mut` receive arm.
                let pending_flush_deadline = rx.pending_unsupported_flush_deadline();
                let received = tokio::select! {
                    biased;
                    command = ping_command_rx.recv() => {
                        let Some(command) = command else {
                            break;
                        };
                        let write_started_at = Instant::now();
                        let probe = begin_ping_probe(
                            &ping_probe_state_for_send,
                            command.baseline_generation,
                            command.nonce,
                            write_started_at,
                        );
                        if let Err(inbound_generation) = probe {
                            clear_ping_probe(&ping_probe_state_for_send, command.nonce);
                            let _ = command.write_outcome.send(
                                PingWriteOutcome::SkippedActivity {
                                    inbound_generation,
                                    outbound_generation: ping_probe_state_for_send
                                        .borrow()
                                        .outbound_generation,
                                }
                            );
                            continue;
                        }
                        let outbound_advanced = ping_probe_state_for_send
                            .borrow()
                            .outbound_generation
                            != command.baseline_outbound_generation;
                        let (ping_write_timeout, ping_write_timeout_reason) =
                            ping_write_timeout_policy(
                                outbound_advanced,
                                max_sojourn,
                                slow_consumer_timeout,
                            );
                        let payload = command.nonce.to_be_bytes().to_vec().into();
                        match complete_ping_write(
                            write_started_at,
                            ping_write_timeout,
                            ping_write_timeout_reason,
                            sender.send(Message::Ping(payload)),
                            &ping_probe_state_for_send,
                            command.nonce,
                            &send_task_close_signal,
                            &server_clone,
                        )
                        .await
                        {
                            Ok(timing) => {
                                let _ = command.write_outcome.send(
                                    PingWriteOutcome::Written(timing)
                                );
                                continue;
                            }
                            Err(PingWriteFailure::Socket(err)) => {
                                tracing::debug!(
                                    error = %err,
                                    "Failed to write WebSocket Ping"
                                );
                            }
                            Err(PingWriteFailure::DeadlineElapsed) => {
                                tracing::info!(
                                    timeout_secs = ping_write_timeout.as_secs(),
                                    ?ping_write_timeout_reason,
                                    "WebSocket Ping write timed out - closing connection"
                                );
                            }
                        }
                        break;
                    }
                    // An idle recipient must still learn about coalesced
                    // omissions within a bounded time: without this the last
                    // range of a burst would wait for the next omission or
                    // for the connection to close. `recv`/`recv_batched` are
                    // cancel-safe, so losing this race costs nothing.
                    () = async {
                        match pending_flush_deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending().await,
                        }
                    } => {
                        let current_player_id =
                            *effective_player_id_for_send.read().await;
                        match write_pending_unsupported_report(
                            &mut sender,
                            &rx,
                            &current_player_id,
                            server_clone.config().max_outbound_message_size,
                        )
                        .await
                        {
                            Ok(true) => {
                                record_outbound_probe_activity(
                                    &ping_probe_state_for_send,
                                    Instant::now(),
                                );
                            }
                            Ok(false) => {}
                            Err(error) => {
                                if let super::sending::SendMessageError::MessageTooLarge {
                                    size,
                                    max,
                                } = error
                                {
                                    tracing::warn!(
                                        %current_player_id,
                                        size,
                                        max,
                                        "Pending delivery report exceeds outbound message-size limit"
                                    );
                                    send_task_close_signal
                                        .request_close(CloseReason::OutboundMessageTooLarge);
                                }
                                break;
                            }
                        }
                        continue;
                    }
                    received = async {
                        if batching_enabled {
                            rx.recv_batched(batch_size, batch_interval).await
                        } else {
                            rx.recv().await
                        }
                    } => received,
                };
                match received {
                    Ok(Some(message)) if message.is_control() => {
                        let current_player_id = *effective_player_id_for_send.read().await;
                        if send_queued(
                            &mut sender,
                            message,
                            None,
                            &rx,
                            &current_player_id,
                            &server_clone,
                            &send_task_close_signal,
                            &ping_probe_state_for_send,
                            max_sojourn,
                            WritePhase::Live,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Ok(Some(message)) => {
                        // The receiver holds a batch in the shared queue
                        // until it is ready, then releases one item at a
                        // time. Never stage multiple undelivered data
                        // messages outside the queue: keyed-latest
                        // coalescing and exact gap reporting must still
                        // see every item not actively being written.
                        batcher.queue(message);
                        let current_player_id = *effective_player_id_for_send.read().await;
                        if send_batch(
                            &mut sender,
                            &mut batcher,
                            &mut rx,
                            &current_player_id,
                            &server_clone,
                            &send_task_close_signal,
                            &ping_probe_state_for_send,
                            max_sojourn,
                            WritePhase::Live,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(TryReceiveError::AccountabilityFailed) => {
                        if send_task_close_signal.request_close(CloseReason::SlowConsumer) {
                            server_clone
                                .metrics()
                                .increment_websocket_slow_consumer_disconnects();
                        }
                        break;
                    }
                    Err(TryReceiveError::Empty | TryReceiveError::Disconnected) => break,
                }
            }
        })
        .await;

        // A close was requested (slow consumer, unregistration): the write
        // loop above was cancelled wherever it was; run the bounded farewell/
        // flush/close sequence. When the loop ended by itself there is nothing
        // more to write, but the drop metric must stay honest: a clean
        // channel-close exits only after a full flush (nothing buffered),
        // while a socket write error abandons whatever is still buffered with
        // the dead connection — count that instead of losing it silently from
        // an observability standpoint.
        // The write loop can end on its own (queue senders dropped by
        // unregistration) in the same instant a close reason was requested;
        // the select above may then have taken the loop-ended arm before it
        // observed the close request.
        // Resolve the close reason and ALWAYS finalize through the one path,
        // so every server-side teardown gets its bounded flush, honest drop
        // accounting, and a SEMANTIC close frame — never a bare, code-less
        // close. The reason is: the select's own outcome if a close was
        // requested; else a reason requested in the same instant the write
        // loop ended on its own (the first-wins race the peek closes); else
        // `None` for a plain rx-closed shutdown (all delivery handles dropped
        // by unregistration), which `finalize_closed_connection` maps to the
        // normal `Unregistered` closure (WebSocket code 1000).
        let reason = resolve_final_close_reason(close_request.flatten(), &send_task_close);
        let current_player_id = *effective_player_id_for_send.read().await;
        finalize_closed_connection(
            &mut sender,
            &mut rx,
            &mut batcher,
            reason,
            &current_player_id,
            &server_clone,
            &send_task_close_signal,
            &ping_probe_state_for_send,
            max_sojourn,
        )
        .await;

        // Cleanup when send task ends
        server_clone
            .unregister_client_with_lifecycle(lifecycle_for_send)
            .await;
        #[cfg(feature = "trace-validation")]
        if let (Some(trace), Some(path)) = (trace_for_output, trace_output_for_send) {
            if trace.has_delivery_attempts() {
                if let Err(error) = trace.append_jsonl(&path) {
                    tracing::error!(
                        %error,
                        path = %path.display(),
                        "Unable to append production socket delivery trace"
                    );
                }
            }
        }
    });

    // Handle incoming messages
    let token_binding_for_receive = token_binding.clone();
    let server_clone = server.clone();
    let effective_player_id_for_receive = Arc::clone(&effective_player_id);
    let lifecycle_for_receive = Arc::clone(&connection_lifecycle);
    let auth_timeout_secs = server.config().websocket_config.auth_timeout_secs;
    let close_signal_for_receive = close_signal.clone();
    let mut receive_task = tokio::spawn(async move {
        let mut active_player_id = player_id;
        let token_binding = token_binding_for_receive;
        let close_signal = close_signal_for_receive;
        let auth_deadline = checked_deadline(connection_start, auth_timeout);

        // Post-handshake idle timeout (0 = disabled). Wrapping each `receiver.next()`
        // means ANY inbound frame — Text, Binary, Ping, Pong, Close — counts as
        // activity and resets the window. The pre-handshake phase below is
        // bounded by the (stricter) handshake deadline instead.
        let idle_timeout_secs = server_clone.config().websocket_config.idle_timeout_secs;
        let idle_timeout = (idle_timeout_secs > 0).then(|| Duration::from_secs(idle_timeout_secs));

        loop {
            let inbound_deadline = InboundDeadline::for_connection(
                app_handshake_complete,
                auth_deadline,
                auth_timeout_secs,
                idle_timeout,
                idle_timeout_secs,
            );
            let msg = match inbound_deadline
                .read(&mut receive_task_close, receiver.next())
                .await
            {
                InboundRead::CloseRequested(reason) => {
                    tracing::debug!(
                        %active_player_id,
                        ?reason,
                        "Connection close requested; ending receive task"
                    );
                    break;
                }
                InboundRead::Completed(Some(msg)) => msg,
                InboundRead::Completed(None) => break,
                InboundRead::DeadlineElapsed => {
                    match inbound_deadline.kind {
                        InboundDeadlineKind::Authentication => {
                            tracing::warn!(
                                %active_player_id,
                                timeout_secs = inbound_deadline.timeout_secs,
                                "Authentication timeout, closing connection"
                            );
                        }
                        InboundDeadlineKind::Idle => {
                            tracing::info!(
                                %active_player_id,
                                timeout_secs = inbound_deadline.timeout_secs,
                                "Idle timeout - no frames received, closing connection"
                            );
                        }
                    }
                    // Timeout farewells use this connection's own outbound
                    // channel and never wait for capacity. The semantic close
                    // reason is pinned before generic unregistration can win.
                    inbound_deadline.expire(&tx_clone, &close_signal, &active_player_id);
                    break;
                }
            };

            // Process the message
            let msg = match msg {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::warn!(%active_player_id, "WebSocket error: {}", e);
                    break;
                }
            };
            let received_at = Instant::now();
            let _inbound_activity_guard = if !matches!(&msg, Message::Pong(_)) {
                // Publish transport liveness before parsing or any awaited
                // application work. Reliable delivery can backpressure the
                // handler, but it must not hide an already-decoded frame from
                // the idle-probe state machine.
                ping_probe_state_for_receive
                    .as_ref()
                    .map(|state| InboundProbeActivityGuard::begin(state, received_at))
            } else {
                None
            };

            match msg {
                Message::Text(text) => {
                    // Check message size limit
                    let max_size = server_clone.config().max_message_size;
                    if text.len() > max_size {
                        tracing::warn!(
                            %active_player_id,
                            size = text.len(),
                            max = max_size,
                            "Message exceeds size limit"
                        );
                        let _ = server_clone
                            .send_error_to_player(
                                &active_player_id,
                                format!(
                                    "Message too large ({} bytes, max {} bytes)",
                                    text.len(),
                                    max_size
                                ),
                                Some(ErrorCode::MessageTooLarge),
                            )
                            .await;
                        continue;
                    }

                    let client_message = match parse_client_message(&text, token_binding.as_ref()) {
                        Ok(message) => message,
                        Err(err) => {
                            tracing::warn!(
                                %active_player_id,
                                error = %err,
                                "Rejected client WebSocket frame"
                            );
                            if err.should_disconnect() {
                                // Farewell semantics: closing immediately, so
                                // never wait or escalate on a full queue.
                                enqueue_farewell_message(
                                    &tx_clone,
                                    &close_signal,
                                    &active_player_id,
                                    ServerMessage::Error {
                                        message: err.user_message().to_string(),
                                        error_code: Some(err.error_code()),
                                    },
                                    "rejected frame (disconnecting)",
                                );
                                break;
                            }
                            // Connection stays alive: the rejection notice
                            // rides the reliable delivery path.
                            let _ = server_clone
                                .send_error_to_player(
                                    &active_player_id,
                                    err.user_message().to_string(),
                                    Some(err.error_code()),
                                )
                                .await;
                            continue;
                        }
                    };

                    match client_message {
                        ClientMessage::Authenticate {
                            app_id,
                            sdk_version,
                            platform,
                            game_data_format,
                            protocol_version,
                            supported_transports,
                            supported_topologies,
                            requested_capabilities,
                        } => {
                            if server_clone.config().app_id_allowlist_enabled
                                && app_handshake_complete
                            {
                                tracing::warn!(%active_player_id, "App-ID handshake already completed");
                                let _ = server_clone
                                    .send_error_to_player(
                                        &active_player_id,
                                        "Authenticate already completed on this connection"
                                            .to_string(),
                                        Some(ErrorCode::InvalidInput),
                                    )
                                    .await;
                                continue;
                            }
                            if !server_clone.config().app_id_allowlist_enabled
                                && (authenticate_processed || received_application_message)
                            {
                                tracing::warn!(
                                    %active_player_id,
                                    "Authenticate must be the first client message"
                                );
                                // `authenticate_processed` survives a reconnect
                                // identity swap, so this refusal also covers a
                                // re-Authenticate on an already-swapped socket.
                                let refusal = if authenticate_processed {
                                    "Authenticate already completed on this connection"
                                } else {
                                    "Authenticate must be the first application message"
                                };
                                let _ = server_clone
                                    .send_error_to_player(
                                        &active_player_id,
                                        refusal.to_string(),
                                        Some(ErrorCode::InvalidInput),
                                    )
                                    .await;
                                continue;
                            }

                            // Validate App ID (the connection's source keys the
                            // per-source share of the app rate budget)
                            match server_clone
                                .app_id_allowlist
                                .resolve_app_id(&app_id, addr.ip())
                                .await
                            {
                                Ok(info) => {
                                    let compatibility = match server_clone
                                        .protocol_config()
                                        .sdk_compatibility
                                        .evaluate(platform.as_deref(), sdk_version.as_deref())
                                    {
                                        Ok(report) => report,
                                        Err(err) => {
                                            let error_message = err.to_string();
                                            tracing::warn!(
                                                %active_player_id,
                                                app_id = %app_id,
                                                ?sdk_version,
                                                ?platform,
                                                error = %error_message,
                                                "SDK compatibility check failed"
                                            );
                                            let _ = enqueue_connection_message(
                                                &tx_clone,
                                                &close_signal,
                                                &server_clone,
                                                slow_consumer_timeout,
                                                &active_player_id,
                                                ServerMessage::AuthenticationError {
                                                    error: error_message,
                                                    error_code: ErrorCode::SdkVersionUnsupported,
                                                },
                                                "SDK compatibility error",
                                            )
                                            .await;
                                            continue;
                                        }
                                    };

                                    // Protocol version + capability negotiation (P1).
                                    // A missing `protocol_version` uses the endpoint
                                    // default. Explicit client values take precedence,
                                    // and are never raised above what the client claims.
                                    let cfg = server_clone.protocol_config();
                                    let client_max =
                                        protocol_version.or(Some(default_protocol_version));
                                    let negotiated_version =
                                        cfg.negotiate_protocol_version(client_max);
                                    if negotiated_version < cfg.min_protocol_version {
                                        let error_message = format!(
                                            "Client protocol version {negotiated_version} is below the server minimum {}",
                                            cfg.min_protocol_version
                                        );
                                        tracing::warn!(
                                            %active_player_id,
                                            client_protocol_version = negotiated_version,
                                            server_min_protocol_version = cfg.min_protocol_version,
                                            "Protocol version negotiation failed"
                                        );
                                        let _ = enqueue_connection_message(
                                            &tx_clone,
                                            &close_signal,
                                            &server_clone,
                                            slow_consumer_timeout,
                                            &active_player_id,
                                            ServerMessage::AuthenticationError {
                                                error: error_message,
                                                error_code: ErrorCode::UnsupportedProtocolVersion,
                                            },
                                            "protocol version compatibility error",
                                        )
                                        .await;
                                        // Open-policy sockets provisionally completed
                                        // the handshake from the endpoint default.
                                        // Once an optional Authenticate contradicts
                                        // that default below the deployment floor,
                                        // continuing would leave a declared-v2 client
                                        // usable as v3. Enforced-policy clients may retry
                                        // their incomplete app-ID handshake.
                                        if !server_clone.config().app_id_allowlist_enabled {
                                            break;
                                        }
                                        continue;
                                    }

                                    app_handshake_complete = true;
                                    authenticate_processed = true;
                                    server_clone
                                        .set_client_app_context(&active_player_id, info.clone());
                                    server_clone.apply_app_bandwidth_policy(&info);
                                    let supported_formats = server_clone
                                        .protocol_config()
                                        .supported_game_data_formats();
                                    let negotiated_format = match game_data_format {
                                        Some(format) if supported_formats.contains(&format) => {
                                            format
                                        }
                                        Some(format) => {
                                            let supported_list: Vec<String> = supported_formats
                                                .iter()
                                                .map(|format| format.as_wire_str().to_string())
                                                .collect();
                                            let error_message = format!(
                                                "Requested game data format '{}' is not supported. Server supports: {}. Falling back to JSON.",
                                                format.as_wire_str(),
                                                supported_list.join(", ")
                                            );
                                            tracing::warn!(
                                                %active_player_id,
                                                requested_format = format.as_wire_str(),
                                                supported_formats = %supported_list.join(", "),
                                                "Client requested unsupported game_data_format"
                                            );
                                            // Send error message to client about capability mismatch
                                            let _ = enqueue_connection_message(
                                                &tx_clone,
                                                &close_signal,
                                                &server_clone,
                                                slow_consumer_timeout,
                                                &active_player_id,
                                                ServerMessage::Error {
                                                    message: error_message,
                                                    error_code: Some(
                                                        ErrorCode::UnsupportedGameDataFormat,
                                                    ),
                                                },
                                                "game data format error",
                                            )
                                            .await;
                                            GameDataEncoding::Json
                                        }
                                        None => GameDataEncoding::Json,
                                    };
                                    server_clone.set_client_game_data_format(
                                        &active_player_id,
                                        negotiated_format,
                                    );

                                    let (negotiated_transports, negotiated_topologies) =
                                        negotiate_capabilities(
                                            negotiated_version,
                                            supported_transports,
                                            supported_topologies,
                                        );
                                    let room_operation_ids = negotiated_version >= 3
                                        && requested_capabilities.as_ref().is_some_and(
                                            |capabilities| {
                                                capabilities.iter().any(|capability| {
                                                    capability == ROOM_OPERATION_IDS_CAPABILITY
                                                })
                                            },
                                        );

                                    let min_protocol_version = cfg.min_protocol_version;
                                    let max_protocol_version = cfg.max_protocol_version;

                                    server_clone.set_client_protocol(
                                        &active_player_id,
                                        NegotiatedProtocol {
                                            version: negotiated_version,
                                            transports: negotiated_transports.clone(),
                                            topologies: negotiated_topologies.clone(),
                                        },
                                    );
                                    server_clone.set_client_room_operation_ids(
                                        &active_player_id,
                                        room_operation_ids,
                                    );

                                    tracing::info!(
                                        %active_player_id,
                                        app_name = %info.name,
                                        app_id = %app_id,
                                        ?sdk_version,
                                        ?platform,
                                        protocol_version = negotiated_version,
                                        ?negotiated_transports,
                                        ?negotiated_topologies,
                                        "Public app ID accepted"
                                    );

                                    // Send success response
                                    let auth_response = ServerMessage::Authenticated {
                                        app_name: info.name.clone(),
                                        organization: info.organization.clone(),
                                        rate_limits: RateLimitInfo {
                                            per_minute: info.rate_limits.per_minute,
                                            per_hour: info.rate_limits.per_hour,
                                            per_day: info.rate_limits.per_day,
                                        },
                                    };

                                    let player_name_rules =
                                        PlayerNameRulesPayload::from_protocol_config(
                                            server_clone.protocol_config(),
                                        );
                                    let (
                                        response_protocol_version,
                                        response_min_protocol_version,
                                        response_max_protocol_version,
                                        response_transports,
                                    ) = if negotiated_version >= 3 {
                                        (
                                            Some(negotiated_version),
                                            Some(min_protocol_version),
                                            Some(max_protocol_version),
                                            Some(vec![
                                                PROTOCOL_INFO_TRANSPORT_WEBSOCKET.to_string()
                                            ]),
                                        )
                                    } else {
                                        (None, None, None, None)
                                    };
                                    let protocol_info = ServerMessage::ProtocolInfo(Box::new(
                                        ProtocolInfoPayload {
                                            platform: compatibility.platform.clone(),
                                            sdk_version: compatibility.sdk_version.clone(),
                                            minimum_version: compatibility.minimum_version.clone(),
                                            recommended_version: compatibility
                                                .recommended_version
                                                .clone(),
                                            capabilities: protocol_info_capabilities(
                                                compatibility.capabilities.clone(),
                                                room_operation_ids,
                                            ),
                                            notes: compatibility.notes.clone(),
                                            game_data_formats: supported_formats,
                                            player_name_rules: Some(player_name_rules),
                                            protocol_version: response_protocol_version,
                                            min_protocol_version: response_min_protocol_version,
                                            max_protocol_version: response_max_protocol_version,
                                            transports: response_transports,
                                            max_outbound_message_size: (negotiated_version >= 3)
                                                .then_some(
                                                    server_clone.config().max_outbound_message_size,
                                                ),
                                        },
                                    ));

                                    let auth_response_queued = enqueue_connection_message(
                                        &tx_clone,
                                        &close_signal,
                                        &server_clone,
                                        slow_consumer_timeout,
                                        &active_player_id,
                                        auth_response,
                                        "authentication success response",
                                    )
                                    .await;
                                    let protocol_info_queued = enqueue_connection_message(
                                        &tx_clone,
                                        &close_signal,
                                        &server_clone,
                                        slow_consumer_timeout,
                                        &active_player_id,
                                        protocol_info,
                                        "protocol info response",
                                    )
                                    .await;
                                    if auth_response_queued && protocol_info_queued {
                                        protocol_handshake_complete.store(true, Ordering::Release);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(%active_player_id, %app_id, "Public app ID rejected: {:?}", e);

                                    // Send error response.
                                    // The AppIdExpired, AppIdRevoked, and AppIdSuspended
                                    // variants are not currently returned by
                                    // `resolve_app_id`, but are retained for future
                                    // backend implementations (e.g., app status management
                                    // or admin-controlled app suspension).
                                    let error_code = match e {
                                        crate::auth::AuthError::DuplicateAppId => {
                                            ErrorCode::InternalError
                                        }
                                        crate::auth::AuthError::InvalidAppId => {
                                            ErrorCode::InvalidAppId
                                        }
                                        crate::auth::AuthError::AppIdExpired => {
                                            ErrorCode::AppIdExpired
                                        }
                                        crate::auth::AuthError::AppIdRevoked => {
                                            ErrorCode::AppIdRevoked
                                        }
                                        crate::auth::AuthError::AppIdSuspended => {
                                            ErrorCode::AppIdSuspended
                                        }
                                        crate::auth::AuthError::RateLimitExceeded => {
                                            ErrorCode::RateLimitExceeded
                                        }
                                    };

                                    // Farewell semantics: the connection is
                                    // closed immediately below, so this frame
                                    // is advisory and must not wait/escalate.
                                    enqueue_farewell_message(
                                        &tx_clone,
                                        &close_signal,
                                        &active_player_id,
                                        ServerMessage::AuthenticationError {
                                            error: format!("{e:?}"),
                                            error_code,
                                        },
                                        "authentication failure response",
                                    );

                                    // Close connection after auth failure
                                    break;
                                }
                            }
                        }
                        other => {
                            if !app_handshake_complete {
                                tracing::warn!(%active_player_id, "Received message before app-ID handshake");
                                // Farewell semantics: closing immediately.
                                enqueue_farewell_message(
                                    &tx_clone,
                                    &close_signal,
                                    &active_player_id,
                                    ServerMessage::Error {
                                        message: "Authentication required".to_string(),
                                        error_code: Some(ErrorCode::MissingAppId),
                                    },
                                    "message before authentication",
                                );
                                break;
                            }

                            received_application_message = true;
                            match other {
                                ClientMessage::Reconnect {
                                    player_id: reconnect_player_id,
                                    room_id,
                                    auth_token,
                                } => {
                                    if server_clone
                                        .handle_reconnect_with_identity(
                                            &active_player_id,
                                            &reconnect_player_id,
                                            &room_id,
                                            &auth_token,
                                            Arc::clone(&effective_player_id_for_receive),
                                        )
                                        .await
                                    {
                                        active_player_id = reconnect_player_id;
                                    }
                                }
                                ClientMessage::RoomOperation {
                                    operation_id,
                                    operation,
                                } => match *operation {
                                    crate::protocol::RoomOperationRequest::Reconnect {
                                        player_id: reconnect_player_id,
                                        room_id,
                                        auth_token,
                                    } if server_clone
                                        .client_supports_room_operation_ids(&active_player_id) =>
                                    {
                                        if server_clone
                                            .handle_reconnect_with_identity_operation(
                                                &active_player_id,
                                                &reconnect_player_id,
                                                &room_id,
                                                &auth_token,
                                                Arc::clone(&effective_player_id_for_receive),
                                                Some(operation_id),
                                            )
                                            .await
                                        {
                                            active_player_id = reconnect_player_id;
                                        }
                                    }
                                    operation => {
                                        server_clone
                                            .handle_client_message(
                                                &active_player_id,
                                                ClientMessage::RoomOperation {
                                                    operation_id,
                                                    operation: Box::new(operation),
                                                },
                                            )
                                            .await;
                                    }
                                },
                                other => {
                                    server_clone
                                        .handle_client_message(&active_player_id, other)
                                        .await;
                                }
                            }
                            if !server_clone.config().app_id_allowlist_enabled
                                && !authenticate_processed
                            {
                                protocol_handshake_complete.store(true, Ordering::Release);
                            }
                        }
                    }
                }
                Message::Binary(payload) => {
                    let payload = if let Some(binding) = token_binding.as_ref() {
                        match parse_binary_message(&payload, binding) {
                            Ok(payload) => bytes::Bytes::from(payload),
                            Err(err) => {
                                tracing::warn!(
                                    %active_player_id,
                                    error = %err,
                                    "Rejected token-bound binary frame"
                                );
                                enqueue_farewell_message(
                                    &tx_clone,
                                    &close_signal,
                                    &active_player_id,
                                    ServerMessage::Error {
                                        message: err.user_message().to_string(),
                                        error_code: Some(err.error_code()),
                                    },
                                    "invalid token-bound binary frame",
                                );
                                break;
                            }
                        }
                    } else {
                        payload
                    };

                    if !app_handshake_complete {
                        tracing::warn!(%active_player_id, "Received binary message before app-ID handshake");
                        // Farewell semantics: closing immediately.
                        enqueue_farewell_message(
                            &tx_clone,
                            &close_signal,
                            &active_player_id,
                            ServerMessage::Error {
                                message: "Authentication required before sending binary data"
                                    .to_string(),
                                error_code: Some(ErrorCode::MissingAppId),
                            },
                            "binary before authentication",
                        );
                        break;
                    }

                    received_application_message = true;
                    let encoding = server_clone.client_game_data_format(&active_player_id);
                    if encoding == GameDataEncoding::Json {
                        tracing::warn!(
                            %active_player_id,
                            "Client negotiated JSON game data but sent binary payload; dropping"
                        );
                        let _ = server_clone
                            .send_error_to_player(
                                &active_player_id,
                                "Binary payloads are disabled for this connection".to_string(),
                                Some(ErrorCode::InvalidInput),
                            )
                            .await;
                        if !server_clone.config().app_id_allowlist_enabled
                            && !authenticate_processed
                        {
                            protocol_handshake_complete.store(true, Ordering::Release);
                        }
                        continue;
                    }

                    // Payload from axum WebSocket is already Bytes - pass directly for zero-copy
                    server_clone
                        .handle_game_data_binary(&active_player_id, encoding, payload)
                        .await;
                    if !server_clone.config().app_id_allowlist_enabled && !authenticate_processed {
                        protocol_handshake_complete.store(true, Ordering::Release);
                    }
                }
                Message::Close(_) => {
                    tracing::info!(%active_player_id, "WebSocket connection closed");
                    break;
                }
                Message::Pong(payload) => {
                    if let Ok(bytes) = <[u8; 8]>::try_from(payload.as_ref()) {
                        let nonce = u64::from_be_bytes(bytes);
                        if let Some(state) = &ping_probe_state_for_receive {
                            try_record_matching_pong(state, nonce, received_at);
                        }
                    }
                    // Publish the probe observation before a potentially slow
                    // activity refresh so deadline evaluation stays independent.
                    server_clone
                        .record_transport_activity(&active_player_id)
                        .await;
                }
                Message::Ping(_) => {
                    // A compliant client keepalive is inbound transport
                    // activity. It already suppressed this window's own probe
                    // via the inbound-activity guard, so liveness must be
                    // refreshed here too — otherwise a Ping-only client
                    // starves the activity reaper and is deterministically
                    // evicted while fully healthy.
                    server_clone
                        .record_transport_activity(&active_player_id)
                        .await;
                }
            }
        }

        // Cleanup when receive task ends
        server_clone
            .unregister_client_with_lifecycle(lifecycle_for_receive)
            .await;
    });

    enum CompletedSocketTask {
        Send,
        Receive,
    }

    let completed_socket_task = tokio::select! {
        result = &mut send_task => {
            let current_player_id = *effective_player_id.read().await;
            match result {
                Ok(()) => tracing::info!(%current_player_id, "Send task completed"),
                Err(err) => tracing::warn!(%current_player_id, error = %err, "Send task failed"),
            }
            CompletedSocketTask::Send
        }
        result = &mut receive_task => {
            let current_player_id = *effective_player_id.read().await;
            match result {
                Ok(()) => tracing::info!(%current_player_id, "Receive task completed"),
                Err(err) => tracing::warn!(%current_player_id, error = %err, "Receive task failed"),
            }
            CompletedSocketTask::Receive
        }
    };

    // Ensure cleanup and keep this handler alive until the remaining socket
    // half observes the close request and finishes its bounded teardown. The
    // shutdown drain waits on this handler lifetime so code 4000 has a chance
    // to hit the wire before process exit.
    let current_player_id = *effective_player_id.read().await;
    server
        .unregister_client_with_lifecycle(connection_lifecycle)
        .await;

    match completed_socket_task {
        CompletedSocketTask::Send => {
            if let Err(err) = receive_task.await {
                tracing::warn!(%current_player_id, error = %err, "Receive task failed");
            }
        }
        CompletedSocketTask::Receive => {
            if let Err(err) = send_task.await {
                tracing::warn!(%current_player_id, error = %err, "Send task failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::DatabaseConfig;
    use crate::protocol::{ClientMessage, ServerMessage};
    use crate::server::ServerConfig;
    use std::net::SocketAddr;
    use std::sync::atomic::AtomicBool;
    use std::task::Context;
    use tokio_tungstenite::{connect_async, tungstenite::Message as TungsteniteMessage};

    async fn test_server() -> Arc<EnhancedGameServer> {
        test_server_with_config(ServerConfig::default()).await
    }

    async fn test_server_with_config(config: ServerConfig) -> Arc<EnhancedGameServer> {
        EnhancedGameServer::new(
            config,
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
        .expect("construct connection test server")
    }

    async fn closed_with_timeout(
        listener: &mut ConnectionCloseListener,
        context: &str,
    ) -> Option<CloseReason> {
        tokio::time::timeout(Duration::from_secs(1), listener.closed())
            .await
            .unwrap_or_else(|_| panic!("{context}: close listener never resolved"))
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_deadlines_reject_frames_ready_at_or_after_boundary() {
        for (kind, advance_by, context) in [
            (
                InboundDeadlineKind::Authentication,
                Duration::from_millis(10),
                "authentication input at deadline",
            ),
            (
                InboundDeadlineKind::Authentication,
                Duration::from_millis(11),
                "authentication input after deadline",
            ),
            (
                InboundDeadlineKind::Idle,
                Duration::from_millis(10),
                "idle input at deadline",
            ),
            (
                InboundDeadlineKind::Idle,
                Duration::from_millis(11),
                "idle input after deadline",
            ),
        ] {
            let (_signal, mut close) = ConnectionCloseSignal::channel();
            let (release, read) = tokio::sync::oneshot::channel();
            let deadline = Instant::now() + Duration::from_millis(10);
            let policy = InboundDeadline::for_connection(
                kind == InboundDeadlineKind::Idle,
                deadline,
                10,
                Some(Duration::from_millis(10)),
                10,
            );
            assert_eq!(policy.kind, kind, "{context}: selected policy");
            let mut bounded = Box::pin(policy.read(&mut close, read));

            assert!(
                futures_util::poll!(&mut bounded).is_pending(),
                "{context}: read must first be observed pending"
            );
            tokio::time::advance(advance_by).await;
            release.send(()).expect("release inbound frame");

            assert!(
                matches!(bounded.await, InboundRead::DeadlineElapsed),
                "{context}: expired input must not be admitted for {kind:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_deadlines_accept_frames_strictly_before_boundary() {
        for kind in [
            InboundDeadlineKind::Authentication,
            InboundDeadlineKind::Idle,
        ] {
            let (_signal, mut close) = ConnectionCloseSignal::channel();
            let (release, read) = tokio::sync::oneshot::channel();
            let deadline = Instant::now() + Duration::from_millis(10);
            let policy = InboundDeadline::for_connection(
                kind == InboundDeadlineKind::Idle,
                deadline,
                10,
                Some(Duration::from_millis(10)),
                10,
            );
            assert_eq!(policy.kind, kind);
            let mut bounded = Box::pin(policy.read(&mut close, read));

            assert!(
                futures_util::poll!(&mut bounded).is_pending(),
                "{kind:?}: read must first be observed pending"
            );
            tokio::time::advance(Duration::from_millis(9)).await;
            release.send(()).expect("release inbound frame");

            assert!(
                matches!(bounded.await, InboundRead::Completed(Ok(()))),
                "{kind:?}: input strictly before the deadline must remain healthy"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_close_request_precedes_ready_deadline_and_frame() {
        let (signal, mut close) = ConnectionCloseSignal::channel();
        let (release, read) = tokio::sync::oneshot::channel();
        let deadline = Instant::now() + Duration::from_millis(10);
        let policy = InboundDeadline::for_connection(false, deadline, 10, None, 0);
        let mut bounded = Box::pin(policy.read(&mut close, read));

        assert!(
            futures_util::poll!(&mut bounded).is_pending(),
            "read must first be observed pending"
        );
        tokio::time::advance(Duration::from_millis(10)).await;
        assert!(signal.request_close(CloseReason::Shutdown));
        release.send(()).expect("release inbound frame");

        assert!(matches!(
            bounded.await,
            InboundRead::CloseRequested(Some(CloseReason::Shutdown))
        ));
    }

    #[tokio::test]
    async fn send_loop_close_request_precedes_ready_socket_work() {
        let (signal, mut close) = ConnectionCloseSignal::channel();
        let (release, gate) = tokio::sync::oneshot::channel();
        let completed = Arc::new(AtomicBool::new(false));
        let completed_by_operation = Arc::clone(&completed);
        let operation = async move {
            gate.await.expect("send-loop gate released");
            completed_by_operation.store(true, Ordering::Release);
        };
        let mut send_loop = Box::pin(run_until_close(&mut close, operation));

        assert!(
            futures_util::poll!(&mut send_loop).is_pending(),
            "send loop must first be pending"
        );
        assert!(signal.request_close(CloseReason::Shutdown));
        release.send(()).expect("make socket work ready");

        assert_eq!(send_loop.await, Some(Some(CloseReason::Shutdown)));
        assert!(
            !completed.load(Ordering::Acquire),
            "an already-requested close must cancel simultaneously-ready socket work"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_read_without_deadline_remains_enabled_after_time_advances() {
        let (_signal, mut close) = ConnectionCloseSignal::channel();
        let (release, read) = tokio::sync::oneshot::channel();
        let policy = InboundDeadline::for_connection(true, Instant::now(), 10, None, 0);
        assert_eq!(policy.at, None);
        let mut unbounded = Box::pin(policy.read(&mut close, read));

        assert!(
            futures_util::poll!(&mut unbounded).is_pending(),
            "read must first be observed pending"
        );
        tokio::time::advance(Duration::from_secs(86_400)).await;
        release.send(()).expect("release inbound frame");

        assert!(
            matches!(unbounded.await, InboundRead::Completed(Ok(()))),
            "disabled idle enforcement must not synthesize a deadline"
        );
    }

    #[tokio::test]
    async fn inbound_timeout_kinds_pin_expected_close_reason_and_error() {
        for (kind, close_reason, error_code, expected_message) in [
            (
                InboundDeadlineKind::Authentication,
                CloseReason::AuthTimeout,
                ErrorCode::AuthenticationTimeout,
                "Authentication timeout - must authenticate within 7 seconds",
            ),
            (
                InboundDeadlineKind::Idle,
                CloseReason::IdleTimeout,
                ErrorCode::ConnectionIdleTimeout,
                "Idle timeout - no messages received for 7 seconds",
            ),
        ] {
            assert_eq!(kind.close_reason(), close_reason);
            assert_eq!(kind.error_code(), error_code);
            assert_eq!(kind.error_message(7), expected_message);

            let (signal, listener) = ConnectionCloseSignal::channel();
            let (tx, mut rx) = outbound_queue::channel(2, 2);
            let policy = InboundDeadline {
                at: Some(Instant::now()),
                kind,
                timeout_secs: 7,
            };
            policy.expire(&tx, &signal, &PlayerId::from_u128(1));
            assert_eq!(listener.requested_reason(), Some(close_reason));
            let farewell = rx
                .try_recv()
                .expect("timeout farewell must be queued synchronously");
            let crate::coordination::outbound_queue::OutboundPayload::Message(message) =
                farewell.payload
            else {
                panic!("{kind:?}: timeout farewell must be a server message");
            };
            let ServerMessage::Error {
                message,
                error_code: observed_code,
            } = message.as_ref()
            else {
                panic!("{kind:?}: timeout farewell must use ServerMessage::Error");
            };
            assert_eq!(
                message, expected_message,
                "{kind:?}: production timeout handler must preserve exact text"
            );
            assert_eq!(
                *observed_code,
                Some(error_code),
                "{kind:?}: production timeout handler must preserve exact error code"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ping_write_expiry_clears_probe_and_preserves_reason_ownership() {
        for (reason, expected_metric) in [
            (CloseReason::ActivityTimeout, 0),
            (CloseReason::SlowConsumer, 1),
        ] {
            for (advance_by, context) in [
                (Duration::from_millis(10), "exact deadline"),
                (Duration::from_millis(11), "after deadline"),
            ] {
                let server = test_server().await;
                let (close, close_listener) = ConnectionCloseSignal::channel();
                let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
                let nonce = 7;
                let started_at = Instant::now();
                assert_eq!(begin_ping_probe(&probe_state, 0, nonce, started_at), Ok(()));
                let (release, gate) = tokio::sync::oneshot::channel();
                let completed = Arc::new(AtomicBool::new(false));
                let completed_by_write = Arc::clone(&completed);
                let write = async move {
                    gate.await.expect("Ping-write gate released");
                    completed_by_write.store(true, Ordering::Release);
                    Ok::<(), ()>(())
                };
                let mut ping_write = Box::pin(complete_ping_write(
                    started_at,
                    Duration::from_millis(10),
                    reason,
                    write,
                    &probe_state,
                    nonce,
                    &close,
                    &server,
                ));

                assert!(
                    futures_util::poll!(&mut ping_write).is_pending(),
                    "{reason:?}, {context}: Ping write must first be pending"
                );
                tokio::time::advance(advance_by).await;
                release.send(()).expect("make Ping write ready");

                assert!(
                    matches!(ping_write.await, Err(PingWriteFailure::DeadlineElapsed)),
                    "{reason:?}, {context}: Ping must expire through the production seam"
                );
                assert!(
                    !completed.load(Ordering::Acquire),
                    "{reason:?}, {context}: expired Ping send must be cancelled"
                );
                assert!(
                    probe_state.borrow().active.is_none(),
                    "{reason:?}, {context}: expired Ping probe must be cleared"
                );
                assert_eq!(
                    close_listener.requested_reason(),
                    Some(reason),
                    "{reason:?}, {context}: policy-selected close reason must win"
                );
                assert_eq!(
                    server
                        .metrics()
                        .websocket_slow_consumer_disconnects
                        .load(Ordering::Relaxed),
                    expected_metric,
                    "{reason:?}, {context}: only close 4002 increments the slow-consumer metric"
                );
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ping_write_completes_strictly_before_deadline_and_keeps_probe_active() {
        let server = test_server().await;
        let (close, close_listener) = ConnectionCloseSignal::channel();
        let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
        let nonce = 7;
        let started_at = Instant::now();
        assert_eq!(begin_ping_probe(&probe_state, 0, nonce, started_at), Ok(()));
        let (release, gate) = tokio::sync::oneshot::channel();
        let mut ping_write = Box::pin(complete_ping_write(
            started_at,
            Duration::from_millis(10),
            CloseReason::ActivityTimeout,
            async move {
                gate.await.expect("Ping-write gate released");
                Ok::<(), ()>(())
            },
            &probe_state,
            nonce,
            &close,
            &server,
        ));

        assert!(
            futures_util::poll!(&mut ping_write).is_pending(),
            "Ping write must first be pending"
        );
        tokio::time::advance(Duration::from_millis(9)).await;
        release.send(()).expect("make Ping write ready");

        let timing = ping_write.await.expect("just-before Ping write succeeds");
        assert_eq!(timing.completed_at, Instant::now());
        assert!(
            probe_state.borrow().active.is_some(),
            "successful Ping write keeps the probe active for Pong resolution"
        );
        assert_eq!(close_listener.requested_reason(), None);
    }

    #[tokio::test]
    async fn ping_socket_error_clears_probe_and_requests_policy_reason() {
        let server = test_server().await;
        let (close, close_listener) = ConnectionCloseSignal::channel();
        let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
        let nonce = 7;
        let started_at = Instant::now();
        assert_eq!(begin_ping_probe(&probe_state, 0, nonce, started_at), Ok(()));

        let result = complete_ping_write(
            started_at,
            Duration::from_secs(1),
            CloseReason::ActivityTimeout,
            async { Err::<(), _>("socket closed") },
            &probe_state,
            nonce,
            &close,
            &server,
        )
        .await;

        assert!(matches!(
            result,
            Err(PingWriteFailure::Socket("socket closed"))
        ));
        assert!(probe_state.borrow().active.is_none());
        assert_eq!(
            close_listener.requested_reason(),
            Some(CloseReason::ActivityTimeout)
        );
    }

    #[test]
    fn ping_probe_state_distinguishes_skip_pong_activity_and_timeout() {
        let (state, _updates) = watch::channel(PingProbeState::default());
        let started_at = Instant::now();
        let deadline_at = started_at + Duration::from_millis(10);

        let activity = InboundProbeActivityGuard::begin(&state, started_at);
        assert_eq!(begin_ping_probe(&state, 0, 1, started_at), Err(1));
        drop(activity);

        assert_eq!(begin_ping_probe(&state, 1, 2, started_at), Ok(()));
        assert!(!try_record_matching_pong(
            &state,
            99,
            started_at + Duration::from_millis(1)
        ));
        assert!(try_record_matching_pong(
            &state,
            2,
            started_at + Duration::from_millis(2)
        ));
        let activity =
            InboundProbeActivityGuard::begin(&state, started_at + Duration::from_millis(3));
        assert_eq!(
            resolve_ping_probe(&state, 2, deadline_at, false),
            Some(PingProbeResolution::MatchingPong(Duration::from_millis(2))),
            "the first valid probe evidence must win"
        );
        drop(activity);

        assert_eq!(begin_ping_probe(&state, 2, 3, started_at), Ok(()));
        let activity =
            InboundProbeActivityGuard::begin(&state, started_at + Duration::from_millis(4));
        assert_eq!(
            resolve_ping_probe(&state, 3, deadline_at, false),
            Some(PingProbeResolution::InboundActivity { generation: 3 })
        );
        drop(activity);

        assert_eq!(begin_ping_probe(&state, 3, 4, started_at), Ok(()));
        assert_eq!(
            resolve_ping_probe(&state, 4, deadline_at, true),
            Some(PingProbeResolution::TimedOut)
        );
    }

    #[test]
    fn ping_probe_deadline_is_inclusive_and_late_evidence_cannot_satisfy_it() {
        let (state, _updates) = watch::channel(PingProbeState::default());
        let started_at = Instant::now();
        let deadline_at = started_at + Duration::from_millis(5);

        assert_eq!(begin_ping_probe(&state, 0, 7, started_at), Ok(()));
        assert!(try_record_matching_pong(&state, 7, deadline_at));
        assert_eq!(
            resolve_ping_probe(&state, 7, deadline_at, true),
            Some(PingProbeResolution::MatchingPong(Duration::from_millis(5)))
        );

        assert_eq!(begin_ping_probe(&state, 0, 8, started_at), Ok(()));
        assert!(try_record_matching_pong(
            &state,
            8,
            deadline_at + Duration::from_nanos(1)
        ));
        assert_eq!(
            resolve_ping_probe(&state, 8, deadline_at, true),
            Some(PingProbeResolution::TimedOut)
        );

        assert_eq!(begin_ping_probe(&state, 0, 9, started_at), Ok(()));
        record_outbound_probe_activity(&state, deadline_at);
        record_outbound_probe_activity(&state, deadline_at + Duration::from_nanos(1));
        assert_eq!(
            resolve_ping_probe(&state, 9, deadline_at, true),
            Some(PingProbeResolution::OutboundActivity { generation: 2 }),
            "the first outbound evidence must stay latched when a late write follows it"
        );

        assert_eq!(begin_ping_probe(&state, 0, 10, started_at), Ok(()));
        record_outbound_probe_activity(&state, deadline_at + Duration::from_nanos(1));
        assert_eq!(
            resolve_ping_probe(&state, 10, deadline_at, true),
            Some(PingProbeResolution::TimedOut),
            "late-only outbound progress cannot satisfy the expired probe"
        );
    }

    #[test]
    fn ping_probe_nonces_never_use_idle_sentinel() {
        for _ in 0..1_024 {
            assert_ne!(random_ping_nonce(), 0);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn extreme_idle_timeout_does_not_become_immediate_expiry() {
        let deadline = InboundDeadline::for_connection(
            true,
            Instant::now(),
            10,
            Some(Duration::from_secs(u64::MAX)),
            u64::MAX,
        );

        assert_eq!(
            deadline.at, None,
            "an unrepresentable positive idle timeout is beyond the process lifetime"
        );
        assert_eq!(deadline.kind, InboundDeadlineKind::Idle);

        let (_close_signal, mut close) = ConnectionCloseSignal::channel();
        assert_eq!(
            deadline.read(&mut close, async { 7 }).await,
            InboundRead::Completed(7),
            "the production inbound-read seam must still accept ready input"
        );
    }

    #[test]
    fn extreme_deadlines_do_not_invert_into_immediate_expiry() {
        let start = Instant::now();
        let unrepresentable = Duration::from_secs(u64::MAX);

        assert!(
            checked_deadline(start, unrepresentable) > start,
            "an unrepresentable positive duration is beyond the process lifetime, \
             not already expired"
        );
        assert_eq!(
            checked_deadline(start, Duration::from_secs(30)),
            start + Duration::from_secs(30),
            "representable deadlines keep their exact absolute instant"
        );
    }

    #[test]
    fn ping_write_timeout_uses_delivery_budget_after_outbound_progress() {
        let max_sojourn = Duration::from_secs(15);
        let slow_consumer_timeout = Duration::from_secs(5);
        assert_eq!(
            ping_write_timeout_policy(false, max_sojourn, slow_consumer_timeout),
            (SERVER_PING_WRITE_TIMEOUT, CloseReason::ActivityTimeout),
            "an idle probe retains the prompt transport-liveness timeout"
        );
        assert_eq!(
            ping_write_timeout_policy(true, max_sojourn, slow_consumer_timeout),
            (slow_consumer_timeout, CloseReason::SlowConsumer),
            "a Ping behind progressing application output must inherit the delivery boundary"
        );
        assert_eq!(
            ping_write_timeout_policy(true, Duration::from_secs(3), slow_consumer_timeout,),
            (Duration::from_secs(3), CloseReason::SlowConsumer),
            "maximum sojourn remains the owner when it is the earlier delivery boundary"
        );
    }

    #[test]
    fn negotiate_capabilities_v2_is_relay_only_even_if_p2p_advertised() {
        // A connection negotiated below v3 must be relay-only regardless of what
        // it advertised — P2P is a v3+ upgrade and must not leak to a v2 peer.
        let (transports, topologies) = negotiate_capabilities(
            2,
            Some(vec![Transport::WebRtc, Transport::Direct]),
            Some(vec![Topology::Mesh, Topology::Host]),
        );
        assert_eq!(transports, vec![Transport::Relay]);
        assert_eq!(topologies, vec![Topology::Relay]);
    }

    #[test]
    fn negotiate_capabilities_v3_forces_relay_floor_and_dedups() {
        // Client advertised P2P but omitted Relay and repeated WebRtc: Relay is
        // forced in, duplicates removed, preference order preserved.
        let (transports, topologies) = negotiate_capabilities(
            3,
            Some(vec![
                Transport::WebRtc,
                Transport::Direct,
                Transport::WebRtc,
            ]),
            Some(vec![Topology::Mesh, Topology::Mesh]),
        );
        assert_eq!(
            transports,
            vec![Transport::WebRtc, Transport::Direct, Transport::Relay]
        );
        assert_eq!(topologies, vec![Topology::Mesh, Topology::Relay]);
    }

    #[test]
    fn negotiate_capabilities_v3_defaults_to_relay_when_unspecified() {
        let (transports, topologies) = negotiate_capabilities(3, None, None);
        assert_eq!(transports, vec![Transport::Relay]);
        assert_eq!(topologies, vec![Topology::Relay]);
    }

    #[test]
    fn reserved_room_operation_capability_requires_explicit_negotiation() {
        let compatibility = vec![
            "sdk_feature".to_string(),
            ROOM_OPERATION_IDS_CAPABILITY.to_string(),
            ROOM_OPERATION_IDS_CAPABILITY.to_string(),
        ];
        assert_eq!(
            protocol_info_capabilities(compatibility.clone(), false),
            vec!["sdk_feature"],
            "a compatibility manifest cannot pre-enable a reserved wire extension"
        );
        assert_eq!(
            protocol_info_capabilities(compatibility, true),
            vec![
                "sdk_feature".to_string(),
                ROOM_OPERATION_IDS_CAPABILITY.to_string()
            ],
            "explicit negotiation advertises the reserved token exactly once"
        );
    }

    #[test]
    fn dedup_preserving_order_drops_non_adjacent_repeats() {
        // Non-adjacent duplicates (the case `Vec::dedup` misses) are removed,
        // first-occurrence order is preserved.
        let mut transports = vec![
            Transport::WebRtc,
            Transport::Relay,
            Transport::WebRtc,
            Transport::Direct,
            Transport::Relay,
        ];
        dedup_preserving_order(&mut transports);
        assert_eq!(
            transports,
            vec![Transport::WebRtc, Transport::Relay, Transport::Direct]
        );

        // Already-unique and empty inputs are unchanged.
        let mut unique = vec![Topology::Relay, Topology::Mesh, Topology::Host];
        dedup_preserving_order(&mut unique);
        assert_eq!(
            unique,
            vec![Topology::Relay, Topology::Mesh, Topology::Host]
        );

        let mut empty: Vec<Transport> = Vec::new();
        dedup_preserving_order(&mut empty);
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn final_close_reason_prefers_current_shutdown_after_older_wake_reason() {
        let (signal, mut listener) = ConnectionCloseSignal::channel();

        assert!(signal.request_close(CloseReason::ActivityTimeout));
        let observed_reason =
            closed_with_timeout(&mut listener, "initial activity-timeout close").await;
        assert_eq!(observed_reason, Some(CloseReason::ActivityTimeout));

        assert!(
            signal.request_close(CloseReason::Shutdown),
            "shutdown drain must be able to supersede an earlier lifecycle reason"
        );

        assert_eq!(
            resolve_final_close_reason(observed_reason, &listener),
            Some(CloseReason::Shutdown),
            "the close frame must use the current shutdown reason, not the older wake reason"
        );
    }

    #[tokio::test]
    async fn close_frame_reason_prefers_shutdown_started_after_reason_resolution() {
        let server = EnhancedGameServer::new(
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
        .expect("failed to construct test server");

        assert_eq!(
            close_frame_reason_for_server(Some(CloseReason::ActivityTimeout), &server),
            CloseReason::ActivityTimeout
        );

        assert!(
            server.begin_shutdown_drain().started_by_this_call,
            "test must transition the server into draining"
        );

        assert_eq!(
            close_frame_reason_for_server(Some(CloseReason::ActivityTimeout), &server),
            CloseReason::Shutdown,
            "shutdown drain that starts during final flush must still own the close frame"
        );
        assert_eq!(
            close_frame_reason_for_server(None, &server),
            CloseReason::Shutdown,
            "plain unregistration also becomes shutdown once drain is active"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn test_websocket_connection() {
        // Add overall test timeout to prevent infinite hanging
        let test_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            test_websocket_connection_impl(),
        )
        .await;

        match test_result {
            Ok(Ok(())) => {} // Test completed successfully
            // A failed setup/exchange MUST fail the test loudly — never a silent
            // `return` that lets CI pass while the server never became reachable.
            Ok(Err(error)) => panic!("websocket connection test failed: {error:#}"),
            Err(_) => panic!("Test timed out after 30 seconds"),
        }
    }

    // Returns `Result` so EVERY setup/exchange failure propagates and fails the
    // test (the wrapper panics on `Err`). A bare `return` here would let a broken
    // server silently pass CI — the exact regression this shape prevents (and the
    // `tests/loud_test_failures_scan.rs` guard enforces repo-wide).
    async fn test_websocket_connection_impl() -> anyhow::Result<()> {
        use anyhow::Context as _;

        // Start test server
        let addr: SocketAddr = "127.0.0.1:0".parse().context("parse test address")?;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .context("bind test listener")?;
        let addr = listener
            .local_addr()
            .context("read local listener address")?;

        let game_server = EnhancedGameServer::new(
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
        .context("create game server")?;
        // Shut the server down at the end instead of leaving it running.
        // Aborting the serve task alone is not enough: the upgraded WebSocket
        // handler is its own task holding room, queue, and reconnection state,
        // so the whole server stays live and LeakSanitizer intermittently
        // reports all of it at process exit (issue #209 — 29 allocations
        // spanning `EnhancedGameServer::new`, `handle_join_room`,
        // `handle_socket`, and `register_disconnection`, all of it this one
        // server). This mirrors `RunningTestServer::shutdown` in
        // tests/test_helpers.rs, which integration tests already use and whose
        // `Drop` asserts it was called.
        let server_for_shutdown = std::sync::Arc::clone(&game_server);
        let app =
            super::super::routes::create_router("http://localhost:3000").with_state(game_server);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server_task = tokio::spawn(async move {
            if let Err(e) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            {
                tracing::error!("Test server failed: {}", e);
            }
        });

        // Every `?` below must still reach the teardown, so the exchange runs
        // inside its own future and its result is returned after the abort.
        let exchange: anyhow::Result<()> = async {
            // Poll the server until it accepts a WebSocket connection, rather than a
            // fixed startup sleep (zero-flakiness policy, .llm/context-testing.md): a
            // fixed sleep flakes when an oversubscribed runner has not bound the
            // listener yet. The happy path connects on the first attempt (typically
            // within a few ms of the spawn); the generous deadline only bites under
            // pathological load.
            let url = format!("ws://{addr}/ws");
            let ready_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
            let ws_stream = loop {
                match tokio::time::timeout(tokio::time::Duration::from_secs(5), connect_async(&url))
                    .await
                {
                    Ok(Ok((stream, _response))) => break stream,
                    outcome => {
                        anyhow::ensure!(
                            tokio::time::Instant::now() < ready_deadline,
                            "WebSocket server did not become ready within 30s: {outcome:?}"
                        );
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    }
                }
            };
            let (mut ws_sender, mut ws_receiver) = ws_stream.split();

            // Send join room message
            let join_message = ClientMessage::JoinRoom {
                game_name: "test_game".to_string(),
                room_code: None,
                player_name: "TestPlayer".to_string(),
                max_players: Some(4),
                supports_authority: Some(true),
                relay_transport: None,
            };

            let json_message =
                serde_json::to_string(&join_message).context("serialize join message")?;
            ws_sender
                .send(TungsteniteMessage::Text(json_message.into()))
                .await
                .context("send join message")?;

            // Receive response with timeout — propagate the elapsed, closed-stream,
            // and transport-error cases instead of swallowing any of them.
            let msg = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws_receiver.next())
                .await
                .context("timed out waiting for join response after 5s")?
                .context("websocket closed before sending a join response")?
                .context("receive websocket message")?;

            let TungsteniteMessage::Text(text) = msg else {
                anyhow::bail!("expected a Text websocket frame, got {msg:?}");
            };
            let server_message: ServerMessage =
                serde_json::from_str(&text).context("deserialize server message")?;
            match server_message {
                ServerMessage::RoomJoined(_) => Ok(()),
                ServerMessage::RoomJoinFailed { reason, .. } => {
                    anyhow::bail!("room join failed: {reason}")
                }
                other => anyhow::bail!("unexpected server message: {other:?}"),
            }
        }
        .await;

        // Teardown, in the same order as `RunningTestServer::shutdown`: stop
        // accepting, close registered connections, wait for their handler tasks
        // to actually finish, then join the serve task. Dropping the client
        // sockets above is not sufficient on its own — the server-side handler
        // has to observe the close and unwind before its state is released.
        let settle = crate::websocket::registered_connection_shutdown_settle_timeout();
        let _drain = server_for_shutdown.begin_shutdown_drain();
        let _ = shutdown_tx.send(());
        server_for_shutdown.close_connections_for_shutdown();
        // Hard failure, not a warning. Upgraded WebSocket handlers are tracked
        // separately from `axum::serve`, so the serve join below can succeed
        // while handlers are still live — and a warning here would let the test
        // pass in exactly the leaky state this teardown exists to prevent.
        // `RunningTestServer::shutdown` asserts on this for the same reason.
        let remaining = server_for_shutdown
            .wait_for_shutdown_connections(settle)
            .await;
        anyhow::ensure!(
            remaining == 0,
            "test server retained {remaining} WebSocket handler(s) after shutdown"
        );
        // `&mut` matters: passing the handle by value would drop it on timeout,
        // and dropping a `JoinHandle` detaches the task rather than cancelling
        // it — leaving the serve task and any still-registered handlers alive,
        // which is the exact failure mode this teardown exists to prevent.
        let mut server_task = server_task;
        match tokio::time::timeout(settle, &mut server_task).await {
            Ok(joined) => {
                joined.context("test server task panicked")?;
            }
            Err(_) => {
                server_task.abort();
                let _ = server_task.await;
                anyhow::bail!("test server task did not stop after connection drain");
            }
        }
        exchange
    }

    /// One real upgraded WebSocket: the production sink type on the server
    /// half, and the client half of the same TCP connection. Nothing here is
    /// mocked — the teardown path under test writes real frames that the
    /// client either observes or does not.
    struct UpgradedSocketPair {
        server_sink: futures_util::stream::SplitSink<WebSocket, Message>,
        _server_stream: futures_util::stream::SplitStream<WebSocket>,
        client: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        serve_task: tokio::task::JoinHandle<()>,
    }

    impl UpgradedSocketPair {
        async fn connect() -> Self {
            Self::connect_with_small_client_recv_buffer(None).await
        }

        async fn connect_with_small_client_recv_buffer(clamped_recv_buffer: Option<u32>) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind upgraded-pair listener");
            let addr = listener.local_addr().expect("read upgraded-pair address");
            let (socket_tx, socket_rx) = tokio::sync::oneshot::channel::<WebSocket>();
            let socket_tx = Arc::new(std::sync::Mutex::new(Some(socket_tx)));
            let app = axum::Router::new().route(
                "/ws",
                axum::routing::get(move |upgrade: axum::extract::WebSocketUpgrade| {
                    let socket_tx = Arc::clone(&socket_tx);
                    async move {
                        upgrade.on_upgrade(move |socket| async move {
                            let handoff = socket_tx
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .take()
                                .expect("upgraded-pair handoff is used once");
                            let _ = handoff.send(socket);
                        })
                    }
                }),
            );
            let serve_task = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            let client = match clamped_recv_buffer {
                Some(recv_buffer_bytes) => {
                    let socket = tokio::net::TcpSocket::new_v4()
                        .expect("create IPv4 TCP socket for upgraded pair");
                    socket
                        .set_recv_buffer_size(recv_buffer_bytes)
                        .expect("clamp SO_RCVBUF before upgraded-pair connect");
                    let stream = socket.connect(addr).await.expect("upgraded-pair connect");
                    let url = format!("ws://{addr}/ws");
                    tokio_tungstenite::client_async(
                        url,
                        tokio_tungstenite::MaybeTlsStream::Plain(stream),
                    )
                    .await
                    .expect("clamped upgraded-pair client upgrade")
                    .0
                }
                None => {
                    connect_async(format!("ws://{addr}/ws"))
                        .await
                        .expect("client upgrade")
                        .0
                }
            };
            let socket = socket_rx.await.expect("server socket handoff");
            let (server_sink, server_stream) = socket.split();
            Self {
                server_sink,
                _server_stream: server_stream,
                client,
                serve_task,
            }
        }

        /// Drain the client until the server's close frame arrives, returning
        /// the `n` values of every `GameData` frame that reached the wire.
        async fn drain_written_game_data(&mut self) -> Vec<u64> {
            let mut written = Vec::new();
            let drain = async {
                while let Some(frame) = self.client.next().await {
                    match frame.expect("client frame") {
                        TungsteniteMessage::Text(text) => {
                            let message: ServerMessage = serde_json::from_str(&text)
                                .unwrap_or_else(|error| panic!("decode {text}: {error}"));
                            if let ServerMessage::GameData { data, .. } = message {
                                written.push(
                                    data.get("n")
                                        .and_then(serde_json::Value::as_u64)
                                        .expect("ledger frame carries its n"),
                                );
                            }
                        }
                        TungsteniteMessage::Close(_) => return,
                        _other_frame => continue,
                    }
                }
            };
            tokio::time::timeout(Duration::from_secs(10), drain)
                .await
                .expect("server never closed the upgraded socket");
            written
        }

        async fn read_exact_close(&mut self) -> (u16, String) {
            match tokio::time::timeout(Duration::from_secs(10), self.client.next()).await {
                Ok(Some(Ok(TungsteniteMessage::Close(Some(frame))))) => {
                    (frame.code.into(), frame.reason.to_string())
                }
                Ok(Some(Ok(frame))) => panic!("application frame leaked before close: {frame:?}"),
                Ok(Some(Err(error))) => panic!("transport error before close: {error}"),
                Ok(None) => panic!("socket ended without a close frame"),
                Err(_elapsed) => panic!("timed out waiting for close frame"),
            }
        }

        async fn shutdown(self) {
            self.serve_task.abort();
            let _ = self.serve_task.await;
        }
    }

    fn ledger_data(seq: u64) -> crate::coordination::outbound_queue::OutboundData {
        let from_player = PlayerId::from_u128(9);
        let room_id = crate::protocol::RoomId::from_u128(11);
        crate::coordination::outbound_queue::OutboundData::new(
            Arc::new(ServerMessage::GameData {
                from_player,
                data: serde_json::json!({ "n": seq }),
                seq: Some(seq),
                epoch: Some(1),
                class: Some(crate::protocol::DeliveryClass::Reliable),
                key: None,
            }),
            crate::coordination::outbound_queue::DataDeliveryMetadata {
                class: crate::protocol::DeliveryClass::Reliable,
                key: None,
                from_player,
                room_id,
                epoch: 1,
                seq,
            },
        )
    }

    /// Issue #274: a graceful teardown must never write the queue that sits
    /// behind a socket write which was abandoned in flight.
    ///
    /// The live write loop runs inside the close `select!`, so a close request
    /// cancels it wherever it is — including while a socket write owns one
    /// queued payload. That payload's wire position is then unknown, and the
    /// close flush used to keep writing everything queued behind it. The
    /// recipient then observes a delivered sequence that skips a sequence no
    /// `DeliveryReport` ever described: the unexplained mid-stream hole issue
    /// #274 recorded (`expected 90, got 91`).
    ///
    /// Both halves run against a real upgraded socket, so the oracle is the
    /// bytes the client actually received.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn close_flush_never_writes_the_queue_behind_an_abandoned_write() {
        const QUEUED: u64 = 3;
        for (abandoned_in_flight, expected_written, context) in [
            (false, vec![1, 2, 3], "healthy teardown flushes its queue"),
            (
                true,
                Vec::new(),
                "teardown after an abandoned in-flight write writes nothing behind it",
            ),
        ] {
            let server = test_server().await;
            let player_id = PlayerId::from_u128(9);
            let (tx, mut rx) = crate::coordination::outbound_queue::channel(16, 16);
            for seq in 1..=QUEUED {
                tx.try_enqueue_data(ledger_data(seq))
                    .unwrap_or_else(|_| panic!("{context}: queue seq {seq}"));
            }

            let (close_signal, _close_listener) = ConnectionCloseSignal::channel();
            let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
            if abandoned_in_flight {
                // The production seam, exactly: a socket write owned one
                // payload and its future was dropped before resolving.
                let accounting = crate::websocket::sending::SendAccounting::new(
                    &rx,
                    &server,
                    &probe_state,
                    player_id,
                    Some(crate::protocol::DeliveryClass::Reliable),
                );
                drop(accounting);
            }

            let mut pair = UpgradedSocketPair::connect().await;
            let mut batcher = MessageBatcher::new(1, 1);
            finalize_closed_connection(
                &mut pair.server_sink,
                &mut rx,
                &mut batcher,
                None,
                &player_id,
                &server,
                &close_signal,
                &probe_state,
                Duration::from_secs(5),
            )
            .await;

            let written = pair.drain_written_game_data().await;
            assert_eq!(
                written, expected_written,
                "{context}: the client's observed stream must match the contract"
            );
            let expected_dropped = if abandoned_in_flight {
                // The abandoned in-flight payload plus the whole queue behind it.
                QUEUED + 1
            } else {
                0
            };
            assert_eq!(
                server
                    .metrics()
                    .websocket_messages_dropped
                    .load(Ordering::Relaxed),
                expected_dropped,
                "{context}: abandoned payloads must be counted, never lost silently"
            );
            pair.shutdown().await;
        }
    }

    /// The server→client messages documented as v3-only must fail closed on a
    /// pre-v3 wire.
    ///
    /// `Signal`, `NewPeer`, `SessionPlan`, `PeerTransportStatus`, `RelayStats`,
    /// `GoingAway`, and `DeliveryReport`, plus the capability-gated
    /// `RoomOperationResult`, exist only on the protocol-v3 wire
    /// (`docs/protocol.md` "New v3 messages"). Every emission site checks the
    /// recipient's negotiated version before enqueueing, but routing and a
    /// reconnect identity swap can race that check (issue #463): the writer is
    /// the last serialization point that owns the recipient's version, so it
    /// suppresses these variants exactly where the per-recipient GameData
    /// stamp projection already lives — accounted, never fenced, never on the
    /// wire of a connection that cannot parse them.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn v3_only_messages_fail_closed_on_a_pre_v3_wire() {
        use crate::protocol::{
            RoomOperationId, RoomOperationResult, SessionGeneration, SessionPlanPayload, Topology,
            Transport,
        };

        let v3_only_messages: Vec<(&'static str, ServerMessage)> = vec![
            (
                "Signal",
                ServerMessage::Signal {
                    from: PlayerId::from_u128(4),
                    generation: SessionGeneration::from_u128(1),
                    signal: serde_json::json!({ "offer": {} }),
                },
            ),
            (
                "NewPeer",
                ServerMessage::NewPeer {
                    peer_id: PlayerId::from_u128(4),
                    you_initiate: true,
                },
            ),
            (
                "SessionPlan",
                ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
                    generation: SessionGeneration::from_u128(1),
                    topology: Topology::Relay,
                    transport: Transport::Relay,
                    host: None,
                    direct_endpoint: None,
                    peers: Vec::new(),
                    ice_servers: Vec::new(),
                    fallback: Transport::Relay,
                })),
            ),
            (
                "PeerTransportStatus",
                ServerMessage::PeerTransportStatus {
                    peer_id: PlayerId::from_u128(4),
                    transport: Transport::WebRtc,
                    connected: true,
                },
            ),
            (
                "RelayStats",
                ServerMessage::RelayStats {
                    interval_ms: 1_000,
                    sent_to_you: 1,
                    dropped_for_you: 0,
                    backpressure_events: 0,
                },
            ),
            (
                "GoingAway",
                ServerMessage::GoingAway {
                    deadline_ms: 1,
                    retry_after_secs: None,
                },
            ),
            (
                "DeliveryReport",
                ServerMessage::DeliveryReport(Box::default()),
            ),
            (
                "RoomOperationResult",
                ServerMessage::RoomOperationResult {
                    operation_id: RoomOperationId::from_u128(1),
                    result: Box::new(RoomOperationResult::RoomLeft),
                },
            ),
        ];

        for (name, message) in v3_only_messages {
            for (recipient_is_v3, context) in [
                (false, "{name}: pre-v3 queue must suppress the frame"),
                (true, "{name}: v3 queue must still deliver the frame"),
            ] {
                let context = context.replace("{name}", name);
                let server = test_server().await;
                let player_id = PlayerId::from_u128(9);
                let (tx, rx) = crate::coordination::outbound_queue::channel(4, 4);
                if recipient_is_v3 {
                    tx.set_protocol_version(3);
                }

                let (close_signal, _close_listener) = ConnectionCloseSignal::channel();
                let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
                let mut pair = UpgradedSocketPair::connect().await;

                let queued = Arc::new(message.clone());
                send_queued(
                    &mut pair.server_sink,
                    crate::coordination::outbound_queue::QueuedOutbound::test_control(Arc::clone(
                        &queued,
                    )),
                    None,
                    &rx,
                    &player_id,
                    &server,
                    &close_signal,
                    &probe_state,
                    Duration::ZERO,
                    WritePhase::Live,
                )
                .await
                .unwrap_or_else(|error| {
                    panic!("{context}: the suppression must never fail the writer: {error:?}")
                });

                if recipient_is_v3 {
                    let frame = tokio::time::timeout(Duration::from_secs(10), pair.client.next())
                        .await
                        .unwrap_or_else(|_elapsed| {
                            panic!("{context}: frame write completed before this read")
                        })
                        .expect("client frame");
                    match frame.expect("client frame") {
                        TungsteniteMessage::Text(text) => {
                            let decoded: ServerMessage = serde_json::from_str(&text)
                                .unwrap_or_else(|error| {
                                    panic!("{context}: decode {text}: {error}")
                                });
                            assert_eq!(
                                std::mem::discriminant(&decoded),
                                std::mem::discriminant(queued.as_ref()),
                                "{context}: the delivered variant must be the queued one"
                            );
                        }
                        other => panic!("{context}: expected a text frame, got {other:?}"),
                    }
                } else {
                    let leaked =
                        tokio::time::timeout(Duration::from_millis(500), pair.client.next()).await;
                    match leaked {
                        Err(_elapsed) => {}
                        Ok(Some(Ok(TungsteniteMessage::Close(_)))) => {}
                        Ok(other) => {
                            panic!("{context}: v3-only frame reached the pre-v3 wire: {other:?}")
                        }
                    }
                    assert!(
                        !rx.abandoned_in_flight_write(),
                        "{context}: a known-rejected-before-write frame must never fence the queue"
                    );
                    assert_eq!(
                        server
                            .metrics()
                            .websocket_messages_dropped
                            .load(Ordering::Relaxed),
                        1,
                        "{context}: the suppressed frame must be accounted, not lost silently"
                    );
                }
                pair.shutdown().await;
            }
        }
    }

    /// A 1009 teardown must flush coalesced unsupported-format omission
    /// reports like every other finalize branch.
    ///
    /// A queued `DeliveryReport` carrier pops with its post-write flush still
    /// ahead of it (`flush_before` is deliberately false for carriers), so an
    /// oversized carrier's own TooLarge failure lands in the
    /// `OutboundMessageTooLarge` finalize branch with writable coalesced
    /// omissions still pending. The recipient already observed those frames
    /// being skipped; letting them die silently leaves them unreported even
    /// though the socket remained writable, contradicting the documented
    /// close-flush promise ("a closing connection flushes them too").
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn too_large_close_still_flushes_coalesced_omission_reports() {
        let server = test_server().await;
        let player_id = PlayerId::from_u128(9);
        let (tx, mut rx) = crate::coordination::outbound_queue::channel(16, 16);
        tx.set_protocol_version(3);
        let omitted = |seq| crate::coordination::outbound_queue::DataDeliveryMetadata {
            class: crate::protocol::DeliveryClass::Volatile,
            key: None,
            from_player: PlayerId::from_u128(4),
            room_id: crate::protocol::RoomId::from_u128(11),
            epoch: 1,
            seq,
        };
        assert!(rx.record_unsupported_format(omitted(7)));
        assert!(rx.record_unsupported_format(omitted(8)));
        assert!(
            rx.pending_unsupported_report().is_some(),
            "precondition: coalesced omissions must be pending before teardown"
        );

        let (close_signal, _close_listener) = ConnectionCloseSignal::channel();
        let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
        let mut pair = UpgradedSocketPair::connect().await;
        let mut batcher = MessageBatcher::new(1, 1);
        finalize_closed_connection(
            &mut pair.server_sink,
            &mut rx,
            &mut batcher,
            Some(CloseReason::OutboundMessageTooLarge),
            &player_id,
            &server,
            &close_signal,
            &probe_state,
            Duration::from_secs(5),
        )
        .await;

        // The oracle is the bytes the client received: the omission report
        // must reach the writable socket BEFORE the coded close frame.
        let (reports, close_frame) = tokio::time::timeout(Duration::from_secs(10), async {
            let mut reports = Vec::new();
            let mut close_frame = None;
            while let Some(frame) = pair.client.next().await {
                match frame.expect("client frame") {
                    TungsteniteMessage::Text(text) => {
                        if matches!(
                            serde_json::from_str::<ServerMessage>(&text),
                            Ok(ServerMessage::DeliveryReport(_))
                        ) {
                            reports.push(text);
                        }
                    }
                    TungsteniteMessage::Close(close) => {
                        close_frame =
                            Some(close.expect("the semantic close frame carries its code"));
                        break;
                    }
                    _other_frame => continue,
                }
            }
            (reports, close_frame)
        })
        .await
        .expect("server never closed the upgraded socket");
        assert_eq!(
            reports.len(),
            1,
            "the coalesced omission report must be flushed exactly once on a 1009 close"
        );
        let report: ServerMessage =
            serde_json::from_str(&reports[0]).expect("flushed report decodes");
        let ServerMessage::DeliveryReport(report) = report else {
            panic!("only a DeliveryReport was collected");
        };
        assert_eq!(report.gaps.len(), 1, "contiguous omissions stay one range");
        let gap = &report.gaps[0];
        assert_eq!(gap.from_player, PlayerId::from_u128(4));
        assert_eq!((gap.from_seq, gap.to_seq), (7, 8));
        assert_eq!(
            gap.reason,
            crate::protocol::DeliveryGapReason::UnsupportedFormat,
            "the flushed ranges must be exactly the omissions seeded before teardown"
        );
        let close_frame = close_frame.expect("the teardown ends in its coded close frame");
        assert_eq!(
            close_frame.code,
            1009.into(),
            "the oversized-message close code is unchanged by the flush"
        );
        assert!(
            rx.pending_unsupported_report().is_none(),
            "a flushed report must be retired from pending accounting"
        );
        drop(tx);
        pair.shutdown().await;
    }

    /// The one write path that bypasses the fail-closed v3-only arm
    /// (`write_pending_unsupported_report`) must itself fail closed on a
    /// pre-v3 queue.
    ///
    /// A pre-v3 queue can never accumulate a pending unsupported-format
    /// report: accumulation is v3-gated at record time, and no `Authenticate`
    /// is ever processed after any other client message has been seen (later
    /// attempts are refused, or the connection is closed outright in allowlist
    /// mode), while omissions require room data flow. This pin simulates that
    /// invariant being violated anyway and requires the write path to fail
    /// closed: the v3-only frame never reaches the pre-v3 wire, and the
    /// unwritable ranges stay pending (ranges retire only after the wire,
    /// never before).
    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn pending_omission_report_fails_closed_on_a_pre_v3_wire() {
        let server = test_server().await;
        let player_id = PlayerId::from_u128(9);
        let (tx, mut rx) = crate::coordination::outbound_queue::channel(16, 16);
        tx.set_protocol_version(3);
        let omitted = |seq| crate::coordination::outbound_queue::DataDeliveryMetadata {
            class: crate::protocol::DeliveryClass::Volatile,
            key: None,
            from_player: PlayerId::from_u128(4),
            room_id: crate::protocol::RoomId::from_u128(11),
            epoch: 1,
            seq,
        };
        assert!(rx.record_unsupported_format(omitted(7)));
        assert!(
            rx.pending_unsupported_report().is_some(),
            "precondition: the omission must be pending before teardown"
        );
        // The only way a pre-v3 queue could hold a pending report: a version
        // regression after v3 accumulation. Unreachable in production; that is
        // exactly why the write path may only fail closed.
        tx.set_protocol_version(2);

        let (close_signal, _close_listener) = ConnectionCloseSignal::channel();
        let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
        let mut pair = UpgradedSocketPair::connect().await;
        let mut batcher = MessageBatcher::new(1, 1);
        finalize_closed_connection(
            &mut pair.server_sink,
            &mut rx,
            &mut batcher,
            Some(CloseReason::OutboundMessageTooLarge),
            &player_id,
            &server,
            &close_signal,
            &probe_state,
            Duration::from_secs(5),
        )
        .await;

        // The oracle is the bytes the client received: no DeliveryReport may
        // appear on the pre-v3 wire, and the coded close still completes.
        let (reports, close_frame) = tokio::time::timeout(Duration::from_secs(10), async {
            let mut reports = Vec::new();
            let mut close_frame = None;
            while let Some(frame) = pair.client.next().await {
                match frame.expect("client frame") {
                    TungsteniteMessage::Text(text) => {
                        if matches!(
                            serde_json::from_str::<ServerMessage>(&text),
                            Ok(ServerMessage::DeliveryReport(_))
                        ) {
                            reports.push(text);
                        }
                    }
                    TungsteniteMessage::Close(close) => {
                        close_frame =
                            Some(close.expect("the semantic close frame carries its code"));
                        break;
                    }
                    _other_frame => continue,
                }
            }
            (reports, close_frame)
        })
        .await
        .expect("server never closed the upgraded socket");
        assert!(
            reports.is_empty(),
            "a v3-only omission report must be fail-closed on a pre-v3 wire"
        );
        let close_frame = close_frame.expect("the teardown ends in its coded close frame");
        assert_eq!(
            close_frame.code,
            1009.into(),
            "the fail-closed bypass must not disturb the teardown close code"
        );
        assert!(
            rx.pending_unsupported_report().is_some(),
            "unwritten ranges stay pending: they retire only after the wire"
        );
        drop(tx);
        pair.shutdown().await;
    }

    /// Issue #415: teardown after an abandoned in-flight write must never hand
    /// the client a corrupt byte stream.
    ///
    /// A large binary frame's write is cancelled mid-flight on a real upgraded
    /// socket (the client clamps its receive buffer tiny and never reads, so
    /// on backpressure-applying platforms part of the frame stays unflushed),
    /// the queue is fenced
    /// exactly as
    /// `SendAccounting::drop` does in production, and then the slow-consumer
    /// teardown — farewell Error plus coded close — writes onto the same sink.
    /// The client must observe the abandoned frame COMPLETE (never truncated),
    /// every later frame on a clean frame boundary, and a decodable stream all
    /// the way through the close handshake.
    /// Client-side SO_RCVBUF clamp used to make kernel buffering (and hence a
    /// cancelled-mid-flush write) deterministic across platforms.
    const CLIENT_CLAMPED_RECV_BUFFER_BYTES: u32 = 4 << 10;

    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn teardown_behind_an_abandoned_write_still_delivers_a_clean_stream() {
        const PAYLOAD_BYTES: usize = 8 << 20;
        let server = test_server().await;
        let player_id = PlayerId::from_u128(9);
        let (_tx, mut rx) = crate::coordination::outbound_queue::channel(16, 16);
        let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());

        // The client clamps SO_RCVBUF tiny before connecting and then never
        // reads, so the server's socket buffers saturate inside the very first
        // flush on every supported OS and the huge frame cannot complete.
        let mut pair = UpgradedSocketPair::connect_with_small_client_recv_buffer(Some(
            CLIENT_CLAMPED_RECV_BUFFER_BYTES,
        ))
        .await;

        // Drive one flush poll, then cancel the write exactly as the close
        // select cancels the live loop: wherever the cancellation lands (part
        // of the frame still unflushed on kernels that apply backpressure,
        // fully accepted into buffers elsewhere), the wire position is unknown
        // to the accounting layer — which is precisely the state teardown must
        // handle cleanly.
        let send =
            pair.server_sink
                .send(axum::extract::ws::Message::Binary(axum::body::Bytes::from(
                    vec![0xAB_u8; PAYLOAD_BYTES],
                )));
        tokio::pin!(send);
        let mut context = Context::from_waker(futures_util::task::noop_waker_ref());
        let _polled = send.as_mut().poll(&mut context);
        // Dropping the pinned send future cancels it mid-flight.
        let _cancelled = send;

        // The production fence: a socket write owned one payload and its
        // future was dropped before resolving.
        let accounting = crate::websocket::sending::SendAccounting::new(
            &rx,
            &server,
            &probe_state,
            player_id,
            Some(crate::protocol::DeliveryClass::Reliable),
        );
        drop(accounting);
        assert!(
            rx.abandoned_in_flight_write(),
            "the dropped write must fence the connection"
        );

        // Split the pair so the client half drains concurrently while the
        // server half runs teardown: the bounded farewell writes need an
        // active reader to make progress, exactly like production.
        let UpgradedSocketPair {
            mut server_sink,
            mut client,
            serve_task,
            ..
        } = pair;
        let drain = tokio::spawn(async move {
            let mut frames = Vec::new();
            while let Some(frame) = client.next().await {
                match frame.expect("client stream must stay decodable") {
                    TungsteniteMessage::Close(_) => break,
                    other => frames.push(other),
                }
            }
            frames
        });

        let (close_signal, _close_listener) = ConnectionCloseSignal::channel();
        let mut batcher = MessageBatcher::new(1, 1);
        finalize_closed_connection(
            &mut server_sink,
            &mut rx,
            &mut batcher,
            Some(CloseReason::SlowConsumer),
            &player_id,
            &server,
            &close_signal,
            &probe_state,
            Duration::from_secs(5),
        )
        .await;

        let frames = tokio::time::timeout(Duration::from_secs(10), drain)
            .await
            .expect("client drain must finish in time")
            .expect("client drain task must not panic");
        serve_task.abort();
        let _ = serve_task.await;

        // The abandoned frame must arrive whole: tungstenite retains any
        // unflushed tail inside the sink, so appended teardown bytes continue
        // behind it instead of truncating it.
        assert!(
            matches!(
                frames.first(),
                Some(TungsteniteMessage::Binary(data)) if data.len() == PAYLOAD_BYTES
            ),
            "first client frame must be the complete abandoned binary payload, got {:?}",
            frames.first().map(|frame| match frame {
                TungsteniteMessage::Binary(data) => format!("binary len {}", data.len()),
                other => format!("other {other:?}"),
            })
        );
        // The farewell Error frame must follow on a clean boundary...
        assert!(
            frames
                .iter()
                .any(|frame| matches!(frame, TungsteniteMessage::Text(_))),
            "the slow-consumer farewell must reach the client"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn oversized_message_discovered_during_normal_close_flush_promotes_close_to_1009() {
        let server = test_server_with_config(ServerConfig {
            max_message_size: 64,
            max_signal_bytes: 64,
            // Pairing-legal: the relay envelope headroom above the inbound
            // cap; the 512-byte blob overflows it either way.
            max_outbound_message_size: 64 + crate::config::defaults::RELAY_ENVELOPE_HEADROOM_BYTES,
            ..ServerConfig::default()
        })
        .await;
        let player_id = PlayerId::from_u128(9);
        let (tx, mut rx) = crate::coordination::outbound_queue::channel(4, 4);
        let from_player = PlayerId::from_u128(10);
        tx.try_enqueue_data(crate::coordination::outbound_queue::OutboundData::new(
            Arc::new(ServerMessage::GameData {
                from_player,
                data: serde_json::json!({"blob": "x".repeat(512)}),
                seq: Some(1),
                epoch: Some(1),
                class: Some(crate::protocol::DeliveryClass::Reliable),
                key: None,
            }),
            crate::coordination::outbound_queue::DataDeliveryMetadata {
                class: crate::protocol::DeliveryClass::Reliable,
                key: None,
                from_player,
                room_id: crate::protocol::RoomId::from_u128(11),
                epoch: 1,
                seq: 1,
            },
        ))
        .expect("enqueue oversized close-flush message");
        drop(tx);

        let (close_signal, _listener) = ConnectionCloseSignal::channel();
        let (probe_state, _probe_updates) = watch::channel(PingProbeState::default());
        let mut pair = UpgradedSocketPair::connect().await;
        let mut batcher = MessageBatcher::new(1, 1);
        finalize_closed_connection(
            &mut pair.server_sink,
            &mut rx,
            &mut batcher,
            None,
            &player_id,
            &server,
            &close_signal,
            &probe_state,
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(
            pair.read_exact_close().await,
            (1009, "outbound_message_too_large".to_string())
        );
        assert_eq!(
            server
                .metrics()
                .websocket_messages_dropped
                .load(Ordering::Relaxed),
            1,
            "the rejected queued item must be counted exactly once"
        );
        pair.shutdown().await;
    }
}
