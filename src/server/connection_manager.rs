use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// `tokio::time::Instant` (not `std::time::Instant`) so the activity reaper
// (`collect_expired_clients`) and the heartbeat-update throttle
// (`should_update_last_seen`) read the runtime clock. In production this wraps
// the same monotonic std clock (identical behavior); under
// `#[tokio::test(start_paused = true)]` it lets tests drive these windows with
// `tokio::time::advance(..)` deterministically, at zero wall-clock cost. Every
// `Instant::now()` here runs inside the tokio runtime (all callers are async /
// `#[tokio::test]`), so the runtime clock is always available.
use tokio::time::Instant;

use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::AppContext;
use crate::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, DeliverySender, MessageCoordinator,
};
use crate::metrics::ServerMetrics;
use crate::protocol::{GameDataEncoding, PlayerId, RoomId, ServerMessage, Topology, Transport};

use super::RegisterClientError;

/// Protocol capabilities negotiated for a single connection during `Authenticate`.
///
/// The default is a pure v2 client: protocol version 2, relay-only transport and
/// relay-only topology. v3 negotiation overwrites this via [`ConnectionManager::set_protocol`].
#[derive(Debug, Clone)]
pub(crate) struct NegotiatedProtocol {
    pub version: u16,
    pub transports: Vec<Transport>,
    /// Session topologies the client supports; consumed by the P3 session-plan
    /// selection path (`session_policy::choose_session_plan`).
    pub topologies: Vec<Topology>,
}

impl Default for NegotiatedProtocol {
    fn default() -> Self {
        Self {
            version: crate::config::SERVER_MIN_PROTOCOL_VERSION,
            transports: vec![Transport::Relay],
            topologies: vec![Topology::Relay],
        }
    }
}

#[derive(Debug, Clone)]
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
    pub sender: DeliverySender,
    /// Kill switch for this connection's socket tasks (slow-consumer
    /// disconnects, server-side eviction). Paired with `sender`: together they
    /// form the connection's [`ClientDeliveryHandle`].
    pub close: ConnectionCloseSignal,
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
}

/// Per-physical-connection lifecycle identity and serialization gate.
///
/// The player id is stored beside the gate so an unregister task that was
/// queued under the transient pre-reconnect id follows the surviving socket to
/// its restored id after it acquires the gate.
#[derive(Debug)]
pub(crate) struct ClientLifecycle {
    gate: Arc<tokio::sync::Mutex<()>>,
    player_id: std::sync::Mutex<PlayerId>,
}

impl ClientLifecycle {
    fn new(player_id: PlayerId) -> Self {
        Self {
            gate: Arc::new(tokio::sync::Mutex::new(())),
            player_id: std::sync::Mutex::new(player_id),
        }
    }

    pub(crate) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.gate.lock().await
    }

    pub(crate) async fn lock_owned(self: Arc<Self>) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.gate).lock_owned().await
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportStatusUpdate {
    Changed,
    Duplicate,
    MissingConnection,
    UnsupportedProtocolVersion,
    UnsupportedTransport,
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
        | CloseReason::Unregistered => true,
        // Identity/room-scoped: a drain must close restored connections, and
        // a room pin reflects the room the claim just verified.
        CloseReason::Shutdown | CloseReason::RoomInactive => false,
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
    max_connections_per_ip: usize,
    /// Whether per-connection delivery statistics (the v3 `RelayStats`
    /// ledger) are registered with the metrics sink for each connection.
    /// Mirrors `websocket.delivery_stats_interval_secs > 0` so a disabled
    /// deployment keeps the per-delivery bookkeeping at a single map miss.
    track_delivery_stats: bool,
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
    ) -> Self {
        Self {
            clients: DashMap::new(),
            connections_per_ip: DashMap::new(),
            metrics,
            message_coordinator,
            max_connections,
            live_connections: AtomicUsize::new(0),
            max_connections_per_ip,
            track_delivery_stats,
        }
    }

    pub async fn register_client(
        &self,
        sender: mpsc::Sender<Arc<ServerMessage>>,
        close: ConnectionCloseSignal,
        client_addr: SocketAddr,
        instance_id: Uuid,
    ) -> Result<PlayerId, RegisterClientError> {
        self.register_delivery(sender.into(), close, client_addr, instance_id)
            .await
    }

    pub(crate) async fn register_classified_client(
        &self,
        sender: DeliverySender,
        close: ConnectionCloseSignal,
        client_addr: SocketAddr,
        instance_id: Uuid,
    ) -> Result<PlayerId, RegisterClientError> {
        self.register_delivery(sender, close, client_addr, instance_id)
            .await
    }

    async fn register_delivery(
        &self,
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
        // Global ceiling second: if it refuses, the just-taken per-IP slot is
        // released so the two budgets stay independently consistent.
        if let Err(current) = self.try_reserve_global_slot() {
            self.release_ip_slot(ip);
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

        let player_id = Uuid::new_v4();
        let connection = ClientConnection {
            room_id: None,
            lifecycle: Arc::new(ClientLifecycle::new(player_id)),
            last_ping: Instant::now(),
            last_heartbeat_update: None,
            sender: sender.clone(),
            close: close.clone(),
            client_addr,
            game_data_format: GameDataEncoding::Json,
            app_context: None,
            reconnection_identity: None,
            protocol: NegotiatedProtocol::default(),
            room_operation_ids: false,
            transport_status: None,
            membership_generation: Uuid::nil(),
            prior_membership_generation: None,
            game_data_seq: 0,
            game_data_epoch: 0,
        };

        self.clients.insert(player_id, connection);
        self.metrics.increment_connections();
        if self.track_delivery_stats {
            self.metrics.register_connection_delivery_stats(player_id);
        }

        if let Err(err) = self
            .message_coordinator
            .register_local_client(player_id, None, ClientDeliveryHandle { sender, close })
            .await
        {
            warn!(%player_id, %err, "Failed to register client with coordinator");
        }

        info!(%player_id, instance_id = %instance_id, client_addr = %client_addr, "Client registered");
        Ok(player_id)
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
            last_heartbeat_update: None,
            sender: sender.clone(),
            close: close.clone(),
            client_addr,
            game_data_format: GameDataEncoding::Json,
            app_context: None,
            reconnection_identity: None,
            protocol: NegotiatedProtocol::default(),
            room_operation_ids: false,
            transport_status: None,
            membership_generation: Uuid::nil(),
            prior_membership_generation: None,
            game_data_seq: 0,
            game_data_epoch: 0,
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

        if let Err(err) = self
            .message_coordinator
            .register_local_client(player_id, None, ClientDeliveryHandle { sender, close })
            .await
        {
            warn!(%player_id, %err, "Failed to register test client with coordinator");
        }
    }

    pub async fn assign_client_to_room(&self, player_id: &PlayerId, room_id: RoomId) {
        if let Some((delivery, _stamp)) = self.prepare_client_to_room(player_id, room_id) {
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
            }
        }
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
        client.room_id = Some(room_id);
        Self::advance_membership_generation(&mut client);
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
        Some((client.delivery_handle(), stamp))
    }

    /// Undo [`Self::prepare_client_to_room`] when its transition frame never
    /// committed. The queue still owns the previous generation, so restore the
    /// wrapper instead of advancing again.
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

    // Read the full negotiated protocol for a connection. Consumed by the
    // session-plan/topology selection path.
    pub fn protocol(&self, player_id: &PlayerId) -> NegotiatedProtocol {
        self.clients
            .get(player_id)
            .map(|conn| conn.protocol.clone())
            .unwrap_or_default()
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
            if connection.protocol.version < 3 {
                return TransportStatusUpdate::UnsupportedProtocolVersion;
            }

            if !connection.protocol.transports.contains(&transport) {
                return TransportStatusUpdate::UnsupportedTransport;
            }

            let new_status = Some((connection.membership_generation, transport, connected));
            if connection.transport_status == new_status {
                return TransportStatusUpdate::Duplicate;
            }

            connection.transport_status = new_status;
            return TransportStatusUpdate::Changed;
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
        self.app_context(player_id).map(|info| info.id)
    }

    /// Capture the terminal relay watermark and clear membership under one
    /// connection-entry lock, but only while the player is still assigned to
    /// `expected_room`. On any other room (a stale leave racing a room switch)
    /// this returns `None` untouched instead of publishing that room's live
    /// stamp as a foreign room's phantom terminal watermark. Note the refusal
    /// is only `ConnectionManager`-level: the coordinator's
    /// `unroute_local_client_with_tail` treats a `None` closure result as
    /// "unroute without tail" and sweeps stale coordinator entries, so a
    /// mismatch still tears down routing (minus the watermark publication).
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

    pub async fn advance_delivery_generation(&self, player_id: &PlayerId) {
        let delivery = self.clients.get_mut(player_id).map(|mut client| {
            Self::advance_membership_generation(&mut client);
            client.sender = client.sender.next_generation();
            client.delivery_handle()
        });
        if let Some(delivery) = delivery {
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
    pub fn next_relay_stamp_in_room(
        &self,
        player_id: &PlayerId,
        expected_room: &RoomId,
    ) -> Option<RelayStamp> {
        let mut client = self.clients.get_mut(player_id)?;
        if client.room_id != Some(*expected_room) {
            return None;
        }
        let Some(next_seq) = client.game_data_seq.checked_add(1) else {
            tracing::error!(%player_id, %expected_room, "Relay sequence exhausted; canceling delivery");
            return None;
        };
        client.game_data_seq = next_seq;
        Some(RelayStamp {
            seq: client.game_data_seq,
            epoch: client.game_data_epoch,
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

    /// Checks if we should update `last_seen` for this player.
    /// Returns true if the threshold has elapsed since the last update, and marks
    /// the update as performed. Returns false if update should be skipped.
    ///
    /// This throttling mechanism reduces database writes while keeping local
    /// liveness and cleanup timestamps current.
    pub fn should_update_last_seen(
        &self,
        player_id: &PlayerId,
        threshold: std::time::Duration,
    ) -> bool {
        if let Some(mut client) = self.clients.get_mut(player_id) {
            let now = Instant::now();
            let should_update = match client.last_heartbeat_update {
                None => true, // Never updated, should update
                Some(last) => now.duration_since(last) >= threshold,
            };

            if should_update {
                client.last_heartbeat_update = Some(now);
            }

            should_update
        } else {
            // Unknown player: suppress. A missing entry can never take a
            // throttle stamp, so allowing the update would fire the heartbeat
            // metric and the persistence attempt on every in-flight frame that
            // races its teardown, breaking the once-per-player-per-window
            // throttle contract. Any live connection (seated player or
            // spectator) has an entry; a refused lookup here means the socket
            // is unregistering and no refresh can land anywhere meaningful.
            false
        }
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
            last_heartbeat_update: None, // Reset on reconnection, will update immediately
            sender: delivery.sender.clone(),
            close: delivery.close.clone(),
            client_addr: old_connection.client_addr,
            game_data_format: old_connection.game_data_format,
            app_context: old_connection.app_context,
            reconnection_identity: old_connection.reconnection_identity,
            protocol: old_connection.protocol,
            room_operation_ids: old_connection.room_operation_ids,
            // Preserve any roomless transient-socket status only as rollback
            // state. Advancing the membership generation below makes it
            // ineligible for reconnect deduplication, so the restored player
            // must establish and report its P2P path afresh.
            transport_status: old_connection.transport_status,
            membership_generation,
            prior_membership_generation: Some(prior_membership_generation),
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
        };

        // IP slot is already reserved from the old entry -- no need to
        // release and re-reserve for the same IP address.
        self.clients.insert(*reconnect_player_id, new_client);
        old_connection.lifecycle.set_player_id(*reconnect_player_id);
        // The RelayStats ledger follows the surviving connection so its
        // cumulative counters stay meaningful across the reassignment.
        self.metrics
            .rekey_connection_delivery_stats(current_player_id, *reconnect_player_id);
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
            last_heartbeat_update: None,
            sender: delivery.sender.clone(),
            close: delivery.close.clone(),
            client_addr: reassigned_connection.client_addr,
            game_data_format: reassigned_connection.game_data_format,
            app_context: reassigned_connection.app_context,
            reconnection_identity: reassigned_connection.reconnection_identity,
            protocol: reassigned_connection.protocol,
            room_operation_ids: reassigned_connection.room_operation_ids,
            transport_status: reassigned_connection.transport_status,
            membership_generation,
            prior_membership_generation: None,
            game_data_seq: 0,
            game_data_epoch: 0,
        };

        self.clients.insert(*current_player_id, restored_client);
        reassigned_connection
            .lifecycle
            .set_player_id(*current_player_id);
        self.metrics
            .rekey_connection_delivery_stats(reconnect_player_id, *current_player_id);
        Some(delivery)
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
    /// ceiling nor lose an increment.
    fn try_reserve_global_slot(&self) -> Result<usize, usize> {
        let mut current = self.live_connections.load(Ordering::Acquire);
        loop {
            if current >= self.max_connections {
                return Err(current);
            }
            match self.live_connections.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(current + 1),
                Err(observed) => current = observed,
            }
        }
    }

    /// Count a test-only registration without admission checks (mirrors
    /// [`Self::increment_ip_slot_unbounded`]): the counter must stay balanced
    /// with the map, so every entry shape counts and every removal releases.
    fn increment_global_slot_unbounded(&self) -> usize {
        self.live_connections.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Release one server-wide slot at unregistration. Paired exactly with
    /// one successful reservation (or unbounded test increment) per entry.
    /// The release saturates at zero instead of panicking: an underflow would
    /// mean an unpaired release (an invariant bug), which is logged loudly —
    /// the crate keeps panic-prone macros out of production paths.
    fn release_global_slot(&self) {
        let mut current = self.live_connections.load(Ordering::Acquire);
        loop {
            if current == 0 {
                tracing::error!("global connection counter underflowed; ignoring unpaired release");
                return;
            }
            match self.live_connections.compare_exchange_weak(
                current,
                current - 1,
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
mod tests {
    use super::*;
    use crate::coordination::{
        MembershipUpdate, MessageCoordinator, RoomEventCompletion, RoomEventJob,
        RoomEventMutationGuard, RoomEventSequencer,
    };
    use crate::distributed::SequencedMessage;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::net::SocketAddr;
    use tokio::sync::{mpsc, Mutex};

    #[derive(Default)]
    struct TestCoordinator {
        room_events: Arc<RoomEventSequencer>,
        registrations: Mutex<Vec<(PlayerId, Option<RoomId>)>>,
        unregisters: Mutex<Vec<PlayerId>>,
    }

    #[async_trait]
    impl MessageCoordinator for TestCoordinator {
        async fn lock_room_event_mutation(&self, room_id: &RoomId) -> RoomEventMutationGuard {
            self.room_events.lock(*room_id).await
        }

        fn enqueue_room_event(
            &self,
            mutation_guard: RoomEventMutationGuard,
            job: RoomEventJob,
        ) -> RoomEventCompletion {
            self.room_events.enqueue(mutation_guard, job)
        }

        async fn send_to_player(
            &self,
            _player_id: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn try_send_to_player(
            &self,
            player_id: &PlayerId,
            message: Arc<ServerMessage>,
        ) -> Result<bool> {
            // Test double: send_to_player is non-blocking here, so delegating
            // honors the non-waiting farewell contract while preserving
            // whatever recording/blocking behavior the double implements.
            self.send_to_player(player_id, message).await.map(|()| true)
        }

        async fn broadcast_to_room(
            &self,
            _room_id: &RoomId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn broadcast_to_room_except(
            &self,
            _room_id: &RoomId,
            _except_player: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn broadcast_to_room_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            before_send().await;
            self.broadcast_to_room(room_id, message).await?;
            Ok(true)
        }

        async fn broadcast_to_room_if_members_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            _expected_members: &[PlayerId],
            message: Arc<ServerMessage>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            self.broadcast_to_room_with_hook(room_id, message, before_send)
                .await
        }

        async fn broadcast_to_room_except_if_with_hook<'a>(
            &'a self,
            room_id: &RoomId,
            except_player: &PlayerId,
            message: Arc<ServerMessage>,
            should_send: &(dyn Fn() -> bool + Send + Sync),
            drain: tokio::sync::watch::Receiver<bool>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                    + Send
                    + 'a,
            >,
        ) -> Result<bool> {
            if *drain.borrow() || !should_send() {
                return Ok(false);
            }
            before_send().await;
            self.broadcast_to_room_except(room_id, except_player, message)
                .await?;
            Ok(true)
        }

        async fn commit_room_messages_if_members_with_hook<'a>(
            &'a self,
            _room_id: &RoomId,
            _expected_members: &[PlayerId],
            _recipient_messages: Vec<crate::coordination::RoomRecipientMessages>,
            before_send: Box<
                dyn FnOnce() -> std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>,
                    > + Send
                    + 'a,
            >,
            after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
        ) -> Result<crate::coordination::RoomMessageTransactionOutcome> {
            if before_send().await? {
                let _ = after_first_phase(0);
                Ok(crate::coordination::RoomMessageTransactionOutcome::Committed)
            } else {
                Ok(crate::coordination::RoomMessageTransactionOutcome::HookRejected)
            }
        }

        async fn register_local_client(
            &self,
            player_id: PlayerId,
            room_id: Option<RoomId>,
            _delivery: crate::coordination::ClientDeliveryHandle,
        ) -> Result<()> {
            self.registrations.lock().await.push((player_id, room_id));
            Ok(())
        }

        async fn unroute_local_client_with_tail<'a>(
            &'a self,
            player_id: PlayerId,
            _room_id: RoomId,
            clear_assignment: Box<
                dyn FnOnce() -> Option<(crate::coordination::ClientDeliveryHandle, u32, u64)>
                    + Send
                    + 'a,
            >,
        ) -> Result<Option<(u32, u64)>> {
            let Some((_delivery, epoch, final_seq)) = clear_assignment() else {
                return Ok(None);
            };
            self.registrations.lock().await.push((player_id, None));
            Ok(Some((epoch, final_seq)))
        }

        async fn unregister_local_client(&self, player_id: &PlayerId) -> Result<()> {
            self.unregisters.lock().await.push(*player_id);
            Ok(())
        }

        async fn should_process_message(&self, _message: &SequencedMessage) -> Result<bool> {
            Ok(true)
        }

        async fn mark_message_processed(&self, _message: &SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_bus_message(&self, _message: SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_membership_update(&self, _update: MembershipUpdate) -> Result<()> {
            Ok(())
        }
    }

    fn make_manager(max_connections_per_ip: usize) -> ConnectionManager {
        make_limited_manager(usize::MAX, max_connections_per_ip)
    }

    fn make_limited_manager(
        max_connections: usize,
        max_connections_per_ip: usize,
    ) -> ConnectionManager {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator: Arc<dyn MessageCoordinator> = Arc::new(TestCoordinator::default());
        ConnectionManager::new(
            max_connections,
            max_connections_per_ip,
            metrics,
            coordinator,
            false,
        )
    }

    fn channel() -> (
        mpsc::Sender<Arc<ServerMessage>>,
        mpsc::Receiver<Arc<ServerMessage>>,
    ) {
        mpsc::channel(4)
    }

    #[track_caller]
    fn expect_reassigned(outcome: ReassignmentOutcome) -> ClientDeliveryHandle {
        match outcome {
            ReassignmentOutcome::Reassigned(delivery) => delivery,
            other => panic!("expected reassignment, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_coordinator_uses_unknown_routing_default() {
        let coordinator = TestCoordinator::default();

        assert_eq!(
            coordinator
                .routed_player_ids(&RoomId::new_v4())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn register_client_enforces_ip_limits_and_releases_on_remove() {
        let manager = make_manager(1);
        let addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

        let (tx1, _rx1) = channel();
        let first_id = manager
            .register_client(tx1, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("first registration succeeds");

        let (tx2, _rx2) = channel();
        let err = manager
            .register_client(tx2, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect_err("second client hits per-IP limit");
        match err {
            RegisterClientError::IpLimitExceeded { current, limit } => {
                assert_eq!(current, 1);
                assert_eq!(limit, 1);
            }
            RegisterClientError::CapacityExceeded { .. } => {
                panic!("per-IP refusal must win when both caps bind at one IP")
            }
            RegisterClientError::ServerDraining => {
                panic!("connection manager does not own shutdown drain admission")
            }
        }

        manager.remove_client(&first_id);

        let (tx3, _rx3) = channel();
        manager
            .register_client(tx3, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registrations resume after slot release");
    }

    /// Issue #502 item 2: the server-wide ceiling refuses admission no matter
    /// how many distinct source IPs are involved (per-IP caps alone are
    /// multiplied by IP count), and releases the slot at unregistration so
    /// admission resumes. Also pins the slot conservation invariant: exactly
    /// `limit` admissions are possible again after every live entry is removed.
    #[tokio::test]
    async fn global_connection_ceiling_refuses_across_ips_and_releases_on_remove() {
        let manager = make_limited_manager(2, 8);
        let addrs: Vec<SocketAddr> = ["127.0.0.1:6001", "198.51.100.1:6002", "198.51.100.2:6003"]
            .iter()
            .map(|raw| raw.parse().unwrap())
            .collect();

        let mut ids = Vec::new();
        for (i, addr) in addrs.iter().take(2).enumerate() {
            let (tx, _rx) = channel();
            ids.push(
                manager
                    .register_client(tx, ConnectionCloseSignal::detached(), *addr, Uuid::new_v4())
                    .await
                    .unwrap_or_else(|err| {
                        panic!("registration {i} under the global ceiling must succeed: {err:?}")
                    }),
            );
        }

        let (tx3, _rx3) = channel();
        match manager
            .register_client(
                tx3,
                ConnectionCloseSignal::detached(),
                addrs[2],
                Uuid::new_v4(),
            )
            .await
        {
            Err(RegisterClientError::CapacityExceeded { current, limit }) => {
                assert_eq!((current, limit), (2, 2));
            }
            other => panic!("expected CapacityExceeded, got {other:?}"),
        }

        manager.remove_client(&ids[0]);

        let (tx4, _rx4) = channel();
        let fourth_id = manager
            .register_client(
                tx4,
                ConnectionCloseSignal::detached(),
                addrs[2],
                Uuid::new_v4(),
            )
            .await
            .expect("registration resumes after a global slot is released");

        // Slot conservation: draining every entry frees the full ceiling.
        manager.remove_client(&ids[1]);
        manager.remove_client(&fourth_id);
        for i in 0..2 {
            let (tx, _rx) = channel();
            manager
                .register_client(
                    tx,
                    ConnectionCloseSignal::detached(),
                    addrs[2],
                    Uuid::new_v4(),
                )
                .await
                .unwrap_or_else(|err| {
                    panic!("re-admission {i} after full drain must succeed: {err:?}")
                });
        }
    }

    /// When both caps bind at once, the per-IP refusal wins (the connection
    /// manager checks the per-IP budget first), keeping the pre-existing
    /// error precedence stable.
    #[tokio::test]
    async fn per_ip_refusal_wins_when_both_caps_bind() {
        let manager = make_limited_manager(1, 1);
        let addr: SocketAddr = "127.0.0.1:6004".parse().unwrap();
        let (tx1, _rx1) = channel();
        manager
            .register_client(tx1, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("first registration succeeds");

        let (tx2, _rx2) = channel();
        let err = manager
            .register_client(tx2, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect_err("second registration hits both caps");
        assert!(
            matches!(err, RegisterClientError::IpLimitExceeded { .. }),
            "per-IP refusal must take precedence, got {err:?}"
        );
    }

    /// The test-only registration path bypasses admission (like its per-IP
    /// sibling) but still counts through the global counter so that
    /// unregistration releases stay balanced: mixed live entries must not
    /// wedge the counter above the ceiling or below zero.
    #[tokio::test]
    async fn test_only_registrations_count_but_bypass_the_global_ceiling() {
        let manager = make_limited_manager(1, 8);
        let addr: SocketAddr = "127.0.0.1:6005".parse().unwrap();

        let (tx1, _rx1) = channel();
        let production_id = manager
            .register_client(tx1, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("production registration consumes the one slot");

        let (tx2, _rx2) = channel();
        let test_id = Uuid::new_v4();
        manager.connect_test_client(test_id, tx2, addr).await;

        manager.remove_client(&production_id);
        manager.remove_client(&test_id);

        let (tx3, _rx3) = channel();
        manager
            .register_client(tx3, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("counter must be fully released after mixed entries drain");
    }

    /// A same-id test registration replaces the prior entry; the displaced
    /// entry's slots are released in place so the counters stay paired with
    /// live entries (a stomp must not permanently wedge the ceiling).
    #[tokio::test]
    async fn same_id_test_replacement_releases_the_displaced_slots() {
        let manager = make_limited_manager(1, 8);
        let addr: SocketAddr = "127.0.0.1:6006".parse().unwrap();
        let reused_id = Uuid::new_v4();

        let (tx1, _rx1) = channel();
        manager.connect_test_client(reused_id, tx1, addr).await;
        let (tx2, _rx2) = channel();
        manager.connect_test_client(reused_id, tx2, addr).await;

        manager.remove_client(&reused_id);

        let (tx3, _rx3) = channel();
        manager
            .register_client(tx3, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("the one slot must be free after the replaced entry drains");
    }

    #[tokio::test]
    async fn remove_client_for_unregistration_uses_drain_predicate_at_close_request() {
        let manager = make_manager(1);
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();
        let (tx, _rx) = channel();
        let (close, listener) = ConnectionCloseSignal::channel();
        let player_id = manager
            .register_client(tx, close, addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");

        let (_connection, reason) = manager
            .remove_client_for_unregistration(&player_id, || true)
            .expect("client should be removed");

        assert_eq!(reason, crate::coordination::CloseReason::Shutdown);
        assert_eq!(
            listener.requested_reason(),
            Some(crate::coordination::CloseReason::Shutdown),
            "removal must request shutdown when the final drain predicate is true"
        );
    }

    #[tokio::test]
    async fn generic_removal_cannot_discard_a_room_members_terminal_tail() {
        let manager = make_manager(1);
        let addr: SocketAddr = "127.0.0.1:5002".parse().unwrap();
        let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_00A0);
        let (tx, _rx) = channel();
        let player_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");
        manager.assign_client_to_room(&player_id, room_id).await;
        assert_eq!(
            manager.next_relay_stamp_in_room(&player_id, &room_id),
            Some(RelayStamp { epoch: 1, seq: 1 })
        );

        assert!(
            manager
                .remove_client_for_unregistration(&player_id, || false)
                .is_none(),
            "generic teardown must leave a room-bound connection intact"
        );
        assert!(manager.has_client(&player_id));
        let (_, terminal_tail) = manager
            .clear_room_assignment_with_tail(&player_id, &room_id)
            .expect("terminal unroute retains the connection and tail");
        assert_eq!(terminal_tail, RelayStamp { epoch: 1, seq: 1 });
        assert!(
            manager
                .remove_client_for_unregistration(&player_id, || false)
                .is_some(),
            "connection removal becomes valid after terminal capture"
        );
    }

    /// The resumed reconnect epoch is part of the reassignment itself: no
    /// provisional value (the transient socket's epoch+1) may ever be visible
    /// between reassignment and the caller's epoch resume.
    #[tokio::test]
    async fn reassign_connection_applies_the_resumed_epoch_immediately() {
        let manager = make_manager(4);
        let addr: SocketAddr = "127.0.0.1:5003".parse().unwrap();
        let (tx, _rx) = channel();
        let transient_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");
        let restored_id = PlayerId::new_v4();
        let room_id = RoomId::new_v4();

        manager.reassign_connection(&transient_id, &restored_id, room_id, 7);

        // The very first metadata read through the room-scoped projection API
        // already observes the final incarnation.
        assert_eq!(
            manager.current_relay_stamp_in_room(&restored_id, &room_id),
            Some(RelayStamp { epoch: 7, seq: 0 }),
            "reassignment must publish the resumed epoch atomically"
        );
        // And the next allocated stamp continues that incarnation.
        assert_eq!(
            manager.next_relay_stamp_in_room(&restored_id, &room_id),
            Some(RelayStamp { epoch: 7, seq: 1 }),
        );
    }

    /// A stale terminal unroute for a room the player no longer (or never did)
    /// belongs to must be refused untouched — publishing the current
    /// assignment's live stamp as a foreign room's terminal watermark would
    /// fabricate a phantom `PlayerLeft` tail and sever live membership.
    #[tokio::test]
    async fn clear_room_assignment_with_tail_refuses_a_foreign_room() {
        let manager = make_manager(4);
        let addr: SocketAddr = "127.0.0.1:5004".parse().unwrap();
        let (tx, _rx) = channel();
        let player_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");
        let room_a = RoomId::new_v4();
        let room_b = RoomId::new_v4();
        manager.assign_client_to_room(&player_id, room_a).await;
        assert_eq!(
            manager.next_relay_stamp_in_room(&player_id, &room_a),
            Some(RelayStamp { epoch: 1, seq: 1 })
        );

        // Wrong-room capture: refused, nothing consumed or cleared.
        assert!(
            manager
                .clear_room_assignment_with_tail(&player_id, &room_b)
                .is_none(),
            "a foreign expected_room must not capture a tail"
        );
        assert_eq!(
            manager.current_relay_stamp_in_room(&player_id, &room_a),
            Some(RelayStamp { epoch: 1, seq: 1 }),
            "membership and stamp stream stay intact after the refusal"
        );

        // Correct-room capture still works exactly as before.
        let (_, tail) = manager
            .clear_room_assignment_with_tail(&player_id, &room_a)
            .expect("matching expected_room captures the terminal tail");
        assert_eq!(tail, RelayStamp { epoch: 1, seq: 1 });
    }

    /// GAP-3 regression: a 16-player session behind a single NAT must be
    /// admissible at the DEFAULT per-IP cap. Before A3 the default was 10, so
    /// the 11th same-IP client was refused. Builds a manager at the real
    /// `default_max_connections_per_ip()` and registers 16 clients from one IP.
    #[tokio::test]
    async fn default_ip_cap_admits_a_sixteen_player_nat() {
        let cap = crate::config::defaults::default_max_connections_per_ip();
        assert!(
            cap >= 16,
            "default per-IP cap ({cap}) must admit a 16-player NAT session"
        );

        let manager = make_manager(cap);
        let mut ids = Vec::new();
        for i in 0..16u16 {
            let (tx, _rx) = channel();
            let addr: SocketAddr = format!("203.0.113.7:{}", 6000 + i).parse().unwrap();
            let id = manager
                .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
                .await
                .unwrap_or_else(|e| panic!("client {i} from one IP must be admitted: {e:?}"));
            ids.push(id);
        }
        assert_eq!(
            ids.len(),
            16,
            "all 16 same-IP clients admitted at default cap"
        );
    }

    /// The heartbeat-update throttle (`should_update_last_seen`) is deterministic
    /// under the paused-clock runtime: the first observation always updates, then
    /// updates are suppressed until the threshold has *elapsed* on the runtime
    /// clock. Driven purely by `tokio::time::advance(..)` — no wall-clock sleep,
    /// so nothing can flake under load (this is the B4 payoff: the reaper /
    /// throttle windows read `tokio::time::Instant`).
    #[tokio::test(start_paused = true)]
    async fn should_update_last_seen_throttles_until_threshold_elapses() {
        let manager = make_manager(4);
        let addr: SocketAddr = "127.0.0.1:7100".parse().unwrap();
        let (tx, _rx) = channel();
        let player_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");

        let threshold = std::time::Duration::from_secs(30);

        // First observation always updates (no prior timestamp recorded).
        assert!(
            manager.should_update_last_seen(&player_id, threshold),
            "first observation must update"
        );
        // Immediately after, the throttle suppresses another update.
        assert!(
            !manager.should_update_last_seen(&player_id, threshold),
            "an update within the throttle window must be suppressed"
        );

        // Just below the threshold: still suppressed (the boundary is `>=`).
        tokio::time::advance(threshold - std::time::Duration::from_millis(1)).await;
        assert!(
            !manager.should_update_last_seen(&player_id, threshold),
            "an update just under the threshold must stay suppressed"
        );

        // Crossing the threshold releases exactly one update, then re-throttles
        // from the new baseline.
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        assert!(
            manager.should_update_last_seen(&player_id, threshold),
            "an update at/after the threshold must fire"
        );
        assert!(
            !manager.should_update_last_seen(&player_id, threshold),
            "the update after the release is throttled from the new baseline"
        );
    }

    /// A missing player must be throttled OFF, not waved through: an unregistered
    /// id can never take a throttle stamp, so "allow" would fire the metric
    /// increment and persistence attempt on every in-flight frame racing its
    /// teardown — violating the once-per-player-per-window invariant. A player
    /// registered afterwards starts with a clean throttle baseline.
    /// Clock-independent (the not-found branch never reads the clock).
    #[tokio::test]
    async fn should_update_last_seen_suppresses_unknown_player() {
        let manager = make_manager(4);
        let unknown = Uuid::new_v4();
        assert!(
            !manager.should_update_last_seen(&unknown, std::time::Duration::from_secs(30)),
            "unknown player must be suppressed, not allowed"
        );

        // The suppression is not sticky state: a player registered after the
        // refused lookups still observes a normal first-update release.
        let addr: SocketAddr = "127.0.0.1:7101".parse().unwrap();
        let (tx, _rx) = channel();
        let late_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");
        assert!(
            manager.should_update_last_seen(&late_id, std::time::Duration::from_secs(30)),
            "freshly registered player keeps the first-update release"
        );
    }

    #[tokio::test]
    async fn assign_client_to_room_updates_coordinator_membership() {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(TestCoordinator::default());
        let manager = ConnectionManager::new(
            usize::MAX,
            4,
            metrics.clone(),
            coordinator.clone() as Arc<dyn MessageCoordinator>,
            false,
        );

        let (tx, _rx) = channel();
        let addr: SocketAddr = "127.0.0.1:6000".parse().unwrap();
        let player_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");

        let room_id = RoomId::new_v4();
        manager.assign_client_to_room(&player_id, room_id).await;

        assert_eq!(manager.get_client_room(&player_id), Some(room_id));

        let registrations = coordinator.registrations.lock().await;
        assert_eq!(registrations.len(), 2);
        assert_eq!(registrations[0], (player_id, None));
        assert_eq!(registrations[1], (player_id, Some(room_id)));
    }

    #[tokio::test]
    async fn relay_stamp_allocation_is_bound_to_the_expected_room() {
        let manager = make_manager(4);
        let (tx, _rx) = channel();
        let addr: SocketAddr = "127.0.0.1:6001".parse().unwrap();
        let player_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");
        let room_a = RoomId::from_u128(0xAAAA);
        let room_b = RoomId::from_u128(0xBBBB);

        manager.assign_client_to_room(&player_id, room_a).await;
        assert_eq!(
            manager.next_relay_stamp_in_room(&player_id, &room_a),
            Some(RelayStamp { epoch: 1, seq: 1 })
        );
        assert_eq!(
            manager.current_relay_stamp_in_room(&player_id, &room_a),
            Some(RelayStamp { epoch: 1, seq: 1 })
        );

        manager.clear_room_assignment(&player_id);
        assert_eq!(
            manager.next_relay_stamp_in_room(&player_id, &room_a),
            None,
            "roomless teardown window must cancel the old-room relay"
        );
        assert_eq!(
            manager.current_relay_stamp_in_room(&player_id, &room_a),
            None,
            "roomless players must be filtered from live snapshots"
        );

        manager.assign_client_to_room(&player_id, room_b).await;
        assert_eq!(
            manager.next_relay_stamp_in_room(&player_id, &room_a),
            None,
            "a room switch must not allocate against the stale room"
        );
        assert_eq!(
            manager.next_relay_stamp_in_room(&player_id, &room_b),
            Some(RelayStamp { epoch: 2, seq: 1 })
        );
    }

    // -----------------------------------------------------------------------
    // D. Thread safety tests for ConnectionManager
    // -----------------------------------------------------------------------

    /// D17: Many clients from the same IP; verify counter accuracy.
    ///
    /// max_connections_per_ip = 5.
    /// 20 tasks concurrently try to register from the same IP.
    /// Exactly 5 should succeed.
    /// After removing all 5, the counter should be back to 0.
    #[tokio::test]
    async fn test_concurrent_ip_slot_reservation() {
        let manager = make_manager(5);
        let addr: SocketAddr = "10.0.0.1:9000".parse().unwrap();

        let task_count = 20;
        let barrier = Arc::new(tokio::sync::Barrier::new(task_count));
        let manager = Arc::new(manager);
        let mut handles = Vec::with_capacity(task_count);

        for _ in 0..task_count {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let (tx, _rx) = channel();
                manager
                    .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
                    .await
            }));
        }

        let mut successes = Vec::new();
        let mut failures = 0usize;
        for handle in handles {
            match handle.await.expect("task should not panic") {
                Ok(player_id) => successes.push(player_id),
                Err(_) => failures += 1,
            }
        }

        assert_eq!(
            successes.len(),
            5,
            "Exactly 5 should succeed, got {}",
            successes.len()
        );
        assert_eq!(failures, 15, "15 should be rejected, got {failures}");

        // Remove all 5 successful clients
        for pid in &successes {
            manager.remove_client(pid);
        }

        // After removal, new registrations should work (counter is back to 0)
        let (tx, _rx) = channel();
        let result = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await;
        assert!(
            result.is_ok(),
            "Registration should succeed after all clients removed"
        );
    }

    /// D18: Reassignment does not leak IP slots.
    ///
    /// The swap refuses when a live entry already occupies the target id
    /// instead of silently stomping it, and both entries stay untouched — the
    /// same defense the rollback sibling (`restore_reassigned_connection`)
    /// applies with its `contains_key` check.
    #[tokio::test]
    async fn reassign_connection_refuses_an_occupied_target_instead_of_stomping_it() {
        let manager = make_manager(8);
        let occupant_id = Uuid::new_v4();
        let (tx, _rx) = channel();
        manager
            .connect_test_client(occupant_id, tx, "10.0.0.64:9000".parse().unwrap())
            .await;

        let (transient_tx, _transient_rx) = channel();
        let transient_id = manager
            .register_client(
                transient_tx,
                ConnectionCloseSignal::detached(),
                "10.0.0.65:9000".parse().unwrap(),
                Uuid::new_v4(),
            )
            .await
            .expect("transient registration succeeds");

        let outcome = manager.reassign_connection(&transient_id, &occupant_id, RoomId::new_v4(), 1);
        assert!(
            matches!(outcome, ReassignmentOutcome::RefusedTargetOccupied),
            "an occupied target must be refused, got {outcome:?}"
        );
        assert!(
            manager.has_client(&occupant_id),
            "the occupant entry must survive the refused swap"
        );
        assert!(
            manager.has_client(&transient_id),
            "the transient entry must survive the refused swap"
        );
    }

    /// Register a client, reassign to a new player_id.
    /// IP count should still be 1 (not 0 or 2).
    /// Verify by filling up to the per-IP limit, then remove the reassigned
    /// client and confirm the freed slot allows a new registration.
    #[tokio::test]
    async fn test_reassign_connection_preserves_ip_count() {
        let manager = make_manager(5);
        let addr: SocketAddr = "10.0.0.2:9000".parse().unwrap();

        let (tx, _rx) = channel();
        let original_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration should succeed");

        let room_id = RoomId::new_v4();
        let new_player_id = Uuid::new_v4();

        let reassigned = manager.reassign_connection(&original_id, &new_player_id, room_id, 1);
        assert!(
            matches!(reassigned, ReassignmentOutcome::Reassigned(_)),
            "Reassignment should succeed"
        );

        // Original player should be gone
        assert!(
            !manager.has_client(&original_id),
            "Original player should no longer exist"
        );
        assert!(
            manager.has_client(&new_player_id),
            "New player should exist"
        );

        // IP slot should still be 1 (not 0 or 2)
        // Verify by trying to register 4 more (max is 5, 1 already used)
        for i in 0..4 {
            let (tx, _rx) = channel();
            let port = 9001 + i;
            let new_addr: SocketAddr = format!("10.0.0.2:{port}").parse().unwrap();
            manager
                .register_client(
                    tx,
                    ConnectionCloseSignal::detached(),
                    new_addr,
                    Uuid::new_v4(),
                )
                .await
                .expect("should succeed within limit");
        }

        // 5th attempt from same IP should fail (already at limit)
        let (tx, _rx) = channel();
        let new_addr: SocketAddr = "10.0.0.2:10000".parse().unwrap();
        let result = manager
            .register_client(
                tx,
                ConnectionCloseSignal::detached(),
                new_addr,
                Uuid::new_v4(),
            )
            .await;
        assert!(
            result.is_err(),
            "6th connection from same IP should be rejected"
        );

        // Remove the reassigned client and verify IP slot is freed
        manager.remove_client(&new_player_id);
        assert!(
            !manager.has_client(&new_player_id),
            "Client should be removed"
        );

        // After removing the reassigned client, the slot should be freed.
        // Verify by registering one more from the same IP (was at limit before removal).
        let (tx_verify, _rx_verify) = channel();
        let verify_addr: SocketAddr = "10.0.0.2:10001".parse().unwrap();
        let result = manager
            .register_client(
                tx_verify,
                ConnectionCloseSignal::detached(),
                verify_addr,
                Uuid::new_v4(),
            )
            .await;
        assert!(
            result.is_ok(),
            "Registration should succeed after removing the reassigned client"
        );
    }

    #[tokio::test]
    async fn reassign_and_restore_preserve_physical_lifecycle_identity() {
        let manager = make_manager(5);
        let addr: SocketAddr = "10.0.0.20:9000".parse().unwrap();
        let (tx, _rx) = channel();
        let transient_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration should succeed");
        let restored_id = PlayerId::new_v4();
        let lifecycle = manager
            .client_lifecycle(&transient_id)
            .expect("registered connection has lifecycle identity");

        expect_reassigned(manager.reassign_connection(
            &transient_id,
            &restored_id,
            RoomId::new_v4(),
            1,
        ));
        let reassigned_lifecycle = manager
            .client_lifecycle(&restored_id)
            .expect("restored id owns lifecycle identity");
        assert!(Arc::ptr_eq(&lifecycle, &reassigned_lifecycle));
        assert_eq!(lifecycle.player_id(), restored_id);
        assert!(manager.lifecycle_matches(&restored_id, &lifecycle));
        assert!(!manager.lifecycle_matches(&transient_id, &lifecycle));

        manager
            .restore_reassigned_connection(&transient_id, &restored_id)
            .expect("rollback succeeds");
        let rolled_back_lifecycle = manager
            .client_lifecycle(&transient_id)
            .expect("transient id regains lifecycle identity");
        assert!(Arc::ptr_eq(&lifecycle, &rolled_back_lifecycle));
        assert_eq!(lifecycle.player_id(), transient_id);
        assert!(manager.lifecycle_matches(&transient_id, &lifecycle));
        assert!(!manager.lifecycle_matches(&restored_id, &lifecycle));
    }

    /// A per-socket close pinned on the transient entry must refuse the
    /// identity swap and restore the entry untouched: the pending close may
    /// tear down only the transient socket, never the restored connection.
    #[tokio::test]
    async fn reassign_refused_while_transient_socket_carries_per_socket_close() {
        let manager = make_manager(4);
        let addr: SocketAddr = "127.0.0.1:5104".parse().unwrap();
        let restored_id = PlayerId::new_v4();
        let room_id = RoomId::new_v4();

        for reason in [
            crate::coordination::CloseReason::ActivityTimeout,
            crate::coordination::CloseReason::IdleTimeout,
            crate::coordination::CloseReason::SlowConsumer,
            crate::coordination::CloseReason::AuthTimeout,
            crate::coordination::CloseReason::OutboundMessageTooLarge,
            crate::coordination::CloseReason::Unregistered,
        ] {
            let (tx, _rx) = channel();
            let transient_id = manager
                .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
                .await
                .expect("registration succeeds");
            let pinned = manager.request_close_for(&transient_id, reason);
            assert!(
                pinned,
                "{reason:?}: pin must be the first close on the entry"
            );
            match manager.reassign_connection(&transient_id, &restored_id, room_id, 1) {
                ReassignmentOutcome::RefusedTransientClose(pinned) => {
                    assert_eq!(pinned, reason);
                }
                other => panic!("{reason:?}: expected refusal, got {other:?}"),
            }
            // The transient entry must be back, keyed by its own id, still the
            // only registration — the eviction proceeds against it.
            assert!(
                manager.has_client(&transient_id),
                "{reason:?}: refusal must restore the transient entry"
            );
            assert!(
                !manager.has_client(&restored_id),
                "{reason:?}: refusal must not publish the restored identity"
            );
            manager.remove_client(&transient_id);
        }
    }

    /// Identity/room-scoped closes still cross the swap: a drain must close
    /// restored connections, and a room-inactive pin reflects the room the
    /// claim just verified.
    #[tokio::test]
    async fn reassign_still_adopts_entry_pinned_for_shutdown() {
        let manager = make_manager(4);
        let addr: SocketAddr = "127.0.0.1:5105".parse().unwrap();
        for reason in [
            crate::coordination::CloseReason::Shutdown,
            crate::coordination::CloseReason::RoomInactive,
        ] {
            let (tx, _rx) = channel();
            let transient_id = manager
                .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
                .await
                .expect("registration succeeds");
            assert!(manager.request_close_for(&transient_id, reason));
            expect_reassigned(manager.reassign_connection(
                &transient_id,
                &PlayerId::new_v4(),
                RoomId::new_v4(),
                1,
            ));
        }
    }

    // -----------------------------------------------------------------------
    // Protocol capability negotiation (P1).
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn protocol_defaults_to_v2_relay_only() {
        let manager = make_manager(4);
        let addr: SocketAddr = "127.0.0.1:7100".parse().unwrap();
        let (tx, _rx) = channel();
        let pid = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("register");

        let proto = manager.protocol(&pid);
        assert_eq!(proto.version, 2);
        assert_eq!(proto.transports, vec![Transport::Relay]);
        assert_eq!(proto.topologies, vec![Topology::Relay]);

        assert!(!manager.supports_v3(&pid));
        assert!(manager.supports_transport(&pid, Transport::Relay));
        assert!(!manager.supports_transport(&pid, Transport::WebRtc));
    }

    #[tokio::test]
    async fn set_protocol_updates_capabilities_and_v3_gate() {
        let manager = make_manager(4);
        let addr: SocketAddr = "127.0.0.1:7101".parse().unwrap();
        let (tx, _rx) = channel();
        let pid = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("register");

        manager.set_protocol(
            &pid,
            NegotiatedProtocol {
                version: 3,
                transports: vec![Transport::Relay, Transport::WebRtc],
                topologies: vec![Topology::Relay, Topology::Mesh],
            },
        );

        assert!(manager.supports_v3(&pid));
        assert!(manager.supports_transport(&pid, Transport::WebRtc));
        assert!(manager.supports_transport(&pid, Transport::Relay));
        assert!(!manager.supports_transport(&pid, Transport::Direct));

        let proto = manager.protocol(&pid);
        assert_eq!(proto.version, 3);
        assert_eq!(proto.topologies, vec![Topology::Relay, Topology::Mesh]);
    }

    #[tokio::test]
    async fn protocol_helpers_default_for_unknown_player() {
        let manager = make_manager(4);
        let unknown = Uuid::new_v4();
        // Unknown player => default (v2 relay-only), not v3.
        let proto = manager.protocol(&unknown);
        assert_eq!(proto.version, 2);
        assert!(!manager.supports_v3(&unknown));
        assert!(!manager.supports_transport(&unknown, Transport::Relay));
    }

    #[tokio::test]
    async fn protocol_is_preserved_across_reconnect() {
        let manager = make_manager(4);
        let addr: SocketAddr = "127.0.0.1:7102".parse().unwrap();
        let (tx, _rx) = channel();
        let original = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("register");

        manager.set_protocol(
            &original,
            NegotiatedProtocol {
                version: 3,
                transports: vec![Transport::Relay, Transport::WebRtc],
                topologies: vec![Topology::Relay, Topology::Mesh],
            },
        );

        let new_pid = Uuid::new_v4();
        let room = RoomId::new_v4();
        assert!(matches!(
            manager.reassign_connection(&original, &new_pid, room, 1),
            ReassignmentOutcome::Reassigned(_)
        ));

        // The migrated connection keeps its negotiated v3 capabilities.
        let proto = manager.protocol(&new_pid);
        assert_eq!(proto.version, 3);
        assert!(manager.supports_v3(&new_pid));
        assert!(manager.supports_transport(&new_pid, Transport::WebRtc));
    }

    /// D19: Multiple concurrent releases do not underflow the IP counter.
    ///
    /// Register 3 clients from the same IP.
    /// Concurrently remove all 3.
    /// After removal, new registrations should work (no underflow).
    #[tokio::test]
    async fn test_concurrent_release_ip_slot_no_underflow() {
        let manager = Arc::new(make_manager(10));

        // Register 3 clients from same IP (different ports for each)
        let mut player_ids = Vec::new();
        for i in 0..3u16 {
            let (tx, _rx) = channel();
            let port_addr: SocketAddr = format!("10.0.0.3:{}", 9000 + i).parse().unwrap();
            let pid = manager
                .register_client(
                    tx,
                    ConnectionCloseSignal::detached(),
                    port_addr,
                    Uuid::new_v4(),
                )
                .await
                .expect("registration should succeed");
            player_ids.push(pid);
        }

        // Concurrently remove all 3
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for pid in player_ids {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                manager.remove_client(&pid);
            }));
        }

        for handle in handles {
            handle.await.expect("task should not panic");
        }

        // After all removals, IP should be completely cleared.
        // Verify by registering up to max_connections_per_ip (10).
        for i in 0..10u16 {
            let (tx, _rx) = channel();
            let port_addr: SocketAddr = format!("10.0.0.3:{}", 8000 + i).parse().unwrap();
            let result = manager
                .register_client(
                    tx,
                    ConnectionCloseSignal::detached(),
                    port_addr,
                    Uuid::new_v4(),
                )
                .await;
            assert!(
                result.is_ok(),
                "Registration #{} should succeed after complete removal (no underflow)",
                i + 1
            );
        }
    }
}
