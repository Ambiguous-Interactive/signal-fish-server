use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// `tokio::time::Instant` (not `std::time::Instant`) so the activity reaper
// (`collect_expired_clients`) and the heartbeat-update throttle
// (`reserve_last_seen_update`) read the runtime clock. In production this wraps
// the same monotonic std clock (identical behavior); under
// `#[tokio::test(start_paused = true)]` it lets tests drive these windows with
// `tokio::time::advance(..)` deterministically, at zero wall-clock cost. Every
// `Instant::now()` here runs inside the tokio runtime (all callers are async /
// `#[tokio::test]`), so the runtime clock is always available.
use tokio::time::Instant;

use dashmap::DashMap;
use futures_util::FutureExt;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::AppContext;
use crate::coordination::{
    ClientDeliveryHandle, CloseReason, ConnectionCloseSignal, DeliverySender, MessageCoordinator,
};
use crate::metrics::{RateLimitRejection, ServerMetrics};
use crate::protocol::{ErrorCode, GameDataEncoding, PlayerId, RoomId, ServerMessage, Transport};
use crate::rate_limit::ErrorReplyGate;

use super::RegisterClientError;

pub(crate) use crate::session_capability::{NegotiatedProtocol, TransportStatusUpdate};

#[derive(Debug)]
pub(crate) struct ClientConnection {
    pub room_id: Option<RoomId>,
    /// Serializes room-role and connection-lifecycle transitions for this
    /// physical socket. The same gate survives reconnect identity swaps.
    pub lifecycle: Arc<ClientLifecycle>,
    pub last_ping: Instant,
    /// Tracks when we last recorded `last_seen` for this client.
    /// Used to throttle heartbeat updates - we only record if this is older
    /// than the configured threshold (default 30 seconds).
    pub last_heartbeat_update: Option<Instant>,
    heartbeat_update_revision: u64,
    pub sender: DeliverySender,
    /// Kill switch for this connection's socket tasks (slow-consumer
    /// disconnects, server-side eviction). Paired with `sender`: together they
    /// form the connection's [`ClientDeliveryHandle`].
    pub close: ConnectionCloseSignal,
    /// Serializes room-assignment publication and its compensating rollback.
    /// Held through the coordinator await so a canceled caller cannot race a
    /// newer assignment against the restored prior route.
    pub assignment_gate: Arc<tokio::sync::Mutex<()>>,
    pub client_addr: SocketAddr,
    pub game_data_format: GameDataEncoding,
    pub app_context: Option<AppContext>,
    /// Authenticated identity that newly issued reconnect credentials must
    /// retain. Present only for certificate-bound token-binding sessions.
    pub reconnection_identity: Option<Arc<str>>,
    /// Protocol version + transport/topology capabilities negotiated at auth.
    pub protocol: NegotiatedProtocol,
    /// Whether this physical connection explicitly negotiated correlated room
    /// operation envelopes. Preserved across a successful reconnect identity
    /// swap because the WebSocket connection itself survives that swap.
    pub room_operation_ids: bool,
    /// Negotiated framing and private delivery receipt counter for this socket.
    addressed_binary: bool,
    addressed_receipt: u64,
    /// Last data-path transport state this client reported via
    /// [`ClientMessage::TransportStatus`](crate::protocol::ClientMessage::TransportStatus)
    /// (v3 only), tagged with the room/spectator membership generation in which
    /// it was observed. `None` until the client reports — the relay floor is the
    /// implicit default and never closes regardless of what is (or is not)
    /// reported. A status from an older generation is retained only so a failed
    /// prepared transition can roll back without losing its dedup baseline.
    pub transport_status: Option<(Uuid, Transport, bool)>,
    /// Opaque room/spectator membership token for transport-status
    /// deduplication. Every committed or prepared role transition replaces it
    /// with a fresh token; a failed prepared transition restores the exact
    /// prior token. This is deliberately
    /// independent of `game_data_epoch`, which models seated sender
    /// incarnations only and does not advance for spectators.
    pub membership_generation: Uuid,
    /// Exact rollback state for the latest prepared membership transition.
    /// A later transition supersedes it; replacements are collision-resistant
    /// UUIDs explicitly distinct from the current and retained-status tokens.
    pub prior_membership_generation: Option<Uuid>,
    /// Epoch of an `assign_client_to_room` transition that is prepared but not
    /// published. Relay stamp allocation refuses while set, so a failed
    /// publication cannot reuse a stamp that reached recipients.
    pub pending_room_assignment: Option<u32>,
    /// Last relay sequence number stamped on this client's outbound game data
    /// (protocol v3): the per-(sender, room) counter behind
    /// [`ServerMessage::GameData::seq`](crate::protocol::ServerMessage). `0`
    /// means "nothing stamped yet" (the first stamp is 1). Owned here because
    /// its lifecycle is exactly the connection's room membership: it RESETS
    /// wherever that membership does — [`ConnectionManager::assign_client_to_room`],
    /// [`ConnectionManager::clear_room_assignment`], and the fresh connection
    /// state built by [`ConnectionManager::reassign_connection`] (restart-on-
    /// rejoin: recipients treat a sender's rejoin/reconnect as a seq reset) —
    /// and it is cleaned up with the connection, with no separate map to leak.
    pub game_data_seq: u64,
    /// Incarnation epoch for this client's outbound game-data stream (protocol
    /// v3), behind [`ServerMessage::GameData::epoch`](crate::protocol::ServerMessage).
    ///
    /// It is a single **monotonic per-connection** counter that increments once
    /// each time a NEW incarnation of a room membership begins —
    /// [`ConnectionManager::assign_client_to_room`] (join / seat-fill join) and
    /// [`ConnectionManager::reassign_connection`] (reconnect) — so `0` means
    /// "never joined a room" and the first incarnation is epoch `1`. Leaving a
    /// room ([`ConnectionManager::clear_room_assignment`]) resets `seq` but does
    /// NOT bump the epoch: the NEXT join does. It is deliberately NOT reset when
    /// the same connection switches rooms — a room-B membership entered after a
    /// room-A one carries a higher epoch, not a fresh `1`. This is what upholds
    /// the client-facing contract: `(epoch, seq)` is strictly increasing per
    /// `(sender, room)` as observed by ANY single recipient. A counter reset per
    /// room membership would instead REPEAT an epoch it already used when a
    /// sender leaves and rejoins the SAME room — a recipient that stayed would
    /// see `(epoch, seq)` collide (the same `epoch`, with `seq` restarting at
    /// 1), the very ambiguity `epoch` exists to remove. Keeping a distinct
    /// per-`(player, room)` epoch across leave/rejoin would instead require
    /// unbounded server state; a single monotonic counter guarantees the
    /// strictly-increasing invariant for free. The
    /// absolute value is not meaningful to clients — they baseline each sender
    /// from the epoch on its snapshot / first frame and only compare relatively.
    /// Paired with `game_data_seq` (which restarts at 1 per epoch) it makes a
    /// `seq` restart self-describing.
    pub game_data_epoch: u32,
    /// Per-connection inbound error-reply budget (issue #518): every polite
    /// per-frame reply this connection buys — a refusal or failure envelope
    /// of any kind, or the `Pong` keepalive answer — charges it. An exhausted
    /// budget closes the connection with `4006 inbound_rate_limited` instead
    /// of replying again (see [`ConnectionManager::charge_error_reply`]).
    /// The gate follows the physical socket across a reconnect identity swap.
    pub error_reply_gate: std::sync::Mutex<ErrorReplyGate>,
}

/// Exact connection state replaced by a room assignment. A failed or canceled
/// coordinator publication restores this snapshot instead of guessing the
/// previous room and stamp.
#[derive(Debug, Clone)]
struct RoomAssignmentSnapshot {
    room_id: Option<RoomId>,
    game_data_seq: u64,
    game_data_epoch: u32,
    membership_generation: Uuid,
    prior_membership_generation: Option<Uuid>,
    pending_room_assignment: Option<u32>,
    delivery: ClientDeliveryHandle,
}

impl RoomAssignmentSnapshot {
    fn capture(client: &ClientConnection) -> Self {
        Self {
            room_id: client.room_id,
            game_data_seq: client.game_data_seq,
            game_data_epoch: client.game_data_epoch,
            membership_generation: client.membership_generation,
            prior_membership_generation: client.prior_membership_generation,
            pending_room_assignment: client.pending_room_assignment,
            delivery: client.delivery_handle(),
        }
    }
}

/// The prepared state a room assignment published, plus the snapshot it must
/// restore while that publication is uncommitted.
#[derive(Debug)]
struct PreparedRoomAssignment {
    room_id: RoomId,
    epoch: u32,
    membership_generation: Uuid,
    previous: RoomAssignmentSnapshot,
}

/// Restore an unpublished [`PreparedRoomAssignment`] on error or cancellation.
///
/// Local state is restored before the guard returns, so a canceled caller
/// never observes ghost membership. An owned task then republishes the prior
/// coordinator route after taking the lifecycle gate and the assignment gate,
/// in that order, so a newer assignment cannot race or deadlock with it.
struct RoomAssignmentRollback {
    manager: Arc<ConnectionManager>,
    player_id: PlayerId,
    lifecycle: Arc<ClientLifecycle>,
    prepared: Option<PreparedRoomAssignment>,
    gate: Option<tokio::sync::OwnedMutexGuard<()>>,
    gate_handle: Arc<tokio::sync::Mutex<()>>,
}

impl RoomAssignmentRollback {
    fn commit(mut self) {
        if let Some(prepared) = self.prepared.take() {
            self.manager
                .commit_prepared_room_assignment(&self.player_id, prepared.epoch);
        }
    }
}

impl Drop for RoomAssignmentRollback {
    fn drop(&mut self) {
        let Some(prepared) = self.prepared.take() else {
            return;
        };
        let Some(gate) = self.gate.take() else {
            return;
        };
        if !self
            .manager
            .lifecycle_matches(&self.player_id, &self.lifecycle)
        {
            return;
        }
        let previous = prepared.previous.clone();
        let previous_room = previous.room_id;
        let previous_delivery = previous.delivery.clone();
        let previous_epoch = previous.game_data_epoch;
        let previous_membership_generation = previous.membership_generation;
        if !self
            .manager
            .restore_uncommitted_room_assignment(&self.player_id, prepared)
        {
            return;
        }
        // Cancellation drops the caller's lifecycle guard after this future.
        // Acquire the lifecycle gate before the assignment gate to keep the
        // same lock order as a new wrapper assignment.
        drop(gate);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let manager = Arc::clone(&self.manager);
        let player_id = self.player_id;
        let lifecycle = Arc::clone(&self.lifecycle);
        let gate = Arc::clone(&self.gate_handle);
        runtime.spawn(async move {
            let _lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
            if !manager.lifecycle_matches(&player_id, &lifecycle) {
                return;
            }
            let _gate = gate.lock_owned().await;
            if !manager.has_restored_room_assignment(
                &player_id,
                previous_room,
                previous_epoch,
                previous_membership_generation,
            ) {
                return;
            }
            if let Err(err) = manager
                .message_coordinator
                .register_local_client(player_id, previous_room, previous_delivery)
                .await
            {
                warn!(
                    %player_id,
                    %err,
                    "Failed to restore the prior coordinator route after a canceled room assignment"
                );
                manager.request_close_for(&player_id, CloseReason::Unregistered);
            }
        });
    }
}

/// Release an unfinished refresh without changing a newer connection or refresh.
pub(super) struct HeartbeatUpdateReservation<'a> {
    manager: &'a ConnectionManager,
    player_id: PlayerId,
    lifecycle: Arc<ClientLifecycle>,
    revision: u64,
    completed: bool,
}

impl HeartbeatUpdateReservation<'_> {
    pub(super) fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for HeartbeatUpdateReservation<'_> {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        if let Some(mut client) = self.manager.clients.get_mut(&self.player_id) {
            if Arc::ptr_eq(&client.lifecycle, &self.lifecycle)
                && client.heartbeat_update_revision == self.revision
            {
                client.last_heartbeat_update = None;
            }
        }
    }
}

/// Per-physical-connection lifecycle identity and serialization gate.
///
/// The player id is stored beside the gate so an unregister task that was
/// queued under the transient pre-reconnect id follows the surviving socket to
/// its restored id after it acquires the gate.
#[derive(Debug)]
pub(crate) struct ClientLifecycle {
    transport_reports: tokio::sync::Mutex<()>,
    unregister_running: std::sync::atomic::AtomicBool,
    unregister_pending: std::sync::atomic::AtomicBool,
    unregister_finished: tokio::sync::Notify,
    unregister_departure: std::sync::Mutex<Option<Arc<super::room_service::UnregisterDeparture>>>,
    gate: Arc<tokio::sync::Mutex<()>>,
    player_id: std::sync::Mutex<PlayerId>,
    admission_pending: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    lock_attempts: AtomicUsize,
    #[cfg(test)]
    lock_attempt_notify: tokio::sync::Notify,
}

/// Release an interrupted teardown for a later maintenance attempt.
pub(super) struct UnregisterAttempt {
    lifecycle: Arc<ClientLifecycle>,
    complete: bool,
}

impl UnregisterAttempt {
    pub(super) fn complete(&mut self) {
        self.complete = true;
    }
}

impl Drop for UnregisterAttempt {
    fn drop(&mut self) {
        self.lifecycle
            .unregister_pending
            .store(!self.complete, Ordering::Release);
        self.lifecycle
            .unregister_running
            .store(false, Ordering::Release);
        self.lifecycle.unregister_finished.notify_waiters();
    }
}

impl ClientLifecycle {
    fn new(player_id: PlayerId) -> Self {
        Self {
            transport_reports: tokio::sync::Mutex::new(()),
            unregister_running: std::sync::atomic::AtomicBool::new(false),
            unregister_pending: std::sync::atomic::AtomicBool::new(false),
            unregister_finished: tokio::sync::Notify::new(),
            unregister_departure: std::sync::Mutex::new(None),
            gate: Arc::new(tokio::sync::Mutex::new(())),
            player_id: std::sync::Mutex::new(player_id),
            admission_pending: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            lock_attempts: AtomicUsize::new(0),
            #[cfg(test)]
            lock_attempt_notify: tokio::sync::Notify::new(),
        }
    }

    pub(super) fn unregister_departure(
        &self,
        room_id: Option<RoomId>,
    ) -> Option<Arc<super::room_service::UnregisterDeparture>> {
        let mut departure = self
            .unregister_departure
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if departure.is_none() {
            if let Some(room_id) = room_id {
                *departure = Some(Arc::new(super::room_service::UnregisterDeparture::new(
                    room_id,
                )));
            }
        }
        departure.clone()
    }

    /// Admit one owned transaction for this physical socket.
    pub(super) fn start_unregister(self: &Arc<Self>) -> Option<UnregisterAttempt> {
        if self.unregister_running.swap(true, Ordering::AcqRel) {
            return None;
        }
        self.unregister_pending.store(true, Ordering::Release);
        Some(UnregisterAttempt {
            lifecycle: Arc::clone(self),
            complete: false,
        })
    }

    pub(super) fn retain_unregister_retry(&self) {
        self.unregister_pending.store(true, Ordering::Release);
    }

    pub(super) fn unregister_needs_retry(&self) -> bool {
        self.unregister_pending.load(Ordering::Acquire)
            && !self.unregister_running.load(Ordering::Acquire)
    }

    pub(super) async fn wait_for_unregister(&self) {
        loop {
            let finished = self.unregister_finished.notified();
            tokio::pin!(finished);
            finished.as_mut().enable();
            if !self.unregister_running.load(Ordering::Acquire) {
                return;
            }
            finished.await;
        }
    }

    /// Serialize transport reports through queue admission without holding the
    /// lifecycle gate during backpressure. Leave and reconnect remain free to
    /// invalidate the old report while its recipients wait for capacity.
    pub(crate) async fn lock_transport_reports(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.transport_reports.lock().await
    }

    pub(crate) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.gate.lock().await
    }

    pub(crate) async fn lock_owned(self: Arc<Self>) -> tokio::sync::OwnedMutexGuard<()> {
        #[cfg(test)]
        {
            self.lock_attempts.fetch_add(1, Ordering::Release);
            self.lock_attempt_notify.notify_one();
        }
        Arc::clone(&self.gate).lock_owned().await
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn lock_attempt_count_for_test(&self) -> usize {
        self.lock_attempts.load(Ordering::Acquire)
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) async fn wait_for_lock_attempt_after_for_test(&self, previous: usize) {
        while self.lock_attempt_count_for_test() <= previous {
            self.lock_attempt_notify.notified().await;
        }
    }

    pub(crate) fn player_id(&self) -> PlayerId {
        *self
            .player_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_player_id(&self, player_id: PlayerId) {
        *self
            .player_id
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = player_id;
    }
}

/// One relay stamp read atomically from a sender's `ClientConnection`: the
/// per-`(sender, room)` [`game_data_seq`](ClientConnection::game_data_seq) and
/// its [`game_data_epoch`](ClientConnection::game_data_epoch). Read together
/// under one map lock so a recipient always observes a consistent `(epoch,
/// seq)` pair even if the sender is reassigned concurrently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayStamp {
    pub seq: u64,
    pub epoch: u32,
}

impl ClientConnection {
    /// The pair (outbound queue, close signal) the delivery layer needs to
    /// reach — or, failing that, terminate — this connection.
    pub fn delivery_handle(&self) -> ClientDeliveryHandle {
        ClientDeliveryHandle {
            sender: self.sender.clone(),
            close: self.close.clone(),
        }
    }
}

/// Result of the atomic reconnect identity swap.
#[derive(Debug)]
pub(crate) enum ReassignmentOutcome {
    /// The swap committed; the handle routes to the restored identity.
    Reassigned(ClientDeliveryHandle),
    /// The transient connection vanished before the swap.
    TransientConnectionMissing,
    /// The transient connection already carried a per-socket close
    /// (inactivity/idle timeout, slow consumer, oversized outbound frame, or
    /// teardown) when the swap was attempted. The swap was refused and the
    /// transient entry restored exactly as it was: the pending close tears
    /// down only the transient socket — for every close that resolves the
    /// connection through the connection map, which the removal fences — and
    /// the reconnection record stays spendable for a retry from a fresh
    /// connection. Without this refusal the shared close signal would kill
    /// the freshly restored connection with a stale reason and its teardown
    /// would remove the restored membership. A same-instant pin from the
    /// socket's own I/O tasks (which hold signal clones) belongs to the
    /// shared physical socket and follows the restored identity.
    /// `Shutdown` and `RoomInactive` are identity/room-scoped and still cross
    /// the swap (drain must close restored connections; a room pin reflects
    /// the room the claim just verified).
    RefusedTransientClose(crate::coordination::CloseReason),
    /// A live entry already existed under the reconnection target id when the
    /// swap was attempted. The swap was refused
    /// before any map mutation: the transient connection keeps its identity
    /// and the reconnection record stays spendable for a retry. Unreachable
    /// while the reconnection claim lifecycle holds (the target is free
    /// before any claim can spend); a guard against that lifecycle ever
    /// regressing into a silent stomp, matching the rollback sibling's
    /// `contains_key` check.
    RefusedTargetOccupied,
}

/// Per-socket close reasons that must not cross a reconnect identity swap.
/// Exhaustive on purpose: a future `CloseReason` variant must be classified
/// here explicitly instead of silently inheriting either default.
fn is_transient_socket_close_reason(reason: crate::coordination::CloseReason) -> bool {
    use crate::coordination::CloseReason;
    match reason {
        // Per-socket lifecycle closes: pinned for the transient socket's own
        // quietness, congestion, or teardown.
        CloseReason::AuthTimeout
        | CloseReason::ActivityTimeout
        | CloseReason::IdleTimeout
        | CloseReason::SlowConsumer
        | CloseReason::OutboundMessageTooLarge
        | CloseReason::InboundRateLimited
        | CloseReason::Unregistered => true,
        // Identity/room-scoped: a drain must close restored connections, and
        // a room pin reflects the room the claim just verified. A kick
        // removes the seat behind the identity, so a restored connection
        // must observe it too.
        CloseReason::Shutdown | CloseReason::RoomInactive | CloseReason::Kicked => false,
    }
}

/// Server-wide outbound-queue posture read at scrape time by the metrics
/// endpoints (the capacity runner's `queue_depth` / `queue_age` samples).
/// Both fields describe the moment of the scrape; neither is maintained on
/// the write path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OutboundQueueSample {
    /// Sum of resident items over every live classified outbound queue.
    pub total_depth: u64,
    /// Enqueue instant of the oldest resident item across those queues;
    /// `None` when nothing is queued anywhere.
    pub oldest_enqueued_at: Option<Instant>,
}

impl OutboundQueueSample {
    /// Age of the oldest resident item in whole milliseconds, floored at
    /// zero (an empty sample reports no age). Whole milliseconds keep the
    /// Prometheus exposition integer-valued, which is what the capacity
    /// runner's lenient parser reads.
    pub fn oldest_age_millis(&self, now: Instant) -> u64 {
        self.oldest_enqueued_at.map_or(0, |oldest| {
            u64::try_from(now.saturating_duration_since(oldest).as_millis()).unwrap_or(u64::MAX)
        })
    }
}

pub(crate) struct ConnectionManager {
    clients: DashMap<PlayerId, ClientConnection>,
    connections_per_ip: DashMap<IpAddr, usize>,
    metrics: Arc<ServerMetrics>,
    message_coordinator: Arc<dyn MessageCoordinator>,
    /// Server-wide concurrent-connection ceiling (`security.max_connections`).
    /// Bounds total memory/ownership independently of how many distinct
    /// source IPs are in play (per-IP caps alone are multiplied by IP count —
    /// a botnet or a large NAT pool; issue #502 item 2).
    max_connections: usize,
    /// Authoritative count of live connection entries. Reserved under the
    /// ceiling by production registration, transferred untouched by a
    /// reconnect identity swap, and released exactly once per entry at
    /// unregistration. Test-only registration counts through the same
    /// counter (unbounded, like its per-IP sibling) so release stays
    /// balanced for every entry shape.
    live_connections: AtomicUsize,
    #[cfg(test)]
    admission_reply_sent: tokio::sync::Notify,
    #[cfg(all(test, signal_fish_repository_tests))]
    admission_pauses:
        DashMap<SocketAddr, (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>, bool)>,
    max_connections_per_ip: usize,
    /// Whether per-connection delivery statistics (the v3 `RelayStats`
    /// ledger) are registered with the metrics sink for each connection.
    /// Mirrors `websocket.delivery_stats_interval_secs > 0` so a disabled
    /// deployment keeps the per-delivery bookkeeping at a single map miss.
    track_delivery_stats: bool,
    /// Per-connection inbound error-reply budget the registration arms every
    /// gate with: `(max_inbound_error_replies, time_window)`.
    error_reply_budget: (u32, tokio::time::Duration),
}

/// Admission owns its budgets until the caller accepts the returned identity.
/// Drop only rolls back local state; coordinator cleanup runs in the already
/// owned admission task before this guard is dropped.
struct PendingAdmission {
    manager: Arc<ConnectionManager>,
    ip: IpAddr,
    close: ConnectionCloseSignal,
    global_reserved: bool,
    player_id: Option<PlayerId>,
    active_counted: bool,
    lifecycle: Option<Arc<ClientLifecycle>>,
    armed: bool,
}

impl PendingAdmission {
    async fn rollback_routing(&self, player_id: PlayerId) {
        let result = std::panic::AssertUnwindSafe(async {
            self.manager
                .message_coordinator
                .unregister_local_client(&player_id)
                .await
        })
        .catch_unwind()
        .await;
        if !matches!(result, Ok(Ok(()))) {
            // Diagnostics also belong to the unwind boundary. A subscriber
            // failure must not interrupt local admission rollback.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                warn!(%player_id, panicked = result.is_err(), "Admission routing cleanup failed");
            }));
        }
    }
}

impl Drop for PendingAdmission {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.close.request_close(CloseReason::Unregistered);
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.admission_pending.store(false, Ordering::Release);
        }
        if let Some(player_id) = self.player_id {
            if self
                .manager
                .remove_client_for_unregistration(&player_id, || false)
                .is_some()
                && self.active_counted
            {
                self.manager.metrics.decrement_active_connections();
            }
        } else {
            self.manager.release_ip_slot(self.ip);
            if self.global_reserved {
                self.manager.release_global_slot();
            }
        }
    }
}

impl ConnectionManager {
    fn fresh_membership_generation(
        current: Uuid,
        retained_status: Option<(Uuid, Transport, bool)>,
    ) -> Uuid {
        loop {
            let candidate = Uuid::new_v4();
            if candidate != current
                && retained_status.is_none_or(|(generation, _, _)| candidate != generation)
            {
                return candidate;
            }
        }
    }

    fn advance_membership_generation(client: &mut ClientConnection) {
        client.prior_membership_generation = Some(client.membership_generation);
        client.membership_generation = Self::fresh_membership_generation(
            client.membership_generation,
            client.transport_status,
        );
    }

    fn rollback_membership_generation(client: &mut ClientConnection) {
        if let Some(prior) = client.prior_membership_generation.take() {
            client.membership_generation = prior;
        }
    }

    pub fn new(
        max_connections: usize,
        max_connections_per_ip: usize,
        metrics: Arc<ServerMetrics>,
        message_coordinator: Arc<dyn MessageCoordinator>,
        track_delivery_stats: bool,
        error_reply_budget: (u32, tokio::time::Duration),
    ) -> Self {
        Self {
            clients: DashMap::new(),
            connections_per_ip: DashMap::new(),
            metrics,
            message_coordinator,
            max_connections,
            live_connections: AtomicUsize::new(0),
            #[cfg(test)]
            admission_reply_sent: tokio::sync::Notify::new(),
            #[cfg(all(test, signal_fish_repository_tests))]
            admission_pauses: DashMap::new(),
            max_connections_per_ip,
            track_delivery_stats,
            error_reply_budget,
        }
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn pause_admission_for_test(
        &self,
        client_addr: SocketAddr,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let reached = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        self.admission_pauses.insert(
            client_addr,
            (Arc::clone(&reached), Arc::clone(&release), false),
        );
        (reached, release)
    }

    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) fn fail_admission_for_test(
        &self,
        client_addr: SocketAddr,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let (reached, release) = self.pause_admission_for_test(client_addr);
        self.admission_pauses.get_mut(&client_addr).unwrap().2 = true;
        (reached, release)
    }

    pub async fn register_client(
        self: &Arc<Self>,
        sender: mpsc::Sender<Arc<ServerMessage>>,
        close: ConnectionCloseSignal,
        client_addr: SocketAddr,
        instance_id: Uuid,
    ) -> Result<PlayerId, RegisterClientError> {
        self.register_delivery(sender.into(), close, client_addr, instance_id)
            .await
    }

    pub(crate) async fn register_classified_client(
        self: &Arc<Self>,
        sender: DeliverySender,
        close: ConnectionCloseSignal,
        client_addr: SocketAddr,
        instance_id: Uuid,
    ) -> Result<PlayerId, RegisterClientError> {
        self.register_delivery(sender, close, client_addr, instance_id)
            .await
    }

    async fn register_delivery(
        self: &Arc<Self>,
        sender: DeliverySender,
        close: ConnectionCloseSignal,
        client_addr: SocketAddr,
        instance_id: Uuid,
    ) -> Result<PlayerId, RegisterClientError> {
        let ip = client_addr.ip();
        if let Err(current) = self.try_reserve_ip_slot(ip) {
            warn!(
                %ip,
                current,
                max = self.max_connections_per_ip,
                "IP connection limit exceeded"
            );
            return Err(RegisterClientError::IpLimitExceeded {
                current,
                limit: self.max_connections_per_ip,
            });
        }
        let mut admission = PendingAdmission {
            manager: Arc::clone(self),
            ip,
            close: close.clone(),
            global_reserved: false,
            player_id: None,
            active_counted: false,
            lifecycle: None,
            armed: true,
        };
        // Global ceiling second: if it refuses, the just-taken per-IP slot is
        // released so the two budgets stay independently consistent.
        if let Err(current) = self.try_reserve_global_slot() {
            warn!(
                current,
                max = self.max_connections,
                "Server connection limit exceeded"
            );
            return Err(RegisterClientError::CapacityExceeded {
                current,
                limit: self.max_connections,
            });
        }

        admission.global_reserved = true;
        let player_id = Uuid::new_v4();
        let lifecycle = Arc::new(ClientLifecycle::new(player_id));
        // Acquire the fresh gate before publishing any registration. A reaper
        // cannot remove the entry or release its budgets while admission owns it.
        let lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
        lifecycle.admission_pending.store(true, Ordering::Release);
        admission.lifecycle = Some(Arc::clone(&lifecycle));
        let connection = ClientConnection {
            room_id: None,
            lifecycle: Arc::clone(&lifecycle),
            last_ping: Instant::now(),
            heartbeat_update_revision: 0,
            last_heartbeat_update: None,
            sender: sender.clone(),
            close: close.clone(),
            assignment_gate: Arc::new(tokio::sync::Mutex::new(())),
            client_addr,
            game_data_format: GameDataEncoding::Json,
            app_context: None,
            reconnection_identity: None,
            protocol: NegotiatedProtocol::default(),
            room_operation_ids: false,
            addressed_binary: false,
            addressed_receipt: 0,
            transport_status: None,
            membership_generation: Uuid::nil(),
            prior_membership_generation: None,
            pending_room_assignment: None,
            game_data_seq: 0,
            game_data_epoch: 0,
            error_reply_gate: std::sync::Mutex::new(self.new_error_reply_gate()),
        };

        self.clients.insert(player_id, connection);
        admission.player_id = Some(player_id);
        self.metrics.increment_connections();
        admission.active_counted = true;
        if self.track_delivery_stats {
            self.metrics.register_connection_delivery_stats(player_id);
        }
        // The eviction-attribution ledger's lifetime is the registration's
        // (issue #530): a departed sender can never resurrect its series.
        self.metrics
            .register_slow_consumer_eviction_attributions(player_id);

        // Reserve both budgets before spawning. Pending or abandoned callbacks
        // cannot create more owned admission tasks than the connection ceiling.
        let (mut reply, response) =
            tokio::sync::oneshot::channel::<Result<PlayerId, RegisterClientError>>();
        let (acknowledge, acknowledged) = tokio::sync::oneshot::channel();
        let caller_lifecycle = Arc::clone(&lifecycle);
        tokio::spawn(async move {
            let _lifecycle_guard = lifecycle_guard;
            let manager = Arc::clone(&admission.manager);
            let mut admission_close = close.subscribe();
            let result = std::panic::AssertUnwindSafe(async {
                let registration = tokio::select! {
                    biased;
                    () = reply.closed() => return false,
                    _reason = admission_close.closed() => {
                        // Hand off the still-live, already-closed connection.
                        // Its normal socket owner writes the semantic close
                        // and unregisters after this gate is released.
                        return true;
                    },
                    result = async {
                        #[cfg(all(test, signal_fish_repository_tests))]
                        manager.run_admission_pause_for_test(client_addr).await;
                        manager.message_coordinator.register_local_client(
                            player_id, None, ClientDeliveryHandle { sender, close },
                        ).await
                    } => result,
                };
                if let Err(error) = registration {
                    warn!(%player_id, %error, "Failed to register client with coordinator");
                }
                // Diagnostics stay before handoff so subscriber unwind cannot
                // strand an identity that the caller has not received.
                info!(%player_id, instance_id = %instance_id, client_addr = %client_addr, "Client registered");
                true
            }).catch_unwind().await;
            match result {
                Ok(ready) => {
                    if ready {
                        let sent = reply.send(Ok(player_id)).is_ok();
                        #[cfg(test)]
                        if sent {
                            manager.admission_reply_sent.notify_one();
                        }
                        if sent && acknowledged.await.is_ok() {
                            lifecycle.admission_pending.store(false, Ordering::Release);
                            admission.armed = false;
                            drop(admission);
                            return;
                        }
                    }
                    admission.rollback_routing(player_id).await;
                    drop(admission);
                }
                Err(_panic) => {
                    // Log before routing cleanup, which an adapter can delay.
                    // A subscriber panic must not interrupt admission rollback.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        tracing::error!(%player_id, %client_addr, failure = "admission_unwind", "Connection admission failed");
                    }));
                    admission.rollback_routing(player_id).await;
                    drop(admission);
                    let sent = reply
                        .send(Err(RegisterClientError::AdmissionFailed))
                        .is_ok();
                    #[cfg(test)]
                    if sent {
                        manager.admission_reply_sent.notify_one();
                    }
                    #[cfg(not(test))]
                    let _ = sent;
                }
            }
        });
        match response.await {
            Ok(Ok(player_id)) => {
                // No await or diagnostic callback may separate acknowledgement
                // from returning ownership to the caller.
                caller_lifecycle
                    .admission_pending
                    .store(false, Ordering::Release);
                let _ = acknowledge.send(());
                Ok(player_id)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    tracing::error!(%player_id, %client_addr, failure = "handoff_channel_closed", "Connection admission failed");
                }));
                Err(RegisterClientError::AdmissionFailed)
            }
        }
    }

    /// Test-only registration under an arbitrary player id. Production
    /// registration always mints a fresh UUID, so map-key collisions with an
    /// existing entry (including a just-restored reconnection entry) are
    /// impossible there. Kept `pub` only because
    /// [`crate::server::EnhancedGameServer::connect_client`] wraps it for
    /// in-crate test harnesses; embedders must not use it to fabricate
    /// collisions.
    ///
    /// This path bypasses both admission budgets (per-IP and the server-wide
    /// ceiling) exactly as it bypasses the drain gate, but still counts
    /// through the shared live-connection counter (unbounded increment) so
    /// every unregistration releases exactly one slot (issue #502 item 3:
    /// accepted-as-is test-first shape; production admission semantics live
    /// only on the wire registration path).
    pub async fn connect_test_client(
        &self,
        player_id: PlayerId,
        sender: mpsc::Sender<Arc<ServerMessage>>,
        client_addr: SocketAddr,
    ) {
        let close = ConnectionCloseSignal::detached();
        let sender: DeliverySender = sender.into();
        let connection = ClientConnection {
            room_id: None,
            lifecycle: Arc::new(ClientLifecycle::new(player_id)),
            last_ping: Instant::now(),
            heartbeat_update_revision: 0,
            last_heartbeat_update: None,
            sender: sender.clone(),
            close: close.clone(),
            assignment_gate: Arc::new(tokio::sync::Mutex::new(())),
            client_addr,
            game_data_format: GameDataEncoding::Json,
            app_context: None,
            reconnection_identity: None,
            protocol: NegotiatedProtocol::default(),
            room_operation_ids: false,
            addressed_binary: false,
            addressed_receipt: 0,
            transport_status: None,
            membership_generation: Uuid::nil(),
            prior_membership_generation: None,
            pending_room_assignment: None,
            game_data_seq: 0,
            game_data_epoch: 0,
            error_reply_gate: std::sync::Mutex::new(self.new_error_reply_gate()),
        };

        self.increment_ip_slot_unbounded(client_addr.ip());
        self.increment_global_slot_unbounded();
        // A same-id replacement is legitimate in test harnesses; the
        // displaced entry leaves the map here without its own unregistration,
        // so its per-IP and global slots are released to keep the counters
        // exactly paired with live entries.
        if let Some(displaced) = self.clients.insert(player_id, connection) {
            self.release_ip_slot(displaced.client_addr.ip());
            self.release_global_slot();
        }
        self.metrics.increment_connections();
        if self.track_delivery_stats {
            self.metrics.register_connection_delivery_stats(player_id);
        }
        // The eviction-attribution ledger's lifetime is the registration's
        // (issue #530): a departed sender can never resurrect its series.
        self.metrics
            .register_slow_consumer_eviction_attributions(player_id);

        if let Err(err) = self
            .message_coordinator
            .register_local_client(player_id, None, ClientDeliveryHandle { sender, close })
            .await
        {
            warn!(%player_id, %err, "Failed to register test client with coordinator");
        }
    }

    /// Assign a connected client to a room and publish its coordinator route.
    ///
    /// The local room transition is prepared before the coordinator call. If
    /// that call fails or the caller is canceled, the rollback restores the
    /// exact previous room, relay stamp, membership generation, sender
    /// generation and coordinator route, so no ghost membership survives.
    pub async fn assign_client_to_room(self: &Arc<Self>, player_id: &PlayerId, room_id: RoomId) {
        let Some(lifecycle) = self.client_lifecycle(player_id) else {
            return;
        };
        let Some(gate) = self
            .clients
            .get(player_id)
            .map(|client| Arc::clone(&client.assignment_gate))
        else {
            return;
        };
        let gate_handle = Arc::clone(&gate);
        let gate = gate.lock_owned().await;
        // A removal or reconnect identity swap may win before the gate lands.
        if !self.lifecycle_matches(player_id, &lifecycle) {
            return;
        }
        let Some((delivery, _stamp, prepared)) =
            self.prepare_client_to_room_with_rollback(player_id, room_id)
        else {
            return;
        };
        let rollback = RoomAssignmentRollback {
            manager: Arc::clone(self),
            player_id: *player_id,
            lifecycle,
            prepared: Some(prepared),
            gate: Some(gate),
            gate_handle,
        };
        if let Err(err) = self
            .message_coordinator
            .register_local_client(*player_id, Some(room_id), delivery)
            .await
        {
            warn!(
                %player_id,
                %room_id,
                %err,
                "Failed to update coordinator membership when assigning client to room"
            );
            return;
        }
        rollback.commit();
    }

    /// Prepare a fresh seated-room incarnation without publishing it to room
    /// routing. Ordinary joins use the returned handle with the coordinator's
    /// atomic initial-message registration so `RoomJoined` is queued before
    /// any room control can target the new generation.
    pub(crate) fn prepare_client_to_room(
        &self,
        player_id: &PlayerId,
        room_id: RoomId,
    ) -> Option<(ClientDeliveryHandle, RelayStamp)> {
        let mut client = self.clients.get_mut(player_id)?;
        // Joins publish through the coordinator's atomic initial-message path,
        // so they never retain a pending assignment marker.
        client.pending_room_assignment = None;
        Some(Self::prepare_client_to_room_locked(
            player_id,
            room_id,
            &mut client,
        ))
    }

    /// Prepare a room transition and retain the exact state it replaced.
    /// The snapshot and mutation share one entry lock so no concurrent
    /// transition can slip between them.
    fn prepare_client_to_room_with_rollback(
        &self,
        player_id: &PlayerId,
        room_id: RoomId,
    ) -> Option<(ClientDeliveryHandle, RelayStamp, PreparedRoomAssignment)> {
        let mut client = self.clients.get_mut(player_id)?;
        let previous = RoomAssignmentSnapshot::capture(&client);
        let (delivery, stamp) =
            Self::prepare_client_to_room_locked(player_id, room_id, &mut client);
        client.pending_room_assignment = Some(stamp.epoch);
        let prepared = PreparedRoomAssignment {
            room_id,
            epoch: stamp.epoch,
            membership_generation: client.membership_generation,
            previous,
        };
        Some((delivery, stamp, prepared))
    }

    fn prepare_client_to_room_locked(
        player_id: &PlayerId,
        room_id: RoomId,
        client: &mut ClientConnection,
    ) -> (ClientDeliveryHandle, RelayStamp) {
        client.room_id = Some(room_id);
        Self::advance_membership_generation(client);
        // Fresh room membership => fresh per-(sender, room) relay stamp stream.
        client.game_data_seq = 0;
        // Saturation cannot regress an epoch, unlike wrapping at u32::MAX.
        let prior_epoch = client.game_data_epoch;
        client.game_data_epoch = prior_epoch.saturating_add(1);
        if prior_epoch == u32::MAX {
            // The terminal incarnation is reused with seq restarting at 1 —
            // the ambiguity `epoch` exists to remove. Unreachable in practice
            // (~2^32 joins of one sender), but never silent (mirrors the
            // reconnect-path saturation log).
            tracing::error!(
                %player_id,
                %room_id,
                "Join epoch saturated at u32::MAX; the new stream reuses it"
            );
        }
        client.sender = client.sender.next_generation();
        let stamp = RelayStamp {
            seq: client.game_data_seq,
            epoch: client.game_data_epoch,
        };
        (client.delivery_handle(), stamp)
    }

    /// Restore the exact snapshot an uncommitted room assignment replaced.
    /// The expected room, epoch and membership token must still match the
    /// prepared transition; a newer transition owns its own state and is left
    /// untouched.
    fn restore_uncommitted_room_assignment(
        &self,
        player_id: &PlayerId,
        prepared: PreparedRoomAssignment,
    ) -> bool {
        let Some(mut client) = self.clients.get_mut(player_id) else {
            return false;
        };
        if client.room_id != Some(prepared.room_id)
            || client.game_data_epoch != prepared.epoch
            || client.membership_generation != prepared.membership_generation
            || client.pending_room_assignment != Some(prepared.epoch)
        {
            return false;
        }
        client.room_id = prepared.previous.room_id;
        client.game_data_seq = prepared.previous.game_data_seq;
        client.game_data_epoch = prepared.previous.game_data_epoch;
        client.membership_generation = prepared.previous.membership_generation;
        client.prior_membership_generation = prepared.previous.prior_membership_generation;
        client.pending_room_assignment = prepared.previous.pending_room_assignment;
        client.sender = prepared.previous.delivery.sender;
        true
    }

    /// Mark a prepared room assignment as published so relay stamps may
    /// advance. Only the expected epoch clears the marker.
    fn commit_prepared_room_assignment(&self, player_id: &PlayerId, expected_epoch: u32) {
        if let Some(mut client) = self.clients.get_mut(player_id) {
            if client.pending_room_assignment == Some(expected_epoch) {
                client.pending_room_assignment = None;
            }
        }
    }

    /// Whether a canceled assignment's compensation still owns the restored
    /// room, epoch and membership token. A newer assignment changes at least
    /// one of them and owns its own route.
    fn has_restored_room_assignment(
        &self,
        player_id: &PlayerId,
        room_id: Option<RoomId>,
        epoch: u32,
        membership_generation: Uuid,
    ) -> bool {
        self.clients.get(player_id).is_some_and(|client| {
            client.room_id == room_id
                && client.game_data_epoch == epoch
                && client.membership_generation == membership_generation
        })
    }

    /// Undo [`Self::prepare_client_to_room`] when its transition frame never
    /// committed. The queue still owns the previous generation, so restore the
    /// wrapper instead of advancing again.
    ///
    /// This clears a fresh join to the roomless state. Use
    /// [`Self::restore_uncommitted_room_assignment`] for a published assignment
    /// that must return to an exact prior room and stamp.
    pub(crate) fn rollback_prepared_room_assignment(
        &self,
        player_id: &PlayerId,
        expected_room: RoomId,
        expected_epoch: u32,
    ) -> Option<ClientDeliveryHandle> {
        let mut client = self.clients.get_mut(player_id)?;
        if client.room_id != Some(expected_room) || client.game_data_epoch != expected_epoch {
            return None;
        }
        client.room_id = None;
        Self::rollback_membership_generation(&mut client);
        client.game_data_seq = 0;
        client.sender = client.sender.previous_generation();
        Some(client.delivery_handle())
    }

    pub fn set_game_data_format(&self, player_id: &PlayerId, format: GameDataEncoding) {
        if let Some(mut connection) = self.clients.get_mut(player_id) {
            connection.sender.set_game_data_format(format);
            connection.game_data_format = format;
        }
    }

    pub fn game_data_format(&self, player_id: &PlayerId) -> GameDataEncoding {
        self.clients
            .get(player_id)
            .map(|conn| conn.game_data_format)
            .unwrap_or(GameDataEncoding::Json)
    }

    pub fn prefers_encoding(&self, player_id: &PlayerId, encoding: GameDataEncoding) -> bool {
        self.game_data_format(player_id) == encoding
    }

    pub fn set_protocol(&self, player_id: &PlayerId, protocol: NegotiatedProtocol) {
        if let Some(mut connection) = self.clients.get_mut(player_id) {
            connection.sender.set_protocol_version(protocol.version);
            connection.protocol = protocol;
        }
    }

    pub fn set_room_operation_ids(&self, player_id: &PlayerId, enabled: bool) {
        if let Some(mut connection) = self.clients.get_mut(player_id) {
            connection.room_operation_ids = enabled;
        }
    }

    pub fn supports_room_operation_ids(&self, player_id: &PlayerId) -> bool {
        self.clients
            .get(player_id)
            .map(|connection| connection.room_operation_ids)
            .unwrap_or(false)
    }

    pub(super) fn set_addressed_binary(&self, player_id: &PlayerId, enabled: bool) {
        if let Some(mut client) = self.clients.get_mut(player_id) {
            client.addressed_binary = enabled;
        }
    }

    pub(super) fn supports_addressed_binary(&self, player_id: &PlayerId) -> bool {
        self.clients
            .get(player_id)
            .is_some_and(|client| client.addressed_binary)
    }

    /// Resolve one live addressed seat and its committed room queue.
    ///
    /// `required_format` is `Some` for a directed send and `None` for plan
    /// publication. Both callers must use the same eligibility predicate: a
    /// prepared join updates `ClientConnection` before the coordinator queue
    /// scope commits, so a room assignment alone is not a routable seat.
    fn addressed_room_scope(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        required_format: Option<GameDataEncoding>,
    ) -> Option<(GameDataEncoding, u32, Uuid)> {
        let (sender, format, epoch) = {
            let client = self.clients.get(player_id)?;
            if !client.addressed_binary
                || client.protocol.version < 3
                || client.game_data_format == GameDataEncoding::Json
                || client.room_id != Some(*room_id)
                || client.game_data_epoch == 0
                || required_format.is_some_and(|required| client.game_data_format != required)
            {
                return None;
            }
            (
                client.sender.clone(),
                client.game_data_format,
                client.game_data_epoch,
            )
        };
        // A prepared transition updates ClientConnection before the queue scope.
        // Keep connection registry reads independent of the queue lock. Every
        // wire connection is classified; legacy senders exist only in test
        // harnesses and carry no queue scope to commit.
        if sender.classified_queue().is_some() && !sender.has_active_room_scope(*room_id) {
            return None;
        }
        let client = self.clients.get(player_id)?;
        (client.sender.same_channel(&sender)
            && client.room_id == Some(*room_id)
            && client.game_data_epoch == epoch
            && client.game_data_format == format
            && client.addressed_binary
            && client.close.requested_reason().is_none()
            && required_format.is_none_or(|required| client.game_data_format == required))
        .then_some((format, epoch, client.membership_generation))
    }

    pub(super) fn addressed_plan_seat(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
    ) -> Option<(GameDataEncoding, u32)> {
        self.addressed_room_scope(player_id, room_id, None)
            .map(|(format, epoch, _)| (format, epoch))
    }

    pub(super) fn addressed_target_generation(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        encoding: GameDataEncoding,
    ) -> Option<Uuid> {
        self.addressed_room_scope(player_id, room_id, Some(encoding))
            .map(|(_, _, generation)| generation)
    }

    pub(super) fn next_addressed_receipt(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        source: &Arc<ClientLifecycle>,
    ) -> Option<RelayStamp> {
        let mut client = self.clients.get_mut(player_id)?;
        if !client.addressed_binary
            || client.room_id != Some(*room_id)
            || !Arc::ptr_eq(source, &client.lifecycle)
        {
            return None;
        }
        let client = client.value_mut();
        let receipt = client.addressed_receipt.checked_add(1)?;
        client.close.commit_if_open(|| {
            client.addressed_receipt = receipt;
            RelayStamp {
                seq: receipt,
                epoch: client.game_data_epoch,
            }
        })
    }

    /// Read an owned negotiation snapshot with the complete capability lists.
    pub fn protocol(&self, player_id: &PlayerId) -> NegotiatedProtocol {
        self.clients
            .get(player_id)
            .map(|conn| conn.protocol.clone())
            .unwrap_or_default()
    }

    /// Project one negotiation snapshot while borrowing its connection.
    pub(super) fn project_protocol<R>(
        &self,
        player_id: &PlayerId,
        project: impl FnOnce(&NegotiatedProtocol) -> R,
    ) -> Option<R> {
        self.clients
            .get(player_id)
            .map(|connection| project(&connection.protocol))
    }

    /// Record the last-reported data-path transport state for a connection
    /// (mirrors [`Self::set_protocol`]). Driven by
    /// [`ClientMessage::TransportStatus`](crate::protocol::ClientMessage::TransportStatus).
    /// Returns whether the persisted state changed. Duplicate reports leave
    /// state untouched so event counters are not inflated.
    pub fn set_transport_status(
        &self,
        player_id: &PlayerId,
        transport: Transport,
        connected: bool,
    ) -> TransportStatusUpdate {
        if let Some(mut connection) = self.clients.get_mut(player_id) {
            let connection = &mut *connection;
            return crate::session_capability::update_transport_status(
                &connection.protocol,
                connection.membership_generation,
                &mut connection.transport_status,
                transport,
                connected,
            );
        }

        TransportStatusUpdate::MissingConnection
    }

    /// Read the last-reported data-path transport state for a connection.
    /// `None` until the client reports one (the relay floor is the implicit
    /// default). Mirrors [`Self::protocol`]. Consumed by tests and the future
    /// targeted-relay path; not yet read in production.
    #[allow(dead_code)]
    pub fn transport_status(&self, player_id: &PlayerId) -> Option<(Transport, bool)> {
        self.clients
            .get(player_id)
            .and_then(|conn| match conn.transport_status {
                Some((generation, transport, connected))
                    if generation == conn.membership_generation =>
                {
                    Some((transport, connected))
                }
                _ => None,
            })
    }

    /// Whether the client negotiated protocol v3+ (the single unshipped
    /// "current" version). v3 is the ONE gate for every additive feature over
    /// the frozen v2 floor: the WebRTC signaling surface
    /// (`Signal`/`NewPeer`/`SessionPlan`/`TransportStatus`) AND the delivery
    /// reliability surface (relayed `GameData.seq` + incarnation `epoch`, and
    /// `RelayStats` emission). A v2 client gets none of it (byte-identical wire).
    pub fn supports_v3(&self, player_id: &PlayerId) -> bool {
        self.clients
            .get(player_id)
            .map(|conn| conn.protocol.version >= 3)
            .unwrap_or(false)
    }

    pub fn supports_transport(&self, player_id: &PlayerId, transport: Transport) -> bool {
        self.clients
            .get(player_id)
            .map(|conn| conn.protocol.transports.contains(&transport))
            .unwrap_or(false)
    }

    pub fn set_app_context(&self, player_id: &PlayerId, app_context: AppContext) {
        if let Some(mut connection) = self.clients.get_mut(player_id) {
            connection.app_context = Some(app_context);
        }
    }

    pub fn app_context(&self, player_id: &PlayerId) -> Option<AppContext> {
        self.clients
            .get(player_id)
            .and_then(|conn| conn.app_context.clone())
    }

    /// Read the sender's relay-relevant allowlist policy without cloning the
    /// whole [`AppContext`] (issue #530): this runs once per relayed frame on
    /// the byte-budget gate, so only the `Copy` fields are projected.
    pub fn app_relay_policy(
        &self,
        player_id: &PlayerId,
    ) -> Option<crate::rate_limit::AppRelayPolicy> {
        self.clients.get(player_id).and_then(|conn| {
            conn.app_context
                .as_ref()
                .map(|ctx| crate::rate_limit::AppRelayPolicy {
                    app_id: ctx.id,
                    max_relay_bytes: ctx.max_relay_bytes,
                })
        })
    }

    pub(crate) fn set_reconnection_identity(
        &self,
        player_id: &PlayerId,
        identity: Option<Arc<str>>,
    ) {
        if let Some(mut connection) = self.clients.get_mut(player_id) {
            connection.reconnection_identity = identity;
        }
    }

    pub(crate) fn reconnection_identity(&self, player_id: &PlayerId) -> Option<Arc<str>> {
        self.clients
            .get(player_id)
            .and_then(|connection| connection.reconnection_identity.clone())
    }

    pub fn app_id(&self, player_id: &PlayerId) -> Option<Uuid> {
        self.clients
            .get(player_id)
            .and_then(|connection| connection.app_context.as_ref().map(|context| context.id))
    }

    /// Capture the terminal relay watermark and clear membership under one
    /// connection-entry lock, but only while the player is still assigned to
    /// `expected_room`. On any other room (a stale leave racing a room switch)
    /// this returns `None` untouched instead of publishing that room's live
    /// stamp as a foreign room's phantom terminal watermark. Note the refusal
    /// is only `ConnectionManager`-level: the coordinator still clears the
    /// stale route for `expected_room`, while preserving any newer room route.
    /// The coordinator calls this while holding its room-routing write lock,
    /// making stamp allocation and terminal unroute mutually exclusive.
    pub fn clear_room_assignment_with_tail(
        &self,
        player_id: &PlayerId,
        expected_room: &RoomId,
    ) -> Option<(ClientDeliveryHandle, RelayStamp)> {
        self.clear_room_assignment_guarded(player_id, Some(expected_room))
    }

    fn clear_room_assignment_guarded(
        &self,
        player_id: &PlayerId,
        expected_room: Option<&RoomId>,
    ) -> Option<(ClientDeliveryHandle, RelayStamp)> {
        self.clients.get_mut(player_id).and_then(|mut client| {
            if expected_room.is_some_and(|expected| client.room_id != Some(*expected)) {
                return None;
            }
            let tail = RelayStamp {
                seq: client.game_data_seq,
                epoch: client.game_data_epoch,
            };
            client.room_id = None;
            client.pending_room_assignment = None;
            Self::advance_membership_generation(&mut client);
            // Membership ended: the next room (same or different) starts a
            // fresh stamp stream (see the `game_data_seq` field doc).
            client.game_data_seq = 0;
            client.sender = client.sender.next_generation();
            Some((client.delivery_handle(), tail))
        })
    }

    pub fn clear_room_assignment(&self, player_id: &PlayerId) -> Option<ClientDeliveryHandle> {
        self.clear_room_assignment_guarded(player_id, None)
            .map(|(delivery, _)| delivery)
    }

    /// Commit the local generation before publishing its handle to the coordinator.
    /// Callers that recover a publication panic must reuse this handle instead
    /// of advancing the connection a second time.
    pub(super) fn advance_delivery_generation_local(
        &self,
        player_id: &PlayerId,
    ) -> Option<ClientDeliveryHandle> {
        self.clients.get_mut(player_id).map(|mut client| {
            Self::advance_membership_generation(&mut client);
            client.sender = client.sender.next_generation();
            client.delivery_handle()
        })
    }

    pub(super) fn client_delivery_handle(
        &self,
        player_id: &PlayerId,
    ) -> Option<ClientDeliveryHandle> {
        self.clients
            .get(player_id)
            .map(|client| client.delivery_handle())
    }

    pub async fn advance_delivery_generation(&self, player_id: &PlayerId) {
        if let Some(delivery) = self.advance_delivery_generation_local(player_id) {
            if let Err(err) = self
                .message_coordinator
                .register_local_client(*player_id, None, delivery)
                .await
            {
                warn!(%player_id, %err, "Failed to advance delivery generation");
            }
        }
    }

    /// Undo an unpublished spectator transition and republish the prior queue
    /// generation to the coordinator.
    pub async fn rollback_delivery_generation(&self, player_id: &PlayerId) {
        let delivery = self.clients.get_mut(player_id).map(|mut client| {
            Self::rollback_membership_generation(&mut client);
            client.sender = client.sender.previous_generation();
            client.delivery_handle()
        });
        if let Some(delivery) = delivery {
            if let Err(err) = self
                .message_coordinator
                .register_local_client(*player_id, None, delivery)
                .await
            {
                warn!(%player_id, %err, "Failed to roll back delivery generation");
            }
        }
    }

    /// Advance the relay sequence and return the full relay stamp — the next
    /// `seq` (protocol v3; first stamp is 1) together with the current
    /// incarnation `epoch` — for `player_id`'s next relayed game-data message
    /// in `expected_room`. Room membership and the counter advance are checked
    /// under one entry lock, so concurrent leave/unregister cannot reset `seq`
    /// and then leak a duplicate stamp into the old room. `None` cancels that
    /// stale relay without consuming a sequence number.
    #[cfg(test)]
    pub fn next_relay_stamp_in_room(
        &self,
        player_id: &PlayerId,
        expected_room: &RoomId,
    ) -> Option<RelayStamp> {
        self.next_relay_stamp_in_room_from_lifecycle(player_id, expected_room, None)
    }

    pub(crate) fn next_relay_stamp_in_room_from_lifecycle(
        &self,
        player_id: &PlayerId,
        expected_room: &RoomId,
        source_lifecycle: Option<&Arc<ClientLifecycle>>,
    ) -> Option<RelayStamp> {
        let mut client = self.clients.get_mut(player_id)?;
        if client.room_id != Some(*expected_room)
            || client.pending_room_assignment.is_some()
            || source_lifecycle.is_some_and(|source| !Arc::ptr_eq(source, &client.lifecycle))
        {
            return None;
        }
        let client = client.value_mut();
        let Some(next_seq) = client.game_data_seq.checked_add(1) else {
            tracing::error!(%player_id, %expected_room, "Relay sequence exhausted; canceling delivery");
            return None;
        };
        // A stamped frame owns delivery even if the source closes later.
        client.close.commit_if_open(|| {
            client.game_data_seq = next_seq;
            RelayStamp {
                seq: next_seq,
                epoch: client.game_data_epoch,
            }
        })
    }

    /// Read the current relay stamp without advancing, only while the player is
    /// still assigned to `expected_room`. Snapshot projection uses this single
    /// read for both `PlayerInfo.epoch` and reconnect watermarks, filtering the
    /// leave/unregister window instead of emitting incomplete v3 metadata.
    pub fn current_relay_stamp_in_room(
        &self,
        player_id: &PlayerId,
        expected_room: &RoomId,
    ) -> Option<RelayStamp> {
        let client = self.clients.get(player_id)?;
        if client.room_id != Some(*expected_room) {
            return None;
        }
        Some(RelayStamp {
            seq: client.game_data_seq,
            epoch: client.game_data_epoch,
        })
    }

    /// Read a transport report's generation only while its physical socket
    /// can still participate. Delivery may request a close before teardown
    /// clears the room assignment.
    pub(crate) fn live_membership_generation_in_room(
        &self,
        player_id: &PlayerId,
        expected_room: &RoomId,
    ) -> Option<Uuid> {
        let client = self.clients.get(player_id)?;
        (client.room_id == Some(*expected_room) && client.close.requested_reason().is_none())
            .then_some(client.membership_generation)
    }

    /// Read a connection's current epoch in unit tests without advancing it.
    /// Production metadata reads must use [`Self::current_relay_stamp_in_room`]
    /// so an epoch cannot be projected across a concurrent room transition.
    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub fn game_data_epoch(&self, player_id: &PlayerId) -> Option<u32> {
        self.clients
            .get(player_id)
            .map(|client| client.game_data_epoch)
    }

    /// Force an incarnation bump in unit tests (simulates what a reconnect
    /// reassignment does). Production epochs only move through join/reassign.
    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub fn bump_game_data_epoch_for_test(&self, player_id: &PlayerId) {
        if let Some(mut client) = self.clients.get_mut(player_id) {
            client.game_data_epoch = client.game_data_epoch.saturating_add(1);
        }
    }

    pub fn record_ping(&self, player_id: &PlayerId) {
        if let Some(mut client) = self.clients.get_mut(player_id) {
            client.last_ping = Instant::now();
        }
    }

    /// Reserve one throttle window until both persistence updates succeed.
    pub(super) fn reserve_last_seen_update(
        &self,
        player_id: &PlayerId,
        threshold: std::time::Duration,
    ) -> Option<HeartbeatUpdateReservation<'_>> {
        let mut client = self.clients.get_mut(player_id)?;
        let now = Instant::now();
        if client
            .last_heartbeat_update
            .is_some_and(|last| now.duration_since(last) < threshold)
        {
            return None;
        }
        client.heartbeat_update_revision = client.heartbeat_update_revision.wrapping_add(1);
        client.last_heartbeat_update = Some(now);
        Some(HeartbeatUpdateReservation {
            manager: self,
            player_id: *player_id,
            lifecycle: Arc::clone(&client.lifecycle),
            revision: client.heartbeat_update_revision,
            completed: false,
        })
    }

    pub fn get_client_room(&self, player_id: &PlayerId) -> Option<RoomId> {
        self.clients
            .get(player_id)
            .and_then(|client| client.room_id)
    }

    pub(crate) fn client_lifecycle(&self, player_id: &PlayerId) -> Option<Arc<ClientLifecycle>> {
        self.clients
            .get(player_id)
            .map(|client| Arc::clone(&client.lifecycle))
    }

    pub(crate) fn lifecycle_matches(
        &self,
        player_id: &PlayerId,
        lifecycle: &Arc<ClientLifecycle>,
    ) -> bool {
        self.clients
            .get(player_id)
            .is_some_and(|client| Arc::ptr_eq(&client.lifecycle, lifecycle))
    }

    /// Current and prior membership generation for the player, for tests that
    /// pin advance/rollback symmetry of the delivery state.
    #[cfg(test)]
    pub(crate) fn delivery_generation_for_test(
        &self,
        player_id: &PlayerId,
    ) -> Option<(Uuid, Option<Uuid>)> {
        self.clients.get(player_id).map(|client| {
            (
                client.membership_generation,
                client.prior_membership_generation,
            )
        })
    }

    pub fn has_client(&self, player_id: &PlayerId) -> bool {
        self.clients.contains_key(player_id)
    }

    /// Snapshot currently registered client ids.
    pub fn client_ids(&self) -> Vec<PlayerId> {
        self.clients.iter().map(|entry| *entry.key()).collect()
    }

    /// Discover room cleanup hints; workers revalidate membership before teardown.
    pub(super) fn assigned_room_candidates(&self) -> Vec<(RoomId, PlayerId)> {
        self.clients
            .iter()
            .filter_map(|entry| entry.room_id.map(|room_id| (room_id, *entry.key())))
            .collect()
    }

    /// Discover interrupted teardown hints without changing retry ownership.
    pub(super) fn pending_unregistration_candidates(&self) -> Vec<(RoomId, PlayerId)> {
        self.clients
            .iter()
            .filter_map(|entry| {
                entry
                    .lifecycle
                    .unregister_needs_retry()
                    .then_some((RoomId::nil(), *entry.key()))
            })
            .collect()
    }

    /// Scrape-time walk over every live connection's classified outbound
    /// queue: total resident items and the oldest resident item's enqueue
    /// instant. This runs on the metrics endpoint, never the write path;
    /// Queue observers are captured before taking any queue lock, so a busy
    /// queue cannot retain a connection-registry guard. Observers clone no
    /// payload and do not keep producers open. Legacy senders carry no
    /// inspectable queue and contribute nothing (every wire connection is classified; legacy entries exist
    /// only in test harnesses).
    pub fn outbound_queue_sample(&self) -> OutboundQueueSample {
        let mut queues = Vec::with_capacity(self.clients.len());
        for client in self.clients.iter() {
            if let Some(queue) = client.sender.classified_queue() {
                queues.push(queue.observer());
            }
        }
        let mut sample = OutboundQueueSample::default();
        for queue in queues {
            let (depth, oldest) = queue.depth_and_oldest();
            sample.total_depth = sample.total_depth.saturating_add(depth as u64);
            if let Some(enqueued_at) = oldest {
                sample.oldest_enqueued_at = Some(match sample.oldest_enqueued_at {
                    Some(existing) if existing < enqueued_at => existing,
                    _ => enqueued_at,
                });
            }
        }
        sample
    }

    pub fn reassign_connection(
        &self,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        room_id: RoomId,
        game_data_epoch: u32,
    ) -> ReassignmentOutcome {
        // Collision refusal mirrors the rollback sibling
        // (`restore_reassigned_connection`): a live entry under the target id
        // must never be silently stomped by the blind insert below. Today's
        // reconnection lifecycle makes the target free before any claim can
        // spend, so this is defense-in-depth against a claim-lifecycle
        // regression — the transient entry is left untouched and the record
        // stays spendable for a retry.
        if self.clients.contains_key(reconnect_player_id) {
            return ReassignmentOutcome::RefusedTargetOccupied;
        }
        // Atomically remove the old entry (no separate get-then-remove race).
        // The removal is the refusal fence: once it lands, no reaper-style
        // map lookup can pin the transient entry anymore, so the close-reason
        // inspection below is the final word for every path that resolves the
        // connection through this map.
        let Some((_, old_connection)) = self.clients.remove(current_player_id) else {
            return ReassignmentOutcome::TransientConnectionMissing;
        };
        if let Some(reason) = old_connection.close.requested_reason() {
            if is_transient_socket_close_reason(reason) {
                // Restore the entry untouched (same object, same `last_ping`,
                // same pinned signal) so the pending eviction tears down only
                // the transient socket. A fresh UUID per connection means no
                // other producer can race an insert under this key.
                self.clients.insert(*current_player_id, old_connection);
                return ReassignmentOutcome::RefusedTransientClose(reason);
            }
        }
        let mut delivery = old_connection.delivery_handle();
        delivery.sender = delivery.sender.next_generation();
        let prior_membership_generation = old_connection.membership_generation;
        let membership_generation = Self::fresh_membership_generation(
            prior_membership_generation,
            old_connection.transport_status,
        );
        let new_client = ClientConnection {
            room_id: Some(room_id),
            lifecycle: Arc::clone(&old_connection.lifecycle),
            last_ping: Instant::now(),
            heartbeat_update_revision: old_connection.heartbeat_update_revision,
            last_heartbeat_update: None, // Reset on reconnection, will update immediately
            sender: delivery.sender.clone(),
            close: delivery.close.clone(),
            assignment_gate: Arc::clone(&old_connection.assignment_gate),
            client_addr: old_connection.client_addr,
            game_data_format: old_connection.game_data_format,
            app_context: old_connection.app_context,
            reconnection_identity: old_connection.reconnection_identity,
            protocol: old_connection.protocol,
            room_operation_ids: old_connection.room_operation_ids,
            addressed_binary: old_connection.addressed_binary,
            addressed_receipt: old_connection.addressed_receipt,
            // Preserve any roomless transient-socket status only as rollback
            // state. Advancing the membership generation below makes it
            // ineligible for reconnect deduplication, so the restored player
            // must establish and report its P2P path afresh.
            transport_status: old_connection.transport_status,
            membership_generation,
            prior_membership_generation: Some(prior_membership_generation),
            pending_room_assignment: None,
            // Restart-on-rejoin: a reconnecting sender's relay stamp
            // stream starts over at 1; recipients treat its
            // `PlayerReconnected` as a seq reset (field doc above).
            game_data_seq: 0,
            // The caller supplies the resumed incarnation epoch (the
            // surviving reconnection record's `last_epoch + 1`), so the
            // sender's `(epoch, seq)` stream is strictly increasing for a
            // recipient that never left from the moment this entry becomes
            // visible — no provisional value is ever observable. The
            // transient socket itself never joined a room (epoch 0); the
            // reconnect path always dominates it.
            game_data_epoch,
            // The budget belongs to the physical socket, which survives the
            // identity swap: carried charged state keeps per-connection
            // accounting exact.
            error_reply_gate: old_connection.error_reply_gate,
        };

        // IP slot is already reserved from the old entry -- no need to
        // release and re-reserve for the same IP address.
        self.clients.insert(*reconnect_player_id, new_client);
        old_connection.lifecycle.set_player_id(*reconnect_player_id);
        // The RelayStats ledger follows the surviving connection so its
        // cumulative counters stay meaningful across the reassignment.
        self.metrics
            .rekey_connection_delivery_stats(current_player_id, *reconnect_player_id);
        self.metrics
            .rekey_slow_consumer_eviction_attributions(current_player_id, *reconnect_player_id);
        ReassignmentOutcome::Reassigned(delivery)
    }

    /// Undo a reconnect identity swap after the post-reassign restore path fails.
    ///
    /// `reassign_connection` must run before the reconnect baseline is built so
    /// the payload can read the restored player's negotiated protocol and fresh
    /// epoch. If that baseline cannot be enqueued, the WebSocket task keeps using
    /// `current_player_id` because `handle_reconnect` returns `false`; restore the
    /// connection map to match that task before it handles more teardown or input.
    pub fn restore_reassigned_connection(
        &self,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
    ) -> Option<ClientDeliveryHandle> {
        if self.clients.contains_key(current_player_id) {
            return None;
        }

        let (_, reassigned_connection) = self.clients.remove(reconnect_player_id)?;

        let mut delivery = reassigned_connection.delivery_handle();
        delivery.sender = delivery.sender.previous_generation();
        let membership_generation = reassigned_connection
            .prior_membership_generation
            .unwrap_or(reassigned_connection.membership_generation);
        let restored_client = ClientConnection {
            room_id: None,
            lifecycle: Arc::clone(&reassigned_connection.lifecycle),
            last_ping: Instant::now(),
            heartbeat_update_revision: reassigned_connection.heartbeat_update_revision,
            last_heartbeat_update: None,
            sender: delivery.sender.clone(),
            close: delivery.close.clone(),
            assignment_gate: Arc::clone(&reassigned_connection.assignment_gate),
            client_addr: reassigned_connection.client_addr,
            game_data_format: reassigned_connection.game_data_format,
            app_context: reassigned_connection.app_context,
            reconnection_identity: reassigned_connection.reconnection_identity,
            protocol: reassigned_connection.protocol,
            room_operation_ids: reassigned_connection.room_operation_ids,
            addressed_binary: reassigned_connection.addressed_binary,
            addressed_receipt: reassigned_connection.addressed_receipt,
            transport_status: reassigned_connection.transport_status,
            membership_generation,
            prior_membership_generation: None,
            pending_room_assignment: None,
            game_data_seq: 0,
            game_data_epoch: 0,
            error_reply_gate: reassigned_connection.error_reply_gate,
        };

        self.clients.insert(*current_player_id, restored_client);
        reassigned_connection
            .lifecycle
            .set_player_id(*current_player_id);
        self.metrics
            .rekey_connection_delivery_stats(reconnect_player_id, *current_player_id);
        self.metrics
            .rekey_slow_consumer_eviction_attributions(reconnect_player_id, *current_player_id);
        Some(delivery)
    }

    /// Arm a fresh per-connection error-reply gate from the configured budget.
    fn new_error_reply_gate(&self) -> ErrorReplyGate {
        let (limit, window) = self.error_reply_budget;
        ErrorReplyGate::new(limit, window)
    }

    /// Charge one polite per-frame reply — a refusal or failure envelope of
    /// any kind, or the `Pong` keepalive answer — against `player_id`'s
    /// fixed-window budget
    /// (issue #518). Returns `false` when the window is exhausted: the caller
    /// must NOT send the reply. The exhaustion side effects fire here, once
    /// per exhaustion event — the rejection metric, the best-effort advisory
    /// farewell, and the semantic `4006 inbound_rate_limited` close request,
    /// in that order — while the close lands.
    ///
    /// The close pins the per-socket close signal captured under the charge
    /// guard, not the map key: a reconnect identity swap landing inside the
    /// farewell await moves the entry to the restored identity's key, and the
    /// one-shot exhaustion report never retries a skipped pin (issue #697).
    ///
    /// An unknown player id is admitted (`true`): there is no budget to
    /// charge, and the caller's send fails downstream anyway.
    pub async fn charge_error_reply(&self, player_id: &PlayerId) -> bool {
        let (first_exhaustion, close) = {
            let Some(entry) = self.clients.get_mut(player_id) else {
                return true;
            };
            let mut gate = entry
                .error_reply_gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if gate.charge(Instant::now()) {
                return true;
            }
            // Exhausted: capture the per-socket close signal under the charge
            // guard, before any await. A reconnect identity swap landing
            // inside the farewell await moves the entry to the restored
            // identity's key, and the one-shot `report_exhaustion` below never
            // retries a skipped pin (issue #697). The signal belongs to the
            // physical socket, so the pin follows the swap.
            (gate.report_exhaustion(), entry.close.clone())
        };
        if !first_exhaustion {
            return false;
        }
        warn!(
            %player_id,
            "Inbound error-reply rate limit exceeded, closing connection"
        );
        self.metrics
            .record_rate_limit_rejection(RateLimitRejection::InboundErrorReply);
        // The budget-exhaustion farewell rides the reliable delivery path and
        // never waits for capacity; the semantic close reason is pinned after
        // it so the close frame remains the authoritative attribution signal.
        let _ = self
            .message_coordinator
            .try_send_to_player(
                player_id,
                Arc::new(ServerMessage::Error {
                    message: "Inbound error-reply rate limit exceeded".to_string(),
                    error_code: Some(ErrorCode::RateLimitExceeded),
                }),
            )
            .await;
        // Pin through the captured signal: it belongs to the physical socket,
        // so the close follows a concurrent identity swap (issue #697).
        close.request_close(CloseReason::InboundRateLimited);
        false
    }

    /// Request a close for `player_id`'s connection with an explicit reason,
    /// without unregistering it here (the caller's own teardown follows).
    /// First requested reason wins, so callers use this to pin a SPECIFIC
    /// close code — e.g. the activity reaper's `ActivityTimeout` — before the
    /// generic `Unregistered` of [`Self::remove_client`] would apply. Shutdown
    /// is the priority reason and can supersede an earlier lifecycle close.
    /// Returns whether this call initiated the close.
    pub fn request_close_for(
        &self,
        player_id: &PlayerId,
        reason: crate::coordination::CloseReason,
    ) -> bool {
        self.clients
            .get(player_id)
            .is_some_and(|connection| connection.close.request_close(reason))
    }

    /// Snapshot the per-socket close signal for `player_id`.
    ///
    /// Callers that hold a fence excluding identity swaps (the connection's
    /// own lifecycle gate) capture here and pin through the captured signal
    /// after the fence drops: the pin then follows the physical socket even
    /// when a swap rekeys the entry in between, and a one-shot reason never
    /// loses its close frame to a map miss (issue #697 class).
    pub(crate) fn close_signal_for(&self, player_id: &PlayerId) -> Option<ConnectionCloseSignal> {
        self.clients.get(player_id).map(|entry| entry.close.clone())
    }

    /// Bind a pre-admission wait to its physical socket before taking the lifecycle gate.
    pub(super) fn close_signal_for_lifecycle(
        &self,
        player_id: &PlayerId,
        lifecycle: &Arc<ClientLifecycle>,
    ) -> Option<ConnectionCloseSignal> {
        let entry = self.clients.get(player_id)?;
        Arc::ptr_eq(&entry.lifecycle, lifecycle).then(|| entry.close.clone())
    }

    #[cfg(test)]
    pub fn remove_client(&self, player_id: &PlayerId) -> Option<ClientConnection> {
        // Test fixtures use this convenience to simulate a complete departure;
        // honor the production ordering invariant by clearing membership (and
        // therefore capturing/resetting its tail) before physical removal.
        let _ = self.clear_room_assignment_guarded(player_id, None);
        self.remove_client_for_unregistration(player_id, || false)
            .map(|(connection, _)| connection)
    }

    pub fn remove_client_for_unregistration<F>(
        &self,
        player_id: &PlayerId,
        is_draining: F,
    ) -> Option<(ClientConnection, crate::coordination::CloseReason)>
    where
        F: FnOnce() -> bool,
    {
        if self.clients.get(player_id).is_some_and(|connection| {
            connection
                .lifecycle
                .admission_pending
                .load(Ordering::Acquire)
        }) {
            return None;
        }
        // A room-bound entry owns the only authoritative terminal relay tail.
        // Normal unregister always calls `leave_room_locked` first; refusing an
        // out-of-order removal encodes that ordering invariant and prevents a
        // later PlayerLeft from being forced to guess its final sequence.
        if self
            .clients
            .get(player_id)
            .is_some_and(|connection| connection.room_id.is_some())
        {
            warn!(%player_id, "Refusing to remove room-bound connection before terminal unroute");
            return None;
        }
        self.clients.remove(player_id).map(|(_, connection)| {
            self.release_ip_slot(connection.client_addr.ip());
            self.release_global_slot();
            self.metrics.unregister_connection_delivery_stats(player_id);
            self.metrics
                .unregister_slow_consumer_eviction_attributions(player_id);
            // Every unregistration positively tears down the socket tasks:
            // without this, a connection unregistered by the activity reaper
            // lingers half-alive (undeliverable but still holding its socket)
            // until an idle timeout fires. The caller supplies the final reason
            // at removal time so shutdown drain can still win a late race.
            // Reconnection reassignment deliberately bypasses this method, so
            // surviving connections are never closed here.
            let close_reason = if is_draining() {
                crate::coordination::CloseReason::Shutdown
            } else {
                crate::coordination::CloseReason::Unregistered
            };
            connection.close.request_close(close_reason);
            (connection, close_reason)
        })
    }

    pub fn collect_expired_clients(&self, ping_timeout: std::time::Duration) -> Vec<PlayerId> {
        let now = Instant::now();
        self.clients
            .iter()
            .filter_map(|entry| {
                if now.duration_since(entry.last_ping) > ping_timeout {
                    Some(*entry.key())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Atomically revalidate liveness and pin the activity-timeout close.
    ///
    /// Holding the DashMap entry guard excludes [`Self::record_ping`] between
    /// the timestamp check and the close decision. A Pong that arrives after a
    /// cleanup snapshot but before this call therefore rescues the connection;
    /// a Pong that races after the guard is acquired observes an already-final
    /// close instead of being silently discarded by a stale snapshot.
    pub fn request_activity_timeout_if_expired(
        &self,
        player_id: &PlayerId,
        ping_timeout: std::time::Duration,
    ) -> bool {
        let now = Instant::now();
        let Some(connection) = self.clients.get(player_id) else {
            return false;
        };
        if now.duration_since(connection.last_ping) <= ping_timeout {
            return false;
        }
        connection
            .close
            .request_close(crate::coordination::CloseReason::ActivityTimeout)
    }

    fn try_reserve_ip_slot(&self, ip: IpAddr) -> Result<usize, usize> {
        match self.connections_per_ip.entry(ip) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let current = *entry.get();
                if current >= self.max_connections_per_ip {
                    Err(current)
                } else {
                    let count = entry.get_mut();
                    *count = count.saturating_add(1);
                    Ok(*count)
                }
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                if self.max_connections_per_ip == 0 {
                    Err(0)
                } else {
                    entry.insert(1);
                    Ok(1)
                }
            }
        }
    }

    fn increment_ip_slot_unbounded(&self, ip: IpAddr) -> usize {
        // Use entry API for atomicity: prevents TOCTOU race where two threads
        // both see the key as absent and both insert 1 instead of 2
        match self.connections_per_ip.entry(ip) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let count = entry.get_mut();
                *count = count.saturating_add(1);
                *entry.get()
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(1);
                1
            }
        }
    }

    fn release_ip_slot(&self, ip: IpAddr) {
        // Use entry API for atomicity: prevents TOCTOU race where the count
        // is read as 1, the ref is dropped, another thread increments to 2,
        // then this thread removes the entry (losing the increment)
        if let dashmap::mapref::entry::Entry::Occupied(mut entry) =
            self.connections_per_ip.entry(ip)
        {
            if *entry.get() > 1 {
                let count = entry.get_mut();
                *count = count.saturating_sub(1);
            } else {
                entry.remove();
            }
        }
    }

    /// Reserve one server-wide connection slot, refusing at
    /// [`Self::max_connections`]. The compare-exchange loop makes check and
    /// spend atomic, so concurrent registrations can neither overshoot the
    /// ceiling nor lose an increment. Checked arithmetic keeps every step
    /// total (the crate denies unchecked arithmetic in production paths).
    fn try_reserve_global_slot(&self) -> Result<usize, usize> {
        let mut current = self.live_connections.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(1) else {
                // usize::MAX live connections: the counter is exhausted.
                return Err(current);
            };
            if next > self.max_connections {
                return Err(current);
            }
            match self.live_connections.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(next),
                Err(observed) => current = observed,
            }
        }
    }

    /// Count a test-only registration without admission checks (mirrors
    /// [`Self::increment_ip_slot_unbounded`]): the counter must stay balanced
    /// with the map, so every entry shape counts and every removal releases.
    /// The returned value wraps at the theoretical usize::MAX live-connection
    /// point; a release then refuses (loud log) rather than underflowing.
    fn increment_global_slot_unbounded(&self) -> usize {
        self.live_connections
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    /// Release one server-wide slot at unregistration. Paired exactly with
    /// one successful reservation (or unbounded test increment) per entry.
    /// The release saturates at zero instead of panicking: an underflow would
    /// mean an unpaired release (an invariant bug), which is logged loudly —
    /// the crate keeps panic-prone macros out of production paths.
    fn release_global_slot(&self) {
        let mut current = self.live_connections.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_sub(1) else {
                tracing::error!("global connection counter underflowed; ignoring unpaired release");
                return;
            };
            match self.live_connections.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }
}

#[cfg(test)]
#[path = "connection_manager_tests.rs"]
mod tests;
