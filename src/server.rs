use crate::auth::AppContext;
use crate::config::AppRegistrationEntry;
use crate::coordination::{
    ClientDeliveryHandle, CloseReason, ConnectionCloseSignal, DeliveryOutcome, DeliveryPermit,
    DeliveryReserveError, DeliverySender, DeliveryTrySendError, ImmediateGameDataBroadcast,
    InMemoryRoomOperationCoordinator, MessageCoordinator, RoomEventCompletion, RoomEventJob,
    RoomEventMutationGuard, RoomEventSequencer, RoomMessageTransactionOutcome,
    RoomOperationCoordinatorTrait, RoomRecipientMessages,
};
use crate::database::{create_database, DatabaseConfig, GameDatabase};
use crate::distributed::{DistributedLock, InMemoryDistributedLock};
use crate::metrics::ConnectionDeliveryStats;
use crate::protocol::{
    room_codes, GameDataEncoding, PlayerId, RoomId, ServerMessage, SpectatorStateChangeReason,
    Transport,
};
use crate::rate_limit::{RateLimitConfig, RoomRateLimiter};
use anyhow::Result;
use dashmap::DashMap;
#[cfg(test)]
use std::collections::VecDeque;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use thiserror::Error;
use tokio::sync::{mpsc, watch, Notify, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};
use tokio::time::Duration;
use uuid::Uuid;

fn chrono_duration_from_std(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::MAX)
}

#[cfg(test)]
mod duration_conversion_tests {
    use super::*;

    #[test]
    fn extreme_std_duration_saturates_without_panicking() {
        let converted = std::panic::catch_unwind(|| chrono_duration_from_std(Duration::MAX));
        assert_eq!(converted.ok(), Some(chrono::Duration::MAX));
    }
}

mod admin;
#[cfg(test)]
mod app_admission_tests;
mod authority;
mod connection_manager;
mod dashboard_cache;
mod game_data;
#[cfg(test)]
mod game_data_tests;
mod heartbeat;
mod maintenance;
#[cfg(test)]
mod message_coordinator_tests;
mod message_router;
#[cfg(test)]
mod message_router_tests;
mod messaging;
mod ready_state;
#[cfg(test)]
mod ready_state_tests;
mod reconnection_service;
mod relay_policy;
mod room_service;
#[cfg(test)]
mod room_service_tests;
mod session_policy;
#[cfg(test)]
mod session_policy_tests;
mod shutdown;
mod signaling;
#[cfg(test)]
mod signaling_tests;
mod spectator_handlers;
mod spectator_service;

use connection_manager::{ClientLifecycle, ConnectionManager};
pub(crate) use connection_manager::{NegotiatedProtocol, TransportStatusUpdate};
use dashboard_cache::{DashboardMetricsCache, DashboardMetricsView};
pub use shutdown::ShutdownDrain;
use spectator_service::SpectatorService;

/// Narrow dev-only entry point for measuring the production game-data handoff
/// without constructing an `EnhancedGameServer` or spawning background tasks.
#[cfg(feature = "allocation-tracking")]
#[doc(hidden)]
pub mod allocation_benchmark {
    pub use super::game_data::broadcast_game_data_with;
}

// Removed unused imports

/// Enhanced game server with process-local coordination.
pub struct EnhancedGameServer {
    /// In-memory game state storage
    database: Arc<dyn GameDatabase>,
    /// Connection management (clients, IP accounting)
    connection_manager: Arc<ConnectionManager>,
    /// Server configuration
    config: ServerConfig,
    /// Protocol configuration for validation
    protocol_config: crate::config::ProtocolConfig,
    /// Relay type configuration for game-specific networking
    relay_type_config: crate::config::RelayTypeConfig,
    /// Session topology/transport selection policy (protocol v3)
    session_config: crate::config::SessionConfig,
    /// TURN / STUN ICE-server policy (protocol v3); drives `SessionPlan.ice_servers`
    turn_config: crate::config::TurnConfig,
    /// Rate limiter for room operations
    rate_limiter: Arc<RoomRateLimiter>,
    /// Server metrics
    pub(crate) metrics: Arc<crate::metrics::ServerMetrics>,
    /// Process-local message coordinator behind a future remote-backend seam
    message_coordinator: Arc<dyn MessageCoordinator>,
    /// Process-local room operation coordinator
    room_coordinator: Arc<dyn RoomOperationCoordinatorTrait>,
    /// Process-local coordination lock
    distributed_lock: Arc<dyn DistributedLock>,
    /// Instance identifier
    instance_id: Uuid,
    /// Reconnection manager for player reconnection support
    reconnection_manager: Option<Arc<crate::reconnection::ReconnectionManager>>,
    /// Public app-ID allowlist and accounting-context resolver.
    pub(crate) app_id_allowlist: Arc<crate::auth::AppIdAllowlist>,
    /// Mapping from room IDs to owning application IDs (for relay policies)
    room_applications: Arc<DashMap<RoomId, Uuid>>,
    /// Sticky per-room session decision recorded at finalize (protocol v3):
    /// consulted by late-join/reconnect pairing and departure re-planning
    /// instead of re-running the selection ladder (see `session_policy.rs`).
    active_session_plans: Arc<DashMap<RoomId, session_policy::ActiveSessionPlan>>,
    /// Durable player removals that failed after the physical connection was
    /// already gone. Maintenance retries these independently of whether
    /// reconnect support is enabled.
    pending_durable_player_detaches:
        Arc<DashMap<(RoomId, PlayerId), Option<PendingApplicationClaimRollback>>>,
    #[cfg(test)]
    fail_retain_room_publication_snapshot: AtomicBool,
    #[cfg(test)]
    reconnect_teardown_test_gate: StdMutex<Option<Arc<ReconnectTeardownTestGate>>>,
    #[cfg(test)]
    scripted_room_codes: StdMutex<VecDeque<String>>,
    /// Spectator lifecycle manager
    spectator_service: SpectatorService,
    /// Transport-level security options (TLS, token binding, etc.)
    transport_security: crate::config::TransportSecurityConfig,
    /// Cached metrics used by the admin dashboard
    dashboard_metrics_cache: Arc<DashboardMetricsCache>,
    /// Nonzero once graceful shutdown drain has started; stores the advertised
    /// Unix epoch millisecond close deadline.
    shutdown_drain_deadline_ms: AtomicU64,
    /// Wakes drain-sensitive delivery paths so they can cancel backpressured
    /// normal traffic before it is enqueued after drain begins.
    shutdown_drain_tx: watch::Sender<bool>,
    /// Active real WebSocket handlers. During shutdown these stay registered
    /// until both socket halves have completed their bounded close path.
    active_socket_tasks: AtomicUsize,
    active_socket_tasks_notify: Notify,
}

#[derive(Clone, Debug)]
struct PendingApplicationClaimRollback {
    application_id: Uuid,
}

/// Test-only synchronization point for the narrow interval after a reconnect
/// record is armed but before the old connection is removed. Production has no
/// hook or branch at this boundary; `cfg(test)` keeps the deterministic H6 race
/// harness out of release builds.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct ReconnectTeardownTestGate {
    armed: Notify,
    release: Notify,
}

#[cfg(test)]
impl ReconnectTeardownTestGate {
    pub(crate) async fn wait_until_armed(&self) {
        self.armed.notified().await;
    }

    pub(crate) fn release(&self) {
        self.release.notify_one();
    }
}

#[derive(Debug, Error)]
pub enum RegisterClientError {
    #[error("Too many connections from your IP ({current}/{limit})")]
    IpLimitExceeded { current: usize, limit: usize },
    #[error("Server is draining for shutdown")]
    ServerDraining,
}

#[derive(Debug, Error)]
#[error("Game `{game_name}` already has {current} rooms (limit {limit})")]
pub struct MaxRoomsPerGameExceededError {
    pub game_name: String,
    pub current: usize,
    pub limit: usize,
}

#[derive(Debug, Error)]
#[error("Application already has {current} rooms (limit {limit})")]
pub struct MaxRoomsPerApplicationExceededError {
    pub current: usize,
    pub limit: usize,
}

#[derive(Debug, Error)]
#[error("Requested room capacity {requested} exceeds the application limit {limit}")]
pub struct MaxPlayersPerApplicationExceededError {
    pub requested: u8,
    pub limit: u8,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub default_max_players: u8,
    pub ping_timeout: Duration,
    pub room_cleanup_interval: Duration,
    pub drain_grace: Duration,
    pub max_rooms_per_game: usize,
    pub rate_limit_config: RateLimitConfig,
    pub empty_room_timeout: Duration,
    pub inactive_room_timeout: Duration,
    pub max_message_size: usize,
    /// Maximum serialized size in bytes of a v3 `Signal` payload (the opaque
    /// `signal` JSON value). Mirrors `security.max_signal_bytes`.
    pub max_signal_bytes: usize,
    pub max_connections_per_ip: usize,
    pub require_metrics_auth: bool,
    pub metrics_auth_token: Option<String>,
    pub reconnection_window: Duration,
    pub event_buffer_size: usize,
    pub enable_reconnection: bool,
    pub websocket_config: crate::config::WebSocketConfig,
    pub app_id_allowlist_enabled: bool,
    /// Threshold for heartbeat update throttling.
    /// Only update `last_seen` if this duration has passed since the last update.
    /// Set to Duration::ZERO to disable throttling (update on every heartbeat).
    pub heartbeat_throttle: Duration,
    /// Identifier for the deployment region (used in player info and room codes).
    pub region_id: String,
    /// Optional prefix prepended to generated room codes.
    pub room_code_prefix: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            default_max_players: 8,
            ping_timeout: Duration::from_secs(30),
            room_cleanup_interval: Duration::from_secs(60),
            drain_grace: Duration::from_secs(30),
            max_rooms_per_game: 1000,
            rate_limit_config: RateLimitConfig::default(),
            empty_room_timeout: Duration::from_secs(300),
            inactive_room_timeout: Duration::from_secs(3600),
            max_message_size: 65536, // 64KB
            max_signal_bytes: 16384, // 16KB
            max_connections_per_ip: 24,
            require_metrics_auth: true,
            metrics_auth_token: None,
            reconnection_window: Duration::from_secs(300), // 5 minutes
            event_buffer_size: 100,
            enable_reconnection: true,
            websocket_config: crate::config::WebSocketConfig::default(),
            app_id_allowlist_enabled: false, // Disabled by default for backward compatibility
            heartbeat_throttle: Duration::from_secs(30), // 30 second update throttle by default
            region_id: "default".to_string(),
            room_code_prefix: None,
        }
    }
}

impl EnhancedGameServer {
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    pub async fn new(
        config: ServerConfig,
        protocol_config: crate::config::ProtocolConfig,
        relay_type_config: crate::config::RelayTypeConfig,
        session_config: crate::config::SessionConfig,
        turn_config: crate::config::TurnConfig,
        database_config: DatabaseConfig,
        metrics_config: crate::config::MetricsConfig,
        _coordination_config: crate::config::CoordinationConfig,
        transport_security: crate::config::TransportSecurityConfig,
        allowed_apps: Vec<AppRegistrationEntry>,
    ) -> anyhow::Result<Arc<Self>> {
        // Library embedders can construct `ServerConfig` directly without the
        // top-level config loader. Enforce the same generation/join closure
        // here before initializing storage or background tasks.
        protocol_config.validate_room_code_generation(config.room_code_prefix.as_deref())?;

        let database: Arc<dyn GameDatabase> =
            Arc::from(create_database(database_config.clone()).await?);
        database.initialize().await?;

        let instance_id = Uuid::new_v4();

        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let rate_limiter = Arc::new(RoomRateLimiter::with_metrics(
            config.rate_limit_config.clone(),
            metrics.clone(),
        ));
        let _rate_limit_cleanup = rate_limiter.clone().start_cleanup_task()?;

        let cache_refresh_interval =
            Duration::from_secs(metrics_config.dashboard_cache_refresh_interval_secs.max(1));
        let cache_ttl = Duration::from_secs(metrics_config.dashboard_cache_ttl_secs.max(1));
        let history_capacity = DashboardMetricsCache::history_capacity_for_window(
            cache_refresh_interval,
            metrics_config.dashboard_cache_history_window_secs.max(1),
        );
        let dashboard_metrics_cache = Arc::new(DashboardMetricsCache::new(
            cache_refresh_interval,
            cache_ttl,
            metrics.clone(),
            history_capacity,
            &metrics_config.dashboard_cache_history_fields,
        ));
        dashboard_metrics_cache.spawn(database.clone());

        // Set up process-local coordination behind the extension interfaces.
        let distributed_lock = Arc::new(InMemoryDistributedLock::new());
        let message_coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_millis(config.websocket_config.slow_consumer_timeout_ms),
            metrics.clone(),
        ));

        let connection_manager = Arc::new(ConnectionManager::new(
            config.max_connections_per_ip,
            metrics.clone(),
            message_coordinator.clone(),
            // Per-connection delivery ledgers exist only when RelayStats
            // emission is enabled, keeping the delivery hot path at a single
            // cheap registry miss otherwise.
            config.websocket_config.delivery_stats_interval_secs > 0,
        ));

        // Initialize reconnection manager if enabled (in-memory only). Built
        // before the room coordinator and spectator service so both can record
        // their room-uniform broadcasts for reconnection replay.
        let reconnection_manager = if config.enable_reconnection {
            Some(Arc::new(crate::reconnection::ReconnectionManager::new(
                config.reconnection_window.as_secs(),
                config.event_buffer_size,
                metrics.clone(),
            )))
        } else {
            None
        };

        let room_coordinator: Arc<dyn RoomOperationCoordinatorTrait> =
            Arc::new(InMemoryRoomOperationCoordinator::new(
                message_coordinator.clone(),
                distributed_lock.clone(),
                database.clone(),
                reconnection_manager.clone(),
            ));

        // Initialize public app-ID access policy based on configuration.
        let app_id_allowlist = if config.app_id_allowlist_enabled {
            if allowed_apps.is_empty() {
                tracing::warn!(
                    "App-ID allowlist enforcement is enabled but no allowed_apps are configured; \
                     every app-ID handshake will be rejected"
                );
            } else {
                tracing::info!(
                    app_count = allowed_apps.len(),
                    "App-ID allowlist enabled with configured applications"
                );
            }
            Arc::new(crate::auth::AppIdAllowlist::with_metrics(
                allowed_apps,
                metrics.clone(),
            )?)
        } else {
            Arc::new(crate::auth::AppIdAllowlist::disabled())
        };

        let room_applications = Arc::new(DashMap::new());
        let spectator_service = SpectatorService::new(
            database.clone(),
            Arc::clone(&room_coordinator),
            message_coordinator.clone(),
            room_applications.clone(),
            protocol_config.clone(),
            reconnection_manager.clone(),
            Arc::clone(&connection_manager),
            config.app_id_allowlist_enabled,
        );

        let (shutdown_drain_tx, _) = watch::channel(false);
        let server = Arc::new(Self {
            database,
            connection_manager,
            config,
            protocol_config,
            relay_type_config,
            session_config,
            turn_config,
            rate_limiter,
            metrics,
            message_coordinator,
            room_coordinator,
            distributed_lock,
            instance_id,
            reconnection_manager,
            app_id_allowlist,
            room_applications,
            active_session_plans: Arc::new(DashMap::new()),
            pending_durable_player_detaches: Arc::new(DashMap::new()),
            #[cfg(test)]
            fail_retain_room_publication_snapshot: AtomicBool::new(false),
            #[cfg(test)]
            reconnect_teardown_test_gate: StdMutex::new(None),
            #[cfg(test)]
            scripted_room_codes: StdMutex::new(VecDeque::new()),
            spectator_service,
            transport_security,
            dashboard_metrics_cache: dashboard_metrics_cache.clone(),
            shutdown_drain_deadline_ms: AtomicU64::new(0),
            shutdown_drain_tx,
            active_socket_tasks: AtomicUsize::new(0),
            active_socket_tasks_notify: Notify::new(),
        });

        Ok(server)
    }

    pub async fn dashboard_metrics_view(&self) -> DashboardMetricsView {
        self.dashboard_metrics_cache.view().await
    }

    /// Identifier for the current deployment region.
    pub fn region_id(&self) -> &str {
        &self.config.region_id
    }

    /// Optional room-code prefix configured for this deployment.
    pub fn room_code_prefix(&self) -> Option<&str> {
        self.config.room_code_prefix.as_deref()
    }

    fn generate_region_room_code(&self) -> String {
        #[cfg(test)]
        if let Some(code) = self
            .scripted_room_codes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
        {
            return code;
        }

        room_codes::generate_region_room_code(
            &self.protocol_config,
            self.config.room_code_prefix.as_deref(),
        )
    }

    #[cfg(test)]
    fn script_room_codes_for_test(&self, codes: impl IntoIterator<Item = &'static str>) {
        self.scripted_room_codes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(codes.into_iter().map(str::to_string));
    }

    /// Register a new client connection.
    ///
    /// The connection is registered with a detached close signal: delivery
    /// failures still prune it from routing, but there are no socket tasks to
    /// tear down. The WebSocket layer uses
    /// [`register_client_with_close`](Self::register_client_with_close) so
    /// slow-consumer disconnects actually close the socket.
    pub async fn register_client(
        &self,
        sender: mpsc::Sender<Arc<ServerMessage>>,
        client_addr: SocketAddr,
    ) -> Result<PlayerId, RegisterClientError> {
        self.register_client_with_close(sender, ConnectionCloseSignal::detached(), client_addr)
            .await
    }

    /// Register a new client connection along with the close signal that lets
    /// the delivery layer terminate it (slow consumer, server-side eviction).
    pub async fn register_client_with_close(
        &self,
        sender: mpsc::Sender<Arc<ServerMessage>>,
        close: ConnectionCloseSignal,
        client_addr: SocketAddr,
    ) -> Result<PlayerId, RegisterClientError> {
        if self.is_draining() {
            return Err(RegisterClientError::ServerDraining);
        }
        let player_id = self
            .connection_manager
            .register_client(sender, close, client_addr, self.instance_id)
            .await?;
        if self.is_draining() {
            self.connection_manager
                .request_close_for(&player_id, CloseReason::Shutdown);
        }
        Ok(player_id)
    }

    pub(crate) async fn register_classified_client_with_close(
        &self,
        sender: crate::coordination::outbound_queue::OutboundSender,
        close: ConnectionCloseSignal,
        client_addr: SocketAddr,
    ) -> Result<PlayerId, RegisterClientError> {
        if self.is_draining() {
            return Err(RegisterClientError::ServerDraining);
        }
        let player_id = self
            .connection_manager
            .register_classified_client(
                DeliverySender::classified(sender),
                close,
                client_addr,
                self.instance_id,
            )
            .await?;
        if self.is_draining() {
            self.connection_manager
                .request_close_for(&player_id, CloseReason::Shutdown);
        }
        Ok(player_id)
    }

    /// Record inbound activity for a client so the activity reaper
    /// (`server.ping_timeout`) treats it as alive. Called for every routed
    /// client message — not just `Ping` — because a client streaming game
    /// data at a high rate without heartbeats is emphatically alive.
    pub(crate) fn record_client_activity(&self, player_id: &PlayerId) {
        self.connection_manager.record_ping(player_id);
    }

    /// Update a client's preferred game data encoding.
    pub fn set_client_game_data_format(&self, player_id: &PlayerId, format: GameDataEncoding) {
        self.connection_manager
            .set_game_data_format(player_id, format);
    }

    /// Fetch the negotiated game data encoding for a client.
    pub fn client_game_data_format(&self, player_id: &PlayerId) -> GameDataEncoding {
        self.connection_manager.game_data_format(player_id)
    }

    /// Persist the negotiated protocol version + transport/topology capabilities
    /// for a client (mirrors [`set_client_game_data_format`](Self::set_client_game_data_format)).
    pub(crate) fn set_client_protocol(&self, player_id: &PlayerId, protocol: NegotiatedProtocol) {
        self.connection_manager.set_protocol(player_id, protocol);
    }

    /// Fetch the negotiated protocol capabilities for a client (defaults to v2 relay-only).
    ///
    /// Built in P1; consumed by the P3 session-plan/topology selection path
    /// (`session_policy::EnhancedGameServer::emit_session_plan`).
    pub(crate) fn client_protocol(&self, player_id: &PlayerId) -> NegotiatedProtocol {
        self.connection_manager.protocol(player_id)
    }

    /// Persist the client's last-reported data-path transport state (mirrors
    /// [`set_client_protocol`](Self::set_client_protocol)). Driven by the v3-only
    /// [`ClientMessage::TransportStatus`](crate::protocol::ClientMessage::TransportStatus).
    pub(crate) fn set_client_transport_status(
        &self,
        player_id: &PlayerId,
        transport: Transport,
        connected: bool,
    ) -> TransportStatusUpdate {
        self.connection_manager
            .set_transport_status(player_id, transport, connected)
    }

    /// Fetch the client's last-reported data-path transport state, or `None` if it
    /// has not reported one (the relay floor is the implicit default). Mirrors
    /// [`client_protocol`](Self::client_protocol). Consumed by tests and the future
    /// targeted-relay path; not yet read in production.
    #[allow(dead_code)]
    pub(crate) fn client_transport_status(
        &self,
        player_id: &PlayerId,
    ) -> Option<(Transport, bool)> {
        self.connection_manager.transport_status(player_id)
    }

    /// Whether the client negotiated protocol v3+ (the single unshipped
    /// "current" version). Gates ALL additive emission over the frozen v2 floor:
    /// the WebRTC signaling surface AND the delivery reliability surface
    /// (`GameData.seq`/`epoch` stamps + `RelayStats`).
    pub fn client_supports_v3(&self, player_id: &PlayerId) -> bool {
        self.connection_manager.supports_v3(player_id)
    }

    /// Whether the client negotiated support for the given transport.
    pub fn client_supports_transport(&self, player_id: &PlayerId, transport: Transport) -> bool {
        self.connection_manager
            .supports_transport(player_id, transport)
    }

    /// Attach the accepted public app-ID context to a connected client.
    pub fn set_client_app_context(&self, player_id: &PlayerId, app_context: AppContext) {
        self.connection_manager
            .set_app_context(player_id, app_context);
    }

    /// Fetch the public app-ID context for a connected client, if known.
    pub fn client_app_context(&self, player_id: &PlayerId) -> Option<AppContext> {
        self.connection_manager.app_context(player_id)
    }

    /// Fetch just the application UUID for a connected client.
    pub fn client_app_id(&self, player_id: &PlayerId) -> Option<Uuid> {
        self.connection_manager.app_id(player_id)
    }

    /// Persist a room -> application mapping before publishing it in the
    /// process-local relay cache. Persistence is the authorization authority;
    /// callers must fail closed rather than accepting a cache-only owner.
    pub async fn record_room_application(&self, room_id: &RoomId, app_id: Uuid) -> Result<()> {
        self.database
            .set_room_application_id(room_id, app_id)
            .await?;
        self.cache_room_application(room_id, app_id);
        Ok(())
    }

    fn cache_room_application(&self, room_id: &RoomId, app_id: Uuid) {
        self.room_applications.insert(*room_id, app_id);
    }

    /// Lookup the owning application for a room, if any.
    pub fn room_application_id(&self, room_id: &RoomId) -> Option<Uuid> {
        self.room_applications
            .get(room_id)
            .map(|entry| *entry.value())
    }

    /// Remove the process-local room -> application cache after storage has
    /// already confirmed the room is deleted.
    pub fn clear_room_application(&self, room_id: &RoomId) {
        self.room_applications.remove(room_id);
    }

    /// Determine whether the client expects a binary payload for the given encoding.
    pub fn prefers_encoding(&self, player_id: &PlayerId, encoding: GameDataEncoding) -> bool {
        self.connection_manager
            .prefers_encoding(player_id, encoding)
    }

    /// Connect a client with a specific player ID (used for testing)
    pub async fn connect_client(
        &self,
        player_id: PlayerId,
        sender: mpsc::Sender<Arc<ServerMessage>>,
    ) {
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));
        self.connection_manager
            .connect_test_client(player_id, sender, addr)
            .await;
        tracing::info!(%player_id, instance_id = %self.instance_id, "Client connected");
    }

    /// Assign a connected client to a room (used by integration tests that hydrate server state).
    pub async fn assign_client_to_room(&self, player_id: &PlayerId, room_id: RoomId) {
        let Some(lifecycle) = self.connection_manager.client_lifecycle(player_id) else {
            return;
        };
        let _lifecycle_guard = lifecycle.lock().await;
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return;
        }
        self.connection_manager
            .assign_client_to_room(player_id, room_id)
            .await;
    }

    /// Disconnect a client (alias for unregister_client for testing compatibility)
    pub async fn disconnect_client(self: &Arc<Self>, player_id: &PlayerId) {
        self.unregister_client(player_id).await;
    }

    /// Unregister a client connection
    pub async fn unregister_client(self: &Arc<Self>, player_id: &PlayerId) {
        if let Some(lifecycle) = self.connection_manager.client_lifecycle(player_id) {
            self.unregister_client_with_lifecycle(lifecycle).await;
        } else {
            let server = Arc::clone(self);
            let player_id = *player_id;
            let task = tokio::spawn(async move {
                server.unregister_client_locked(&player_id).await;
            });
            if let Err(error) = task.await {
                tracing::error!(%player_id, %error, "Owned client unregister transaction failed");
            }
        }
    }

    pub(crate) fn client_lifecycle(&self, player_id: &PlayerId) -> Option<Arc<ClientLifecycle>> {
        self.connection_manager.client_lifecycle(player_id)
    }

    pub(crate) async fn unregister_client_with_lifecycle(
        self: &Arc<Self>,
        lifecycle: Arc<ClientLifecycle>,
    ) {
        let server = Arc::clone(self);
        let task = tokio::spawn(async move {
            let _lifecycle_guard = Arc::clone(&lifecycle).lock_owned().await;
            let player_id = lifecycle.player_id();
            if !server
                .connection_manager
                .lifecycle_matches(&player_id, &lifecycle)
            {
                return;
            }
            server.unregister_client_locked(&player_id).await;
        });
        if let Err(error) = task.await {
            tracing::error!(%error, "Owned client unregister transaction failed");
        }
    }

    async fn unregister_client_locked(&self, player_id: &PlayerId) {
        let room_id_opt = self.get_client_room(player_id).await;
        // One authoritative snapshot supplies every reconnect-restoration
        // field. A missing player/room or storage error must not degrade into a
        // pending record with `was_authority = false` and no player metadata.
        let reconnect_snapshot = if let Some(room_id) = room_id_opt {
            match self.database.get_room_by_id(&room_id).await {
                Ok(Some(room)) => match room.players.get(player_id).cloned() {
                    Some(player_info) => Some((
                        room_id,
                        room.authority_player == Some(*player_id),
                        player_info,
                    )),
                    None => {
                        tracing::warn!(%player_id, %room_id, "Assigned player was absent from the reconnect room snapshot");
                        None
                    }
                },
                Ok(None) => {
                    tracing::warn!(%player_id, %room_id, "Assigned reconnect room was absent from storage");
                    None
                }
                Err(error) => {
                    tracing::warn!(%player_id, %room_id, %error, "Failed to capture reconnect room snapshot");
                    None
                }
            }
        } else {
            None
        };

        // Clean up spectator state (if this client was observing a room)
        let should_send_spectator_detach = || !self.is_draining();
        let _ = self
            .spectator_service
            .detach_if(
                player_id,
                SpectatorStateChangeReason::Disconnected,
                &should_send_spectator_detach,
                self.shutdown_drain_receiver(),
            )
            .await;

        let mut registered_reconnect = false;

        // Register disconnection for potential reconnection (before removing from room)
        if self.is_draining() {
            self.connection_manager
                .request_close_for(player_id, CloseReason::Shutdown);
            self.discard_pre_issued_reconnection_token(player_id).await;
        } else if let Some((room_id, was_authority, player_info)) = reconnect_snapshot {
            self.register_disconnection_for_reconnect(
                player_id,
                room_id,
                was_authority,
                player_info,
            )
            .await;
            registered_reconnect = true;
        } else if room_id_opt.is_none() {
            // No room to reconnect into: any token pre-issued at an earlier
            // join must not outlive the connection (bounded-map contract).
            self.discard_pre_issued_reconnection_token(player_id).await;
        }

        #[cfg(test)]
        if registered_reconnect {
            self.pause_after_reconnect_registration_for_test().await;
        }

        if self.is_draining() {
            self.connection_manager
                .request_close_for(player_id, CloseReason::Shutdown);
            // Stop routing before quiet room cleanup so shutdown teardown cannot
            // enqueue normal room-leave traffic ahead of the semantic close.
            if let Err(e) = self
                .message_coordinator
                .unregister_local_client(player_id)
                .await
            {
                tracing::warn!(%player_id, "Failed to unregister client from coordinator: {}", e);
            }

            if let Some(room_id) = room_id_opt {
                tracing::info!(%player_id, %room_id, draining = true, "Removing player from room during unregister");
                self.remove_player_for_shutdown_drain(player_id, &room_id)
                    .await;
            }
        } else {
            // Remove from room if joined
            if let Some(room_id) = room_id_opt {
                tracing::info!(%player_id, %room_id, draining = false, "Removing player from room during unregister");
                self.leave_room_locked(player_id, false).await;
                // Note: We previously had a sleep here, but it's been removed to eliminate sleeps from production code
                // Tests should properly handle the asynchronous nature of message delivery
            }

            if self.is_draining() {
                self.connection_manager
                    .request_close_for(player_id, CloseReason::Shutdown);
                if registered_reconnect {
                    self.discard_pending_reconnection_for_shutdown_drain(player_id)
                        .await;
                }
                // Stop routing before any remaining quiet room cleanup.
                if let Err(e) = self
                    .message_coordinator
                    .unregister_local_client(player_id)
                    .await
                {
                    tracing::warn!(%player_id, "Failed to unregister client from coordinator: {}", e);
                }
                if let Some(room_id) = self.get_client_room(player_id).await {
                    tracing::info!(%player_id, %room_id, draining = true, "Removing player from room during unregister");
                    self.remove_player_for_shutdown_drain(player_id, &room_id)
                        .await;
                }
            } else {
                // Unregister from the message coordinator BEFORE tearing the socket
                // down: once routing stops, no new message can be enqueued into a
                // connection whose send task is already flushing its final drain.
                if let Err(e) = self
                    .message_coordinator
                    .unregister_local_client(player_id)
                    .await
                {
                    tracing::warn!(%player_id, "Failed to unregister client from coordinator: {}", e);
                }
            }
        }

        if self.is_draining() {
            self.connection_manager
                .request_close_for(player_id, CloseReason::Shutdown);
            if registered_reconnect {
                self.discard_pending_reconnection_for_shutdown_drain(player_id)
                    .await;
            }
        }

        // Remove client connection (also requests the socket tasks to close).
        let removed = self
            .connection_manager
            .remove_client_for_unregistration(player_id, || self.is_draining());
        if let Some((_connection, close_reason)) = removed {
            if close_reason == CloseReason::Shutdown && registered_reconnect {
                self.discard_pending_reconnection_for_shutdown_drain(player_id)
                    .await;
            }
            self.metrics.decrement_active_connections();
        }

        tracing::info!(%player_id, instance_id = %self.instance_id, "Client unregistered");
    }

    pub async fn get_client_room(&self, player_id: &PlayerId) -> Option<RoomId> {
        self.connection_manager.get_client_room(player_id)
    }

    pub fn database(&self) -> &dyn GameDatabase {
        self.database.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn fail_retain_room_publication_snapshot_for_test(&self, fail: bool) {
        self.fail_retain_room_publication_snapshot
            .store(fail, std::sync::atomic::Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn install_reconnect_teardown_test_gate(&self) -> Arc<ReconnectTeardownTestGate> {
        let gate = Arc::new(ReconnectTeardownTestGate::default());
        *self
            .reconnect_teardown_test_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&gate));
        gate
    }

    #[cfg(test)]
    async fn pause_after_reconnect_registration_for_test(&self) {
        let gate = self
            .reconnect_teardown_test_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(gate) = gate {
            gate.armed.notify_one();
            gate.release.notified().await;
        }
    }

    pub(crate) fn should_retain_room_publication_snapshot(&self) -> bool {
        #[cfg(test)]
        {
            !self
                .fail_retain_room_publication_snapshot
                .load(std::sync::atomic::Ordering::Acquire)
        }
        #[cfg(not(test))]
        {
            true
        }
    }

    pub fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    /// Get server configuration
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Access zero-trust / token binding settings.
    pub fn token_binding_config(&self) -> &crate::config::TokenBindingConfig {
        &self.transport_security.token_binding
    }

    pub fn protocol_config(&self) -> &crate::config::ProtocolConfig {
        &self.protocol_config
    }

    /// Get server metrics
    pub fn metrics(&self) -> Arc<crate::metrics::ServerMetrics> {
        self.metrics.clone()
    }

    /// Access the reconnection manager for integration tests or admin tooling.
    pub fn reconnection_manager(&self) -> Option<Arc<crate::reconnection::ReconnectionManager>> {
        self.reconnection_manager.clone()
    }
}

/// In-memory message coordinator: the production (single-instance) delivery
/// layer that routes server messages onto per-connection outbound queues.
///
/// Delivery contract: **a message is never silently dropped.** Each delivery
/// either enqueues the message (waiting — backpressure — for queue space when
/// the recipient is momentarily full) or, if the recipient cannot absorb a
/// single message for the whole `slow_consumer_timeout`, disconnects that
/// recipient loudly (metrics + log + close signal). Senders in the same room
/// are therefore paced to their slowest healthy recipient instead of having
/// messages vanish, and an unhealthy recipient costs the room at most one
/// timeout window before being evicted.
pub struct InMemoryMessageCoordinator {
    local_clients: Arc<RwLock<HashMap<PlayerId, ClientDeliveryHandle>>>,
    room_players: Arc<RwLock<HashMap<RoomId, HashSet<PlayerId>>>>,
    room_routing_gates: RoutingGateRegistry,
    player_routing_gates: RoutingGateRegistry,
    metrics: Arc<crate::metrics::ServerMetrics>,
    slow_consumer_timeout: Duration,
    room_event_sequencer: Arc<RoomEventSequencer>,
    #[cfg(test)]
    fail_room_transactions: AtomicBool,
    #[allow(dead_code)]
    instance_id: Uuid,
}

/// Stable keyed routing fence used to contain recipient-map exclusion to one
/// room (or one player identity) while an exact publication awaits.
///
/// The directory stores weak entries so inactive rooms and players do not
/// accumulate. Owned guards retain the gate identity until they release the
/// lock; pointer-checked cleanup prevents a stale destructor from deleting a
/// replacement installed by a concurrent acquisition.
#[derive(Clone, Default)]
struct RoutingGateRegistry {
    inner: Arc<RoutingGateRegistryInner>,
}

#[derive(Default)]
struct RoutingGateRegistryInner {
    gates: StdMutex<HashMap<Uuid, std::sync::Weak<RoutingGate>>>,
    active: StdMutex<HashMap<Uuid, Arc<RoutingGate>>>,
}

struct RoutingGate {
    key: Uuid,
    owner: std::sync::Weak<RoutingGateRegistryInner>,
    lock: Arc<RwLock<()>>,
}

struct RoutingReadGuard {
    _guard: OwnedRwLockReadGuard<()>,
    _gate: Arc<RoutingGate>,
}

struct RoutingWriteGuard {
    _guard: OwnedRwLockWriteGuard<()>,
    _gate: Arc<RoutingGate>,
}

struct PlayerRoutingWriteGuards {
    _player: RoutingWriteGuard,
    _rooms: Vec<RoutingWriteGuard>,
}

impl RoutingGateRegistry {
    fn gate(&self, key: Uuid) -> Arc<RoutingGate> {
        let mut gates = self
            .inner
            .gates
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(gate) = gates.get(&key).and_then(std::sync::Weak::upgrade) {
            return gate;
        }

        let gate = Arc::new(RoutingGate {
            key,
            owner: Arc::downgrade(&self.inner),
            lock: Arc::new(RwLock::new(())),
        });
        gates.insert(key, Arc::downgrade(&gate));
        gate
    }

    async fn read(&self, key: Uuid) -> RoutingReadGuard {
        let gate = self.gate(key);
        let guard = Arc::clone(&gate.lock).read_owned().await;
        RoutingReadGuard {
            _guard: guard,
            _gate: gate,
        }
    }

    fn try_read(&self, key: Uuid) -> Option<RoutingReadGuard> {
        let gate = self.gate(key);
        let guard = Arc::clone(&gate.lock).try_read_owned().ok()?;
        Some(RoutingReadGuard {
            _guard: guard,
            _gate: gate,
        })
    }

    async fn write(&self, key: Uuid) -> RoutingWriteGuard {
        let gate = self.gate(key);
        let guard = Arc::clone(&gate.lock).write_owned().await;
        RoutingWriteGuard {
            _guard: guard,
            _gate: gate,
        }
    }

    async fn write_many(&self, keys: &[Uuid]) -> Vec<RoutingWriteGuard> {
        let mut guards = Vec::with_capacity(keys.len());
        for key in keys {
            guards.push(self.write(*key).await);
        }
        guards
    }

    fn mark_active(&self, gate: &Arc<RoutingGate>) {
        self.inner
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(gate.key, Arc::clone(gate));
    }

    fn mark_inactive(&self, key: Uuid) {
        self.inner
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&key);
    }
}

impl Drop for RoutingGate {
    fn drop(&mut self) {
        let Some(owner) = self.owner.upgrade() else {
            return;
        };
        let mut gates = owner
            .gates
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if gates
            .get(&self.key)
            .is_some_and(|gate| std::ptr::eq(gate.as_ptr(), self))
        {
            gates.remove(&self.key);
        }
    }
}

enum ConditionalDeliveryReservation {
    Reserved {
        player_id: PlayerId,
        sender: DeliverySender,
        permit: DeliveryPermit,
        stats: Option<Arc<ConnectionDeliveryStats>>,
    },
    ChannelClosed {
        player_id: PlayerId,
        sender: DeliverySender,
    },
    SlowConsumer {
        player_id: PlayerId,
        sender: DeliverySender,
    },
    Canceled,
}

enum RoomBatchReservation {
    Reserved {
        player_id: PlayerId,
        sender: DeliverySender,
        permits: Vec<Option<DeliveryPermit>>,
        stats: Option<Arc<ConnectionDeliveryStats>>,
    },
    ChannelClosed {
        player_id: PlayerId,
        sender: DeliverySender,
    },
    SlowConsumer {
        player_id: PlayerId,
        sender: DeliverySender,
    },
    Canceled,
}

#[derive(Default)]
struct StartedDeliveries {
    // Healthy queues leave both vectors empty. Only exceptional outcomes own
    // state past the routing-guarded synchronous start phase.
    pending: Vec<crate::coordination::BackpressuredDelivery>,
    slow_consumers: Vec<(PlayerId, DeliverySender)>,
}

fn room_message_for_recipient(
    message: &Arc<ServerMessage>,
    player_id: &PlayerId,
) -> Arc<ServerMessage> {
    match message.as_ref() {
        ServerMessage::AuthorityChanged {
            authority_player, ..
        } => Arc::new(ServerMessage::AuthorityChanged {
            authority_player: *authority_player,
            you_are_authority: *authority_player == Some(*player_id),
        }),
        _ => Arc::clone(message),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelayProjectionCohort {
    TextV2,
    TextV3,
    BinaryDirectV2,
    BinaryDirectV3,
    BinaryFallbackV2,
    BinaryFallbackV3,
}

impl RelayProjectionCohort {
    const fn cache_bit(self) -> u8 {
        1 << self as u8
    }

    const fn is_binary_fallback(self) -> bool {
        matches!(self, Self::BinaryFallbackV2 | Self::BinaryFallbackV3)
    }
}

fn relay_projection_cohort(
    message: &ServerMessage,
    supports_v3: bool,
    recipient_format: GameDataEncoding,
) -> Option<RelayProjectionCohort> {
    Some(match message {
        ServerMessage::GameData { .. } if supports_v3 => RelayProjectionCohort::TextV3,
        ServerMessage::GameData { .. } => RelayProjectionCohort::TextV2,
        // Frozen-v2 JSON and Rkyv binary frames already own shared `Bytes` and
        // project by cloning that handle. There is no serialization/decode
        // work to reuse, so a relay-wide frame cache would only add an
        // allocation in rooms with multiple compatible recipients.
        ServerMessage::GameDataBinary { encoding, .. }
            if !supports_v3
                && *encoding == recipient_format
                && matches!(encoding, GameDataEncoding::Json | GameDataEncoding::Rkyv) =>
        {
            return None;
        }
        ServerMessage::GameDataBinary { encoding, .. }
            if *encoding == recipient_format && supports_v3 =>
        {
            RelayProjectionCohort::BinaryDirectV3
        }
        ServerMessage::GameDataBinary { encoding, .. } if *encoding == recipient_format => {
            RelayProjectionCohort::BinaryDirectV2
        }
        ServerMessage::GameDataBinary { .. } if supports_v3 => {
            RelayProjectionCohort::BinaryFallbackV3
        }
        ServerMessage::GameDataBinary { .. } => RelayProjectionCohort::BinaryFallbackV2,
        _ => return None,
    })
}

fn relay_projection_summary(
    recipients: impl IntoIterator<Item = (bool, Option<RelayProjectionCohort>)>,
) -> (bool, bool) {
    let mut seen = 0u8;
    let mut saw_binary_fallback = false;
    let mut repeats = false;
    let mut all_recipients_are_classified = true;
    for (classified, cohort) in recipients {
        all_recipients_are_classified &= classified;
        let Some(cohort) = cohort else {
            continue;
        };
        let bit = cohort.cache_bit();
        repeats |= seen & bit != 0 || (cohort.is_binary_fallback() && saw_binary_fallback);
        seen |= bit;
        saw_binary_fallback |= cohort.is_binary_fallback();
    }
    (repeats, all_recipients_are_classified)
}

fn relay_projection_work_repeats(cohorts: impl IntoIterator<Item = RelayProjectionCohort>) -> bool {
    relay_projection_summary(cohorts.into_iter().map(|cohort| (true, Some(cohort)))).0
}

impl InMemoryMessageCoordinator {
    /// Create a coordinator with default delivery policy and private metrics.
    ///
    /// Production wiring uses [`Self::with_delivery_policy`] so backpressure
    /// events surface in the server-wide metrics; this constructor remains for
    /// tests and embedders that only need routing behavior.
    pub fn new() -> Self {
        Self::with_delivery_policy(
            Duration::from_millis(crate::config::defaults::default_slow_consumer_timeout_ms()),
            Arc::new(crate::metrics::ServerMetrics::new()),
        )
    }

    /// Create a coordinator with an explicit slow-consumer timeout and shared
    /// metrics sink.
    pub fn with_delivery_policy(
        slow_consumer_timeout: Duration,
        metrics: Arc<crate::metrics::ServerMetrics>,
    ) -> Self {
        Self {
            local_clients: Arc::new(RwLock::new(HashMap::new())),
            room_players: Arc::new(RwLock::new(HashMap::new())),
            room_routing_gates: RoutingGateRegistry::default(),
            player_routing_gates: RoutingGateRegistry::default(),
            metrics,
            slow_consumer_timeout,
            room_event_sequencer: Arc::new(RoomEventSequencer::default()),
            #[cfg(test)]
            fail_room_transactions: AtomicBool::new(false),
            instance_id: Uuid::new_v4(),
        }
    }

    /// Deliver a message to many recipients concurrently, then prune every
    /// recipient flagged as a slow consumer so subsequent deliveries stop
    /// waiting on a connection that is already being torn down.
    ///
    /// Concurrency here bounds a broadcast's latency to the *slowest single
    /// recipient* rather than the sum over recipients, while per-recipient
    /// ordering is preserved because each caller awaits the whole broadcast
    /// before issuing its next message.
    async fn deliver_to_all(
        &self,
        recipients: Vec<(PlayerId, ClientDeliveryHandle)>,
        message: Arc<ServerMessage>,
        room_id: Option<RoomId>,
    ) {
        if recipients.is_empty() {
            return;
        }

        let shared_relay = relay_projection_work_repeats(
            recipients
                .iter()
                .filter_map(|(_, handle)| handle.sender.relay_projection())
                .filter_map(|(supports_v3, format)| {
                    relay_projection_cohort(&message, supports_v3, format)
                }),
        )
        .then(|| {
            crate::coordination::outbound_queue::DeliveryMessage::shared_relay(Arc::clone(&message))
        });
        let started = self.start_deliveries(
            recipients
                .iter()
                .map(|(player_id, handle)| (*player_id, handle)),
            room_id,
            |player_id| {
                shared_relay.clone().unwrap_or_else(|| {
                    crate::coordination::outbound_queue::DeliveryMessage::new(
                        room_message_for_recipient(&message, player_id),
                    )
                })
            },
        );
        // `start_deliveries` clones only the handles whose full queues must
        // survive into the async wait. Release the owned routing snapshot now
        // so one backpressured recipient cannot prolong every unrelated
        // healthy recipient's connection lifetime.
        drop(recipients);
        self.finish_deliveries(started).await;
    }

    fn start_deliveries<'a>(
        &self,
        recipients: impl IntoIterator<Item = (PlayerId, &'a ClientDeliveryHandle)>,
        room_id: Option<RoomId>,
        mut delivery_for_player: impl FnMut(
            &PlayerId,
        )
            -> crate::coordination::outbound_queue::DeliveryMessage,
    ) -> StartedDeliveries {
        // The negotiated queue policy normally resolves through `try_send`.
        // Start every recipient synchronously and build async machinery only
        // for queues that are actually full. Waiting recipients still enter
        // one join_all below, so waits remain concurrent and each grace
        // deadline begins when that queue first reports `Full`.
        let mut started = StartedDeliveries::default();
        for (player_id, handle) in recipients {
            let delivery = delivery_for_player(&player_id);
            match crate::coordination::start_message_delivery_in_room(
                &self.metrics,
                self.slow_consumer_timeout,
                &player_id,
                handle,
                delivery,
                room_id,
            ) {
                crate::coordination::DeliveryStart::Complete(outcome) => {
                    if outcome == DeliveryOutcome::SlowConsumer {
                        started
                            .slow_consumers
                            .push((player_id, handle.sender.clone()));
                    }
                }
                crate::coordination::DeliveryStart::Backpressured(delivery) => {
                    started.pending.push(delivery);
                }
            }
        }
        started
    }

    async fn finish_deliveries(&self, mut started: StartedDeliveries) {
        if !started.pending.is_empty() {
            let outcomes =
                futures_util::future::join_all(started.pending.into_iter().map(|delivery| {
                    crate::coordination::finish_backpressured_delivery_in_room(
                        &self.metrics,
                        delivery,
                    )
                }))
                .await;
            started.slow_consumers.extend(
                outcomes
                    .into_iter()
                    .filter(|(_, _, outcome)| *outcome == DeliveryOutcome::SlowConsumer)
                    .map(|(player_id, sender, _)| (player_id, sender)),
            );
        }

        if !started.slow_consumers.is_empty() {
            // Remove immediately so senders stop paying the timeout for a
            // connection that is already closing; the connection's own
            // unregister flow performs the full cleanup (room membership,
            // reconnection window, peer notifications).
            for (player_id, attempted_sender) in &started.slow_consumers {
                self.remove_client_if_same_sender(*player_id, attempted_sender)
                    .await;
            }
        }
    }

    /// Start one exact routing snapshot without copying its recipients.
    ///
    /// The caller holds the room routing gate plus both routing-map read
    /// guards throughout this synchronous function. Any capacity wait is
    /// returned as owned state and must be awaited only after those guards are
    /// dropped.
    fn start_routed_deliveries(
        &self,
        room_players: &HashMap<RoomId, HashSet<PlayerId>>,
        clients: &HashMap<PlayerId, ClientDeliveryHandle>,
        room_id: &RoomId,
        except_player: Option<&PlayerId>,
        message: &Arc<ServerMessage>,
    ) -> StartedDeliveries {
        let Some(players) = room_players.get(room_id) else {
            return StartedDeliveries::default();
        };
        let shared_relay = relay_projection_work_repeats(
            players
                .iter()
                .filter(|player_id| Some(*player_id) != except_player)
                .filter_map(|player_id| clients.get(player_id))
                .filter_map(|handle| handle.sender.relay_projection())
                .filter_map(|(supports_v3, format)| {
                    relay_projection_cohort(message, supports_v3, format)
                }),
        )
        .then(|| {
            crate::coordination::outbound_queue::DeliveryMessage::shared_relay(Arc::clone(message))
        });
        self.start_routed_deliveries_with_shared(
            room_players,
            clients,
            room_id,
            except_player,
            message,
            shared_relay,
        )
    }

    fn start_routed_owned_deliveries(
        &self,
        room_players: &HashMap<RoomId, HashSet<PlayerId>>,
        clients: &HashMap<PlayerId, ClientDeliveryHandle>,
        room_id: &RoomId,
        except_player: Option<&PlayerId>,
        message: ServerMessage,
    ) -> StartedDeliveries {
        let Some(players) = room_players.get(room_id) else {
            return StartedDeliveries::default();
        };
        let (projection_work_repeats, all_recipients_are_classified) = relay_projection_summary(
            players
                .iter()
                .filter(|player_id| Some(*player_id) != except_player)
                .filter_map(|player_id| clients.get(player_id))
                .map(|handle| match handle.sender.relay_projection() {
                    Some((supports_v3, format)) => {
                        (true, relay_projection_cohort(&message, supports_v3, format))
                    }
                    None => (false, None),
                }),
        );

        if projection_work_repeats && all_recipients_are_classified {
            let shared_relay =
                crate::coordination::outbound_queue::DeliveryMessage::coowned_shared_relay(message);
            let recipients = players
                .iter()
                .filter(|player_id| Some(*player_id) != except_player)
                .filter_map(|player_id| clients.get(player_id).map(|handle| (*player_id, handle)));
            return self.start_deliveries(recipients, Some(*room_id), |_| shared_relay.clone());
        }

        let message = Arc::new(message);
        let shared_relay = projection_work_repeats.then(|| {
            crate::coordination::outbound_queue::DeliveryMessage::shared_relay(Arc::clone(&message))
        });
        self.start_routed_deliveries_with_shared(
            room_players,
            clients,
            room_id,
            except_player,
            &message,
            shared_relay,
        )
    }

    fn start_routed_deliveries_with_shared(
        &self,
        room_players: &HashMap<RoomId, HashSet<PlayerId>>,
        clients: &HashMap<PlayerId, ClientDeliveryHandle>,
        room_id: &RoomId,
        except_player: Option<&PlayerId>,
        message: &Arc<ServerMessage>,
        shared_relay: Option<crate::coordination::outbound_queue::DeliveryMessage>,
    ) -> StartedDeliveries {
        let Some(players) = room_players.get(room_id) else {
            return StartedDeliveries::default();
        };
        let recipients = players
            .iter()
            .filter(|player_id| Some(*player_id) != except_player)
            .filter_map(|player_id| clients.get(player_id).map(|handle| (*player_id, handle)));
        self.start_deliveries(recipients, Some(*room_id), |player_id| {
            shared_relay.clone().unwrap_or_else(|| {
                crate::coordination::outbound_queue::DeliveryMessage::new(
                    room_message_for_recipient(message, player_id),
                )
            })
        })
    }

    fn try_borrowed_room_broadcast<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        build_message: &mut (dyn FnMut() -> Option<Arc<ServerMessage>> + Send),
    ) -> ImmediateGameDataBroadcast<'a> {
        let Some(_routing) = self.room_routing_gates.try_read(*room_id) else {
            return ImmediateGameDataBroadcast::Unavailable;
        };
        let Ok(room_players) = self.room_players.try_read() else {
            return ImmediateGameDataBroadcast::Unavailable;
        };
        let Ok(clients) = self.local_clients.try_read() else {
            return ImmediateGameDataBroadcast::Unavailable;
        };
        let Some(message) = build_message() else {
            return ImmediateGameDataBroadcast::Complete;
        };
        let started = self.start_routed_deliveries(
            &room_players,
            &clients,
            room_id,
            Some(except_player),
            &message,
        );
        drop(clients);
        drop(room_players);

        if started.pending.is_empty() && started.slow_consumers.is_empty() {
            ImmediateGameDataBroadcast::Complete
        } else {
            ImmediateGameDataBroadcast::Pending(Box::pin(self.finish_deliveries(started)))
        }
    }

    async fn borrowed_room_broadcast_after_contention(
        &self,
        room_id: &RoomId,
        except_player: &PlayerId,
        build_message: &mut (dyn FnMut() -> Option<Arc<ServerMessage>> + Send),
    ) {
        let _routing = self.room_routing_gates.read(*room_id).await;
        let room_players = self.room_players.read().await;
        let clients = self.local_clients.read().await;
        let started = build_message().map(|message| {
            self.start_routed_deliveries(
                &room_players,
                &clients,
                room_id,
                Some(except_player),
                &message,
            )
        });
        drop(clients);
        drop(room_players);
        drop(_routing);
        if let Some(started) = started {
            self.finish_deliveries(started).await;
        }
    }

    fn collect_routed_recipients(
        room_players: &HashMap<RoomId, HashSet<PlayerId>>,
        clients: &HashMap<PlayerId, ClientDeliveryHandle>,
        room_id: &RoomId,
        except_player: Option<&PlayerId>,
    ) -> Vec<(PlayerId, ClientDeliveryHandle)> {
        let Some(players) = room_players.get(room_id) else {
            return Vec::new();
        };
        // Membership gives an exact upper bound. The old iterator `collect`
        // started from a filtered size hint of zero, then grew this vector one,
        // two, or three times for 2-, 8-, and 16-player rooms respectively.
        // This path runs for every relayed game-data frame, so reserve once.
        let capacity = players.len().saturating_sub(usize::from(
            except_player.is_some_and(|player_id| players.contains(player_id)),
        ));
        let mut recipients = Vec::with_capacity(capacity);
        recipients.extend(
            players
                .iter()
                .filter(|player_id| Some(*player_id) != except_player)
                .filter_map(|player_id| {
                    clients
                        .get(player_id)
                        .map(|handle| (*player_id, handle.clone()))
                }),
        );
        recipients
    }

    /// Snapshot the delivery handles for a room's members (optionally skipping
    /// one player) and release the room gate plus both map guards before any
    /// await on delivery, so a backpressured recipient can never stall
    /// registration or other broadcasts through held locks.
    async fn collect_room_recipients(
        &self,
        room_id: &RoomId,
        except_player: Option<&PlayerId>,
    ) -> Vec<(PlayerId, ClientDeliveryHandle)> {
        let _routing = self.room_routing_gates.read(*room_id).await;
        // Lock ordering: room_players first, then local_clients (matches
        // register/unregister to prevent ABBA deadlocks).
        let room_players = self.room_players.read().await;
        let clients = self.local_clients.read().await;
        Self::collect_routed_recipients(&room_players, &clients, room_id, except_player)
    }

    async fn lock_player_routing_write(
        &self,
        player_id: PlayerId,
        target_room: Option<RoomId>,
    ) -> PlayerRoutingWriteGuards {
        // Serialize one identity first, then take every affected room in UUID
        // order. The player gate closes the otherwise-disjoint empty-route
        // race where two concurrent first registrations could each lock only
        // their destination and leave the identity routed twice.
        let player = self.player_routing_gates.write(player_id).await;
        let mut room_ids: Vec<RoomId> = {
            let room_players = self.room_players.read().await;
            room_players
                .iter()
                .filter_map(|(room_id, players)| players.contains(&player_id).then_some(*room_id))
                .collect()
        };
        if let Some(room_id) = target_room {
            room_ids.push(room_id);
        }
        room_ids.sort_unstable();
        room_ids.dedup();
        let rooms = self.room_routing_gates.write_many(&room_ids).await;
        PlayerRoutingWriteGuards {
            _player: player,
            _rooms: rooms,
        }
    }

    async fn remove_client_if_same_sender(
        &self,
        player_id: PlayerId,
        attempted_sender: &DeliverySender,
    ) {
        let _routing = self.lock_player_routing_write(player_id, None).await;
        let mut clients = self.local_clients.write().await;
        if clients
            .get(&player_id)
            .is_some_and(|current| current.sender.same_channel(attempted_sender))
        {
            clients.remove(&player_id);
        }
    }

    fn sync_active_room_gates(
        &self,
        room_players: &HashMap<RoomId, HashSet<PlayerId>>,
        routing: &PlayerRoutingWriteGuards,
    ) {
        for guard in &routing._rooms {
            let room_id = guard._gate.key;
            if room_players
                .get(&room_id)
                .is_some_and(|players| !players.is_empty())
            {
                self.room_routing_gates.mark_active(&guard._gate);
            } else {
                self.room_routing_gates.mark_inactive(room_id);
            }
        }
    }

    fn record_canceled_delivery(&self, player_id: PlayerId) {
        self.metrics.increment_websocket_deliveries_canceled();
        tracing::debug!(%player_id, "Conditional delivery canceled after attempt");
    }

    fn record_reserved_cancellations(&self, reservations: &[ConditionalDeliveryReservation]) {
        for reservation in reservations {
            if let ConditionalDeliveryReservation::Reserved { player_id, .. } = reservation {
                self.record_canceled_delivery(*player_id);
            }
        }
    }

    async fn reserve_initial_transition(
        &self,
        player_id: PlayerId,
        delivery: &ClientDeliveryHandle,
    ) -> Result<DeliveryPermit, DeliveryOutcome> {
        self.metrics.increment_websocket_delivery_attempts();
        let stats = self.metrics.connection_delivery_stats(&player_id);
        let capacity_witness = match delivery.sender.try_reserve_control(None) {
            Ok(permit) => return Ok(permit),
            Err(DeliveryReserveError::Closed) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                return Err(DeliveryOutcome::ChannelClosed);
            }
            Err(DeliveryReserveError::Canceled) => {
                self.record_canceled_delivery(player_id);
                return Err(DeliveryOutcome::Canceled);
            }
            Err(DeliveryReserveError::Full(capacity_witness)) => capacity_witness,
        };

        let full_observed_at = capacity_witness
            .as_ref()
            .map(|witness| witness.full_observed_at())
            .unwrap_or_else(tokio::time::Instant::now);
        let deadline = full_observed_at
            .checked_add(self.slow_consumer_timeout)
            .unwrap_or(full_observed_at);
        self.metrics.increment_websocket_backpressure_events();
        if let Some(stats) = &stats {
            stats
                .backpressure_events
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let reserve = delivery.sender.reserve_control(None);
        tokio::pin!(reserve);
        let reservation = tokio::select! {
            // Capacity returning at or after the deadline cannot revive an
            // expired transition. Tokio's `timeout` polls its inner future
            // first, so use a timer-first biased select here.
            biased;
            _ = tokio::time::sleep_until(deadline) => None,
            result = &mut reserve => Some(result),
        };
        match reservation {
            Some(Ok(permit)) => Ok(permit),
            Some(Err(DeliveryReserveError::Canceled)) => {
                self.record_canceled_delivery(player_id);
                Err(DeliveryOutcome::Canceled)
            }
            Some(Err(DeliveryReserveError::Closed | DeliveryReserveError::Full(_))) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                Err(DeliveryOutcome::ChannelClosed)
            }
            None => {
                match delivery.sender.try_reserve_control_released_before(
                    None,
                    capacity_witness.as_ref(),
                    deadline,
                ) {
                    Ok(Some(permit)) => Ok(permit),
                    // A terminal queue state that became observable at the
                    // deadline is more specific than backpressure expiry. In
                    // particular, classified queues use `Canceled` to fence stale
                    // generations; that fence must not be rewritten as a
                    // slow-consumer disconnect merely because the timer is also
                    // ready.
                    Err(DeliveryReserveError::Canceled) => {
                        self.record_canceled_delivery(player_id);
                        Err(DeliveryOutcome::Canceled)
                    }
                    Err(DeliveryReserveError::Closed) => {
                        self.metrics.increment_websocket_deliveries_channel_closed();
                        Err(DeliveryOutcome::ChannelClosed)
                    }
                    Ok(None) | Err(DeliveryReserveError::Full(_)) => {
                        let initiated_close =
                            delivery.close.request_close(CloseReason::SlowConsumer);
                        if initiated_close {
                            self.metrics.increment_websocket_slow_consumer_disconnects();
                        }
                        self.metrics.increment_websocket_messages_dropped();
                        if let Some(stats) = &stats {
                            stats
                                .dropped_for_you
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        tracing::warn!(
                            %player_id,
                            timeout_ms = u64::try_from(self.slow_consumer_timeout.as_millis())
                                .unwrap_or(u64::MAX),
                            initiated_close,
                            "Initial room transition queue stayed full; closing recipient"
                        );
                        Err(DeliveryOutcome::SlowConsumer)
                    }
                }
            }
        }
    }

    fn commit_initial_transition(
        &self,
        player_id: PlayerId,
        permit: DeliveryPermit,
        message: Arc<ServerMessage>,
    ) -> DeliveryOutcome {
        let stats = self.metrics.connection_delivery_stats(&player_id);
        match permit.send(message) {
            Ok(outcome) => {
                crate::coordination::record_queue_outcome(&self.metrics, stats.as_ref(), outcome)
            }
            Err(_) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                DeliveryOutcome::ChannelClosed
            }
        }
    }

    async fn deliver_to_one_if(
        &self,
        player_id: PlayerId,
        handle: ClientDeliveryHandle,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        mut drain: watch::Receiver<bool>,
    ) -> Option<DeliveryOutcome> {
        if *drain.borrow() || !should_send() {
            return None;
        }

        self.metrics.increment_websocket_delivery_attempts();
        let connection_stats = self.metrics.connection_delivery_stats(&player_id);
        let (message, capacity_witness) = match handle.sender.try_send(message, None) {
            Ok(outcome) => {
                return Some(crate::coordination::record_queue_outcome(
                    &self.metrics,
                    connection_stats.as_ref(),
                    outcome,
                ));
            }
            Err(DeliveryTrySendError::Closed) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                tracing::debug!(
                    %player_id,
                    "Recipient connection already closing; message unroutable"
                );
                return Some(DeliveryOutcome::ChannelClosed);
            }
            Err(DeliveryTrySendError::Full(message, capacity_witness)) => {
                (message, capacity_witness)
            }
            Err(
                DeliveryTrySendError::AccountabilityUnavailable
                | DeliveryTrySendError::InvalidMetadata,
            ) => {
                return Some(crate::coordination::fail_delivery_closed(
                    &self.metrics,
                    connection_stats.as_ref(),
                    &player_id,
                    &handle,
                    "Conditional delivery queue failed closed",
                ));
            }
        };
        let full_observed_at = capacity_witness
            .as_ref()
            .map(|witness| witness.full_observed_at())
            .unwrap_or_else(tokio::time::Instant::now);
        let deadline = full_observed_at
            .checked_add(self.slow_consumer_timeout)
            .unwrap_or(full_observed_at);
        let (message, _) = message.into_parts();

        self.metrics.increment_websocket_backpressure_events();
        if let Some(stats) = &connection_stats {
            stats
                .backpressure_events
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let reserve = handle.sender.reserve_control(None);
        tokio::pin!(reserve);
        let timeout = tokio::time::sleep_until(deadline);
        tokio::pin!(timeout);

        tokio::select! {
            // Drain completion retains cancellation precedence. The timeout
            // branch then wins when capacity and expiry are both ready.
            biased;
            changed = drain.changed() => {
                if changed.is_ok() && *drain.borrow() {
                    tracing::debug!(%player_id, "Conditional delivery canceled for shutdown drain");
                }
                self.record_canceled_delivery(player_id);
                None
            }
            _ = &mut timeout => {
                if *drain.borrow() || !should_send() {
                    self.record_canceled_delivery(player_id);
                    return None;
                }
                match handle.sender.try_reserve_control_released_before(
                    None,
                    capacity_witness.as_ref(),
                    deadline,
                ) {
                    Ok(Some(permit)) =>
                    {
                        let outcome = match permit.send(message) {
                            Ok(outcome) if outcome.enqueued => outcome,
                            Ok(_) => {
                                self.record_canceled_delivery(player_id);
                                return Some(DeliveryOutcome::Canceled);
                            }
                            Err(_) => {
                                self.metrics.increment_websocket_deliveries_channel_closed();
                                return Some(DeliveryOutcome::ChannelClosed);
                            }
                        };
                        return Some(crate::coordination::record_queue_outcome(
                            &self.metrics,
                            connection_stats.as_ref(),
                            outcome,
                        ));
                    }
                    Err(DeliveryReserveError::Canceled) => {
                        self.record_canceled_delivery(player_id);
                        return Some(DeliveryOutcome::Canceled);
                    }
                    Err(DeliveryReserveError::Closed) => {
                        self.metrics.increment_websocket_deliveries_channel_closed();
                        return Some(DeliveryOutcome::ChannelClosed);
                    }
                    Ok(None) | Err(DeliveryReserveError::Full(_)) => {}
                }
                let initiated_close = handle.close.request_close(CloseReason::SlowConsumer);
                if initiated_close {
                    self.metrics.increment_websocket_slow_consumer_disconnects();
                }
                self.metrics.increment_websocket_messages_dropped();
                if let Some(stats) = &connection_stats {
                    stats
                        .dropped_for_you
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                tracing::warn!(
                    %player_id,
                    timeout_ms = u64::try_from(self.slow_consumer_timeout.as_millis())
                        .unwrap_or(u64::MAX),
                    initiated_close,
                    "Outbound queue full past the slow-consumer timeout; disconnecting recipient \
                     instead of silently dropping messages"
                );
                Some(DeliveryOutcome::SlowConsumer)
            }
            result = &mut reserve => match result {
                Ok(permit) => {
                    if *drain.borrow() || !should_send() {
                        self.record_canceled_delivery(player_id);
                        return None;
                    }
                    let outcome = match permit.send(message) {
                        Ok(outcome) if outcome.enqueued => outcome,
                        Ok(_) => {
                            self.record_canceled_delivery(player_id);
                            return Some(DeliveryOutcome::Canceled);
                        }
                        Err(_) => {
                            self.metrics.increment_websocket_deliveries_channel_closed();
                            return Some(DeliveryOutcome::ChannelClosed);
                        }
                    };
                    Some(crate::coordination::record_queue_outcome(
                        &self.metrics,
                        connection_stats.as_ref(),
                        outcome,
                    ))
                }
                Err(DeliveryReserveError::Closed | DeliveryReserveError::Full(_)) => {
                    self.metrics.increment_websocket_deliveries_channel_closed();
                    tracing::debug!(%player_id, "Recipient connection closed while backpressured");
                    Some(DeliveryOutcome::ChannelClosed)
                }
                Err(DeliveryReserveError::Canceled) => {
                    self.record_canceled_delivery(player_id);
                    Some(DeliveryOutcome::Canceled)
                }
            },
        }
    }

    async fn reserve_one_if(
        &self,
        player_id: PlayerId,
        handle: ClientDeliveryHandle,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        mut drain: watch::Receiver<bool>,
        room_id: Option<RoomId>,
    ) -> ConditionalDeliveryReservation {
        if *drain.borrow() || !should_send() {
            return ConditionalDeliveryReservation::Canceled;
        }

        self.metrics.increment_websocket_delivery_attempts();
        let stats = self.metrics.connection_delivery_stats(&player_id);
        let sender = handle.sender.clone();
        match sender.try_reserve_control(room_id) {
            Ok(permit) => ConditionalDeliveryReservation::Reserved {
                player_id,
                sender,
                permit,
                stats,
            },
            Err(DeliveryReserveError::Closed) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                tracing::debug!(
                    %player_id,
                    "Recipient connection already closing; message unroutable"
                );
                ConditionalDeliveryReservation::ChannelClosed { player_id, sender }
            }
            Err(DeliveryReserveError::Canceled) => {
                self.record_canceled_delivery(player_id);
                ConditionalDeliveryReservation::Canceled
            }
            Err(DeliveryReserveError::Full(capacity_witness)) => {
                let full_observed_at = capacity_witness
                    .as_ref()
                    .map(|witness| witness.full_observed_at())
                    .unwrap_or_else(tokio::time::Instant::now);
                let deadline = full_observed_at
                    .checked_add(self.slow_consumer_timeout)
                    .unwrap_or(full_observed_at);
                self.metrics.increment_websocket_backpressure_events();
                if let Some(stats) = &stats {
                    stats
                        .backpressure_events
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }

                let reserved_sender = sender.clone();
                let reserve = sender.reserve_control(room_id);
                tokio::pin!(reserve);
                let timeout = tokio::time::sleep_until(deadline);
                tokio::pin!(timeout);

                tokio::select! {
                    // Drain completion retains cancellation precedence. Expiry
                    // then wins an exact-boundary race with returned capacity.
                    biased;
                    changed = drain.changed() => {
                        if changed.is_ok() && *drain.borrow() {
                            tracing::debug!(%player_id, "Conditional delivery reservation canceled for shutdown drain");
                        }
                        self.record_canceled_delivery(player_id);
                        ConditionalDeliveryReservation::Canceled
                    }
                    _ = &mut timeout => {
                        if *drain.borrow() || !should_send() {
                            self.record_canceled_delivery(player_id);
                            return ConditionalDeliveryReservation::Canceled;
                        }
                        match sender.try_reserve_control_released_before(
                            room_id,
                            capacity_witness.as_ref(),
                            deadline,
                        ) {
                            Ok(Some(permit)) =>
                            {
                                return ConditionalDeliveryReservation::Reserved {
                                    player_id,
                                    sender: reserved_sender,
                                    permit,
                                    stats,
                                };
                            }
                            Err(DeliveryReserveError::Canceled) => {
                                self.record_canceled_delivery(player_id);
                                return ConditionalDeliveryReservation::Canceled;
                            }
                            Err(DeliveryReserveError::Closed) => {
                                self.metrics.increment_websocket_deliveries_channel_closed();
                                return ConditionalDeliveryReservation::ChannelClosed {
                                    player_id,
                                    sender: reserved_sender,
                                };
                            }
                            Ok(None) | Err(DeliveryReserveError::Full(_)) => {}
                        }
                        let initiated_close = handle.close.request_close(CloseReason::SlowConsumer);
                        if initiated_close {
                            self.metrics.increment_websocket_slow_consumer_disconnects();
                        }
                        self.metrics.increment_websocket_messages_dropped();
                        if let Some(stats) = &stats {
                            stats
                                .dropped_for_you
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        tracing::warn!(
                            %player_id,
                            timeout_ms = u64::try_from(self.slow_consumer_timeout.as_millis())
                                .unwrap_or(u64::MAX),
                            initiated_close,
                            "Outbound queue full past the slow-consumer timeout; disconnecting recipient \
                             instead of silently dropping messages"
                        );
                        ConditionalDeliveryReservation::SlowConsumer {
                            player_id,
                            sender: reserved_sender,
                        }
                    }
                    result = &mut reserve => match result {
                        Ok(permit) => {
                            if *drain.borrow() || !should_send() {
                                self.record_canceled_delivery(player_id);
                                ConditionalDeliveryReservation::Canceled
                            } else {
                                ConditionalDeliveryReservation::Reserved {
                                    player_id,
                                    sender: reserved_sender.clone(),
                                    permit,
                                    stats,
                                }
                            }
                        }
                        Err(DeliveryReserveError::Closed | DeliveryReserveError::Full(_)) => {
                            self.metrics.increment_websocket_deliveries_channel_closed();
                            tracing::debug!(%player_id, "Recipient connection closed while backpressured");
                            ConditionalDeliveryReservation::ChannelClosed {
                                player_id,
                                sender: reserved_sender.clone(),
                            }
                        }
                        Err(DeliveryReserveError::Canceled) => {
                            self.record_canceled_delivery(player_id);
                            ConditionalDeliveryReservation::Canceled
                        }
                    },
                }
            }
        }
    }

    async fn reserve_room_batch(
        &self,
        player_id: PlayerId,
        handle: ClientDeliveryHandle,
        frame_count: usize,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        drain: watch::Receiver<bool>,
        room_id: RoomId,
    ) -> RoomBatchReservation {
        let mut permits = Vec::with_capacity(frame_count);
        let sender = handle.sender.clone();
        let mut stats = None;

        for _ in 0..frame_count {
            match self
                .reserve_one_if(
                    player_id,
                    handle.clone(),
                    should_send,
                    drain.clone(),
                    Some(room_id),
                )
                .await
            {
                ConditionalDeliveryReservation::Reserved {
                    permit,
                    stats: reservation_stats,
                    ..
                } => {
                    stats = reservation_stats;
                    permits.push(Some(permit));
                }
                ConditionalDeliveryReservation::ChannelClosed { player_id, sender } => {
                    for _ in &permits {
                        self.record_canceled_delivery(player_id);
                    }
                    return RoomBatchReservation::ChannelClosed { player_id, sender };
                }
                ConditionalDeliveryReservation::SlowConsumer { player_id, sender } => {
                    for _ in &permits {
                        self.record_canceled_delivery(player_id);
                    }
                    return RoomBatchReservation::SlowConsumer { player_id, sender };
                }
                ConditionalDeliveryReservation::Canceled => {
                    for _ in &permits {
                        self.record_canceled_delivery(player_id);
                    }
                    return RoomBatchReservation::Canceled;
                }
            }
        }

        RoomBatchReservation::Reserved {
            player_id,
            sender,
            permits,
            stats,
        }
    }

    fn record_batch_cancellations(&self, reservations: &[RoomBatchReservation]) {
        for reservation in reservations {
            if let RoomBatchReservation::Reserved {
                player_id, permits, ..
            } = reservation
            {
                for _ in permits {
                    self.record_canceled_delivery(*player_id);
                }
            }
        }
    }

    fn batch_reservations_cover_recipients(
        reservations: &[RoomBatchReservation],
        recipients: &[(PlayerId, ClientDeliveryHandle)],
    ) -> bool {
        reservations.len() == recipients.len()
            && recipients.iter().all(|(player_id, handle)| {
                reservations.iter().any(|reservation| match reservation {
                    RoomBatchReservation::Reserved {
                        player_id: reserved_player,
                        sender,
                        ..
                    } => *reserved_player == *player_id && sender.same_channel(&handle.sender),
                    RoomBatchReservation::ChannelClosed { .. }
                    | RoomBatchReservation::SlowConsumer { .. }
                    | RoomBatchReservation::Canceled => false,
                })
            })
    }

    async fn broadcast_to_room_if_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: Option<&PlayerId>,
        expected_members: Option<&[PlayerId]>,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        drain: watch::Receiver<bool>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> anyhow::Result<bool> {
        let mut before_send = Some(before_send);

        loop {
            if *drain.borrow() || !should_send() {
                tracing::debug!(%room_id, ?except_player, "Conditional room broadcast skipped before replay hook");
                return Ok(false);
            }

            let recipients = self.collect_room_recipients(room_id, except_player).await;

            let reservations =
                futures_util::future::join_all(recipients.iter().map(|(player_id, handle)| {
                    self.reserve_one_if(
                        *player_id,
                        handle.clone(),
                        should_send,
                        drain.clone(),
                        Some(*room_id),
                    )
                }))
                .await;

            if *drain.borrow() || !should_send() {
                self.record_reserved_cancellations(&reservations);
                tracing::debug!(%room_id, ?except_player, "Conditional room broadcast canceled before replay record");
                return Ok(false);
            }

            // A recipient can change queue generation or room scope while its
            // reservation is pending. That invalidates only this routing
            // snapshot, not the room event itself. Cancel permits already held
            // for stable peers and retry resolution; aborting here would drop a
            // valid event for every stable recipient.
            if reservations
                .iter()
                .any(|reservation| matches!(reservation, ConditionalDeliveryReservation::Canceled))
            {
                self.record_reserved_cancellations(&reservations);
                tokio::task::yield_now().await;
                continue;
            }

            let slow_consumers: Vec<(PlayerId, DeliverySender)> = reservations
                .iter()
                .filter_map(|reservation| match reservation {
                    ConditionalDeliveryReservation::SlowConsumer { player_id, sender } => {
                        Some((*player_id, sender.clone()))
                    }
                    ConditionalDeliveryReservation::Reserved { .. }
                    | ConditionalDeliveryReservation::ChannelClosed { .. }
                    | ConditionalDeliveryReservation::Canceled => None,
                })
                .collect();
            if !slow_consumers.is_empty() {
                self.record_reserved_cancellations(&reservations);
                for (player_id, attempted_sender) in &slow_consumers {
                    self.remove_client_if_same_sender(*player_id, attempted_sender)
                        .await;
                }
                continue;
            }

            let _routing = self.room_routing_gates.read(*room_id).await;
            let room_players = self.room_players.read().await;
            let clients = self.local_clients.read().await;
            let current_recipients: Vec<(PlayerId, ClientDeliveryHandle)> = room_players
                .get(room_id)
                .map(|players| {
                    players
                        .iter()
                        .filter(|player_id| except_player != Some(*player_id))
                        .filter_map(|player_id| {
                            clients
                                .get(player_id)
                                .map(|handle| (*player_id, handle.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();

            if let Some(expected_members) = expected_members {
                let mut current: Vec<PlayerId> = current_recipients
                    .iter()
                    .map(|(player_id, _)| *player_id)
                    .collect();
                let mut expected = expected_members.to_vec();
                current.sort_unstable();
                expected.sort_unstable();
                if current != expected {
                    self.record_reserved_cancellations(&reservations);
                    tracing::debug!(%room_id, "Room broadcast canceled because published membership changed");
                    return Ok(false);
                }
            }

            if !Self::reservations_cover_recipients(&reservations, &current_recipients) {
                self.record_reserved_cancellations(&reservations);
                drop(clients);
                drop(room_players);
                continue;
            }

            if *drain.borrow() || !should_send() {
                self.record_reserved_cancellations(&reservations);
                tracing::debug!(%room_id, ?except_player, "Conditional room broadcast canceled before replay record");
                return Ok(false);
            }

            drop(clients);
            drop(room_players);

            // No capacity wait happens while the room-scoped routing gate is
            // held. Replay recording and permit sends remain one commit
            // relative to this room's registration fence without excluding
            // routing work in unrelated rooms.
            let Some(before_send) = before_send.take() else {
                tracing::error!(%room_id, ?except_player, "Conditional room broadcast replay hook was already consumed");
                return Ok(false);
            };
            before_send().await;

            let mut delivered = false;
            for reservation in reservations {
                match reservation {
                    ConditionalDeliveryReservation::Reserved {
                        player_id,
                        permit,
                        stats,
                        ..
                    } => {
                        let recipient_message = room_message_for_recipient(&message, &player_id);
                        match permit.send(recipient_message) {
                            Ok(outcome) if outcome.enqueued => {
                                delivered = true;
                                self.metrics.increment_websocket_deliveries_enqueued();
                                if let Some(stats) = &stats {
                                    stats
                                        .sent_to_you
                                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                            }
                            Ok(_) => {
                                self.metrics.increment_websocket_deliveries_canceled();
                                tracing::debug!(
                                    %room_id,
                                    ?except_player,
                                    %player_id,
                                    "Conditional room broadcast permit became stale at commit"
                                );
                            }
                            Err(_) => {
                                self.metrics.increment_websocket_deliveries_channel_closed();
                            }
                        }
                    }
                    ConditionalDeliveryReservation::ChannelClosed { .. } => {}
                    ConditionalDeliveryReservation::SlowConsumer { .. }
                    | ConditionalDeliveryReservation::Canceled => {
                        tracing::debug!(
                            %room_id,
                            ?except_player,
                            "Conditional room broadcast reached an invalid reservation at commit"
                        );
                        return Ok(false);
                    }
                }
            }

            return Ok(delivered);
        }
    }

    fn reservations_cover_recipients(
        reservations: &[ConditionalDeliveryReservation],
        recipients: &[(PlayerId, ClientDeliveryHandle)],
    ) -> bool {
        reservations.len() == recipients.len()
            && recipients.iter().all(|(player_id, handle)| {
                reservations.iter().any(|reservation| match reservation {
                    ConditionalDeliveryReservation::Reserved {
                        player_id: reserved_player,
                        sender,
                        ..
                    }
                    | ConditionalDeliveryReservation::ChannelClosed {
                        player_id: reserved_player,
                        sender,
                    } => *reserved_player == *player_id && sender.same_channel(&handle.sender),
                    ConditionalDeliveryReservation::SlowConsumer { .. }
                    | ConditionalDeliveryReservation::Canceled => false,
                })
            })
    }
}

#[async_trait::async_trait]
impl MessageCoordinator for InMemoryMessageCoordinator {
    async fn lock_room_event_mutation(&self, room_id: &RoomId) -> RoomEventMutationGuard {
        self.room_event_sequencer.lock(*room_id).await
    }

    fn enqueue_room_event(
        &self,
        mutation_guard: RoomEventMutationGuard,
        job: RoomEventJob,
    ) -> RoomEventCompletion {
        self.room_event_sequencer.enqueue(mutation_guard, job)
    }

    #[cfg(test)]
    fn fail_room_transactions_for_test(&self, fail: bool) {
        self.fail_room_transactions
            .store(fail, std::sync::atomic::Ordering::Release);
    }

    async fn send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        let handle = { self.local_clients.read().await.get(player_id).cloned() };
        if let Some(handle) = handle {
            self.deliver_to_all(vec![(*player_id, handle)], message, None)
                .await;
        } else {
            // Normal during disconnect races (e.g. a room notification issued
            // while the target is unregistering); nothing to deliver to.
            tracing::debug!(%player_id, "Player not registered with coordinator; message unroutable");
        }
        Ok(())
    }

    async fn send_to_player_in_room(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<bool> {
        let handle = {
            let _routing = self.room_routing_gates.read(*room_id).await;
            let room_players = self.room_players.read().await;
            let clients = self.local_clients.read().await;
            room_players
                .get(room_id)
                .filter(|players| players.contains(player_id))
                .and_then(|_| clients.get(player_id).cloned())
        };
        let Some(handle) = handle else {
            return Ok(false);
        };

        let outcome = crate::coordination::deliver_or_disconnect_in_room(
            &self.metrics,
            self.slow_consumer_timeout,
            player_id,
            &handle,
            message,
            Some(*room_id),
        )
        .await;
        if outcome == DeliveryOutcome::SlowConsumer {
            self.remove_client_if_same_sender(*player_id, &handle.sender)
                .await;
        }
        Ok(outcome == DeliveryOutcome::Delivered)
    }

    async fn routed_player_ids(&self, room_id: &RoomId) -> anyhow::Result<Option<Vec<PlayerId>>> {
        let _routing = self.room_routing_gates.read(*room_id).await;
        let room_players = self.room_players.read().await;
        let clients = self.local_clients.read().await;
        let mut players: Vec<PlayerId> = room_players
            .get(room_id)
            .into_iter()
            .flat_map(|players| players.iter().copied())
            .filter(|player_id| clients.contains_key(player_id))
            .collect();
        players.sort_unstable();
        Ok(Some(players))
    }

    async fn send_to_player_in_room_if_members(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        expected_members: &[PlayerId],
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<bool> {
        let handle = {
            let _routing = self.room_routing_gates.read(*room_id).await;
            let room_players = self.room_players.read().await;
            let clients = self.local_clients.read().await;
            room_players
                .get(room_id)
                .filter(|players| players.contains(player_id))
                .and_then(|_| clients.get(player_id).cloned())
        };
        let Some(handle) = handle else {
            return Ok(false);
        };

        let (_drain_tx, drain) = watch::channel(false);
        let should_send = || true;
        let reservation = self
            .reserve_one_if(*player_id, handle, &should_send, drain, Some(*room_id))
            .await;
        let ConditionalDeliveryReservation::Reserved {
            sender,
            permit,
            stats,
            ..
        } = reservation
        else {
            return Ok(false);
        };

        let _routing = self.room_routing_gates.read(*room_id).await;
        let room_players = self.room_players.read().await;
        let clients = self.local_clients.read().await;
        let mut current_members: Vec<PlayerId> = room_players
            .get(room_id)
            .into_iter()
            .flat_map(|players| players.iter().copied())
            .filter(|routed_player| clients.contains_key(routed_player))
            .collect();
        current_members.sort_unstable();
        let mut expected_members = expected_members.to_vec();
        expected_members.sort_unstable();
        let recipient_matches = clients
            .get(player_id)
            .is_some_and(|current| current.sender.same_channel(&sender));
        if current_members != expected_members || !recipient_matches {
            self.record_canceled_delivery(*player_id);
            return Ok(false);
        }

        let delivered = match permit.send(message) {
            Ok(outcome) if outcome.enqueued => {
                self.metrics.increment_websocket_deliveries_enqueued();
                if let Some(stats) = &stats {
                    stats
                        .sent_to_you
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                true
            }
            Ok(_) => {
                self.record_canceled_delivery(*player_id);
                false
            }
            Err(_) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                false
            }
        };
        drop(clients);
        drop(room_players);
        Ok(delivered)
    }

    async fn send_to_player_if(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        drain: watch::Receiver<bool>,
    ) -> anyhow::Result<bool> {
        let handle = { self.local_clients.read().await.get(player_id).cloned() };
        let Some(handle) = handle else {
            tracing::debug!(%player_id, "Player not registered with coordinator; conditional message unroutable");
            return Ok(false);
        };
        let attempted_sender = handle.sender.clone();
        let outcome = self
            .deliver_to_one_if(*player_id, handle, message, should_send, drain)
            .await;
        if outcome == Some(DeliveryOutcome::SlowConsumer) {
            self.remove_client_if_same_sender(*player_id, &attempted_sender)
                .await;
        }
        Ok(outcome == Some(DeliveryOutcome::Delivered))
    }

    async fn try_send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<bool> {
        let handle = { self.local_clients.read().await.get(player_id).cloned() };
        let Some(handle) = handle else {
            tracing::debug!(%player_id, "Farewell skipped: player not registered with coordinator");
            return Ok(false);
        };
        match handle.sender.try_send(message, None) {
            Ok(outcome) => Ok(outcome.enqueued),
            Err(DeliveryTrySendError::Full(_, _)) => {
                // Advisory frame to a connection that is being closed anyway:
                // do not wait, do not escalate, do not overwrite the close
                // reason. The teardown itself is the loud signal.
                tracing::debug!(
                    %player_id,
                    "Farewell skipped: outbound queue full on a closing connection"
                );
                Ok(false)
            }
            Err(
                DeliveryTrySendError::Closed
                | DeliveryTrySendError::AccountabilityUnavailable
                | DeliveryTrySendError::InvalidMetadata,
            ) => {
                tracing::debug!(%player_id, "Farewell skipped: connection already closed");
                Ok(false)
            }
        }
    }

    async fn try_send_to_player_if(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
    ) -> anyhow::Result<bool> {
        let handle = { self.local_clients.read().await.get(player_id).cloned() };
        let Some(handle) = handle else {
            tracing::debug!(%player_id, "Farewell skipped: player not registered with coordinator");
            return Ok(false);
        };
        if !should_send() {
            tracing::debug!(%player_id, "Farewell skipped: caller state changed before enqueue");
            return Ok(false);
        }
        match handle.sender.try_send(message, None) {
            Ok(outcome) => Ok(outcome.enqueued),
            Err(DeliveryTrySendError::Full(_, _)) => {
                tracing::debug!(
                    %player_id,
                    "Farewell skipped: outbound queue full on a closing connection"
                );
                Ok(false)
            }
            Err(
                DeliveryTrySendError::Closed
                | DeliveryTrySendError::AccountabilityUnavailable
                | DeliveryTrySendError::InvalidMetadata,
            ) => {
                tracing::debug!(%player_id, "Farewell skipped: connection already closed");
                Ok(false)
            }
        }
    }

    async fn broadcast_to_room(
        &self,
        room_id: &RoomId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        let recipients = self.collect_room_recipients(room_id, None).await;
        self.deliver_to_all(recipients, message, Some(*room_id))
            .await;
        Ok(())
    }

    async fn broadcast_to_room_except(
        &self,
        room_id: &RoomId,
        except_player: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        let recipients = self
            .collect_room_recipients(room_id, Some(except_player))
            .await;
        self.deliver_to_all(recipients, message, Some(*room_id))
            .await;
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
    ) -> anyhow::Result<bool> {
        let (_drain_tx, drain_rx) = watch::channel(false);
        let should_send = || true;
        self.broadcast_to_room_if_with_hook(
            room_id,
            None,
            None,
            message,
            &should_send,
            drain_rx,
            before_send,
        )
        .await
    }

    async fn broadcast_to_room_if_members_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        expected_members: &[PlayerId],
        message: Arc<ServerMessage>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> anyhow::Result<bool> {
        let (_drain_tx, drain_rx) = watch::channel(false);
        let should_send = || true;
        self.broadcast_to_room_if_with_hook(
            room_id,
            None,
            Some(expected_members),
            message,
            &should_send,
            drain_rx,
            before_send,
        )
        .await
    }

    async fn commit_room_messages_if_members_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        expected_members: &[PlayerId],
        recipient_messages: Vec<RoomRecipientMessages>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + 'a>,
                > + Send
                + 'a,
        >,
        after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
    ) -> anyhow::Result<RoomMessageTransactionOutcome> {
        #[cfg(test)]
        if self
            .fail_room_transactions
            .load(std::sync::atomic::Ordering::Acquire)
        {
            anyhow::bail!("injected room message transaction failure");
        }

        let mut expected = expected_members.to_vec();
        expected.sort_unstable();
        if expected.is_empty() || expected.windows(2).any(|pair| pair.first() == pair.get(1)) {
            anyhow::bail!("room message transaction requires unique non-empty membership");
        }

        let mut batch_members: Vec<PlayerId> = recipient_messages
            .iter()
            .map(|batch| batch.player_id)
            .collect();
        batch_members.sort_unstable();
        if batch_members != expected {
            anyhow::bail!("room message batches must cover every expected member exactly once");
        }
        if recipient_messages
            .iter()
            .all(|batch| batch.messages.is_empty())
        {
            anyhow::bail!("room message transaction requires at least one frame");
        }
        if recipient_messages
            .iter()
            .any(|batch| batch.first_phase > 1 || batch.phase_count() > 2)
        {
            anyhow::bail!("room message transactions support exactly two ordered phases");
        }

        let messages_by_player: HashMap<PlayerId, RoomRecipientMessages> = recipient_messages
            .into_iter()
            .map(|batch| (batch.player_id, batch))
            .collect();
        let max_phases = messages_by_player
            .values()
            .map(RoomRecipientMessages::phase_count)
            .max()
            .unwrap_or(0);
        let mut before_send = Some(before_send);
        let mut after_first_phase = Some(after_first_phase);
        let (_drain_tx, drain) = watch::channel(false);
        let should_send = || true;

        loop {
            let recipients = self.collect_room_recipients(room_id, None).await;
            let mut routed: Vec<PlayerId> =
                recipients.iter().map(|(player_id, _)| *player_id).collect();
            routed.sort_unstable();
            if routed != expected {
                return Ok(RoomMessageTransactionOutcome::RoutingChanged);
            }

            let reservation_inputs: Option<Vec<_>> = recipients
                .iter()
                .map(|(player_id, handle)| {
                    messages_by_player
                        .get(player_id)
                        .map(|batch| (*player_id, handle.clone(), batch.messages.len()))
                })
                .collect();
            let Some(reservation_inputs) = reservation_inputs else {
                anyhow::bail!("validated room transaction recipient lost its message batch");
            };
            let reservations = futures_util::future::join_all(reservation_inputs.into_iter().map(
                |(player_id, handle, frame_count)| {
                    self.reserve_room_batch(
                        player_id,
                        handle,
                        frame_count,
                        &should_send,
                        drain.clone(),
                        *room_id,
                    )
                },
            ))
            .await;

            if reservations
                .iter()
                .any(|reservation| matches!(reservation, RoomBatchReservation::Canceled))
            {
                self.record_batch_cancellations(&reservations);
                tokio::task::yield_now().await;
                continue;
            }

            let unavailable_recipients: Vec<(PlayerId, DeliverySender)> = reservations
                .iter()
                .filter_map(|reservation| match reservation {
                    RoomBatchReservation::SlowConsumer { player_id, sender }
                    | RoomBatchReservation::ChannelClosed { player_id, sender } => {
                        Some((*player_id, sender.clone()))
                    }
                    RoomBatchReservation::Reserved { .. } | RoomBatchReservation::Canceled => None,
                })
                .collect();
            if !unavailable_recipients.is_empty() {
                self.record_batch_cancellations(&reservations);
                for (player_id, attempted_sender) in &unavailable_recipients {
                    self.remove_client_if_same_sender(*player_id, attempted_sender)
                        .await;
                }
                continue;
            }

            let _routing = self.room_routing_gates.read(*room_id).await;
            let room_players = self.room_players.read().await;
            let clients = self.local_clients.read().await;
            let current_recipients: Vec<(PlayerId, ClientDeliveryHandle)> = room_players
                .get(room_id)
                .map(|players| {
                    players
                        .iter()
                        .filter_map(|player_id| {
                            clients
                                .get(player_id)
                                .map(|handle| (*player_id, handle.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut current_members: Vec<PlayerId> = current_recipients
                .iter()
                .map(|(player_id, _)| *player_id)
                .collect();
            current_members.sort_unstable();
            if current_members != expected
                || !Self::batch_reservations_cover_recipients(&reservations, &current_recipients)
            {
                self.record_batch_cancellations(&reservations);
                return Ok(RoomMessageTransactionOutcome::RoutingChanged);
            }

            drop(clients);
            drop(room_players);

            let Some(commit_hook) = before_send.take() else {
                self.record_batch_cancellations(&reservations);
                anyhow::bail!("room message transaction hook was already consumed");
            };
            match commit_hook().await {
                Ok(true) => {}
                Ok(false) => {
                    self.record_batch_cancellations(&reservations);
                    return Ok(RoomMessageTransactionOutcome::HookRejected);
                }
                Err(error) => {
                    self.record_batch_cancellations(&reservations);
                    return Err(error);
                }
            }

            let mut reservations = reservations;
            let mut failed_frames = 0_usize;
            for phase in 0..max_phases {
                let failed_before_phase = failed_frames;
                for reservation in &mut reservations {
                    let RoomBatchReservation::Reserved {
                        player_id,
                        permits,
                        stats,
                        ..
                    } = reservation
                    else {
                        tracing::error!(
                            %room_id,
                            phase,
                            "Non-reserved recipient survived final room transaction validation"
                        );
                        continue;
                    };
                    let Some(batch) = messages_by_player.get(player_id) else {
                        let skipped = permits.iter_mut().filter_map(Option::take).count();
                        failed_frames = failed_frames.saturating_add(skipped);
                        for _ in 0..skipped {
                            self.record_canceled_delivery(*player_id);
                        }
                        tracing::error!(
                            %room_id,
                            %player_id,
                            skipped,
                            "Validated room transaction recipient lost its message batch"
                        );
                        continue;
                    };
                    let Some(message) = batch.message_in_phase(phase) else {
                        continue;
                    };
                    let Some(permit_index) = phase.checked_sub(batch.first_phase) else {
                        failed_frames = failed_frames.saturating_add(1);
                        self.record_canceled_delivery(*player_id);
                        tracing::error!(
                            %room_id,
                            %player_id,
                            phase,
                            first_phase = batch.first_phase,
                            "Room transaction phase preceded its batch origin"
                        );
                        continue;
                    };
                    let Some(permit) = permits.get_mut(permit_index).and_then(Option::take) else {
                        failed_frames = failed_frames.saturating_add(1);
                        self.record_canceled_delivery(*player_id);
                        tracing::error!(
                            %room_id,
                            %player_id,
                            phase,
                            "Room transaction frame lost its reserved permit"
                        );
                        continue;
                    };
                    match permit.send(Arc::clone(message)) {
                        Ok(outcome) if outcome.enqueued => {
                            self.metrics.increment_websocket_deliveries_enqueued();
                            if let Some(stats) = stats {
                                stats
                                    .sent_to_you
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        Ok(_) => {
                            failed_frames = failed_frames.saturating_add(1);
                            self.record_canceled_delivery(*player_id);
                            tracing::warn!(
                                %room_id,
                                %player_id,
                                phase,
                                "Room transaction permit became stale after durable commit"
                            );
                        }
                        Err(_) => {
                            failed_frames = failed_frames.saturating_add(1);
                            self.metrics.increment_websocket_deliveries_channel_closed();
                            tracing::warn!(
                                %room_id,
                                %player_id,
                                phase,
                                "Room transaction recipient closed after durable commit"
                            );
                        }
                    }
                }
                if phase == 0 {
                    let continue_publication = match after_first_phase.take() {
                        Some(after_first_phase) => {
                            after_first_phase(failed_frames.saturating_sub(failed_before_phase))
                        }
                        None => {
                            tracing::error!(
                                %room_id,
                                "Room transaction state callback was already consumed"
                            );
                            false
                        }
                    };
                    if !continue_publication {
                        for reservation in &reservations {
                            let RoomBatchReservation::Reserved {
                                player_id, permits, ..
                            } = reservation
                            else {
                                continue;
                            };
                            let skipped = permits.iter().filter(|permit| permit.is_some()).count();
                            failed_frames = failed_frames.saturating_add(skipped);
                            for _ in 0..skipped {
                                self.record_canceled_delivery(*player_id);
                            }
                        }
                        break;
                    }
                }
            }

            return Ok(if failed_frames == 0 {
                RoomMessageTransactionOutcome::Committed
            } else {
                RoomMessageTransactionOutcome::CommittedDegraded { failed_frames }
            });
        }
    }

    async fn broadcast_to_room_except_if_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        drain: watch::Receiver<bool>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> anyhow::Result<bool> {
        self.broadcast_to_room_if_with_hook(
            room_id,
            Some(except_player),
            None,
            message,
            should_send,
            drain,
            before_send,
        )
        .await
    }

    async fn broadcast_to_room_except_with_message<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        build_message: Box<dyn FnOnce() -> Option<Arc<ServerMessage>> + Send + 'a>,
    ) -> anyhow::Result<()> {
        // Keep this allocation-measured compatibility handoff on the same
        // compact uncontended path as the borrowed builder. The FnOnce remains
        // available to the boxed contention path until one branch consumes it.
        let mut build_message = Some(build_message);
        let mut build_once = || build_message.take().and_then(|build| build());
        match self.try_borrowed_room_broadcast(room_id, except_player, &mut build_once) {
            ImmediateGameDataBroadcast::Complete => {}
            ImmediateGameDataBroadcast::Pending(finish) => finish.await,
            ImmediateGameDataBroadcast::Unavailable => {
                Box::pin(self.borrowed_room_broadcast_after_contention(
                    room_id,
                    except_player,
                    &mut build_once,
                ))
                .await;
            }
        }
        Ok(())
    }

    async fn broadcast_to_room_except_with_borrowed_message<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        build_message: &'a mut (dyn FnMut() -> Option<Arc<ServerMessage>> + Send),
    ) -> anyhow::Result<()> {
        // Keep the uncontended borrowed handoff's boxed trait future compact:
        // routing/map guards live only inside the synchronous helper. The
        // larger wait state is boxed only on actual contention, preserving the
        // checked-in P71 allocation-byte ceiling in the healthy path.
        match self.try_borrowed_room_broadcast(room_id, except_player, build_message) {
            ImmediateGameDataBroadcast::Complete => {}
            ImmediateGameDataBroadcast::Pending(finish) => finish.await,
            ImmediateGameDataBroadcast::Unavailable => {
                Box::pin(self.borrowed_room_broadcast_after_contention(
                    room_id,
                    except_player,
                    build_message,
                ))
                .await;
            }
        }
        Ok(())
    }

    async fn broadcast_to_room_except_with_borrowed_owned_message<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        build_message: &'a mut (dyn FnMut() -> Option<ServerMessage> + Send),
    ) -> anyhow::Result<()> {
        let _routing = self.room_routing_gates.read(*room_id).await;
        let room_players = self.room_players.read().await;
        let clients = self.local_clients.read().await;
        let started = build_message().map(|message| {
            self.start_routed_owned_deliveries(
                &room_players,
                &clients,
                room_id,
                Some(except_player),
                message,
            )
        });
        drop(clients);
        drop(room_players);
        drop(_routing);
        if let Some(started) = started {
            self.finish_deliveries(started).await;
        }
        Ok(())
    }

    fn try_broadcast_to_room_except_with_borrowed_owned_message<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        build_message: &mut (dyn FnMut() -> Option<ServerMessage> + Send),
    ) -> ImmediateGameDataBroadcast<'a> {
        let Some(_routing) = self.room_routing_gates.try_read(*room_id) else {
            return ImmediateGameDataBroadcast::Unavailable;
        };
        let Ok(room_players) = self.room_players.try_read() else {
            return ImmediateGameDataBroadcast::Unavailable;
        };
        let Ok(clients) = self.local_clients.try_read() else {
            return ImmediateGameDataBroadcast::Unavailable;
        };
        let Some(message) = build_message() else {
            return ImmediateGameDataBroadcast::Complete;
        };
        let started = self.start_routed_owned_deliveries(
            &room_players,
            &clients,
            room_id,
            Some(except_player),
            message,
        );
        drop(clients);
        drop(room_players);

        if started.pending.is_empty() && started.slow_consumers.is_empty() {
            ImmediateGameDataBroadcast::Complete
        } else {
            ImmediateGameDataBroadcast::Pending(Box::pin(self.finish_deliveries(started)))
        }
    }

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        delivery: ClientDeliveryHandle,
    ) -> anyhow::Result<()> {
        let routing = self.lock_player_routing_write(player_id, room_id).await;
        // Lock ordering: room_players first, then local_clients. Registration
        // replaces the player's routing scope, including `None` on leave; merely
        // updating the sender map would retain a stale old-room recipient.
        let mut room_players = self.room_players.write().await;
        room_players.retain(|_, players| {
            players.remove(&player_id);
            !players.is_empty()
        });
        if let Some(room_id) = room_id {
            room_players
                .entry(room_id)
                .or_insert_with(HashSet::new)
                .insert(player_id);
        }
        let mut clients = self.local_clients.write().await;
        clients.insert(player_id, delivery);
        self.sync_active_room_gates(&room_players, &routing);
        Ok(())
    }

    async fn unroute_local_client_with_tail<'a>(
        &'a self,
        player_id: PlayerId,
        room_id: RoomId,
        clear_assignment: Box<dyn FnOnce() -> Option<(ClientDeliveryHandle, u32, u64)> + Send + 'a>,
    ) -> anyhow::Result<Option<(u32, u64)>> {
        let routing = self
            .lock_player_routing_write(player_id, Some(room_id))
            .await;
        // Relay broadcasts hold the read side of these locks while allocating
        // the sender stamp and snapshotting recipients. Taking both write
        // locks makes the captured tail and route removal one indivisible
        // boundary: every old-room stamp is <= `final_seq`, and none can be
        // allocated after the member becomes unroutable.
        let mut room_players = self.room_players.write().await;
        let mut clients = self.local_clients.write().await;
        let cleared = clear_assignment();

        let was_routed = room_players
            .get(&room_id)
            .is_some_and(|players| players.contains(&player_id));
        room_players.retain(|_, players| {
            players.remove(&player_id);
            !players.is_empty()
        });
        let terminal_tail = if let Some((delivery, epoch, final_seq)) = cleared {
            clients.insert(player_id, delivery);
            Some((epoch, final_seq))
        } else {
            clients.remove(&player_id);
            None
        };
        self.sync_active_room_gates(&room_players, &routing);
        if !was_routed {
            tracing::debug!(%player_id, %room_id, "Player route was already absent at terminal watermark capture");
        }
        Ok(terminal_tail)
    }

    async fn register_local_client_with_initial_message<'a>(
        &'a self,
        player_id: PlayerId,
        room_id: RoomId,
        delivery: ClientDeliveryHandle,
        build_message: Box<dyn FnOnce() -> Arc<ServerMessage> + Send + 'a>,
    ) -> anyhow::Result<DeliveryOutcome> {
        let permit = match self.reserve_initial_transition(player_id, &delivery).await {
            Ok(permit) => permit,
            Err(outcome) => return Ok(outcome),
        };
        let routing = self
            .lock_player_routing_write(player_id, Some(room_id))
            .await;
        // Lock ordering matches `register_local_client` and
        // `collect_room_recipients`. While this room-scoped write gate is held,
        // broadcasts for this room cannot snapshot recipients. The reconnect
        // path uses that to:
        // (1) wait for every pre-existing broadcast snapshot to finish, (2)
        // capture the sender watermarks, (3) queue `Reconnected`, and (4)
        // register the player into the room before later broadcasts can route.
        let outcome = self.commit_initial_transition(player_id, permit, build_message());
        if outcome == DeliveryOutcome::Delivered {
            let mut room_players = self.room_players.write().await;
            let mut clients = self.local_clients.write().await;
            room_players.retain(|_, players| {
                players.remove(&player_id);
                !players.is_empty()
            });
            room_players
                .entry(room_id)
                .or_insert_with(HashSet::new)
                .insert(player_id);
            clients.insert(player_id, delivery.clone());
            self.sync_active_room_gates(&room_players, &routing);
        }

        Ok(outcome)
    }

    async fn register_local_client_with_initial_message_async<'a>(
        &'a self,
        player_id: PlayerId,
        room_id: RoomId,
        delivery: ClientDeliveryHandle,
        build_message: Box<
            dyn FnOnce(
                    Vec<PlayerId>,
                ) -> std::pin::Pin<
                    Box<
                        dyn std::future::Future<Output = anyhow::Result<Arc<ServerMessage>>>
                            + Send
                            + 'a,
                    >,
                > + Send
                + 'a,
        >,
    ) -> anyhow::Result<DeliveryOutcome> {
        let permit = match self.reserve_initial_transition(player_id, &delivery).await {
            Ok(permit) => permit,
            Err(outcome) => return Ok(outcome),
        };
        let routing = self
            .lock_player_routing_write(player_id, Some(room_id))
            .await;
        // Same lock ordering and routing transition as the sync builder, but
        // the async builder itself is fenced by only this room's gate. Global
        // map guards are held only for the snapshots/updates around it.
        // Reconnect uses this to fetch replay after every older same-room
        // broadcast has either recorded or skipped its event, and before any
        // newer same-room broadcast can snapshot this restored socket as live.
        let mut routed_players: Vec<PlayerId> = {
            let room_players = self.room_players.read().await;
            let clients = self.local_clients.read().await;
            room_players
                .get(&room_id)
                .into_iter()
                .flat_map(|players| players.iter().copied())
                .filter(|routed_player| clients.contains_key(routed_player))
                .collect()
        };
        if !routed_players.contains(&player_id) {
            routed_players.push(player_id);
        }
        routed_players.sort_unstable();
        let message = build_message(routed_players).await?;
        let outcome = self.commit_initial_transition(player_id, permit, message);
        if outcome == DeliveryOutcome::Delivered {
            let mut room_players = self.room_players.write().await;
            let mut clients = self.local_clients.write().await;
            room_players.retain(|_, players| {
                players.remove(&player_id);
                !players.is_empty()
            });
            room_players
                .entry(room_id)
                .or_insert_with(HashSet::new)
                .insert(player_id);
            clients.insert(player_id, delivery.clone());
            self.sync_active_room_gates(&room_players, &routing);
        }

        Ok(outcome)
    }

    async fn unregister_local_client(&self, player_id: &PlayerId) -> anyhow::Result<()> {
        let routing = self.lock_player_routing_write(*player_id, None).await;
        // Lock ordering: room_players first, then local_clients
        // (consistent with broadcast_to_room / broadcast_to_room_except read paths
        //  to prevent ABBA deadlocks)
        let mut room_players = self.room_players.write().await;
        room_players.retain(|_, players| {
            players.remove(player_id);
            !players.is_empty()
        });

        let mut clients = self.local_clients.write().await;
        clients.remove(player_id);
        self.sync_active_room_gates(&room_players, &routing);

        Ok(())
    }

    async fn should_process_message(
        &self,
        _message: &crate::distributed::SequencedMessage,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn mark_message_processed(
        &self,
        _message: &crate::distributed::SequencedMessage,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn handle_bus_message(
        &self,
        message: crate::distributed::SequencedMessage,
    ) -> anyhow::Result<()> {
        if let Some(player_id) = message.target_player {
            self.send_to_player(&player_id, Arc::new(message.message))
                .await
        } else if let Some(room_id) = message.room_id {
            self.broadcast_to_room(&room_id, Arc::new(message.message))
                .await
        } else {
            Ok(())
        }
    }
}

impl Default for InMemoryMessageCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod relay_projection_cache_tests {
    use super::{
        relay_projection_cohort, relay_projection_summary, relay_projection_work_repeats,
        GameDataEncoding, InMemoryMessageCoordinator, RelayProjectionCohort::*,
    };
    use crate::coordination::outbound_queue::DeliveryMessage;
    use crate::coordination::{
        finish_backpressured_delivery_in_room, start_message_delivery_in_room,
        ClientDeliveryHandle, ConnectionCloseSignal, DeliveryOutcome, DeliveryStart,
    };
    use crate::metrics::ServerMetrics;
    use crate::protocol::{PlayerId, ServerMessage};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    #[test]
    fn cache_is_created_only_when_projection_work_repeats() {
        assert!(!relay_projection_work_repeats([
            BinaryDirectV3,
            BinaryFallbackV3,
        ]));
        assert!(!relay_projection_work_repeats([TextV2, TextV3]));
        assert!(relay_projection_work_repeats([
            BinaryDirectV3,
            BinaryDirectV3,
        ]));
        assert!(relay_projection_work_repeats([
            BinaryFallbackV2,
            BinaryFallbackV3,
        ]));

        let binary = ServerMessage::GameDataBinary {
            from_player: Uuid::nil(),
            encoding: GameDataEncoding::MessagePack,
            payload: vec![0xc1].into(),
            seq: Some(1),
            epoch: Some(1),
        };
        assert_eq!(
            relay_projection_cohort(&binary, true, GameDataEncoding::MessagePack),
            Some(BinaryDirectV3)
        );
        assert_eq!(
            relay_projection_cohort(&binary, true, GameDataEncoding::Json),
            Some(BinaryFallbackV3)
        );

        for encoding in [GameDataEncoding::Json, GameDataEncoding::Rkyv] {
            let raw_v2 = ServerMessage::GameDataBinary {
                from_player: Uuid::nil(),
                encoding,
                payload: vec![0x01].into(),
                seq: Some(1),
                epoch: Some(1),
            };
            assert_eq!(
                relay_projection_cohort(&raw_v2, false, encoding),
                None,
                "v2 {encoding:?} raw passthrough has no reusable projection work"
            );
            assert_eq!(
                relay_projection_cohort(&raw_v2, true, encoding),
                Some(BinaryDirectV3),
                "v3 {encoding:?} still needs its stamped MessagePack envelope cached"
            );
        }
    }

    #[test]
    fn repeated_projection_scan_still_observes_a_later_legacy_recipient() {
        let summary =
            relay_projection_summary([(true, Some(TextV3)), (true, Some(TextV3)), (false, None)]);

        assert_eq!(summary, (true, false));
    }

    fn legacy_delivery_handle(
        capacity: usize,
    ) -> (ClientDeliveryHandle, mpsc::Receiver<Arc<ServerMessage>>) {
        let (sender, receiver) = mpsc::channel(capacity);
        (
            ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            receiver,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn bulk_delivery_polls_every_full_wait_before_the_first_resolves() {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(1),
            Arc::clone(&metrics),
        ));
        let (fast, mut fast_receiver) = legacy_delivery_handle(1);
        let (first_full, mut first_full_receiver) = legacy_delivery_handle(1);
        let (second_full, mut second_full_receiver) = legacy_delivery_handle(1);
        for handle in [&first_full, &second_full] {
            handle
                .sender
                .try_send(Arc::new(ServerMessage::Pong), None)
                .expect("full-recipient prefill must fit");
        }

        let task = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move {
                coordinator
                    .deliver_to_all(
                        vec![
                            (PlayerId::from_u128(1), fast),
                            (PlayerId::from_u128(2), first_full),
                            (PlayerId::from_u128(3), second_full),
                        ],
                        Arc::new(ServerMessage::Pong),
                        None,
                    )
                    .await;
            })
        };

        for _ in 0..100 {
            if metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                == 2
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed),
            2,
            "both full recipients must begin their waits concurrently"
        );
        let fast_message = fast_receiver
            .try_recv()
            .expect("fast recipient must resolve before full queues drain");
        assert!(matches!(fast_message.as_ref(), ServerMessage::Pong));
        assert_eq!(
            metrics
                .websocket_deliveries_enqueued
                .load(Ordering::Relaxed),
            1,
            "only the fast recipient has landed before either full queue drains"
        );

        second_full_receiver
            .recv()
            .await
            .expect("second prefill must be readable");
        let mut second_delivery = None;
        for _ in 0..100 {
            match second_full_receiver.try_recv() {
                Ok(delivered) => {
                    second_delivery = Some(delivered);
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty) => tokio::task::yield_now().await,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    panic!("second full recipient disconnected before delivery")
                }
            }
        }
        let second_delivery =
            second_delivery.expect("second full wait must progress while the first remains full");
        assert!(matches!(second_delivery.as_ref(), ServerMessage::Pong));
        assert_eq!(
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
            3
        );
        assert_eq!(
            metrics
                .websocket_deliveries_enqueued
                .load(Ordering::Relaxed),
            2,
            "the fast and independently drained recipients must be enqueued"
        );

        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(
            !task.is_finished(),
            "the first full recipient retains the entire configured grace"
        );
        tokio::time::advance(Duration::from_millis(2)).await;
        task.await.expect("bulk delivery task must not panic");
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            1,
            "the still-full recipient must time out at the shared deadline"
        );
        let first_prefill = first_full_receiver
            .try_recv()
            .expect("the timed-out recipient's prefill remains queued");
        assert!(matches!(first_prefill.as_ref(), ServerMessage::Pong));
    }

    #[tokio::test(start_paused = true)]
    async fn backpressured_bulk_delivery_releases_unrouted_healthy_recipient() {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(1),
            Arc::clone(&metrics),
        ));
        let healthy_player = PlayerId::from_u128(1);
        let blocked_player = PlayerId::from_u128(2);
        let (healthy, mut healthy_receiver) = legacy_delivery_handle(1);
        let (blocked, _blocked_receiver) = legacy_delivery_handle(1);
        blocked
            .sender
            .try_send(Arc::new(ServerMessage::Pong), None)
            .expect("blocked-recipient prefill must fit");
        {
            let mut clients = coordinator.local_clients.write().await;
            clients.insert(healthy_player, healthy);
            clients.insert(blocked_player, blocked);
        }
        let recipients = coordinator
            .local_clients
            .read()
            .await
            .iter()
            .map(|(player_id, handle)| (*player_id, handle.clone()))
            .collect();

        let task = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move {
                coordinator
                    .deliver_to_all(recipients, Arc::new(ServerMessage::Pong), None)
                    .await;
            })
        };

        for _ in 0..100 {
            if metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed),
            1,
            "blocked recipient must enter its capacity wait"
        );
        assert!(
            !task.is_finished(),
            "bulk delivery must still be waiting on the blocked recipient"
        );

        let removed = coordinator
            .local_clients
            .write()
            .await
            .remove(&healthy_player)
            .expect("healthy recipient must still be routed");
        drop(removed);
        let delivered = healthy_receiver
            .try_recv()
            .expect("healthy recipient must receive the broadcast");
        assert!(matches!(delivered.as_ref(), ServerMessage::Pong));
        let terminal = healthy_receiver.try_recv();
        assert!(
            matches!(terminal, Err(mpsc::error::TryRecvError::Disconnected)),
            "unrouting the healthy recipient must terminate its queue while an unrelated peer is \
             still backpressured"
        );

        task.abort();
        let task_error = task
            .await
            .expect_err("aborted blocked delivery must not complete normally");
        assert!(task_error.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn backpressured_delivery_deadline_starts_when_full_is_observed() {
        let metrics = Arc::new(ServerMetrics::new());
        let player_id = PlayerId::from_u128(1);
        let (handle, _receiver) = legacy_delivery_handle(1);
        handle
            .sender
            .try_send(Arc::new(ServerMessage::Pong), None)
            .expect("prefill must fit");
        let pending = match start_message_delivery_in_room(
            &metrics,
            Duration::from_secs(1),
            &player_id,
            &handle,
            DeliveryMessage::new(Arc::new(ServerMessage::Pong)),
            None,
        ) {
            DeliveryStart::Backpressured(pending) => pending,
            DeliveryStart::Complete(outcome) => {
                panic!("full queue unexpectedly completed with {outcome:?}")
            }
        };

        tokio::time::advance(Duration::from_millis(900)).await;
        let task_metrics = Arc::clone(&metrics);
        let task = tokio::spawn(async move {
            finish_backpressured_delivery_in_room(&task_metrics, pending).await
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(101)).await;

        assert_eq!(
            task.await.expect("deadline task must not panic").2,
            DeliveryOutcome::SlowConsumer,
            "only the unspent portion of the original grace may remain"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_backpressured_delivery_cannot_enqueue_after_capacity_returns() {
        let metrics = Arc::new(ServerMetrics::new());
        let player_id = PlayerId::from_u128(1);
        let (handle, mut receiver) = legacy_delivery_handle(1);
        handle
            .sender
            .try_send(Arc::new(ServerMessage::Pong), None)
            .expect("prefill must fit");
        let pending = match start_message_delivery_in_room(
            &metrics,
            Duration::from_secs(1),
            &player_id,
            &handle,
            DeliveryMessage::new(Arc::new(ServerMessage::Pong)),
            None,
        ) {
            DeliveryStart::Backpressured(pending) => pending,
            DeliveryStart::Complete(outcome) => {
                panic!("full queue unexpectedly completed with {outcome:?}")
            }
        };

        tokio::time::advance(Duration::from_millis(1_001)).await;
        let prefill = receiver
            .try_recv()
            .expect("capacity returns only after the deadline");
        assert!(matches!(prefill.as_ref(), ServerMessage::Pong));
        let (_, _, outcome) = finish_backpressured_delivery_in_room(&metrics, pending).await;

        assert_eq!(outcome, DeliveryOutcome::SlowConsumer);
        let late_delivery = receiver.try_recv();
        assert!(
            matches!(late_delivery, Err(mpsc::error::TryRecvError::Empty)),
            "an expired logical delivery must not use capacity returned after its deadline"
        );
        assert_eq!(
            metrics
                .websocket_deliveries_enqueued
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn production_limiters_export_exact_rejection_categories() {
        let config = super::ServerConfig {
            app_id_allowlist_enabled: true,
            rate_limit_config: crate::rate_limit::RateLimitConfig {
                max_room_creations: 0,
                max_join_attempts: 0,
                max_signals: 0,
                max_signal_errors: 0,
                time_window: Duration::from_secs(60),
            },
            ..super::ServerConfig::default()
        };
        let allowed_apps = vec![crate::config::AppRegistrationEntry {
            app_id: "limited".to_string(),
            app_name: "Limited".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: Some(1),
        }];
        let server = super::EnhancedGameServer::new(
            config,
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            crate::config::SessionConfig::default(),
            crate::config::TurnConfig::default(),
            crate::database::DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            allowed_apps,
        )
        .await
        .expect("construct production server");

        assert!(server
            .app_id_allowlist
            .resolve_app_id("limited")
            .await
            .is_ok());
        assert!(server
            .app_id_allowlist
            .resolve_app_id("limited")
            .await
            .is_err());

        let player_id = Uuid::new_v4();
        assert!(server
            .rate_limiter
            .check_room_creation(&player_id)
            .await
            .is_err());
        assert!(server
            .rate_limiter
            .check_join_attempt(&player_id)
            .await
            .is_err());
        assert!(server.rate_limiter.check_signal(&player_id).await.is_err());
        assert!(server
            .rate_limiter
            .check_signal_error(&player_id)
            .await
            .is_err());

        let snapshot = server.metrics.snapshot().await;
        let rate_limits = &snapshot.rate_limiting;
        assert_eq!(rate_limits.rate_limit_rejections, 5);
        assert_eq!(rate_limits.auth_rejections, 1);
        assert_eq!(rate_limits.room_creation_rejections, 1);
        assert_eq!(rate_limits.join_attempt_rejections, 1);
        assert_eq!(rate_limits.signal_rejections, 1);
        assert_eq!(rate_limits.signal_error_rejections, 1);
        assert_eq!(
            rate_limits.rate_limit_rejections,
            rate_limits.auth_rejections
                + rate_limits.room_creation_rejections
                + rate_limits.join_attempt_rejections
                + rate_limits.signal_rejections
                + rate_limits.signal_error_rejections
        );

        let rendered = crate::websocket::prometheus::render_prometheus_metrics(&snapshot);
        for expected in [
            "signal_fish_rate_limit_rejections_total 5",
            "signal_fish_rate_limit_auth_rejections_total 1",
            "signal_fish_rate_limit_room_creation_rejections_total 1",
            "signal_fish_rate_limit_join_attempt_rejections_total 1",
            "signal_fish_rate_limit_signal_rejections_total 1",
            "signal_fish_rate_limit_signal_error_rejections_total 1",
        ] {
            assert!(rendered.contains(expected), "missing {expected}");
        }
        for removed in [
            "signal_fish_rate_limit_resets_total",
            "signal_fish_rate_limit_minute_",
            "signal_fish_rate_limit_hour_",
            "signal_fish_rate_limit_day_",
        ] {
            assert!(!rendered.contains(removed), "stale series {removed}");
        }
    }

    #[tokio::test]
    async fn library_constructor_rejects_unjoinable_generated_room_codes() {
        let config = super::ServerConfig {
            room_code_prefix: Some("EU-".to_string()),
            ..super::ServerConfig::default()
        };
        let result = super::EnhancedGameServer::new(
            config,
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            crate::config::SessionConfig::default(),
            crate::config::TurnConfig::default(),
            crate::database::DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await;

        let error = match result {
            Ok(_) => panic!("invalid prefix must fail before server construction"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("server.room_code_prefix must contain only ASCII alphanumeric"),
            "constructor must report the exact invalid generation field: {error}"
        );
    }
}
