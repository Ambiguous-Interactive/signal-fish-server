use crate::auth::AppInfo;
use crate::config::AppAuthEntry;
use crate::coordination::{
    InMemoryRoomOperationCoordinator, MessageCoordinator, RoomOperationCoordinatorTrait,
};
use crate::database::{create_database, DatabaseConfig, GameDatabase};
use crate::distributed::{DistributedLock, InMemoryDistributedLock};
use crate::protocol::{
    room_codes, GameDataEncoding, PlayerId, RoomId, ServerMessage, SpectatorStateChangeReason,
};
use crate::rate_limit::{RateLimitConfig, RoomRateLimiter};
use anyhow::Result;
use dashmap::DashMap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, RwLock};
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
mod spectator_handlers;
mod spectator_service;

use connection_manager::ConnectionManager;
use dashboard_cache::{DashboardMetricsCache, DashboardMetricsView};
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
    /// Spectator lifecycle manager
    spectator_service: SpectatorService,
    /// Transport-level security options (TLS, token binding, etc.)
    transport_security: crate::config::TransportSecurityConfig,
    /// Cached metrics used by the admin dashboard
    dashboard_metrics_cache: Arc<DashboardMetricsCache>,
}

#[derive(Debug, Error)]
pub enum RegisterClientError {
    #[error("Too many connections from your IP ({current}/{limit})")]
    IpLimitExceeded { current: usize, limit: usize },
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
    pub max_rooms_per_game: usize,
    pub rate_limit_config: RateLimitConfig,
    pub empty_room_timeout: Duration,
    pub inactive_room_timeout: Duration,
    pub max_message_size: usize,
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
            max_rooms_per_game: 1000,
            rate_limit_config: RateLimitConfig::default(),
            empty_room_timeout: Duration::from_secs(300),
            inactive_room_timeout: Duration::from_secs(3600),
            max_message_size: 65536, // 64KB
            max_connections_per_ip: 10,
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
        let message_coordinator = Arc::new(InMemoryMessageCoordinator::new());

        let connection_manager = ConnectionManager::new(
            config.max_connections_per_ip,
            metrics.clone(),
            message_coordinator.clone(),
        );

        let room_coordinator: Arc<dyn RoomOperationCoordinatorTrait> =
            Arc::new(InMemoryRoomOperationCoordinator::new(
                message_coordinator.clone(),
                distributed_lock.clone(),
                database.clone(),
            ));

        // Initialize reconnection manager if enabled (in-memory only)
        let reconnection_manager = if config.enable_reconnection {
            Some(Arc::new(crate::reconnection::ReconnectionManager::new(
                config.reconnection_window.as_secs(),
                config.event_buffer_size,
                metrics.clone(),
            )))
        } else {
            None
        };

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
        );

        let server = Arc::new(Self {
            database,
            connection_manager,
            config,
            protocol_config,
            relay_type_config,
            rate_limiter,
            metrics,
            message_coordinator,
            room_coordinator,
            distributed_lock,
            instance_id,
            reconnection_manager,
            auth_middleware,
            room_applications,
            spectator_service,
            transport_security,
            dashboard_metrics_cache: dashboard_metrics_cache.clone(),
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

    /// Register a new client connection
    pub async fn register_client(
        &self,
        sender: mpsc::Sender<Arc<ServerMessage>>,
        client_addr: SocketAddr,
    ) -> Result<PlayerId, RegisterClientError> {
        self.connection_manager
            .register_client(sender, client_addr, self.instance_id)
            .await
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
        let _ = self
            .spectator_service
            .detach(player_id, SpectatorStateChangeReason::Disconnected)
            .await;

        // Register disconnection for potential reconnection (before removing from room)
        if let Some(room_id) = room_id_opt {
            self.register_disconnection_for_reconnect(player_id, room_id, was_authority)
                .await;
        }

        // Remove from room if joined
        if let Some(room_id) = room_id_opt {
            tracing::info!(%player_id, %room_id, "Removing player from room during unregister");
            self.leave_room(player_id).await;
            // Note: We previously had a sleep here, but it's been removed to eliminate sleeps from production code
            // Tests should properly handle the asynchronous nature of message delivery
        }

        // Remove client connection
        if self.connection_manager.remove_client(player_id).is_some() {
            self.metrics.decrement_active_connections();
        }

        // Unregister from message coordinator
        if let Err(e) = self
            .message_coordinator
            .unregister_local_client(player_id)
            .await
        {
            tracing::warn!(%player_id, "Failed to unregister client from coordinator: {}", e);
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

/// In-memory message coordinator for testing
pub struct InMemoryMessageCoordinator {
    local_clients: Arc<RwLock<HashMap<PlayerId, mpsc::Sender<Arc<ServerMessage>>>>>,
    room_players: Arc<RwLock<HashMap<RoomId, HashSet<PlayerId>>>>,
    #[allow(dead_code)]
    instance_id: Uuid,
}

use std::collections::HashSet;

impl InMemoryMessageCoordinator {
    pub fn new() -> Self {
        Self {
            local_clients: Arc::new(RwLock::new(HashMap::new())),
            room_players: Arc::new(RwLock::new(HashMap::new())),
            instance_id: Uuid::new_v4(),
        }
    }
}

#[async_trait::async_trait]
impl MessageCoordinator for InMemoryMessageCoordinator {
    async fn send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        let clients = self.local_clients.read().await;
        if let Some(sender) = clients.get(player_id) {
            if sender.try_send(Arc::clone(&message)).is_err() {
                tracing::warn!(%player_id, "Failed to send message to local client");
            }
            tracing::info!(%player_id, ?message, "Message sent to player");
        } else {
            tracing::warn!(%player_id, ?message, "Player not found in local_clients map, message not sent");
        }
        Ok(())
    }

    async fn broadcast_to_room(
        &self,
        room_id: &RoomId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        let room_players = self.room_players.read().await;
        let clients = self.local_clients.read().await;

        if let Some(players) = room_players.get(room_id) {
            for player_id in players {
                if let Some(sender) = clients.get(player_id) {
                    if sender.try_send(Arc::clone(&message)).is_err() {
                        tracing::warn!(%player_id, "Failed to broadcast message to player in room");
                    }
                }
            }
        }
        Ok(())
    }

    async fn broadcast_to_room_except(
        &self,
        room_id: &RoomId,
        except_player: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        let room_players = self.room_players.read().await;
        let clients = self.local_clients.read().await;

        if let Some(players) = room_players.get(room_id) {
            for player_id in players {
                if player_id != except_player {
                    if let Some(sender) = clients.get(player_id) {
                        if sender.try_send(Arc::clone(&message)).is_err() {
                            tracing::warn!(%player_id, "Failed to broadcast message to player in room");
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        sender: mpsc::Sender<Arc<ServerMessage>>,
    ) -> anyhow::Result<()> {
        let mut clients = self.local_clients.write().await;
        clients.insert(player_id, sender);

        if let Some(room_id) = room_id {
            let mut room_players = self.room_players.write().await;
            room_players
                .entry(room_id)
                .or_insert_with(HashSet::new)
                .insert(player_id);
        }
        Ok(())
    }

    async fn unregister_local_client(&self, player_id: &PlayerId) -> anyhow::Result<()> {
        let mut clients = self.local_clients.write().await;
        clients.remove(player_id);

        // Remove from all rooms
        let mut room_players = self.room_players.write().await;
        room_players.retain(|_, players| {
            players.remove(player_id);
            !players.is_empty()
        });

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
