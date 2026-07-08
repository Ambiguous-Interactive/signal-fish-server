use crate::auth::AppInfo;
use crate::config::AppAuthEntry;
use crate::coordination::{
    ClientDeliveryHandle, CloseReason, ConnectionCloseSignal, DeliveryOutcome,
    InMemoryRoomOperationCoordinator, MessageCoordinator, RoomOperationCoordinatorTrait,
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
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, watch, Notify, RwLock};
use tokio::time::Duration;
use uuid::Uuid;

fn chrono_duration_from_std(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX))
}

mod admin;
mod authority;
mod connection_manager;
mod dashboard_cache;
mod game_data;
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

use connection_manager::ConnectionManager;
pub(crate) use connection_manager::{NegotiatedProtocol, TransportStatusUpdate};
use dashboard_cache::{DashboardMetricsCache, DashboardMetricsView};
pub use shutdown::ShutdownDrain;
use spectator_service::SpectatorService;

// Removed unused imports

/// Enhanced GameServer with distributed coordination
pub struct EnhancedGameServer {
    /// In-memory game state storage
    database: Arc<dyn GameDatabase>,
    /// Connection management (clients, IP accounting)
    connection_manager: ConnectionManager,
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
    /// Message coordinator for cross-instance communication
    message_coordinator: Arc<dyn MessageCoordinator>,
    /// Room operation coordinator for distributed state management
    room_coordinator: Arc<dyn RoomOperationCoordinatorTrait>,
    /// Distributed locking system
    distributed_lock: Arc<dyn DistributedLock>,
    /// Instance identifier
    instance_id: Uuid,
    /// Reconnection manager for player reconnection support
    reconnection_manager: Option<Arc<crate::reconnection::ReconnectionManager>>,
    /// Authentication middleware for App ID validation
    pub(crate) auth_middleware: Arc<crate::auth::AuthMiddleware>,
    /// Mapping from room IDs to owning application IDs (for relay policies)
    room_applications: Arc<DashMap<RoomId, Uuid>>,
    /// Sticky per-room session decision recorded at finalize (protocol v3):
    /// consulted by late-join/reconnect pairing and departure re-planning
    /// instead of re-running the selection ladder (see `session_policy.rs`).
    active_session_plans: DashMap<RoomId, session_policy::ActiveSessionPlan>,
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
    pub auth_enabled: bool,
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
            auth_enabled: false, // Disabled by default for backward compatibility
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
        _auth_config: crate::config::AuthMaintenanceConfig,
        _coordination_config: crate::config::CoordinationConfig,
        transport_security: crate::config::TransportSecurityConfig,
        authorized_apps: Vec<AppAuthEntry>,
    ) -> anyhow::Result<Arc<Self>> {
        let database: Arc<dyn GameDatabase> =
            Arc::from(create_database(database_config.clone()).await?);
        database.initialize().await?;

        let instance_id = Uuid::new_v4();

        let rate_limiter = Arc::new(RoomRateLimiter::new(config.rate_limit_config.clone()));
        rate_limiter.clone().start_cleanup_task();

        let metrics = Arc::new(crate::metrics::ServerMetrics::new());

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

        // Setup distributed coordination - in-memory only
        let distributed_lock = Arc::new(InMemoryDistributedLock::new());
        let message_coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_millis(config.websocket_config.slow_consumer_timeout_ms),
            metrics.clone(),
        ));

        let connection_manager = ConnectionManager::new(
            config.max_connections_per_ip,
            metrics.clone(),
            message_coordinator.clone(),
            // Per-connection delivery ledgers exist only when RelayStats
            // emission is enabled, keeping the delivery hot path at a single
            // cheap registry miss otherwise.
            config.websocket_config.delivery_stats_interval_secs > 0,
        );

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

        // Initialize authentication middleware based on configuration.
        let auth_middleware = if config.auth_enabled {
            if authorized_apps.is_empty() {
                tracing::warn!(
                    "Auth is enabled but no authorized_apps are configured; \
                     all authentication attempts will be rejected"
                );
            } else {
                tracing::info!(
                    app_count = authorized_apps.len(),
                    "Auth enabled with configured applications"
                );
            }
            Arc::new(crate::auth::AuthMiddleware::new(authorized_apps))
        } else {
            Arc::new(crate::auth::AuthMiddleware::disabled())
        };

        let room_applications = Arc::new(DashMap::new());
        let spectator_service = SpectatorService::new(
            database.clone(),
            message_coordinator.clone(),
            room_applications.clone(),
            protocol_config.clone(),
            reconnection_manager.clone(),
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
            auth_middleware,
            room_applications,
            active_session_plans: DashMap::new(),
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
        room_codes::generate_region_room_code(
            &self.protocol_config,
            self.config.room_code_prefix.as_deref(),
        )
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

    /// Attach authenticated application context to a connected client.
    pub fn set_client_app_info(&self, player_id: &PlayerId, app_info: AppInfo) {
        self.connection_manager.set_app_info(player_id, app_info);
    }

    /// Fetch full application info for a connected client, if known.
    pub fn client_app_info(&self, player_id: &PlayerId) -> Option<AppInfo> {
        self.connection_manager.app_info(player_id)
    }

    /// Fetch just the application UUID for a connected client.
    pub fn client_app_id(&self, player_id: &PlayerId) -> Option<Uuid> {
        self.connection_manager.app_id(player_id)
    }

    /// Persist a room -> application mapping for relay enforcement.
    pub async fn record_room_application(&self, room_id: &RoomId, app_id: Uuid) {
        self.room_applications.insert(*room_id, app_id);
        if let Err(err) = self.database.set_room_application_id(room_id, app_id).await {
            tracing::warn!(
                %room_id,
                app_id = %app_id,
                error = %err,
                "Failed to persist room application mapping"
            );
        }
    }

    /// Lookup the owning application for a room, if any.
    pub fn room_application_id(&self, room_id: &RoomId) -> Option<Uuid> {
        self.room_applications
            .get(room_id)
            .map(|entry| *entry.value())
    }

    /// Remove the room -> application mapping when a room is deleted.
    pub async fn clear_room_application(&self, room_id: &RoomId) {
        self.room_applications.remove(room_id);
        if let Err(err) = self.database.clear_room_application_id(room_id).await {
            tracing::warn!(
                %room_id,
                error = %err,
                "Failed to clear persisted room application mapping"
            );
        }
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
        // SAFETY: Parsing a valid string literal — this can never fail.
        #[allow(clippy::unwrap_used)]
        let addr = "127.0.0.1:0".parse().unwrap();
        self.connection_manager
            .connect_test_client(player_id, sender, addr)
            .await;
        tracing::info!(%player_id, instance_id = %self.instance_id, "Client connected");
    }

    /// Assign a connected client to a room (used by integration tests that hydrate server state).
    pub async fn assign_client_to_room(&self, player_id: &PlayerId, room_id: RoomId) {
        self.connection_manager
            .assign_client_to_room(player_id, room_id)
            .await;
    }

    /// Disconnect a client (alias for unregister_client for testing compatibility)
    pub async fn disconnect_client(&self, player_id: &PlayerId) {
        self.unregister_client(player_id).await;
    }

    /// Unregister a client connection
    pub async fn unregister_client(&self, player_id: &PlayerId) {
        // Check if player is in a room and register for reconnection
        let (room_id_opt, was_authority) = {
            let room_id = self.get_client_room(player_id).await;
            let was_authority = if let Some(ref room_id) = room_id {
                if let Ok(Some(room)) = self.database.get_room_by_id(room_id).await {
                    room.authority_player == Some(*player_id)
                } else {
                    false
                }
            } else {
                false
            };
            (room_id, was_authority)
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
        } else if let Some(room_id) = room_id_opt {
            self.register_disconnection_for_reconnect(player_id, room_id, was_authority)
                .await;
            registered_reconnect = true;
        } else {
            // No room to reconnect into: any token pre-issued at an earlier
            // join must not outlive the connection (bounded-map contract).
            self.discard_pre_issued_reconnection_token(player_id).await;
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
                self.leave_room(player_id).await;
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
    metrics: Arc<crate::metrics::ServerMetrics>,
    slow_consumer_timeout: Duration,
    #[allow(dead_code)]
    instance_id: Uuid,
}

enum ConditionalDeliveryReservation {
    Reserved {
        player_id: PlayerId,
        sender: mpsc::Sender<Arc<ServerMessage>>,
        permit: mpsc::OwnedPermit<Arc<ServerMessage>>,
        stats: Option<Arc<ConnectionDeliveryStats>>,
    },
    ChannelClosed {
        player_id: PlayerId,
        sender: mpsc::Sender<Arc<ServerMessage>>,
    },
    SlowConsumer(PlayerId),
    Canceled,
}

use std::collections::HashSet;

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
            metrics,
            slow_consumer_timeout,
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
    ) {
        if recipients.is_empty() {
            return;
        }

        let outcomes =
            futures_util::future::join_all(recipients.iter().map(|(player_id, handle)| {
                let message = Arc::clone(&message);
                async move {
                    let outcome = crate::coordination::deliver_or_disconnect(
                        &self.metrics,
                        self.slow_consumer_timeout,
                        player_id,
                        handle,
                        message,
                    )
                    .await;
                    (*player_id, outcome)
                }
            }))
            .await;

        let slow_consumers: Vec<PlayerId> = outcomes
            .into_iter()
            .filter(|(_, outcome)| *outcome == DeliveryOutcome::SlowConsumer)
            .map(|(player_id, _)| player_id)
            .collect();

        if !slow_consumers.is_empty() {
            // Remove immediately so senders stop paying the timeout for a
            // connection that is already closing; the connection's own
            // unregister flow performs the full cleanup (room membership,
            // reconnection window, peer notifications).
            let mut clients = self.local_clients.write().await;
            for player_id in &slow_consumers {
                clients.remove(player_id);
            }
        }
    }

    /// Snapshot the delivery handles for a room's members (optionally skipping
    /// one player) and release both locks before any await on delivery, so a
    /// backpressured recipient can never stall registration or other
    /// broadcasts through held locks.
    async fn collect_room_recipients(
        &self,
        room_id: &RoomId,
        except_player: Option<&PlayerId>,
    ) -> Vec<(PlayerId, ClientDeliveryHandle)> {
        // Lock ordering: room_players first, then local_clients (matches
        // register/unregister to prevent ABBA deadlocks).
        let room_players = self.room_players.read().await;
        let clients = self.local_clients.read().await;

        room_players
            .get(room_id)
            .map(|players| {
                players
                    .iter()
                    .filter(|player_id| Some(*player_id) != except_player)
                    .filter_map(|player_id| {
                        clients
                            .get(player_id)
                            .map(|handle| (*player_id, handle.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default()
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
        let message = match handle.sender.try_send(message) {
            Ok(()) => {
                self.metrics.increment_websocket_deliveries_enqueued();
                if let Some(stats) = &connection_stats {
                    stats
                        .sent_to_you
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                return Some(DeliveryOutcome::Delivered);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                tracing::debug!(
                    %player_id,
                    "Recipient connection already closing; message unroutable"
                );
                return Some(DeliveryOutcome::ChannelClosed);
            }
            Err(mpsc::error::TrySendError::Full(message)) => message,
        };

        self.metrics.increment_websocket_backpressure_events();
        if let Some(stats) = &connection_stats {
            stats
                .backpressure_events
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        let reserve = handle.sender.reserve();
        tokio::pin!(reserve);
        let timeout = tokio::time::sleep(self.slow_consumer_timeout);
        tokio::pin!(timeout);

        tokio::select! {
            result = &mut reserve => match result {
                Ok(permit) => {
                    if *drain.borrow() || !should_send() {
                        self.record_canceled_delivery(player_id);
                        return None;
                    }
                    permit.send(message);
                    self.metrics.increment_websocket_deliveries_enqueued();
                    if let Some(stats) = &connection_stats {
                        stats
                            .sent_to_you
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Some(DeliveryOutcome::Delivered)
                }
                Err(_receiver_gone) => {
                    self.metrics.increment_websocket_deliveries_channel_closed();
                    tracing::debug!(%player_id, "Recipient connection closed while backpressured");
                    Some(DeliveryOutcome::ChannelClosed)
                }
            },
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
                    timeout_ms = self.slow_consumer_timeout.as_millis() as u64,
                    initiated_close,
                    "Outbound queue full past the slow-consumer timeout; disconnecting recipient \
                     instead of silently dropping messages"
                );
                Some(DeliveryOutcome::SlowConsumer)
            }
        }
    }

    async fn reserve_one_if(
        &self,
        player_id: PlayerId,
        handle: ClientDeliveryHandle,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        mut drain: watch::Receiver<bool>,
    ) -> ConditionalDeliveryReservation {
        if *drain.borrow() || !should_send() {
            return ConditionalDeliveryReservation::Canceled;
        }

        self.metrics.increment_websocket_delivery_attempts();
        let stats = self.metrics.connection_delivery_stats(&player_id);
        let sender = handle.sender.clone();
        match sender.clone().try_reserve_owned() {
            Ok(permit) => ConditionalDeliveryReservation::Reserved {
                player_id,
                sender,
                permit,
                stats,
            },
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                tracing::debug!(
                    %player_id,
                    "Recipient connection already closing; message unroutable"
                );
                ConditionalDeliveryReservation::ChannelClosed { player_id, sender }
            }
            Err(mpsc::error::TrySendError::Full(sender)) => {
                self.metrics.increment_websocket_backpressure_events();
                if let Some(stats) = &stats {
                    stats
                        .backpressure_events
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }

                let reserved_sender = sender.clone();
                let reserve = sender.reserve_owned();
                tokio::pin!(reserve);
                let timeout = tokio::time::sleep(self.slow_consumer_timeout);
                tokio::pin!(timeout);

                tokio::select! {
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
                        Err(_receiver_gone) => {
                            self.metrics.increment_websocket_deliveries_channel_closed();
                            tracing::debug!(%player_id, "Recipient connection closed while backpressured");
                            ConditionalDeliveryReservation::ChannelClosed {
                                player_id,
                                sender: reserved_sender.clone(),
                            }
                        }
                    },
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
                            timeout_ms = self.slow_consumer_timeout.as_millis() as u64,
                            initiated_close,
                            "Outbound queue full past the slow-consumer timeout; disconnecting recipient \
                             instead of silently dropping messages"
                        );
                        ConditionalDeliveryReservation::SlowConsumer(player_id)
                    }
                }
            }
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
                    ConditionalDeliveryReservation::SlowConsumer(_)
                    | ConditionalDeliveryReservation::Canceled => false,
                })
            })
    }
}

#[async_trait::async_trait]
impl MessageCoordinator for InMemoryMessageCoordinator {
    async fn send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        let handle = { self.local_clients.read().await.get(player_id).cloned() };
        if let Some(handle) = handle {
            self.deliver_to_all(vec![(*player_id, handle)], message)
                .await;
        } else {
            // Normal during disconnect races (e.g. a room notification issued
            // while the target is unregistering); nothing to deliver to.
            tracing::debug!(%player_id, "Player not registered with coordinator; message unroutable");
        }
        Ok(())
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
        let outcome = self
            .deliver_to_one_if(*player_id, handle, message, should_send, drain)
            .await;
        if outcome == Some(DeliveryOutcome::SlowConsumer) {
            self.local_clients.write().await.remove(player_id);
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
        match handle.sender.try_send(message) {
            Ok(()) => Ok(true),
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Advisory frame to a connection that is being closed anyway:
                // do not wait, do not escalate, do not overwrite the close
                // reason. The teardown itself is the loud signal.
                tracing::debug!(
                    %player_id,
                    "Farewell skipped: outbound queue full on a closing connection"
                );
                Ok(false)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
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
        match handle.sender.try_send(message) {
            Ok(()) => Ok(true),
            Err(mpsc::error::TrySendError::Full(_)) => {
                tracing::debug!(
                    %player_id,
                    "Farewell skipped: outbound queue full on a closing connection"
                );
                Ok(false)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
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
        self.deliver_to_all(recipients, message).await;
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
        self.deliver_to_all(recipients, message).await;
        Ok(())
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
        let mut before_send = Some(before_send);

        loop {
            if *drain.borrow() || !should_send() {
                tracing::debug!(%room_id, %except_player, "Conditional room broadcast skipped: caller state changed before replay hook");
                return Ok(false);
            }

            let recipients = self
                .collect_room_recipients(room_id, Some(except_player))
                .await;

            let reservations =
                futures_util::future::join_all(recipients.iter().map(|(player_id, handle)| {
                    self.reserve_one_if(*player_id, handle.clone(), should_send, drain.clone())
                }))
                .await;

            if reservations
                .iter()
                .any(|reservation| matches!(reservation, ConditionalDeliveryReservation::Canceled))
                || *drain.borrow()
                || !should_send()
            {
                self.record_reserved_cancellations(&reservations);
                tracing::debug!(%room_id, %except_player, "Conditional room broadcast canceled before replay record");
                return Ok(false);
            }

            let slow_consumers: Vec<PlayerId> = reservations
                .iter()
                .filter_map(|reservation| match reservation {
                    ConditionalDeliveryReservation::SlowConsumer(player_id) => Some(*player_id),
                    ConditionalDeliveryReservation::Reserved { .. }
                    | ConditionalDeliveryReservation::ChannelClosed { .. }
                    | ConditionalDeliveryReservation::Canceled => None,
                })
                .collect();
            if !slow_consumers.is_empty() {
                let mut clients = self.local_clients.write().await;
                for player_id in &slow_consumers {
                    clients.remove(player_id);
                }
                continue;
            }

            let room_players = self.room_players.read().await;
            let clients = self.local_clients.read().await;
            let current_recipients: Vec<(PlayerId, ClientDeliveryHandle)> = room_players
                .get(room_id)
                .map(|players| {
                    players
                        .iter()
                        .filter(|player_id| *player_id != except_player)
                        .filter_map(|player_id| {
                            clients
                                .get(player_id)
                                .map(|handle| (*player_id, handle.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();

            if !Self::reservations_cover_recipients(&reservations, &current_recipients) {
                self.record_reserved_cancellations(&reservations);
                drop(clients);
                drop(room_players);
                continue;
            }

            if *drain.borrow() || !should_send() {
                self.record_reserved_cancellations(&reservations);
                tracing::debug!(%room_id, %except_player, "Conditional room broadcast canceled before replay record");
                return Ok(false);
            }

            // No capacity wait happens while these routing locks are held.
            // Once this final drain check passes, the broadcast is committed:
            // the hook and permit sends are kept in the same critical section
            // so reconnect registration cannot observe a baseline between
            // replay recording and live delivery.
            let Some(before_send) = before_send.take() else {
                tracing::error!(%room_id, %except_player, "Conditional room broadcast replay hook was already consumed");
                return Ok(false);
            };
            // The hook runs while the routing locks above are still held.
            // Keep hook implementations from calling back into MessageCoordinator
            // or awaiting work that depends on these locks.
            before_send().await;

            let mut delivered = false;
            for reservation in reservations {
                match reservation {
                    ConditionalDeliveryReservation::Reserved { permit, stats, .. } => {
                        permit.send(Arc::clone(&message));
                        delivered = true;
                        self.metrics.increment_websocket_deliveries_enqueued();
                        if let Some(stats) = &stats {
                            stats
                                .sent_to_you
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    ConditionalDeliveryReservation::ChannelClosed { .. } => {}
                    ConditionalDeliveryReservation::SlowConsumer(_)
                    | ConditionalDeliveryReservation::Canceled => {
                        tracing::debug!(
                            %room_id,
                            %except_player,
                            "Conditional room broadcast canceled by stale reservation state"
                        );
                        return Ok(false);
                    }
                }
            }

            return Ok(delivered);
        }
    }

    async fn broadcast_to_room_except_with_message<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        build_message: Box<dyn FnOnce() -> Arc<ServerMessage> + Send + 'a>,
    ) -> anyhow::Result<()> {
        // Lock ordering matches `collect_room_recipients` and reconnect's
        // initial-message registration path. Holding these read locks while
        // `build_message` allocates a relay stamp makes stamp allocation and
        // recipient snapshot one ordered operation relative to reconnect
        // baselines, which take the write side of the same room lock.
        let room_players = self.room_players.read().await;
        let clients = self.local_clients.read().await;

        let recipients = room_players
            .get(room_id)
            .map(|players| {
                players
                    .iter()
                    .filter(|player_id| *player_id != except_player)
                    .filter_map(|player_id| {
                        clients
                            .get(player_id)
                            .map(|handle| (*player_id, handle.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let message = build_message();
        drop(clients);
        drop(room_players);

        self.deliver_to_all(recipients, message).await;
        Ok(())
    }

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        delivery: ClientDeliveryHandle,
    ) -> anyhow::Result<()> {
        // Lock ordering: room_players first, then local_clients
        // (consistent with broadcast_to_room / broadcast_to_room_except read paths
        //  to prevent ABBA deadlocks)
        if let Some(room_id) = room_id {
            let mut room_players = self.room_players.write().await;
            room_players
                .entry(room_id)
                .or_insert_with(HashSet::new)
                .insert(player_id);
            let mut clients = self.local_clients.write().await;
            clients.insert(player_id, delivery);
        } else {
            // No room_players lock needed when room_id is None
            let mut clients = self.local_clients.write().await;
            clients.insert(player_id, delivery);
        }
        Ok(())
    }

    async fn register_local_client_with_initial_message<'a>(
        &'a self,
        player_id: PlayerId,
        room_id: RoomId,
        delivery: ClientDeliveryHandle,
        build_message: Box<dyn FnOnce() -> Arc<ServerMessage> + Send + 'a>,
    ) -> anyhow::Result<DeliveryOutcome> {
        // Lock ordering matches `register_local_client` and
        // `collect_room_recipients`. While this write lock is held, broadcasts
        // cannot snapshot room recipients. The reconnect path uses that to:
        // (1) wait for every pre-existing broadcast snapshot to finish, (2)
        // capture the sender watermarks, (3) queue `Reconnected`, and (4)
        // register the player into the room before later broadcasts can route.
        let mut room_players = self.room_players.write().await;
        let mut clients = self.local_clients.write().await;

        self.metrics.increment_websocket_delivery_attempts();
        let stats = self.metrics.connection_delivery_stats(&player_id);
        let outcome = match delivery.sender.try_send(build_message()) {
            Ok(()) => {
                self.metrics.increment_websocket_deliveries_enqueued();
                if let Some(stats) = &stats {
                    stats
                        .sent_to_you
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                room_players
                    .entry(room_id)
                    .or_insert_with(HashSet::new)
                    .insert(player_id);
                clients.insert(player_id, delivery);
                DeliveryOutcome::Delivered
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                tracing::debug!(
                    %player_id,
                    %room_id,
                    "Initial room message skipped: recipient connection already closing"
                );
                DeliveryOutcome::ChannelClosed
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.metrics.increment_websocket_backpressure_events();
                let initiated_close = delivery.close.request_close(CloseReason::SlowConsumer);
                if initiated_close {
                    self.metrics.increment_websocket_slow_consumer_disconnects();
                }
                self.metrics.increment_websocket_messages_dropped();
                if let Some(stats) = &stats {
                    stats
                        .backpressure_events
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    stats
                        .dropped_for_you
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                tracing::warn!(
                    %player_id,
                    %room_id,
                    initiated_close,
                    "Initial room message queue was full; closing recipient instead of registering it"
                );
                DeliveryOutcome::SlowConsumer
            }
        };

        Ok(outcome)
    }

    async fn register_local_client_with_initial_message_async<'a>(
        &'a self,
        player_id: PlayerId,
        room_id: RoomId,
        delivery: ClientDeliveryHandle,
        build_message: Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Arc<ServerMessage>> + Send + 'a>,
                > + Send
                + 'a,
        >,
    ) -> anyhow::Result<DeliveryOutcome> {
        // Same lock ordering and routing transition as the sync builder, but
        // the async builder itself is part of the critical section. Reconnect
        // uses this to fetch replay after every older broadcast has either
        // recorded or skipped its event, and before any newer broadcast can
        // snapshot this restored socket as live.
        let mut room_players = self.room_players.write().await;
        let mut clients = self.local_clients.write().await;

        let message = build_message().await;
        self.metrics.increment_websocket_delivery_attempts();
        let stats = self.metrics.connection_delivery_stats(&player_id);
        let outcome = match delivery.sender.try_send(message) {
            Ok(()) => {
                self.metrics.increment_websocket_deliveries_enqueued();
                if let Some(stats) = &stats {
                    stats
                        .sent_to_you
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                room_players
                    .entry(room_id)
                    .or_insert_with(HashSet::new)
                    .insert(player_id);
                clients.insert(player_id, delivery);
                DeliveryOutcome::Delivered
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.metrics.increment_websocket_deliveries_channel_closed();
                tracing::debug!(
                    %player_id,
                    %room_id,
                    "Initial room message skipped: recipient connection already closing"
                );
                DeliveryOutcome::ChannelClosed
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.metrics.increment_websocket_backpressure_events();
                let initiated_close = delivery.close.request_close(CloseReason::SlowConsumer);
                if initiated_close {
                    self.metrics.increment_websocket_slow_consumer_disconnects();
                }
                self.metrics.increment_websocket_messages_dropped();
                if let Some(stats) = &stats {
                    stats
                        .backpressure_events
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    stats
                        .dropped_for_you
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                tracing::warn!(
                    %player_id,
                    %room_id,
                    initiated_close,
                    "Initial room message queue was full; closing recipient instead of registering it"
                );
                DeliveryOutcome::SlowConsumer
            }
        };

        Ok(outcome)
    }

    async fn unregister_local_client(&self, player_id: &PlayerId) -> anyhow::Result<()> {
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
