use super::connection_manager::RelayStamp;
use super::*;
use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig, TurnConfig,
};
use crate::coordination::{
    ClientDeliveryHandle, MessageCoordinator, RoomEventCompletion, RoomEventJob,
    RoomEventMutationGuard, RoomEventSequencer,
};
use crate::database::{
    create_database, DatabaseConfig, GameDatabase, InMemoryDatabase, RoomCleanupOutcome,
};
use crate::protocol::{
    ConnectionInfo, ErrorCode, LobbyState, PlayerId, PlayerInfo, Room, RoomId, ServerMessage,
    SpectatorInfo,
};
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use tokio::sync::{mpsc, watch, Notify, RwLock};
use tokio::time::{timeout, Duration};

async fn create_test_server() -> Arc<EnhancedGameServer> {
    create_test_server_with_config(ServerConfig::default()).await
}

async fn create_test_server_with_config(config: ServerConfig) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        TurnConfig::default(),
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        AuthMaintenanceConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

async fn create_test_server_with_message_coordinator(
    config: ServerConfig,
    message_coordinator: Arc<dyn MessageCoordinator>,
) -> Arc<EnhancedGameServer> {
    let distributed_lock: Arc<dyn DistributedLock> = Arc::new(InMemoryDistributedLock::new());
    let database = create_test_database().await;
    create_test_server_with_message_coordinator_and_lock(
        config,
        message_coordinator,
        distributed_lock,
        database,
    )
    .await
}

async fn create_test_database() -> Arc<dyn GameDatabase> {
    let database: Arc<dyn GameDatabase> = Arc::from(
        create_database(DatabaseConfig::InMemory)
            .await
            .expect("failed to create test database"),
    );
    database
        .initialize()
        .await
        .expect("failed to initialize test database");
    database
}

async fn create_test_server_with_message_coordinator_and_lock(
    config: ServerConfig,
    message_coordinator: Arc<dyn MessageCoordinator>,
    distributed_lock: Arc<dyn DistributedLock>,
    database: Arc<dyn GameDatabase>,
) -> Arc<EnhancedGameServer> {
    let instance_id = uuid::Uuid::new_v4();
    let metrics = Arc::new(crate::metrics::ServerMetrics::new());
    let metrics_config = MetricsConfig::default();
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
        Arc::clone(&metrics),
        history_capacity,
        &metrics_config.dashboard_cache_history_fields,
    ));
    dashboard_metrics_cache.spawn(Arc::clone(&database));

    let rate_limiter = Arc::new(RoomRateLimiter::new(config.rate_limit_config.clone()));
    Arc::clone(&rate_limiter).start_cleanup_task();

    let connection_manager = Arc::new(ConnectionManager::new(
        config.max_connections_per_ip,
        Arc::clone(&metrics),
        Arc::clone(&message_coordinator),
        config.websocket_config.delivery_stats_interval_secs > 0,
    ));
    let reconnection_manager = if config.enable_reconnection {
        Some(Arc::new(crate::reconnection::ReconnectionManager::new(
            config.reconnection_window.as_secs(),
            config.event_buffer_size,
            Arc::clone(&metrics),
        )))
    } else {
        None
    };
    let room_coordinator: Arc<dyn RoomOperationCoordinatorTrait> =
        Arc::new(InMemoryRoomOperationCoordinator::new(
            Arc::clone(&message_coordinator),
            Arc::clone(&distributed_lock),
            Arc::clone(&database),
            reconnection_manager.clone(),
        ));
    let protocol_config = ProtocolConfig::default();
    let room_applications = Arc::new(DashMap::new());
    let spectator_service = SpectatorService::new(
        Arc::clone(&database),
        Arc::clone(&message_coordinator),
        Arc::clone(&room_applications),
        protocol_config.clone(),
        reconnection_manager.clone(),
        Arc::clone(&connection_manager),
    );

    let (shutdown_drain_tx, _) = watch::channel(false);
    Arc::new(EnhancedGameServer {
        database,
        connection_manager,
        config,
        protocol_config,
        relay_type_config: RelayTypeConfig::default(),
        session_config: SessionConfig::default(),
        turn_config: TurnConfig::default(),
        rate_limiter,
        metrics,
        message_coordinator,
        room_coordinator,
        distributed_lock,
        instance_id,
        reconnection_manager,
        auth_middleware: Arc::new(crate::auth::AuthMiddleware::disabled()),
        room_applications,
        active_session_plans: Arc::new(DashMap::new()),
        pending_durable_player_detaches: Arc::new(DashMap::new()),
        fail_retain_room_publication_snapshot: AtomicBool::new(false),
        reconnect_teardown_test_gate: StdMutex::new(None),
        spectator_service,
        transport_security: TransportSecurityConfig::default(),
        dashboard_metrics_cache,
        shutdown_drain_deadline_ms: AtomicU64::new(0),
        shutdown_drain_tx,
        active_socket_tasks: AtomicUsize::new(0),
        active_socket_tasks_notify: Notify::new(),
    })
}

struct DrainOnLockAcquire {
    key: String,
    inner: InMemoryDistributedLock,
    server: StdMutex<Option<Weak<EnhancedGameServer>>>,
    triggered: AtomicBool,
}

impl DrainOnLockAcquire {
    fn new(key: &str) -> Self {
        Self {
            key: key.to_string(),
            inner: InMemoryDistributedLock::new(),
            server: StdMutex::new(None),
            triggered: AtomicBool::new(false),
        }
    }

    fn attach_server(&self, server: &Arc<EnhancedGameServer>) {
        *self
            .server
            .lock()
            .expect("drain trigger server lock poisoned") = Some(Arc::downgrade(server));
    }

    fn begin_drain_after_matching_acquire(&self, key: &str) {
        if key != self.key || self.triggered.swap(true, Ordering::AcqRel) {
            return;
        }
        let server = self
            .server
            .lock()
            .expect("drain trigger server lock poisoned")
            .clone()
            .and_then(|server| server.upgrade())
            .expect("test lock must be attached before triggering drain");
        server.begin_shutdown_drain();
    }
}

#[async_trait::async_trait]
impl DistributedLock for DrainOnLockAcquire {
    async fn acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> anyhow::Result<crate::distributed::LockHandle> {
        let handle = self.inner.acquire(key, ttl).await?;
        self.begin_drain_after_matching_acquire(key);
        Ok(handle)
    }

    async fn try_acquire(
        &self,
        key: &str,
        ttl: Duration,
    ) -> anyhow::Result<Option<crate::distributed::LockHandle>> {
        let handle = self.inner.try_acquire(key, ttl).await?;
        if handle.is_some() {
            self.begin_drain_after_matching_acquire(key);
        }
        Ok(handle)
    }

    async fn extend(
        &self,
        handle: &crate::distributed::LockHandle,
        ttl: Duration,
    ) -> anyhow::Result<bool> {
        self.inner.extend(handle, ttl).await
    }

    async fn release(&self, handle: &crate::distributed::LockHandle) -> anyhow::Result<bool> {
        self.inner.release(handle).await
    }

    async fn is_locked(&self, key: &str) -> anyhow::Result<bool> {
        self.inner.is_locked(key).await
    }

    async fn cleanup_expired_locks(&self) -> anyhow::Result<usize> {
        self.inner.cleanup_expired_locks().await
    }
}

struct DrainAfterCreateDatabase {
    inner: Arc<dyn GameDatabase>,
    server: StdMutex<Option<Weak<EnhancedGameServer>>>,
    triggered: AtomicBool,
}

impl DrainAfterCreateDatabase {
    fn new(inner: Arc<dyn GameDatabase>) -> Self {
        Self {
            inner,
            server: StdMutex::new(None),
            triggered: AtomicBool::new(false),
        }
    }

    fn attach_server(&self, server: &Arc<EnhancedGameServer>) {
        *self
            .server
            .lock()
            .expect("drain trigger server lock poisoned") = Some(Arc::downgrade(server));
    }

    fn begin_drain_once(&self) {
        if self.triggered.swap(true, Ordering::AcqRel) {
            return;
        }
        let server = self
            .server
            .lock()
            .expect("drain trigger server lock poisoned")
            .clone()
            .and_then(|server| server.upgrade())
            .expect("test database must be attached before triggering drain");
        server.begin_shutdown_drain();
    }
}

#[async_trait::async_trait]
impl GameDatabase for DrainAfterCreateDatabase {
    async fn initialize(&self) -> anyhow::Result<()> {
        self.inner.initialize().await
    }

    async fn create_room(
        &self,
        game_name: String,
        room_code: Option<String>,
        max_players: u8,
        supports_authority: bool,
        creator_id: PlayerId,
        relay_type: String,
        region_id: String,
        application_id: Option<uuid::Uuid>,
    ) -> anyhow::Result<Room> {
        let room = self
            .inner
            .create_room(
                game_name,
                room_code,
                max_players,
                supports_authority,
                creator_id,
                relay_type,
                region_id,
                application_id,
            )
            .await?;
        self.begin_drain_once();
        Ok(room)
    }

    async fn set_room_application_id(
        &self,
        room_id: &RoomId,
        application_id: uuid::Uuid,
    ) -> anyhow::Result<()> {
        self.inner
            .set_room_application_id(room_id, application_id)
            .await
    }

    async fn clear_room_application_id(&self, room_id: &RoomId) -> anyhow::Result<()> {
        self.inner.clear_room_application_id(room_id).await
    }

    async fn get_room(&self, game_name: &str, room_code: &str) -> anyhow::Result<Option<Room>> {
        self.inner.get_room(game_name, room_code).await
    }

    async fn get_room_by_id(&self, room_id: &RoomId) -> anyhow::Result<Option<Room>> {
        self.inner.get_room_by_id(room_id).await
    }

    async fn add_player_to_room(
        &self,
        room_id: &RoomId,
        player: PlayerInfo,
    ) -> anyhow::Result<bool> {
        self.inner.add_player_to_room(room_id, player).await
    }

    async fn remove_player_from_room(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> anyhow::Result<Option<PlayerInfo>> {
        self.inner.remove_player_from_room(room_id, player_id).await
    }

    async fn update_room_authority(
        &self,
        room_id: &RoomId,
        authority_player: Option<PlayerId>,
    ) -> anyhow::Result<bool> {
        self.inner
            .update_room_authority(room_id, authority_player)
            .await
    }

    async fn request_room_authority(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> anyhow::Result<(bool, Option<String>)> {
        self.inner
            .request_room_authority(room_id, player_id, become_authority)
            .await
    }

    async fn update_player_name(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        name: &str,
    ) -> anyhow::Result<bool> {
        self.inner
            .update_player_name(room_id, player_id, name)
            .await
    }

    async fn update_player_connection_info(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        connection_info: ConnectionInfo,
    ) -> anyhow::Result<bool> {
        self.inner
            .update_player_connection_info(room_id, player_id, connection_info)
            .await
    }

    async fn get_room_players(&self, room_id: &RoomId) -> anyhow::Result<Vec<PlayerInfo>> {
        self.inner.get_room_players(room_id).await
    }

    async fn cleanup_empty_rooms(
        &self,
        empty_timeout: chrono::Duration,
        protected: &HashSet<RoomId>,
    ) -> anyhow::Result<Vec<RoomId>> {
        self.inner
            .cleanup_empty_rooms(empty_timeout, protected)
            .await
    }

    async fn cleanup_expired_rooms(
        &self,
        empty_timeout: chrono::Duration,
        inactive_timeout: chrono::Duration,
        protected: &HashSet<RoomId>,
    ) -> anyhow::Result<RoomCleanupOutcome> {
        self.inner
            .cleanup_expired_rooms(empty_timeout, inactive_timeout, protected)
            .await
    }

    async fn update_room_activity(&self, room_id: &RoomId) -> anyhow::Result<()> {
        self.inner.update_room_activity(room_id).await
    }

    async fn delete_room(&self, room_id: &RoomId) -> anyhow::Result<bool> {
        self.inner.delete_room(room_id).await
    }

    async fn get_game_room_count(&self, game_name: &str) -> anyhow::Result<usize> {
        self.inner.get_game_room_count(game_name).await
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }

    async fn update_player_last_seen(&self, player_id: &PlayerId) -> anyhow::Result<()> {
        self.inner.update_player_last_seen(player_id).await
    }

    async fn get_rooms_by_game(&self) -> anyhow::Result<HashMap<String, usize>> {
        self.inner.get_rooms_by_game().await
    }

    async fn get_player_count_percentiles(&self) -> anyhow::Result<HashMap<String, f64>> {
        self.inner.get_player_count_percentiles().await
    }

    async fn get_game_player_percentiles(
        &self,
    ) -> anyhow::Result<HashMap<String, HashMap<String, f64>>> {
        self.inner.get_game_player_percentiles().await
    }

    async fn transition_room_to_lobby(&self, room_id: &RoomId) -> anyhow::Result<()> {
        self.inner.transition_room_to_lobby(room_id).await
    }

    async fn toggle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> anyhow::Result<Option<(LobbyState, Vec<PlayerId>, bool)>> {
        self.inner.toggle_player_ready(room_id, player_id).await
    }

    async fn finalize_room_game(
        &self,
        room_id: &RoomId,
        expected: &crate::database::FinalizeRoomGameExpectation,
    ) -> anyhow::Result<crate::database::FinalizeRoomGameOutcome> {
        self.inner.finalize_room_game(room_id, expected).await
    }

    async fn add_spectator_to_room(
        &self,
        room_id: &RoomId,
        spectator: SpectatorInfo,
    ) -> anyhow::Result<bool> {
        self.inner.add_spectator_to_room(room_id, spectator).await
    }

    async fn remove_spectator_from_room(
        &self,
        room_id: &RoomId,
        spectator_id: &PlayerId,
    ) -> anyhow::Result<Option<SpectatorInfo>> {
        self.inner
            .remove_spectator_from_room(room_id, spectator_id)
            .await
    }

    async fn get_room_spectators(&self, room_id: &RoomId) -> anyhow::Result<Vec<SpectatorInfo>> {
        self.inner.get_room_spectators(room_id).await
    }

    async fn try_claim_room_cleanup(
        &self,
        room_id: &RoomId,
        cleanup_type: &str,
        instance_id: &uuid::Uuid,
    ) -> anyhow::Result<bool> {
        self.inner
            .try_claim_room_cleanup(room_id, cleanup_type, instance_id)
            .await
    }

    async fn cleanup_old_room_cleanup_events(&self) -> anyhow::Result<u64> {
        self.inner.cleanup_old_room_cleanup_events().await
    }

    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self.inner.as_any()
    }

    async fn admin_user_exists(&self, email: &str) -> anyhow::Result<bool> {
        self.inner.admin_user_exists(email).await
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DrainTrigger {
    SpectatorLeftSend,
    RoomlessRegister,
    UnregisterLocal,
    RoomLeftSend,
    PlayerLeftBroadcast,
    FirstFarewellTrySend,
    MissingTerminalTail,
}

struct DrainTriggerCoordinator {
    room_events: Arc<RoomEventSequencer>,
    trigger: DrainTrigger,
    server: StdMutex<Option<Weak<EnhancedGameServer>>>,
    triggered: AtomicBool,
    try_send_calls: AtomicUsize,
    room_left_send_calls: AtomicUsize,
    player_left_broadcast_calls: AtomicUsize,
    clients: RwLock<HashMap<PlayerId, ClientDeliveryHandle>>,
    room_players: RwLock<HashMap<RoomId, HashSet<PlayerId>>>,
}

impl DrainTriggerCoordinator {
    fn new(trigger: DrainTrigger) -> Self {
        Self {
            room_events: Arc::new(RoomEventSequencer::default()),
            trigger,
            server: StdMutex::new(None),
            triggered: AtomicBool::new(false),
            try_send_calls: AtomicUsize::new(0),
            room_left_send_calls: AtomicUsize::new(0),
            player_left_broadcast_calls: AtomicUsize::new(0),
            clients: RwLock::new(HashMap::new()),
            room_players: RwLock::new(HashMap::new()),
        }
    }

    fn attach_server(&self, server: &Arc<EnhancedGameServer>) {
        *self
            .server
            .lock()
            .expect("drain trigger server lock poisoned") = Some(Arc::downgrade(server));
    }

    fn begin_drain_once(&self) {
        if self.triggered.swap(true, Ordering::AcqRel) {
            return;
        }
        let server = self
            .server
            .lock()
            .expect("drain trigger server lock poisoned")
            .clone()
            .and_then(|server| server.upgrade())
            .expect("test coordinator must be attached before triggering drain");
        server.begin_shutdown_drain();
    }

    async fn deliver_to(&self, player_id: &PlayerId, message: Arc<ServerMessage>) -> bool {
        self.clients
            .read()
            .await
            .get(player_id)
            .is_some_and(|handle| handle.sender.try_send(message, None).is_ok())
    }

    async fn recipients_for(
        &self,
        room_id: &RoomId,
        except_player: Option<&PlayerId>,
    ) -> Vec<ClientDeliveryHandle> {
        let room_players = self.room_players.read().await;
        let clients = self.clients.read().await;
        room_players
            .get(room_id)
            .map(|players| {
                players
                    .iter()
                    .filter(|player_id| Some(*player_id) != except_player)
                    .filter_map(|player_id| clients.get(player_id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl MessageCoordinator for DrainTriggerCoordinator {
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
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        if self.trigger == DrainTrigger::SpectatorLeftSend
            && matches!(message.as_ref(), ServerMessage::SpectatorLeft { .. })
        {
            self.begin_drain_once();
        }
        let _ = self.deliver_to(player_id, message).await;
        Ok(())
    }

    async fn send_to_player_if(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        drain: watch::Receiver<bool>,
    ) -> anyhow::Result<bool> {
        if self.trigger == DrainTrigger::SpectatorLeftSend
            && matches!(message.as_ref(), ServerMessage::SpectatorLeft { .. })
        {
            self.begin_drain_once();
        }
        if self.trigger == DrainTrigger::RoomLeftSend
            && matches!(message.as_ref(), ServerMessage::RoomLeft)
        {
            self.begin_drain_once();
        }
        if *drain.borrow() || !should_send() {
            return Ok(false);
        }
        if matches!(message.as_ref(), ServerMessage::RoomLeft) {
            self.room_left_send_calls.fetch_add(1, Ordering::Relaxed);
        }
        Ok(self.deliver_to(player_id, message).await)
    }

    async fn try_send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<bool> {
        self.try_send_calls.fetch_add(1, Ordering::Relaxed);
        if self.trigger == DrainTrigger::FirstFarewellTrySend {
            self.begin_drain_once();
        }
        Ok(self.deliver_to(player_id, message).await)
    }

    async fn try_send_to_player_if(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
    ) -> anyhow::Result<bool> {
        self.try_send_calls.fetch_add(1, Ordering::Relaxed);
        if self.trigger == DrainTrigger::FirstFarewellTrySend {
            self.begin_drain_once();
        }
        if !should_send() {
            return Ok(false);
        }
        Ok(self.deliver_to(player_id, message).await)
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
        if self.trigger == DrainTrigger::PlayerLeftBroadcast
            && matches!(message.as_ref(), ServerMessage::PlayerLeft { .. })
        {
            self.begin_drain_once();
        }
        if *drain.borrow() || !should_send() {
            return Ok(false);
        }
        before_send().await;
        if *drain.borrow() || !should_send() {
            return Ok(false);
        }
        if matches!(message.as_ref(), ServerMessage::PlayerLeft { .. }) {
            self.player_left_broadcast_calls
                .fetch_add(1, Ordering::Relaxed);
        }
        for handle in self.recipients_for(room_id, Some(except_player)).await {
            let _ = handle.sender.try_send(Arc::clone(&message), None);
        }
        Ok(true)
    }

    async fn commit_room_messages_if_members_with_hook<'a>(
        &'a self,
        _room_id: &RoomId,
        _expected_members: &[PlayerId],
        recipient_messages: Vec<crate::coordination::RoomRecipientMessages>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = anyhow::Result<bool>> + Send + 'a>,
                > + Send
                + 'a,
        >,
        after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
    ) -> anyhow::Result<crate::coordination::RoomMessageTransactionOutcome> {
        if !before_send().await? {
            return Ok(crate::coordination::RoomMessageTransactionOutcome::HookRejected);
        }
        let max_phases = recipient_messages
            .iter()
            .map(crate::coordination::RoomRecipientMessages::phase_count)
            .max()
            .unwrap_or(0);
        let mut after_first_phase = Some(after_first_phase);
        for phase in 0..max_phases {
            for batch in &recipient_messages {
                if let Some(message) = batch.message_in_phase(phase) {
                    let _ = self.deliver_to(&batch.player_id, Arc::clone(message)).await;
                }
            }
            if phase == 0
                && !after_first_phase
                    .take()
                    .expect("transaction state callback runs once")(0)
            {
                break;
            }
        }
        Ok(crate::coordination::RoomMessageTransactionOutcome::Committed)
    }

    async fn broadcast_to_room(
        &self,
        room_id: &RoomId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        for handle in self.recipients_for(room_id, None).await {
            let _ = handle.sender.try_send(Arc::clone(&message), None);
        }
        Ok(())
    }

    async fn broadcast_to_room_except(
        &self,
        room_id: &RoomId,
        except_player: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        for handle in self.recipients_for(room_id, Some(except_player)).await {
            let _ = handle.sender.try_send(Arc::clone(&message), None);
        }
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
        before_send().await;
        self.broadcast_to_room(room_id, message).await?;
        Ok(true)
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
        let mut routed: Vec<_> = self
            .room_players
            .read()
            .await
            .get(room_id)
            .into_iter()
            .flat_map(|players| players.iter().copied())
            .collect();
        let mut expected = expected_members.to_vec();
        routed.sort_unstable();
        expected.sort_unstable();
        if routed != expected {
            return Ok(false);
        }
        self.broadcast_to_room_with_hook(room_id, message, before_send)
            .await
    }

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        delivery: ClientDeliveryHandle,
    ) -> anyhow::Result<()> {
        let existing_client = self.clients.read().await.contains_key(&player_id);
        if let Some(room_id) = room_id {
            self.room_players
                .write()
                .await
                .entry(room_id)
                .or_default()
                .insert(player_id);
        } else if self.trigger == DrainTrigger::RoomlessRegister && existing_client {
            self.begin_drain_once();
        }
        self.clients.write().await.insert(player_id, delivery);
        Ok(())
    }

    async fn unroute_local_client_with_tail<'a>(
        &'a self,
        player_id: PlayerId,
        _room_id: RoomId,
        clear_assignment: Box<dyn FnOnce() -> Option<(ClientDeliveryHandle, u32, u64)> + Send + 'a>,
    ) -> anyhow::Result<Option<(u32, u64)>> {
        let Some((delivery, epoch, final_seq)) = clear_assignment() else {
            return Ok(None);
        };
        self.register_local_client(player_id, None, delivery)
            .await?;
        if self.trigger == DrainTrigger::MissingTerminalTail {
            return Ok(None);
        }
        Ok(Some((epoch, final_seq)))
    }

    async fn unregister_local_client(&self, player_id: &PlayerId) -> anyhow::Result<()> {
        if self.trigger == DrainTrigger::UnregisterLocal {
            self.begin_drain_once();
        }
        self.room_players.write().await.retain(|_, players| {
            players.remove(player_id);
            !players.is_empty()
        });
        self.clients.write().await.remove(player_id);
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
        _message: crate::distributed::SequencedMessage,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn handle_membership_update(
        &self,
        _update: crate::coordination::MembershipUpdate,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

async fn register_client(
    server: &EnhancedGameServer,
    addr: SocketAddr,
) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
    let (sender, receiver) = mpsc::channel(8);
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            addr,
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    (player_id, receiver)
}

struct JoinedPairFixture {
    server: Arc<EnhancedGameServer>,
    database: Arc<InMemoryDatabase>,
    room_id: RoomId,
    leaver: PlayerId,
    survivor: PlayerId,
    leaver_rx: mpsc::Receiver<Arc<ServerMessage>>,
    survivor_rx: mpsc::Receiver<Arc<ServerMessage>>,
    reconnect_token: String,
}

fn drain_queued_messages(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
) -> Vec<Arc<ServerMessage>> {
    let mut messages = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(message) => messages.push(message),
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                return messages;
            }
        }
    }
}

async fn setup_joined_pair_with_reconnection() -> JoinedPairFixture {
    let database = Arc::new(InMemoryDatabase::new());
    database
        .initialize()
        .await
        .expect("initialize lifecycle test database");
    let coordinator: Arc<dyn MessageCoordinator> = Arc::new(InMemoryMessageCoordinator::new());
    let distributed_lock: Arc<dyn DistributedLock> = Arc::new(InMemoryDistributedLock::new());
    let server_database: Arc<dyn GameDatabase> = database.clone();
    let server = create_test_server_with_message_coordinator_and_lock(
        ServerConfig {
            enable_reconnection: true,
            ..ServerConfig::default()
        },
        coordinator,
        distributed_lock,
        server_database,
    )
    .await;
    let (leaver, mut leaver_rx) =
        register_client(&server, "127.0.0.1:48100".parse().unwrap()).await;
    let (survivor, mut survivor_rx) =
        register_client(&server, "127.0.0.1:48101".parse().unwrap()).await;
    server.set_client_protocol(
        &leaver,
        NegotiatedProtocol {
            version: 3,
            transports: vec![crate::protocol::Transport::Relay],
            topologies: vec![crate::protocol::Topology::Relay],
        },
    );

    server
        .handle_join_room(
            &leaver,
            "leave-convergence".to_string(),
            Some("LVC001".to_string()),
            "leaver".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;
    let initial_messages = drain_queued_messages(&mut leaver_rx);
    let reconnect_token = initial_messages
        .iter()
        .find_map(|message| match message.as_ref() {
            ServerMessage::RoomJoined(payload) => payload.reconnection_token.clone(),
            _ => None,
        })
        .expect("v3 join baseline carries a pre-issued reconnect token");
    let room_id = server
        .get_client_room(&leaver)
        .await
        .expect("creator has a room assignment");

    server
        .handle_join_room(
            &survivor,
            "leave-convergence".to_string(),
            Some("LVC001".to_string()),
            "survivor".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;
    let leaver_messages = drain_queued_messages(&mut leaver_rx);
    assert!(
        leaver_messages.iter().any(|message| matches!(
            message.as_ref(),
            ServerMessage::PlayerJoined { player } if player.id == survivor
        )),
        "peer-visible membership exists before testing terminal convergence"
    );
    let survivor_messages = drain_queued_messages(&mut survivor_rx);
    assert!(
        survivor_messages
            .iter()
            .any(|message| matches!(message.as_ref(), ServerMessage::RoomJoined(_))),
        "survivor baseline commits before the leave scenario"
    );

    JoinedPairFixture {
        server,
        database,
        room_id,
        leaver,
        survivor,
        leaver_rx,
        survivor_rx,
        reconnect_token,
    }
}

async fn wait_for_backpressure_event(server: &EnhancedGameServer) {
    timeout(Duration::from_secs(1), async {
        while server
            .metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            == 0
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("delivery should reach the backpressure wait");
}

async fn wait_for_distributed_lock(server: &EnhancedGameServer, lock_key: &str) {
    timeout(Duration::from_secs(1), async {
        loop {
            if server
                .distributed_lock
                .is_locked(lock_key)
                .await
                .expect("distributed lock state can be read")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("operation should reach its distributed lock");
}

fn assert_next_message_matches(
    receiver: &mut mpsc::Receiver<Arc<ServerMessage>>,
    context: &str,
    matches_expected: impl FnOnce(&ServerMessage) -> bool,
) {
    match receiver.try_recv() {
        Ok(message) if matches_expected(message.as_ref()) => {}
        Ok(message) => panic!("{context}: unexpected message {message:?}"),
        Err(err) => panic!("{context}: expected queued message, got {err:?}"),
    }
}

fn assert_no_queued_message(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>, context: &str) {
    match receiver.try_recv() {
        Ok(message) => panic!("{context}: unexpected queued message {message:?}"),
        Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {}
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn seated_room_join_rejects_an_existing_spectator_role() {
    let server = create_test_server().await;
    let (creator, mut creator_rx) =
        register_client(&server, "127.0.0.1:48019".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "role-guard".to_string(),
            Some("ROLE01".to_string()),
            4,
            true,
            creator,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&creator, room.id)
        .await;

    let (spectator, mut spectator_rx) =
        register_client(&server, "127.0.0.1:48020".parse().unwrap()).await;
    server
        .spectator_service
        .join(
            &spectator,
            room.game_name.clone(),
            room.code.clone(),
            "Watcher".to_string(),
        )
        .await
        .expect("spectator join succeeds");
    assert_next_message_matches(&mut spectator_rx, "spectator baseline", |message| {
        matches!(message, ServerMessage::SpectatorJoined(_))
    });
    assert_next_message_matches(&mut creator_rx, "spectator notification", |message| {
        matches!(message, ServerMessage::NewSpectatorJoined { .. })
    });

    server
        .handle_join_room(
            &spectator,
            room.game_name.clone(),
            Some(room.code.clone()),
            "Dual Role".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    assert_next_message_matches(&mut spectator_rx, "dual-role rejection", |message| {
        matches!(
            message,
            ServerMessage::RoomJoinFailed {
                error_code: Some(ErrorCode::AlreadyInRoom),
                ..
            }
        )
    });
    assert!(server.spectator_service.is_spectating(&spectator));
    assert_eq!(server.get_client_room(&spectator).await, None);
    let players = server
        .database
        .get_room_players(&room.id)
        .await
        .expect("fetch seated players");
    assert!(
        players.iter().all(|player| player.id != spectator),
        "rejected spectator must not be persisted as a seated player"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn spectator_join_waits_for_one_slot_baseline_capacity() {
    let server = create_test_server().await;
    let (creator, mut creator_rx) =
        register_client(&server, "127.0.0.1:47990".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "spectator-capacity".to_string(),
            Some("SPCAP1".to_string()),
            4,
            true,
            creator,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&creator, room.id)
        .await;

    let (spectator_sender, mut spectator_rx) = mpsc::channel(1);
    let fill_sender = spectator_sender.clone();
    let spectator = server
        .connection_manager
        .register_client(
            spectator_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:47991".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("spectator connection registers");
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("fill the one-slot spectator queue");

    let mut join = {
        let service = server.spectator_service.clone();
        let game_name = room.game_name.clone();
        let room_code = room.code.clone();
        tokio::spawn(async move {
            service
                .join(
                    &spectator,
                    game_name,
                    room_code,
                    "capacity-watcher".to_string(),
                )
                .await
        })
    };
    wait_for_backpressure_event(&server).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut join)
            .await
            .is_err(),
        "spectator admission waits instead of rejecting a momentarily full baseline queue"
    );
    assert!(
        !server.spectator_service.is_spectating(&spectator),
        "spectator membership is not published before its baseline commits"
    );
    join.abort();
    join.await
        .expect_err("test aborts only the caller awaiting the owned spectator join");

    assert!(matches!(
        spectator_rx.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    let join_result = timeout(Duration::from_secs(1), spectator_rx.recv())
        .await
        .expect("owned spectator join finishes after capacity frees");
    assert!(matches!(
        join_result.as_deref(),
        Some(ServerMessage::SpectatorJoined(_))
    ));
    let publication = timeout(Duration::from_secs(1), creator_rx.recv())
        .await
        .expect("spectator publication follows its baseline");
    assert!(matches!(
        publication.as_deref(),
        Some(ServerMessage::NewSpectatorJoined { .. })
    ));
    assert!(server.spectator_service.is_spectating(&spectator));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn aborting_backpressured_spectator_detach_still_publishes_departure() {
    let server = create_test_server().await;
    let (creator, mut creator_rx) =
        register_client(&server, "127.0.0.1:47980".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "owned-spectator-detach".to_string(),
            Some("SPDET1".to_string()),
            4,
            true,
            creator,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&creator, room.id)
        .await;
    let (spectator_sender, mut spectator_rx) = mpsc::channel(1);
    let fill_sender = spectator_sender.clone();
    let spectator = server
        .connection_manager
        .register_client(
            spectator_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:47981".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("spectator registration succeeds");
    server
        .spectator_service
        .join(
            &spectator,
            room.game_name.clone(),
            room.code.clone(),
            "watcher".to_string(),
        )
        .await
        .expect("spectator joins before detach");
    assert!(matches!(
        spectator_rx.recv().await.as_deref(),
        Some(ServerMessage::SpectatorJoined(_))
    ));
    assert!(matches!(
        creator_rx.recv().await.as_deref(),
        Some(ServerMessage::NewSpectatorJoined { .. })
    ));
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("fill the one-slot spectator acknowledgement queue");

    let detach = {
        let service = server.spectator_service.clone();
        tokio::spawn(async move { service.leave(&spectator).await })
    };
    wait_for_backpressure_event(&server).await;
    detach.abort();
    detach
        .await
        .expect_err("test aborts only the caller awaiting owned spectator detach");
    assert!(!server.spectator_service.is_spectating(&spectator));

    assert!(matches!(
        spectator_rx.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    let acknowledgement = timeout(Duration::from_secs(1), spectator_rx.recv())
        .await
        .expect("owned detach acknowledgement should arrive");
    assert!(matches!(
        acknowledgement.as_deref(),
        Some(ServerMessage::SpectatorLeft { .. })
    ));
    assert!(matches!(
        timeout(Duration::from_secs(1), creator_rx.recv())
            .await
            .expect("owned spectator departure should publish")
            .as_deref(),
        Some(ServerMessage::SpectatorDisconnected { spectator_id, .. })
            if *spectator_id == spectator
    ));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn aborting_join_while_baseline_is_backpressured_still_completes_admission() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(1);
    let fill_sender = sender.clone();
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:47992".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("fill the one-slot join baseline queue");

    let join = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server
                .handle_join_room(
                    &player_id,
                    "owned-join".to_string(),
                    None,
                    "joiner".to_string(),
                    Some(4),
                    Some(true),
                    None,
                )
                .await;
        })
    };
    wait_for_backpressure_event(&server).await;
    join.abort();
    join.await
        .expect_err("test aborts the request future after the owned join committed");

    assert!(matches!(
        receiver.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    let baseline = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("owned join should finish after capacity frees")
        .expect("join baseline remains deliverable");
    let room_id = match baseline.as_ref() {
        ServerMessage::RoomJoined(payload) => payload.room_id,
        other => panic!("expected RoomJoined after abort, got {other:?}"),
    };
    assert_eq!(server.get_client_room(&player_id).await, Some(room_id));
    assert!(
        server
            .database
            .get_room_by_id(&room_id)
            .await
            .expect("room lookup succeeds")
            .is_some_and(|room| room.players.contains_key(&player_id)),
        "owned join publishes one coherent DB and routing membership"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn aborting_leave_while_ack_is_backpressured_still_publishes_terminal_event() {
    let server = create_test_server().await;
    let (leaver_sender, mut leaver_rx) = mpsc::channel(1);
    let fill_sender = leaver_sender.clone();
    let leaver = server
        .connection_manager
        .register_client(
            leaver_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:47993".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("leaver registration succeeds");
    let (survivor, mut survivor_rx) =
        register_client(&server, "127.0.0.1:47994".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "owned-leave".to_string(),
            Some("OWNLV1".to_string()),
            4,
            true,
            leaver,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&leaver, room.id)
        .await;
    server
        .database
        .add_player_to_room(
            &room.id,
            PlayerInfo {
                id: survivor,
                name: "survivor".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "region-a".to_string(),
            },
        )
        .await
        .expect("survivor insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&survivor, room.id)
        .await;
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("fill the one-slot leave acknowledgement queue");

    let leave = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.leave_room(&leaver).await })
    };
    wait_for_backpressure_event(&server).await;
    leave.abort();
    leave
        .await
        .expect_err("test aborts the request future after terminal unroute");

    assert!(matches!(
        leaver_rx.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    let acknowledgement = timeout(Duration::from_secs(1), leaver_rx.recv())
        .await
        .expect("owned leave sends acknowledgement after capacity frees");
    assert!(matches!(
        acknowledgement.as_deref(),
        Some(ServerMessage::RoomLeft)
    ));
    match timeout(Duration::from_secs(1), survivor_rx.recv())
        .await
        .expect("owned leave publishes terminal event")
        .expect("survivor channel remains open")
        .as_ref()
    {
        ServerMessage::PlayerLeft {
            player_id,
            epoch: Some(epoch),
            final_seq: Some(final_seq),
        } => {
            assert_eq!(*player_id, leaver);
            assert_eq!((*epoch, *final_seq), (1, 0));
        }
        other => panic!("expected complete terminal PlayerLeft, got {other:?}"),
    }
    assert_eq!(server.get_client_room(&leaver).await, None);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn aborting_reconnect_while_baseline_is_backpressured_still_restores_identity() {
    let server = create_test_server().await;
    let (existing, mut existing_rx) =
        register_client(&server, "127.0.0.1:47970".parse().unwrap()).await;
    let (reconnecting, _old_rx) =
        register_client(&server, "127.0.0.1:47971".parse().unwrap()).await;
    let (current_sender, mut current_rx) = mpsc::channel(1);
    let fill_sender = current_sender.clone();
    let current = server
        .connection_manager
        .register_client(
            current_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:47972".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("replacement connection registers");
    let room = server
        .database
        .create_room(
            "owned-reconnect".to_string(),
            Some("OWNRC1".to_string()),
            4,
            true,
            existing,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    let reconnecting_info = PlayerInfo {
        id: reconnecting,
        name: "reconnecting".to_string(),
        is_authority: false,
        is_ready: false,
        connected_at: chrono::Utc::now(),
        connection_info: None,
        epoch: None,
        seq: None,
        region_id: "region-a".to_string(),
    };
    server
        .database
        .add_player_to_room(&room.id, reconnecting_info.clone())
        .await
        .expect("reconnecting member insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&existing, room.id)
        .await;
    server
        .connection_manager
        .assign_client_to_room(&reconnecting, room.id)
        .await;
    let token = server
        .reconnection_manager()
        .expect("reconnection is enabled")
        .register_disconnection(
            reconnecting,
            room.id,
            false,
            Some(reconnecting_info),
            server
                .connection_manager
                .game_data_epoch(&reconnecting)
                .unwrap_or(0),
        )
        .await;
    server
        .database
        .remove_player_from_room(&room.id, &reconnecting)
        .await
        .expect("disconnect removes DB membership");
    server.connection_manager.remove_client(&reconnecting);
    server
        .message_coordinator
        .unregister_local_client(&reconnecting)
        .await
        .expect("disconnect removes routing");
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("fill the one-slot reconnect baseline queue");

    let reconnect = {
        let server = Arc::clone(&server);
        tokio::spawn(async move {
            server
                .handle_reconnect(&current, &reconnecting, &room.id, &token)
                .await
        })
    };
    wait_for_backpressure_event(&server).await;
    reconnect.abort();
    reconnect
        .await
        .expect_err("test aborts only the caller awaiting the owned reconnect");

    assert!(matches!(
        current_rx.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    match timeout(Duration::from_secs(1), current_rx.recv())
        .await
        .expect("owned reconnect baseline should arrive")
        .expect("replacement channel remains open")
        .as_ref()
    {
        ServerMessage::Reconnected(payload) => assert_eq!(payload.player_id, reconnecting),
        other => panic!("expected Reconnected after abort, got {other:?}"),
    }
    assert!(matches!(
        timeout(Duration::from_secs(1), existing_rx.recv())
            .await
            .expect("owned reconnect should notify peers")
            .as_deref(),
        Some(ServerMessage::PlayerReconnected { player_id, .. })
            if *player_id == reconnecting
    ));
    assert!(server.connection_manager.has_client(&reconnecting));
    assert!(!server.connection_manager.has_client(&current));
    assert!(server
        .database
        .get_room_players(&room.id)
        .await
        .expect("room players")
        .iter()
        .any(|player| player.id == reconnecting));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn delayed_ready_event_commits_before_a_concurrent_join_mutates_membership() {
    let server = create_test_server().await;
    let (creator_sender, mut creator_rx) = mpsc::channel(1);
    let fill_sender = creator_sender.clone();
    let creator = server
        .connection_manager
        .register_client(
            creator_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:47995".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("creator registration succeeds");
    let (joiner, mut joiner_rx) =
        register_client(&server, "127.0.0.1:47996".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "ready-join-order".to_string(),
            Some("RJOIN1".to_string()),
            4,
            true,
            creator,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&creator, room.id)
        .await;
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("delay the ready broadcast on creator capacity");

    let ready = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.handle_player_ready(&creator).await })
    };
    wait_for_backpressure_event(&server).await;
    let join = {
        let server = Arc::clone(&server);
        let game_name = room.game_name.clone();
        let room_code = room.code.clone();
        tokio::spawn(async move {
            server
                .handle_join_room(
                    &joiner,
                    game_name,
                    Some(room_code),
                    "joiner".to_string(),
                    Some(4),
                    Some(true),
                    None,
                )
                .await
        })
    };
    wait_for_distributed_lock(
        &server,
        &format!("room_join:{}:{}", room.game_name, room.code),
    )
    .await;
    assert!(
        !server
            .database
            .get_room_players(&room.id)
            .await
            .expect("room players")
            .iter()
            .any(|player| player.id == joiner),
        "join DB mutation waits behind the delayed ready lifecycle event"
    );

    assert!(matches!(
        creator_rx.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    match timeout(Duration::from_secs(1), creator_rx.recv())
        .await
        .expect("ready event becomes deliverable")
        .expect("creator channel remains open")
        .as_ref()
    {
        ServerMessage::LobbyStateChanged { ready_players, .. } => {
            assert_eq!(ready_players, &vec![creator]);
        }
        other => panic!("expected ready LobbyStateChanged first, got {other:?}"),
    }
    ready.await.expect("ready task should not panic");
    let baseline = timeout(Duration::from_secs(1), joiner_rx.recv())
        .await
        .expect("join baseline should arrive");
    assert!(matches!(
        baseline.as_deref(),
        Some(ServerMessage::RoomJoined(_))
    ));
    assert!(matches!(
        timeout(Duration::from_secs(1), creator_rx.recv())
            .await
            .expect("join event should follow ready event")
            .as_deref(),
        Some(ServerMessage::PlayerJoined { player }) if player.id == joiner
    ));
    join.await.expect("join task should not panic");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn two_concurrent_joins_publish_in_database_mutation_order() {
    let server = create_test_server().await;
    let (creator_sender, mut creator_rx) = mpsc::channel(1);
    let fill_sender = creator_sender.clone();
    let creator = server
        .connection_manager
        .register_client(
            creator_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:47997".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("creator registration succeeds");
    let (first, mut first_rx) = register_client(&server, "127.0.0.1:47998".parse().unwrap()).await;
    let (second, mut second_rx) =
        register_client(&server, "127.0.0.1:47999".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "join-join-order".to_string(),
            Some("JJOIN1".to_string()),
            4,
            true,
            creator,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&creator, room.id)
        .await;
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("delay the first PlayerJoined broadcast");

    let spawn_join = |player_id, name: &'static str| {
        let server = Arc::clone(&server);
        let game_name = room.game_name.clone();
        let room_code = room.code.clone();
        tokio::spawn(async move {
            server
                .handle_join_room(
                    &player_id,
                    game_name,
                    Some(room_code),
                    name.to_string(),
                    Some(4),
                    Some(true),
                    None,
                )
                .await
        })
    };
    let first_join = spawn_join(first, "first");
    wait_for_backpressure_event(&server).await;
    let second_join = spawn_join(second, "second");
    wait_for_distributed_lock(
        &server,
        &format!("room_join:{}:{}", room.game_name, room.code),
    )
    .await;
    assert!(
        !server
            .database
            .get_room_players(&room.id)
            .await
            .expect("room players")
            .iter()
            .any(|player| player.id == second),
        "second join cannot mutate DB while first event delivery is delayed"
    );

    assert!(matches!(
        creator_rx.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    let mut published = Vec::new();
    while published.len() < 2 {
        let message = timeout(Duration::from_secs(1), creator_rx.recv())
            .await
            .expect("ordered join event should arrive")
            .expect("creator channel remains open");
        if let ServerMessage::PlayerJoined { player } = message.as_ref() {
            published.push(player.id);
        }
    }
    assert_eq!(published, vec![first, second]);
    first_join.await.expect("first join task should not panic");
    second_join
        .await
        .expect("second join task should not panic");
    assert!(matches!(
        first_rx.recv().await.as_deref(),
        Some(ServerMessage::RoomJoined(_))
    ));
    assert!(matches!(
        second_rx.recv().await.as_deref(),
        Some(ServerMessage::RoomJoined(_))
    ));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn delayed_leave_terminal_event_commits_before_a_concurrent_join() {
    let server = create_test_server().await;
    let (leaver, mut leaver_rx) =
        register_client(&server, "127.0.0.1:48001".parse().unwrap()).await;
    let (survivor_sender, mut survivor_rx) = mpsc::channel(1);
    let fill_sender = survivor_sender.clone();
    let survivor = server
        .connection_manager
        .register_client(
            survivor_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:48002".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("survivor registration succeeds");
    let (joiner, mut joiner_rx) =
        register_client(&server, "127.0.0.1:48003".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "leave-join-order".to_string(),
            Some("LJOIN1".to_string()),
            4,
            true,
            leaver,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&leaver, room.id)
        .await;
    server
        .database
        .add_player_to_room(
            &room.id,
            PlayerInfo {
                id: survivor,
                name: "survivor".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "region-a".to_string(),
            },
        )
        .await
        .expect("survivor insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&survivor, room.id)
        .await;
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("delay PlayerLeft on survivor capacity");

    let leave = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.leave_room(&leaver).await })
    };
    wait_for_backpressure_event(&server).await;
    assert!(matches!(
        leaver_rx.recv().await.as_deref(),
        Some(ServerMessage::RoomLeft)
    ));
    let join = {
        let server = Arc::clone(&server);
        let game_name = room.game_name.clone();
        let room_code = room.code.clone();
        tokio::spawn(async move {
            server
                .handle_join_room(
                    &joiner,
                    game_name,
                    Some(room_code),
                    "replacement".to_string(),
                    Some(4),
                    Some(true),
                    None,
                )
                .await
        })
    };
    wait_for_distributed_lock(
        &server,
        &format!("room_join:{}:{}", room.game_name, room.code),
    )
    .await;
    assert!(
        !server
            .database
            .get_room_players(&room.id)
            .await
            .expect("room players")
            .iter()
            .any(|player| player.id == joiner),
        "replacement join waits until the terminal leave event commits"
    );

    assert!(matches!(
        survivor_rx.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    let terminal = timeout(Duration::from_secs(1), survivor_rx.recv())
        .await
        .expect("terminal leave event should arrive");
    assert!(matches!(
        terminal.as_deref(),
        Some(ServerMessage::PlayerLeft { player_id, .. }) if *player_id == leaver
    ));
    let replacement = timeout(Duration::from_secs(1), joiner_rx.recv())
        .await
        .expect("replacement baseline should arrive");
    assert!(matches!(
        replacement.as_deref(),
        Some(ServerMessage::RoomJoined(_))
    ));
    assert!(matches!(
        timeout(Duration::from_secs(1), survivor_rx.recv())
            .await
            .expect("replacement event should follow terminal leave")
            .as_deref(),
        Some(ServerMessage::PlayerJoined { player }) if player.id == joiner
    ));
    leave.await.expect("leave task should not panic");
    join.await.expect("join task should not panic");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn room_join_snapshot_baselines_preexisting_relay_tail_before_player_left() {
    let server = create_test_server().await;
    let (existing, _existing_rx) =
        register_client(&server, "127.0.0.1:48004".parse().unwrap()).await;
    let (joiner, mut joiner_rx) =
        register_client(&server, "127.0.0.1:48005".parse().unwrap()).await;
    server.set_client_protocol(
        &joiner,
        NegotiatedProtocol {
            version: 3,
            transports: vec![crate::protocol::Transport::Relay],
            topologies: vec![crate::protocol::Topology::Relay],
        },
    );
    let room = server
        .database
        .create_room(
            "relay-baseline".to_string(),
            Some("BASE01".to_string()),
            4,
            true,
            existing,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&existing, room.id)
        .await;
    for expected in 1..=100 {
        assert_eq!(
            server
                .connection_manager
                .next_relay_stamp_in_room(&existing, &room.id),
            Some(RelayStamp {
                epoch: 1,
                seq: expected,
            })
        );
    }

    server
        .handle_join_room(
            &joiner,
            room.game_name.clone(),
            Some(room.code.clone()),
            "late-joiner".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;
    let baseline = timeout(Duration::from_secs(1), joiner_rx.recv())
        .await
        .expect("join baseline should arrive")
        .expect("joiner channel remains open");
    let existing_snapshot = match baseline.as_ref() {
        ServerMessage::RoomJoined(payload) => payload
            .current_players
            .iter()
            .find(|player| player.id == existing)
            .expect("baseline includes existing sender"),
        other => panic!("expected RoomJoined, got {other:?}"),
    };
    assert_eq!(
        (existing_snapshot.epoch, existing_snapshot.seq),
        (Some(1), Some(100))
    );

    server.leave_room(&existing).await;
    loop {
        let message = timeout(Duration::from_secs(1), joiner_rx.recv())
            .await
            .expect("terminal event should arrive")
            .expect("joiner channel remains open");
        if let ServerMessage::PlayerLeft {
            player_id,
            epoch,
            final_seq,
        } = message.as_ref()
        {
            assert_eq!(*player_id, existing);
            assert_eq!((*epoch, *final_seq), (Some(1), Some(100)));
            break;
        }
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn leave_room_sends_confirmation_and_clears_membership() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(8);
    let addr: SocketAddr = "127.0.0.1:48000".parse().unwrap();
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            addr,
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");

    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("ABCD".to_string()),
            4,
            true,
            player_id,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");

    server
        .connection_manager
        .assign_client_to_room(&player_id, room.id)
        .await;

    server.leave_room(&player_id).await;

    let confirmation = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("room left message present");
    assert!(
        matches!(*confirmation, ServerMessage::RoomLeft),
        "expected RoomLeft confirmation"
    );

    assert!(
        server.get_client_room(&player_id).await.is_none(),
        "room assignment should be cleared"
    );

    let room_after = server
        .database
        .get_room_by_id(&room.id)
        .await
        .expect("room lookup succeeds")
        .expect("room still exists");
    assert!(
        !room_after.players.contains_key(&player_id),
        "player should be removed from room state"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn leave_storage_error_preserves_membership_routing_and_reconnect_token() {
    let mut fixture = setup_joined_pair_with_reconnection().await;
    fixture.database.fail_remove_player_from_room_for_test(true);

    fixture.server.leave_room(&fixture.leaver).await;

    let stored_players = fixture
        .database
        .get_room_players(&fixture.room_id)
        .await
        .expect("membership remains readable after injected failure");
    assert!(stored_players
        .iter()
        .any(|player| player.id == fixture.leaver));
    assert_eq!(
        fixture.server.get_client_room(&fixture.leaver).await,
        Some(fixture.room_id),
        "an unknown durable outcome must leave the local assignment intact"
    );
    let routed = fixture
        .server
        .message_coordinator
        .routed_player_ids(&fixture.room_id)
        .await
        .expect("routing lookup succeeds")
        .expect("room has routed members");
    assert!(routed.contains(&fixture.leaver));
    assert_no_queued_message(
        &mut fixture.leaver_rx,
        "storage failure must not acknowledge an uncommitted leave",
    );
    assert_no_queued_message(
        &mut fixture.survivor_rx,
        "storage failure must not publish a false PlayerLeft",
    );
    assert_eq!(
        fixture.server.metrics.players_left.load(Ordering::Relaxed),
        0,
        "failed persistence is not a completed departure"
    );

    let armed_token = fixture
        .server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(fixture.leaver, fixture.room_id, false, None, 1)
        .await;
    assert_eq!(
        armed_token, fixture.reconnect_token,
        "a retryable storage failure must preserve the token already held by the client"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn disconnect_storage_error_forces_terminal_teardown_and_keeps_claim_reachable() {
    let mut fixture = setup_joined_pair_with_reconnection().await;
    fixture.database.fail_remove_player_from_room_for_test(true);

    fixture.server.unregister_client(&fixture.leaver).await;

    assert!(
        !fixture
            .server
            .connection_manager
            .has_client(&fixture.leaver),
        "a dead room-bound connection must release its client/IP slot even when storage fails"
    );
    assert_eq!(fixture.server.get_client_room(&fixture.leaver).await, None);
    let routed = fixture
        .server
        .message_coordinator
        .routed_player_ids(&fixture.room_id)
        .await
        .expect("routing lookup succeeds")
        .expect("survivor remains routed");
    assert!(!routed.contains(&fixture.leaver));
    assert_next_message_matches(
        &mut fixture.survivor_rx,
        "storage-failed disconnect still publishes its terminal epoch",
        |message| matches!(message, ServerMessage::PlayerLeft { player_id, epoch: Some(_), final_seq: Some(_), } if *player_id == fixture.leaver),
    );

    let stored_players = fixture
        .database
        .get_room_players(&fixture.room_id)
        .await
        .expect("failed removal leaves a reconnect-window reservation");
    assert!(stored_players
        .iter()
        .any(|player| player.id == fixture.leaver));
    let reconnection_manager = fixture
        .server
        .reconnection_manager()
        .expect("reconnection enabled");
    assert!(
        reconnection_manager
            .has_pending_reconnection(&fixture.leaver)
            .await
    );
    assert!(reconnection_manager
        .validate_reconnection(&fixture.leaver, &fixture.room_id, &fixture.reconnect_token,)
        .await
        .is_ok());
    let joined_before_reconnect = fixture
        .server
        .metrics
        .players_joined
        .load(Ordering::Relaxed);
    assert_eq!(
        fixture.server.metrics.players_left.load(Ordering::Relaxed),
        1,
        "forced local terminal teardown counts as one disconnected player"
    );

    fixture
        .database
        .fail_remove_player_from_room_for_test(false);
    let (current, mut current_rx) =
        register_client(&fixture.server, "127.0.0.1:48102".parse().unwrap()).await;
    fixture.server.set_client_protocol(
        &current,
        NegotiatedProtocol {
            version: 3,
            transports: vec![crate::protocol::Transport::Relay],
            topologies: vec![crate::protocol::Topology::Relay],
        },
    );
    assert!(
        fixture
            .server
            .handle_reconnect(
                &current,
                &fixture.leaver,
                &fixture.room_id,
                &fixture.reconnect_token,
            )
            .await
    );
    assert_next_message_matches(
        &mut current_rx,
        "reconnect baseline after storage recovery",
        |message| matches!(message, ServerMessage::Reconnected(_)),
    );
    assert_next_message_matches(
        &mut fixture.survivor_rx,
        "survivor observes restored membership",
        |message| matches!(message, ServerMessage::PlayerReconnected { player_id, .. } if *player_id == fixture.leaver),
    );
    assert_eq!(
        fixture
            .server
            .metrics
            .players_joined
            .load(Ordering::Relaxed),
        joined_before_reconnect + 1
    );
    assert_eq!(
        fixture.server.metrics.players_left.load(Ordering::Relaxed),
        1,
        "reconnect must not erase or duplicate the prior terminal transition"
    );
    assert_eq!(
        fixture
            .server
            .metrics
            .players_joined
            .load(Ordering::Relaxed)
            .saturating_sub(fixture.server.metrics.players_left.load(Ordering::Relaxed)),
        2,
        "active-player conservation returns to the two live room members"
    );
    assert_eq!(
        fixture
            .server
            .cleanup_pending_durable_player_detaches()
            .await,
        0,
        "successful reconnect clears the stale durable-detach candidate"
    );
    assert!(fixture
        .database
        .get_room_players(&fixture.room_id)
        .await
        .expect("restored membership remains readable")
        .iter()
        .any(|player| player.id == fixture.leaver));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn disconnect_storage_error_retries_without_reconnection_support() {
    let server = create_test_server_with_config(ServerConfig {
        enable_reconnection: false,
        ..ServerConfig::default()
    })
    .await;
    let (leaver, mut leaver_rx) =
        register_client(&server, "127.0.0.1:48120".parse().unwrap()).await;
    let (survivor, mut survivor_rx) =
        register_client(&server, "127.0.0.1:48121".parse().unwrap()).await;

    server
        .handle_join_room(
            &leaver,
            "detach-retry".to_string(),
            Some("DTR001".to_string()),
            "leaver".to_string(),
            Some(4),
            Some(false),
            None,
        )
        .await;
    drain_queued_messages(&mut leaver_rx);
    let room_id = server
        .get_client_room(&leaver)
        .await
        .expect("leaver joined retry room");
    server
        .handle_join_room(
            &survivor,
            "detach-retry".to_string(),
            Some("DTR001".to_string()),
            "survivor".to_string(),
            Some(4),
            Some(false),
            None,
        )
        .await;
    drain_queued_messages(&mut survivor_rx);
    drain_queued_messages(&mut leaver_rx);

    let database = server
        .database()
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("test server uses in-memory database");
    database.fail_remove_player_from_room_for_test(true);
    server.unregister_client(&leaver).await;

    assert!(!server.connection_manager.has_client(&leaver));
    assert_next_message_matches(
        &mut survivor_rx,
        "storage-failed disconnect still publishes a terminal boundary",
        |message| matches!(message, ServerMessage::PlayerLeft { player_id, .. } if *player_id == leaver),
    );
    assert!(database
        .get_room_players(&room_id)
        .await
        .expect("durable ghost remains visible during outage")
        .iter()
        .any(|player| player.id == leaver));
    assert!(server.reconnection_manager().is_none());

    database.fail_remove_player_from_room_for_test(false);
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 1);
    assert!(!database
        .get_room_players(&room_id)
        .await
        .expect("membership remains readable after recovery")
        .iter()
        .any(|player| player.id == leaver));
    assert_eq!(
        server.metrics.players_left.load(Ordering::Relaxed),
        1,
        "successful retry accounts the durable removal exactly once"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn unpublished_join_rollback_retries_storage_and_conserves_activity() {
    let server = create_test_server_with_config(ServerConfig {
        enable_reconnection: false,
        ..ServerConfig::default()
    })
    .await;
    let (survivor, mut survivor_rx) =
        register_client(&server, "127.0.0.1:48130".parse().unwrap()).await;
    server
        .handle_join_room(
            &survivor,
            "join-rollback".to_string(),
            Some("JRB001".to_string()),
            "survivor".to_string(),
            Some(4),
            Some(false),
            None,
        )
        .await;
    drain_queued_messages(&mut survivor_rx);
    let room_id = server
        .get_client_room(&survivor)
        .await
        .expect("survivor joined rollback room");

    let (joiner, joiner_rx) = register_client(&server, "127.0.0.1:48131".parse().unwrap()).await;
    drop(joiner_rx);
    let database = server
        .database()
        .as_any()
        .downcast_ref::<InMemoryDatabase>()
        .expect("test server uses in-memory database");
    database.fail_remove_player_from_room_for_test(true);
    let joined_before = server.metrics.players_joined.load(Ordering::Relaxed);
    let left_before = server.metrics.players_left.load(Ordering::Relaxed);

    server
        .handle_join_room(
            &joiner,
            "join-rollback".to_string(),
            Some("JRB001".to_string()),
            "joiner".to_string(),
            Some(4),
            Some(false),
            None,
        )
        .await;

    assert_eq!(server.get_client_room(&joiner).await, None);
    assert_no_queued_message(
        &mut survivor_rx,
        "an undeliverable RoomJoined must not publish PlayerJoined",
    );
    assert_eq!(
        server.metrics.players_joined.load(Ordering::Relaxed),
        joined_before + 1,
        "storage admission occurred before baseline delivery failed"
    );
    assert_eq!(
        server.metrics.players_left.load(Ordering::Relaxed),
        left_before + 1,
        "unpublished admission rollback balances the logical player immediately"
    );
    assert_eq!(
        server
            .metrics
            .players_joined
            .load(Ordering::Relaxed)
            .saturating_sub(server.metrics.players_left.load(Ordering::Relaxed)),
        1,
        "only the survivor remains active"
    );
    assert!(database
        .get_room_players(&room_id)
        .await
        .expect("rollback ghost remains readable during outage")
        .iter()
        .any(|player| player.id == joiner));

    database.fail_remove_player_from_room_for_test(false);
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 1);
    assert!(!database
        .get_room_players(&room_id)
        .await
        .expect("membership remains readable after retry")
        .iter()
        .any(|player| player.id == joiner));
    assert_eq!(
        server.metrics.players_left.load(Ordering::Relaxed),
        left_before + 1,
        "durable repair does not double-count the logical rollback"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn spectator_disconnect_storage_error_releases_socket_and_retries_durable_detach() {
    let database = Arc::new(InMemoryDatabase::new());
    database
        .initialize()
        .await
        .expect("initialize spectator retry database");
    let coordinator: Arc<dyn MessageCoordinator> = Arc::new(InMemoryMessageCoordinator::new());
    let distributed_lock: Arc<dyn DistributedLock> = Arc::new(InMemoryDistributedLock::new());
    let server_database: Arc<dyn GameDatabase> = database.clone();
    let server = create_test_server_with_message_coordinator_and_lock(
        ServerConfig::default(),
        coordinator,
        distributed_lock,
        server_database,
    )
    .await;
    let (creator, mut creator_rx) =
        register_client(&server, "127.0.0.1:48102".parse().unwrap()).await;
    let (spectator, mut spectator_rx) =
        register_client(&server, "127.0.0.1:48103".parse().unwrap()).await;
    let room = database
        .create_room(
            "spectator-retry".to_string(),
            Some("SPR001".to_string()),
            4,
            true,
            creator,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("create spectator retry room");
    server
        .connection_manager
        .assign_client_to_room(&creator, room.id)
        .await;
    server
        .spectator_service
        .join(
            &spectator,
            "spectator-retry".to_string(),
            "SPR001".to_string(),
            "watcher".to_string(),
        )
        .await
        .expect("join spectator");
    drain_queued_messages(&mut creator_rx);
    drain_queued_messages(&mut spectator_rx);

    database.fail_remove_spectator_from_room_for_test(true);
    server.unregister_client(&spectator).await;

    assert!(!server.connection_manager.has_client(&spectator));
    assert!(
        server.spectator_service.is_spectating(&spectator),
        "failed durable detach remains indexed for maintenance retry"
    );
    assert!(database
        .get_room_by_id(&room.id)
        .await
        .expect("read room after failed detach")
        .expect("room remains")
        .get_spectators()
        .iter()
        .any(|entry| entry.id == spectator));

    database.fail_remove_spectator_from_room_for_test(false);
    assert_eq!(
        server.spectator_service.retry_disconnected_detaches().await,
        1
    );
    assert!(!server.spectator_service.is_spectating(&spectator));
    assert!(database
        .get_room_by_id(&room.id)
        .await
        .expect("read converged room")
        .expect("room remains")
        .get_spectators()
        .iter()
        .all(|entry| entry.id != spectator));
    assert_next_message_matches(
        &mut creator_rx,
        "retried detach publishes the peer-visible terminal roster",
        |message| matches!(message, ServerMessage::SpectatorDisconnected { spectator_id, .. } if *spectator_id == spectator),
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn absent_storage_member_converges_local_role_and_peer_roster() {
    let mut fixture = setup_joined_pair_with_reconnection().await;
    fixture
        .database
        .remove_player_from_room(&fixture.room_id, &fixture.leaver)
        .await
        .expect("external removal succeeds")
        .expect("test member existed in storage");

    fixture.server.leave_room(&fixture.leaver).await;

    assert!(fixture
        .database
        .get_room_players(&fixture.room_id)
        .await
        .expect("fetch converged roster")
        .iter()
        .all(|player| player.id != fixture.leaver));
    assert_eq!(fixture.server.get_client_room(&fixture.leaver).await, None);
    let routed = fixture
        .server
        .message_coordinator
        .routed_player_ids(&fixture.room_id)
        .await
        .expect("routing lookup succeeds")
        .expect("survivor remains routed");
    assert!(!routed.contains(&fixture.leaver));
    assert!(routed.contains(&fixture.survivor));
    assert_next_message_matches(
        &mut fixture.leaver_rx,
        "authoritative absent-row leave acknowledgement",
        |message| matches!(message, ServerMessage::RoomLeft),
    );
    assert_next_message_matches(
        &mut fixture.survivor_rx,
        "peer-visible absent-row convergence",
        |message| matches!(message, ServerMessage::PlayerLeft { player_id, .. } if *player_id == fixture.leaver),
    );
    assert_eq!(
        fixture.server.metrics.players_left.load(Ordering::Relaxed),
        1,
        "the converging call published one logical terminal membership transition"
    );

    let fallback_token = fixture
        .server
        .reconnection_manager()
        .expect("reconnection enabled")
        .register_disconnection(fixture.leaver, fixture.room_id, false, None, 1)
        .await;
    assert_ne!(
        fallback_token, fixture.reconnect_token,
        "an authoritative absence must discard the old room token"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn unregister_snapshot_failure_creates_no_broken_reconnect_record() {
    let mut fixture = setup_joined_pair_with_reconnection().await;
    fixture.database.fail_get_room_by_id_for_test(true);

    fixture.server.unregister_client(&fixture.leaver).await;

    let reconnection_manager = fixture
        .server
        .reconnection_manager()
        .expect("reconnection enabled");
    assert!(
        !reconnection_manager
            .has_pending_reconnection(&fixture.leaver)
            .await,
        "an incomplete room snapshot must not mint an unrestorable pending record"
    );
    assert!(matches!(
        reconnection_manager
            .validate_reconnection(&fixture.leaver, &fixture.room_id, &fixture.reconnect_token,)
            .await,
        Err(crate::reconnection::ReconnectionError::NoRecord)
    ));
    assert_eq!(
        fixture
            .server
            .metrics
            .reconnection_sessions_active
            .load(Ordering::Relaxed),
        0
    );

    fixture.database.fail_get_room_by_id_for_test(false);
    let stored_room = fixture
        .database
        .get_room_by_id(&fixture.room_id)
        .await
        .expect("room snapshot recovers after injected failure")
        .expect("survivor keeps room alive");
    assert!(!stored_room.players.contains_key(&fixture.leaver));
    assert_eq!(
        stored_room.authority_player, None,
        "the normal leave still clears the departed authority in storage"
    );
    assert_eq!(fixture.server.get_client_room(&fixture.leaver).await, None);
    assert_next_message_matches(
        &mut fixture.survivor_rx,
        "peers still observe the terminal leave",
        |message| matches!(message, ServerMessage::PlayerLeft { player_id, .. } if *player_id == fixture.leaver),
    );
    assert_no_queued_message(
        &mut fixture.leaver_rx,
        "disconnect teardown does not send a voluntary RoomLeft acknowledgement",
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_unregister_removes_membership_without_roomleft_noise() {
    let server = create_test_server().await;
    let (player_id, mut receiver) =
        register_client(&server, "127.0.0.1:48011".parse().unwrap()).await;
    let (survivor_id, mut survivor_receiver) =
        register_client(&server, "127.0.0.1:48012".parse().unwrap()).await;

    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("DRAIN1".to_string()),
            4,
            true,
            player_id,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");

    server
        .connection_manager
        .assign_client_to_room(&player_id, room.id)
        .await;
    server
        .database
        .add_player_to_room(
            &room.id,
            PlayerInfo {
                id: survivor_id,
                name: "survivor".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "region-a".to_string(),
            },
        )
        .await
        .expect("survivor insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&survivor_id, room.id)
        .await;

    assert!(
        server.begin_shutdown_drain().started_by_this_call,
        "test must transition the server into draining"
    );

    server.unregister_client(&player_id).await;

    match receiver.try_recv() {
        Ok(message) => panic!("shutdown unregister must not enqueue room traffic: {message:?}"),
        Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {}
    }
    match survivor_receiver.try_recv() {
        Ok(message) => panic!("shutdown unregister must not broadcast room traffic: {message:?}"),
        Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {}
    }

    assert!(
        server.get_client_room(&player_id).await.is_none(),
        "room assignment should be cleared"
    );

    let room_after = server
        .database
        .get_room_by_id(&room.id)
        .await
        .expect("room lookup succeeds")
        .expect("room still exists");
    assert!(
        !room_after.players.contains_key(&player_id),
        "player should be removed from room state"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn max_room_cap_denial_releases_join_coordination_locks() {
    let server = create_test_server_with_config(ServerConfig {
        max_rooms_per_game: 0,
        ..ServerConfig::default()
    })
    .await;
    let (player_id, mut receiver) =
        register_client(&server, "127.0.0.1:48001".parse().unwrap()).await;

    server
        .handle_join_room(
            &player_id,
            "test-game".to_string(),
            Some("ABCDEF".to_string()),
            "player".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    let response = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("join failure message present");
    match response.as_ref() {
        ServerMessage::RoomJoinFailed { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::MaxRoomsPerGameExceeded),
                "max room cap denial should be reported"
            );
        }
        other => panic!("expected RoomJoinFailed, got {other:?}"),
    }

    assert!(
        !server
            .distributed_lock
            .is_locked("room_join:test-game:ABCDEF")
            .await
            .expect("room join lock check succeeds"),
        "room join lock must be released after max-room-cap denial"
    );
    assert!(
        !server
            .distributed_lock
            .is_locked("game_room_cap:test-game")
            .await
            .expect("room cap lock check succeeds"),
        "room cap lock must be released after max-room-cap denial"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_server_rejects_room_creation_without_consuming_join_locks() {
    let server = create_test_server().await;

    let drain = server.begin_shutdown_drain();
    assert!(
        drain.started_by_this_call,
        "test must transition the server into draining"
    );

    for (room_code, port) in [(None, 48009), (Some("ABSENT"), 48010)] {
        let (player_id, mut receiver) =
            register_client(&server, format!("127.0.0.1:{port}").parse().unwrap()).await;

        server
            .handle_join_room(
                &player_id,
                "test-game".to_string(),
                room_code.map(str::to_string),
                "player".to_string(),
                Some(4),
                Some(true),
                None,
            )
            .await;

        let response = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("channel still open")
            .expect("join failure message present");
        match response.as_ref() {
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                assert_eq!(
                    *error_code,
                    Some(ErrorCode::ServerDraining),
                    "room creation during drain should be reported as SERVER_DRAINING"
                );
                assert!(
                    reason.contains("draining"),
                    "rejection reason should mention draining: {reason}"
                );
            }
            other => panic!("expected RoomJoinFailed, got {other:?}"),
        }

        if let Some(code) = room_code {
            assert!(
                server
                    .database
                    .get_room("test-game", code)
                    .await
                    .expect("room lookup succeeds")
                    .is_none(),
                "provided room code must not be created during drain"
            );
            assert!(
                !server
                    .distributed_lock
                    .is_locked(&format!("room_join:test-game:{code}"))
                    .await
                    .expect("room join lock check succeeds"),
                "early drain rejection should happen before room-join lock acquisition"
            );
        }
        assert!(
            !server
                .distributed_lock
                .is_locked("game_room_cap:test-game")
                .await
                .expect("room cap lock check succeeds"),
            "drain rejection should happen before room-cap lock acquisition"
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_room_creation_rechecks_after_cap_lock_race() {
    let trigger_lock = Arc::new(DrainOnLockAcquire::new("game_room_cap:test-game"));
    let distributed_lock: Arc<dyn DistributedLock> = trigger_lock.clone();
    let message_coordinator: Arc<dyn MessageCoordinator> =
        Arc::new(InMemoryMessageCoordinator::new());
    let database = create_test_database().await;
    let server = create_test_server_with_message_coordinator_and_lock(
        ServerConfig::default(),
        message_coordinator,
        distributed_lock,
        database,
    )
    .await;
    trigger_lock.attach_server(&server);
    let (player_id, mut receiver) =
        register_client(&server, "127.0.0.1:48030".parse().unwrap()).await;

    server
        .handle_join_room(
            &player_id,
            "test-game".to_string(),
            Some("RACE01".to_string()),
            "player".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    assert!(
        server.is_draining(),
        "test lock should transition the server into draining after cap-lock acquisition"
    );
    let response = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("join failure message present");
    match response.as_ref() {
        ServerMessage::RoomJoinFailed { reason, error_code } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::ServerDraining),
                "room creation that races with drain should be reported as SERVER_DRAINING"
            );
            assert!(
                reason.contains("draining"),
                "rejection reason should mention draining: {reason}"
            );
        }
        other => panic!("expected RoomJoinFailed, got {other:?}"),
    }

    assert!(
        server
            .database
            .get_room("test-game", "RACE01")
            .await
            .expect("room lookup succeeds")
            .is_none(),
        "room must not be created after the server enters drain"
    );
    assert!(
        !server
            .distributed_lock
            .is_locked("room_join:test-game:RACE01")
            .await
            .expect("room join lock check succeeds"),
        "room-join lock must be released after late drain rejection"
    );
    assert!(
        !server
            .distributed_lock
            .is_locked("game_room_cap:test-game")
            .await
            .expect("room cap lock check succeeds"),
        "room-cap lock must be released after late drain rejection"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_room_creation_rolls_back_after_create_race() {
    let distributed_lock: Arc<dyn DistributedLock> = Arc::new(InMemoryDistributedLock::new());
    let message_coordinator: Arc<dyn MessageCoordinator> =
        Arc::new(InMemoryMessageCoordinator::new());
    let database = Arc::new(DrainAfterCreateDatabase::new(create_test_database().await));
    let server_database: Arc<dyn GameDatabase> = database.clone();
    let server = create_test_server_with_message_coordinator_and_lock(
        ServerConfig::default(),
        message_coordinator,
        distributed_lock,
        server_database,
    )
    .await;
    database.attach_server(&server);
    let (player_id, mut receiver) =
        register_client(&server, "127.0.0.1:48031".parse().unwrap()).await;

    server
        .handle_join_room(
            &player_id,
            "test-game".to_string(),
            Some("RACE02".to_string()),
            "player".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    assert!(
        server.is_draining(),
        "test lock should transition the server into draining after room creation"
    );
    let response = timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("channel still open")
        .expect("join failure message present");
    match response.as_ref() {
        ServerMessage::RoomJoinFailed { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::ServerDraining),
                "room creation that finishes during drain should be reported as SERVER_DRAINING"
            );
        }
        other => panic!("expected RoomJoinFailed, got {other:?}"),
    }

    assert!(
        server
            .database
            .get_room("test-game", "RACE02")
            .await
            .expect("room lookup succeeds")
            .is_none(),
        "room created during drain must be rolled back before releasing the join lock"
    );
    assert!(
        !server
            .distributed_lock
            .is_locked("room_join:test-game:RACE02")
            .await
            .expect("room join lock check succeeds"),
        "room-join lock must be released after rollback"
    );
    assert!(
        !server
            .distributed_lock
            .is_locked("game_room_cap:test-game")
            .await
            .expect("room cap lock check succeeds"),
        "room-cap lock must be released after rollback"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_room_creation_rejection_does_not_wait_on_full_queue() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(1);
    let fill_sender = sender.clone();
    let player_id = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:48015".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("test setup should fill outbound queue");

    assert!(
        server.begin_shutdown_drain().started_by_this_call,
        "test must transition the server into draining"
    );

    timeout(
        Duration::from_millis(100),
        server.handle_join_room(
            &player_id,
            "test-game".to_string(),
            None,
            "player".to_string(),
            Some(4),
            Some(true),
            None,
        ),
    )
    .await
    .expect("drain rejection must not wait for slow-consumer delivery timeout");

    assert_next_message_matches(&mut receiver, "pre-filled queue item", |message| {
        matches!(message, ServerMessage::Pong)
    });
    assert_no_queued_message(
        &mut receiver,
        "full queue should skip the drain rejection instead of waiting",
    );
    assert_eq!(
        server
            .metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0,
        "best-effort drain rejection must not reclassify the client as slow consumer"
    );
    assert!(
        !server
            .distributed_lock
            .is_locked("game_room_cap:test-game")
            .await
            .expect("room cap lock check succeeds"),
        "drain rejection should still happen before room-cap lock acquisition"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_server_rejects_late_client_registration() {
    let server = create_test_server().await;
    let drain = server.begin_shutdown_drain();
    assert!(
        drain.started_by_this_call,
        "test must transition the server into draining"
    );

    let (sender, _receiver) = mpsc::channel(1);
    let result = server
        .register_client_with_close(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:48013".parse().unwrap(),
        )
        .await;

    assert!(
        matches!(result, Err(RegisterClientError::ServerDraining)),
        "late WebSocket registration after drain must be rejected before entering the connection manager"
    );
    assert!(
        server.connection_manager.client_ids().is_empty(),
        "drain-rejected registration must not leave a client in the connection manager"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_unregister_upgrades_activity_timeout_close_to_shutdown() {
    let server = create_test_server().await;
    let (sender, _receiver) = mpsc::channel(8);
    let (close, listener) = crate::coordination::ConnectionCloseSignal::channel();
    let player_id = server
        .register_client_with_close(sender, close, "127.0.0.1:48016".parse().unwrap())
        .await
        .expect("client registration succeeds before drain");

    assert!(
        server.connection_manager.request_close_for(
            &player_id,
            crate::coordination::CloseReason::ActivityTimeout
        ),
        "test setup should pin the activity-timeout race loser first"
    );
    assert!(
        server.begin_shutdown_drain().started_by_this_call,
        "test must transition the server into draining"
    );

    server.unregister_client(&player_id).await;

    assert_eq!(
        listener.requested_reason(),
        Some(crate::coordination::CloseReason::Shutdown),
        "draining unregister must restore the semantic shutdown close"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_unregister_rechecks_drain_after_mid_unregister_detach() {
    let coordinator = Arc::new(DrainTriggerCoordinator::new(
        DrainTrigger::SpectatorLeftSend,
    ));
    let message_coordinator: Arc<dyn MessageCoordinator> = coordinator.clone();
    let server =
        create_test_server_with_message_coordinator(ServerConfig::default(), message_coordinator)
            .await;
    coordinator.attach_server(&server);

    let (creator, mut creator_rx) =
        register_client(&server, "127.0.0.1:48017".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("WATCH1".to_string()),
            4,
            true,
            creator,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&creator, room.id)
        .await;

    let (sender, mut receiver) = mpsc::channel(8);
    let (close, listener) = crate::coordination::ConnectionCloseSignal::channel();
    let spectator = server
        .register_client_with_close(sender, close, "127.0.0.1:48018".parse().unwrap())
        .await
        .expect("spectator registration succeeds before drain");
    server
        .spectator_service
        .join(
            &spectator,
            "test-game".to_string(),
            "WATCH1".to_string(),
            "watcher".to_string(),
        )
        .await
        .expect("spectator join succeeds");
    assert_next_message_matches(&mut receiver, "spectator join confirmation", |message| {
        matches!(message, ServerMessage::SpectatorJoined(_))
    });
    assert_next_message_matches(
        &mut creator_rx,
        "spectator join room notification",
        |message| matches!(message, ServerMessage::NewSpectatorJoined { .. }),
    );
    let reconnection_manager = server
        .reconnection_manager()
        .expect("reconnection enabled for test server");
    let pending_reconnect = PlayerId::new_v4();
    let _token = reconnection_manager
        .register_disconnection(pending_reconnect, room.id, false, None, 0)
        .await;
    assert!(
        !server.is_draining(),
        "test setup must not enter drain before unregister begins"
    );

    server.unregister_client(&spectator).await;

    assert!(
        coordinator.triggered.load(Ordering::Acquire),
        "SpectatorLeft delivery should start drain during unregister"
    );
    assert_eq!(
        listener.requested_reason(),
        Some(crate::coordination::CloseReason::Shutdown),
        "unregister must re-read drain state after awaited detach work"
    );
    assert_no_queued_message(
        &mut receiver,
        "drain during spectator detach must skip SpectatorLeft",
    );
    assert_no_queued_message(
        &mut creator_rx,
        "drain during spectator detach must skip SpectatorDisconnected",
    );
    let replay = reconnection_manager.get_missed_events(&room.id, 0).await;
    assert!(
        replay
            .events
            .iter()
            .all(|event| !matches!(event, ServerMessage::SpectatorDisconnected { .. })),
        "drain during spectator detach must not replay-record SpectatorDisconnected"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_unregister_discards_reconnect_when_drain_starts_during_leave() {
    let coordinator = Arc::new(DrainTriggerCoordinator::new(DrainTrigger::RoomlessRegister));
    let message_coordinator: Arc<dyn MessageCoordinator> = coordinator.clone();
    let server =
        create_test_server_with_message_coordinator(ServerConfig::default(), message_coordinator)
            .await;
    coordinator.attach_server(&server);

    let (sender, mut receiver) = mpsc::channel(8);
    let (close, listener) = crate::coordination::ConnectionCloseSignal::channel();
    let player_id = server
        .register_client_with_close(sender, close, "127.0.0.1:48020".parse().unwrap())
        .await
        .expect("client registration succeeds before drain");
    let (survivor_id, mut survivor_receiver) =
        register_client(&server, "127.0.0.1:48021".parse().unwrap()).await;

    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("DRAIN2".to_string()),
            4,
            true,
            player_id,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&player_id, room.id)
        .await;
    server
        .database
        .add_player_to_room(
            &room.id,
            PlayerInfo {
                id: survivor_id,
                name: "survivor".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "region-a".to_string(),
            },
        )
        .await
        .expect("survivor insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&survivor_id, room.id)
        .await;

    server.unregister_client(&player_id).await;

    assert!(
        coordinator.triggered.load(Ordering::Acquire),
        "roomless re-registration in leave_room should start drain"
    );
    assert_eq!(
        listener.requested_reason(),
        Some(crate::coordination::CloseReason::Shutdown),
        "shutdown drain must win over normal unregister after leave_room awaits"
    );
    let reconnection_manager = server
        .reconnection_manager()
        .expect("reconnection enabled for test server");
    assert!(
        !reconnection_manager
            .has_pending_reconnection(&player_id)
            .await,
        "pending reconnect created before drain must be discarded"
    );
    assert_eq!(
        server
            .metrics
            .reconnection_sessions_active
            .load(Ordering::Relaxed),
        0,
        "discarding the drain-lost reconnect should restore the active gauge"
    );
    assert_no_queued_message(&mut receiver, "drain during leave_room must skip RoomLeft");
    assert_no_queued_message(
        &mut survivor_receiver,
        "drain during leave_room must skip PlayerLeft",
    );
    assert_eq!(
        coordinator.room_left_send_calls.load(Ordering::Relaxed),
        0,
        "RoomLeft delivery path must not run after drain starts"
    );
    assert_eq!(
        coordinator
            .player_left_broadcast_calls
            .load(Ordering::Relaxed),
        0,
        "PlayerLeft broadcast path must not run after drain starts"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_unregister_upgrades_close_when_drain_starts_during_coordinator_unregister() {
    let coordinator = Arc::new(DrainTriggerCoordinator::new(DrainTrigger::UnregisterLocal));
    let message_coordinator: Arc<dyn MessageCoordinator> = coordinator.clone();
    let server =
        create_test_server_with_message_coordinator(ServerConfig::default(), message_coordinator)
            .await;
    coordinator.attach_server(&server);

    let (sender, _receiver) = mpsc::channel(8);
    let (close, listener) = crate::coordination::ConnectionCloseSignal::channel();
    let player_id = server
        .register_client_with_close(sender, close, "127.0.0.1:48022".parse().unwrap())
        .await
        .expect("client registration succeeds before drain");

    server.unregister_client(&player_id).await;

    assert!(
        coordinator.triggered.load(Ordering::Acquire),
        "coordinator unregister should start drain"
    );
    assert_eq!(
        listener.requested_reason(),
        Some(crate::coordination::CloseReason::Shutdown),
        "final drain check must upgrade close reason before remove_client"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_leave_room_skips_roomleft_when_drain_starts_inside_send() {
    let coordinator = Arc::new(DrainTriggerCoordinator::new(DrainTrigger::RoomLeftSend));
    let message_coordinator: Arc<dyn MessageCoordinator> = coordinator.clone();
    let server =
        create_test_server_with_message_coordinator(ServerConfig::default(), message_coordinator)
            .await;
    coordinator.attach_server(&server);

    let (leaver, mut leaver_rx) =
        register_client(&server, "127.0.0.1:48023".parse().unwrap()).await;
    let (survivor, mut survivor_rx) =
        register_client(&server, "127.0.0.1:48024".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("DRAIN3".to_string()),
            4,
            true,
            leaver,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&leaver, room.id)
        .await;
    server
        .database
        .add_player_to_room(
            &room.id,
            PlayerInfo {
                id: survivor,
                name: "survivor".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "region-a".to_string(),
            },
        )
        .await
        .expect("survivor insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&survivor, room.id)
        .await;

    server.leave_room(&leaver).await;

    assert!(
        coordinator.triggered.load(Ordering::Acquire),
        "RoomLeft conditional send should start drain"
    );
    assert_eq!(
        coordinator.room_left_send_calls.load(Ordering::Relaxed),
        0,
        "RoomLeft must not be delivered after drain starts inside send"
    );
    assert_eq!(
        coordinator
            .player_left_broadcast_calls
            .load(Ordering::Relaxed),
        0,
        "PlayerLeft must not broadcast after drain starts inside RoomLeft send"
    );
    assert_no_queued_message(
        &mut leaver_rx,
        "RoomLeft must not be delivered after drain starts inside send",
    );
    assert_no_queued_message(
        &mut survivor_rx,
        "PlayerLeft must not be delivered after drain starts inside send",
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn missing_terminal_tail_suppresses_incomplete_player_left() {
    let coordinator = Arc::new(DrainTriggerCoordinator::new(
        DrainTrigger::MissingTerminalTail,
    ));
    let message_coordinator: Arc<dyn MessageCoordinator> = coordinator.clone();
    let server =
        create_test_server_with_message_coordinator(ServerConfig::default(), message_coordinator)
            .await;
    let (leaver, mut leaver_rx) =
        register_client(&server, "127.0.0.1:48025".parse().unwrap()).await;
    let (survivor, mut survivor_rx) =
        register_client(&server, "127.0.0.1:48026".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "terminal-tail-test".to_string(),
            Some("TAIL01".to_string()),
            4,
            true,
            leaver,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&leaver, room.id)
        .await;
    server
        .database
        .add_player_to_room(
            &room.id,
            PlayerInfo {
                id: survivor,
                name: "survivor".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "region-a".to_string(),
            },
        )
        .await
        .expect("survivor insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&survivor, room.id)
        .await;

    server.leave_room(&leaver).await;

    assert_no_queued_message(
        &mut leaver_rx,
        "tail failure must suppress RoomLeft with its failed transaction",
    );
    assert_no_queued_message(
        &mut survivor_rx,
        "v3 peers must never receive PlayerLeft without a complete terminal watermark",
    );
    assert_eq!(
        coordinator
            .player_left_broadcast_calls
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_leave_room_cancels_backpressured_roomleft() {
    let server = create_test_server().await;
    let (sender, mut receiver) = mpsc::channel(1);
    let fill_sender = sender.clone();
    let leaver = server
        .connection_manager
        .register_client(
            sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:48027".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("client registration succeeds");
    fill_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("test setup should fill leaver queue");
    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("DRAIN5".to_string()),
            4,
            true,
            leaver,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&leaver, room.id)
        .await;

    let leave_task = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server.leave_room(&leaver).await;
        }
    });

    wait_for_backpressure_event(&server).await;
    assert!(
        server.begin_shutdown_drain().started_by_this_call,
        "test should start drain while RoomLeft is waiting for queue capacity"
    );
    timeout(Duration::from_secs(1), leave_task)
        .await
        .expect("leave task should finish after drain cancels delivery")
        .expect("leave task should not panic");

    assert_next_message_matches(&mut receiver, "pre-filled leaver queue item", |message| {
        matches!(message, ServerMessage::Pong)
    });
    assert_no_queued_message(
        &mut receiver,
        "RoomLeft must not be enqueued after drain starts while backpressured",
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_leave_room_skips_playerleft_when_drain_starts_inside_broadcast() {
    let coordinator = Arc::new(DrainTriggerCoordinator::new(
        DrainTrigger::PlayerLeftBroadcast,
    ));
    let message_coordinator: Arc<dyn MessageCoordinator> = coordinator.clone();
    let server =
        create_test_server_with_message_coordinator(ServerConfig::default(), message_coordinator)
            .await;
    coordinator.attach_server(&server);

    let (leaver, mut leaver_rx) =
        register_client(&server, "127.0.0.1:48025".parse().unwrap()).await;
    let (survivor, mut survivor_rx) =
        register_client(&server, "127.0.0.1:48026".parse().unwrap()).await;
    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("DRAIN4".to_string()),
            4,
            true,
            leaver,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&leaver, room.id)
        .await;
    server
        .database
        .add_player_to_room(
            &room.id,
            PlayerInfo {
                id: survivor,
                name: "survivor".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "region-a".to_string(),
            },
        )
        .await
        .expect("survivor insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&survivor, room.id)
        .await;
    let pending_reconnect = PlayerId::new_v4();
    let reconnection_manager = server
        .reconnection_manager()
        .expect("reconnection enabled for test server");
    let _token = reconnection_manager
        .register_disconnection(pending_reconnect, room.id, false, None, 0)
        .await;

    server.leave_room(&leaver).await;

    assert!(
        coordinator.triggered.load(Ordering::Acquire),
        "PlayerLeft conditional broadcast should start drain"
    );
    assert_eq!(
        coordinator.room_left_send_calls.load(Ordering::Relaxed),
        1,
        "RoomLeft should be delivered before drain starts in the PlayerLeft boundary test"
    );
    assert_eq!(
        coordinator
            .player_left_broadcast_calls
            .load(Ordering::Relaxed),
        0,
        "PlayerLeft must not broadcast after drain starts inside broadcast"
    );
    assert_next_message_matches(&mut leaver_rx, "leaver RoomLeft", |message| {
        matches!(message, ServerMessage::RoomLeft)
    });
    assert_no_queued_message(
        &mut survivor_rx,
        "PlayerLeft must not be delivered after drain starts inside broadcast",
    );
    let replay = reconnection_manager.get_missed_events(&room.id, 0).await;
    assert!(
        replay
            .events
            .iter()
            .all(|event| !matches!(event, ServerMessage::PlayerLeft { .. })),
        "PlayerLeft must not be replay-recorded after drain starts inside broadcast"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_leave_room_cancels_backpressured_playerleft_and_replay() {
    let server = create_test_server().await;
    let (leaver, mut leaver_rx) =
        register_client(&server, "127.0.0.1:48028".parse().unwrap()).await;
    let (survivor_sender, mut survivor_rx) = mpsc::channel(1);
    let fill_survivor = survivor_sender.clone();
    let survivor = server
        .connection_manager
        .register_client(
            survivor_sender,
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:48029".parse().unwrap(),
            server.instance_id,
        )
        .await
        .expect("survivor registration succeeds");
    fill_survivor
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("test setup should fill survivor queue");
    let room = server
        .database
        .create_room(
            "test-game".to_string(),
            Some("DRAIN6".to_string()),
            4,
            true,
            leaver,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");
    server
        .connection_manager
        .assign_client_to_room(&leaver, room.id)
        .await;
    server
        .database
        .add_player_to_room(
            &room.id,
            PlayerInfo {
                id: survivor,
                name: "survivor".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: "region-a".to_string(),
            },
        )
        .await
        .expect("survivor insert succeeds");
    server
        .connection_manager
        .assign_client_to_room(&survivor, room.id)
        .await;
    let pending_reconnect = PlayerId::new_v4();
    let reconnection_manager = server
        .reconnection_manager()
        .expect("reconnection enabled for test server");
    let _token = reconnection_manager
        .register_disconnection(pending_reconnect, room.id, false, None, 0)
        .await;

    let leave_task = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server.leave_room(&leaver).await;
        }
    });

    wait_for_backpressure_event(&server).await;
    assert!(
        server.begin_shutdown_drain().started_by_this_call,
        "test should start drain while PlayerLeft is waiting for queue capacity"
    );
    timeout(Duration::from_secs(1), leave_task)
        .await
        .expect("leave task should finish after drain cancels broadcast")
        .expect("leave task should not panic");

    assert_next_message_matches(&mut leaver_rx, "leaver RoomLeft", |message| {
        matches!(message, ServerMessage::RoomLeft)
    });
    assert_next_message_matches(
        &mut survivor_rx,
        "pre-filled survivor queue item",
        |message| matches!(message, ServerMessage::Pong),
    );
    assert_no_queued_message(
        &mut survivor_rx,
        "PlayerLeft must not be enqueued after drain starts while backpressured",
    );
    let replay = reconnection_manager.get_missed_events(&room.id, 0).await;
    assert!(
        replay
            .events
            .iter()
            .all(|event| !matches!(event, ServerMessage::PlayerLeft { .. })),
        "PlayerLeft must not be replay-recorded when drain cancels the broadcast"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_server_allows_existing_room_join() {
    let server = create_test_server().await;
    let (creator, _creator_rx) = register_client(&server, "127.0.0.1:48011".parse().unwrap()).await;
    server
        .database
        .create_room(
            "test-game".to_string(),
            Some("EXIST1".to_string()),
            4,
            true,
            creator,
            "udp".to_string(),
            "region-a".to_string(),
            None,
        )
        .await
        .expect("room creation succeeds");

    let drain = server.begin_shutdown_drain();
    assert!(
        drain.started_by_this_call,
        "test must transition the server into draining"
    );

    let (joiner, mut joiner_rx) =
        register_client(&server, "127.0.0.1:48012".parse().unwrap()).await;
    server
        .handle_join_room(
            &joiner,
            "test-game".to_string(),
            Some("EXIST1".to_string()),
            "joiner".to_string(),
            Some(4),
            Some(true),
            None,
        )
        .await;

    let response = timeout(Duration::from_secs(1), joiner_rx.recv())
        .await
        .expect("channel still open")
        .expect("join response present");
    match response.as_ref() {
        ServerMessage::RoomJoined(payload) => {
            assert_eq!(payload.room_code, "EXIST1");
            assert_eq!(payload.player_id, joiner);
        }
        other => panic!("expected RoomJoined, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn join_into_full_room_classifies_as_room_full_not_creation_failed() {
    // Regression guard for error-code classification (the same class fixed in
    // `ready_state.rs`/`PlayerReadyError`): a join rejected because the room is
    // at capacity is a BUSINESS rejection and MUST surface as `ROOM_FULL` — so a
    // client knows to try a different room — never the catch-all
    // `ROOM_CREATION_FAILED`, which signals a transient/infra fault a client
    // would (wrongly) retry against the same full room. This also keeps the
    // join path consistent with the reconnection path, which already maps a full
    // room to `ROOM_FULL`. See `JoinRoomError`.
    let server = create_test_server().await;

    // Player 1 creates a room capped at a single seat → full once the creator is
    // seated (room creation seats the creator).
    let (creator, mut creator_rx) =
        register_client(&server, "127.0.0.1:48002".parse().unwrap()).await;
    server
        .handle_join_room(
            &creator,
            "test-game".to_string(),
            Some("FULLRM".to_string()),
            "creator".to_string(),
            Some(1),
            Some(false),
            None,
        )
        .await;
    match timeout(Duration::from_secs(1), creator_rx.recv())
        .await
        .expect("channel still open")
        .expect("creator join response present")
        .as_ref()
    {
        ServerMessage::RoomJoined(_) => {}
        other => panic!("creator expected RoomJoined, got {other:?}"),
    }

    // Player 2 attempts to join the now-full room by its code.
    let (joiner, mut joiner_rx) =
        register_client(&server, "127.0.0.1:48003".parse().unwrap()).await;
    server
        .handle_join_room(
            &joiner,
            "test-game".to_string(),
            Some("FULLRM".to_string()),
            "joiner".to_string(),
            Some(1),
            Some(false),
            None,
        )
        .await;

    match timeout(Duration::from_secs(1), joiner_rx.recv())
        .await
        .expect("channel still open")
        .expect("joiner failure message present")
        .as_ref()
    {
        ServerMessage::RoomJoinFailed { error_code, .. } => {
            assert_eq!(
                *error_code,
                Some(ErrorCode::RoomFull),
                "a full-room join must classify as ROOM_FULL, not ROOM_CREATION_FAILED"
            );
        }
        other => panic!("joiner expected RoomJoinFailed, got {other:?}"),
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn maintenance_cleanup_removes_expired_reconnections() {
    let server = create_test_server_with_config(ServerConfig {
        reconnection_window: Duration::ZERO,
        ..ServerConfig::default()
    })
    .await;
    let player_id = PlayerId::new_v4();
    let room_id = uuid::Uuid::new_v4();
    let reconnection_manager = server
        .reconnection_manager()
        .expect("reconnection enabled for test server");

    let _token = reconnection_manager
        .register_disconnection(player_id, room_id, false, None, 0)
        .await;
    assert!(
        reconnection_manager
            .has_pending_reconnection(&player_id)
            .await,
        "test setup should create a pending reconnection"
    );
    assert_eq!(
        server
            .metrics
            .reconnection_sessions_active
            .load(Ordering::Relaxed),
        1
    );

    let cleaned = server.cleanup_expired_reconnections().await;

    assert_eq!(cleaned, 1);
    assert!(
        !reconnection_manager
            .has_pending_reconnection(&player_id)
            .await,
        "maintenance cleanup should remove expired reconnection records"
    );
    assert_eq!(
        server
            .metrics
            .reconnection_sessions_active
            .load(Ordering::Relaxed),
        0
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn zero_ping_timeout_disables_activity_reaper() {
    let server = create_test_server_with_config(ServerConfig {
        ping_timeout: Duration::ZERO,
        ..ServerConfig::default()
    })
    .await;
    let (player_id, _receiver) = register_client(&server, "127.0.0.1:48013".parse().unwrap()).await;
    let shutdown = Arc::new(Notify::new());
    let cleanup_task = tokio::spawn({
        let server = Arc::clone(&server);
        let shutdown = Arc::clone(&shutdown);
        async move {
            server.cleanup_task_until(shutdown.notified()).await;
        }
    });

    // The cleanup interval fires immediately on startup. Give that first sweep
    // enough time to run, then prove the documented zero value means disabled
    // rather than "expire every client immediately."
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        server.connection_manager.client_ids().contains(&player_id),
        "ping_timeout=0 must disable activity-reaper eviction"
    );

    shutdown.notify_one();
    timeout(Duration::from_secs(1), cleanup_task)
        .await
        .expect("cleanup task should observe shutdown")
        .expect("cleanup task should not panic");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn activity_refresh_after_cleanup_snapshot_prevents_eviction() {
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_millis(
            ServerConfig::default()
                .websocket_config
                .slow_consumer_timeout_ms,
        ),
        Arc::new(crate::metrics::ServerMetrics::new()),
    ));
    let message_coordinator: Arc<dyn MessageCoordinator> = coordinator.clone();
    let server = create_test_server_with_message_coordinator(
        ServerConfig {
            ping_timeout: Duration::from_millis(100),
            ..ServerConfig::default()
        },
        message_coordinator,
    )
    .await;
    let (sender, mut receiver) = mpsc::channel(8);
    let (close, listener) = crate::coordination::ConnectionCloseSignal::channel();
    let player_id = server
        .register_client_with_close(sender, close, "127.0.0.1:48012".parse().unwrap())
        .await
        .expect("register client before activity-reaper snapshot");

    tokio::time::sleep(Duration::from_millis(110)).await;
    let coordinator_write = coordinator.local_clients.write().await;
    let shutdown = Arc::new(Notify::new());
    let cleanup_task = tokio::spawn({
        let server = Arc::clone(&server);
        let shutdown = Arc::clone(&shutdown);
        async move {
            server.cleanup_task_until(shutdown.notified()).await;
        }
    });

    tokio::time::sleep(Duration::from_millis(25)).await;
    server.record_client_activity(&player_id);
    drop(coordinator_write);
    tokio::time::sleep(Duration::from_millis(25)).await;

    assert!(
        server.connection_manager.client_ids().contains(&player_id),
        "activity refreshed after collection must rescue the client"
    );
    assert_eq!(
        listener.requested_reason(),
        None,
        "a stale cleanup snapshot must not pin activity_timeout"
    );
    assert_no_queued_message(
        &mut receiver,
        "a stale cleanup snapshot must not enqueue an ActivityTimeout farewell",
    );
    assert_eq!(
        server
            .metrics
            .expired_players_cleaned
            .load(Ordering::Relaxed),
        0,
        "rescued client must not count as evicted"
    );

    shutdown.notify_one();
    timeout(Duration::from_secs(1), cleanup_task)
        .await
        .expect("cleanup task should observe shutdown")
        .expect("cleanup task should not panic");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn activity_reaper_does_not_override_an_existing_close_owner() {
    let server = create_test_server_with_config(ServerConfig {
        ping_timeout: Duration::from_nanos(1),
        ..ServerConfig::default()
    })
    .await;
    let (sender, mut receiver) = mpsc::channel(8);
    let (close, listener) = crate::coordination::ConnectionCloseSignal::channel();
    let player_id = server
        .register_client_with_close(sender, close, "127.0.0.1:48015".parse().unwrap())
        .await
        .expect("register client before pre-pinning a close");
    assert!(
        server
            .connection_manager
            .request_close_for(&player_id, crate::coordination::CloseReason::SlowConsumer),
        "test setup should pin the delivery owner"
    );

    let shutdown = Arc::new(Notify::new());
    let cleanup_task = tokio::spawn({
        let server = Arc::clone(&server);
        let shutdown = Arc::clone(&shutdown);
        async move {
            server.cleanup_task_until(shutdown.notified()).await;
        }
    });
    tokio::time::sleep(Duration::from_millis(25)).await;

    assert!(
        server.connection_manager.client_ids().contains(&player_id),
        "the activity reaper must not unregister a close owned by another subsystem"
    );
    assert_eq!(
        listener.requested_reason(),
        Some(crate::coordination::CloseReason::SlowConsumer),
        "the original close owner must remain authoritative"
    );
    assert_no_queued_message(
        &mut receiver,
        "the activity reaper must not enqueue a contradictory timeout farewell",
    );
    assert_eq!(
        server
            .metrics
            .expired_players_cleaned
            .load(Ordering::Relaxed),
        0,
        "the activity reaper must not count another close owner's connection",
    );

    shutdown.notify_one();
    timeout(Duration::from_secs(1), cleanup_task)
        .await
        .expect("cleanup task should observe shutdown")
        .expect("cleanup task should not panic");
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_cleanup_task_exits_without_activity_timeout_eviction() {
    let server = create_test_server_with_config(ServerConfig {
        ping_timeout: Duration::from_nanos(1),
        ..ServerConfig::default()
    })
    .await;
    let (player_id, _receiver) = register_client(&server, "127.0.0.1:48014".parse().unwrap()).await;

    assert!(
        server.begin_shutdown_drain().started_by_this_call,
        "test must transition the server into draining"
    );

    timeout(
        Duration::from_secs(1),
        server.cleanup_task_until(std::future::pending::<()>()),
    )
    .await
    .expect("cleanup task should stop after observing shutdown drain");

    assert!(
        server.connection_manager.client_ids().contains(&player_id),
        "shutdown drain must stop the activity reaper before it can evict with 4003"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_cleanup_task_stops_when_drain_starts_during_activity_farewell() {
    let coordinator = Arc::new(DrainTriggerCoordinator::new(
        DrainTrigger::FirstFarewellTrySend,
    ));
    let message_coordinator: Arc<dyn MessageCoordinator> = coordinator.clone();
    let server = create_test_server_with_message_coordinator(
        ServerConfig {
            ping_timeout: Duration::from_nanos(1),
            ..ServerConfig::default()
        },
        message_coordinator,
    )
    .await;
    coordinator.attach_server(&server);

    let (sender, mut receiver) = mpsc::channel(8);
    let (close, listener) = crate::coordination::ConnectionCloseSignal::channel();
    let player_id = server
        .register_client_with_close(sender, close, "127.0.0.1:48019".parse().unwrap())
        .await
        .expect("client registration succeeds before drain");

    timeout(
        Duration::from_secs(1),
        server.cleanup_task_until(std::future::pending::<()>()),
    )
    .await
    .expect("cleanup task should stop after drain starts during farewell");

    assert!(
        coordinator.try_send_calls.load(Ordering::Acquire) > 0,
        "test must drive the activity farewell path"
    );
    assert!(server.is_draining(), "farewell path should trigger drain");
    assert!(
        server.connection_manager.client_ids().contains(&player_id),
        "drain that starts mid-tick must prevent activity-timeout eviction"
    );
    assert_no_queued_message(
        &mut receiver,
        "activity-timeout farewell must not be enqueued after drain starts",
    );
    assert_eq!(
        listener.requested_reason(),
        None,
        "activity reaper must not pin a 4003 close after drain starts"
    );
    assert_eq!(
        server
            .metrics
            .expired_players_cleaned
            .load(Ordering::Relaxed),
        0,
        "no expired-player metric should be recorded when drain stops eviction"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn draining_cleanup_task_skips_activity_farewell_when_drain_starts_during_inmemory_lookup() {
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_millis(
            ServerConfig::default()
                .websocket_config
                .slow_consumer_timeout_ms,
        ),
        Arc::new(crate::metrics::ServerMetrics::new()),
    ));
    let message_coordinator: Arc<dyn MessageCoordinator> = coordinator.clone();
    let server = create_test_server_with_message_coordinator(
        ServerConfig {
            ping_timeout: Duration::from_nanos(1),
            ..ServerConfig::default()
        },
        message_coordinator,
    )
    .await;

    let (sender, mut receiver) = mpsc::channel(8);
    let (close, listener) = crate::coordination::ConnectionCloseSignal::channel();
    let player_id = server
        .register_client_with_close(sender, close, "127.0.0.1:48030".parse().unwrap())
        .await
        .expect("client registration succeeds before drain");

    let coordinator_write = coordinator.local_clients.write().await;
    let cleanup_task = tokio::spawn({
        let server = Arc::clone(&server);
        async move {
            server
                .cleanup_task_until(std::future::pending::<()>())
                .await;
        }
    });
    tokio::pin!(cleanup_task);

    assert!(
        timeout(Duration::from_millis(50), &mut cleanup_task)
            .await
            .is_err(),
        "cleanup should block on the in-memory coordinator lookup before drain starts"
    );
    assert!(
        server.begin_shutdown_drain().started_by_this_call,
        "test should start drain while farewell lookup is blocked"
    );
    drop(coordinator_write);

    timeout(Duration::from_secs(1), &mut cleanup_task)
        .await
        .expect("cleanup task should finish after drain cancels farewell")
        .expect("cleanup task should not panic");

    assert!(
        server.connection_manager.client_ids().contains(&player_id),
        "drain must stop activity eviction after the delayed lookup"
    );
    assert_no_queued_message(
        &mut receiver,
        "ActivityTimeout farewell must not be enqueued after drain starts during lookup",
    );
    assert_eq!(
        listener.requested_reason(),
        None,
        "activity reaper must not pin 4003 after drain starts during lookup"
    );
    assert_eq!(
        server
            .metrics
            .expired_players_cleaned
            .load(Ordering::Relaxed),
        0,
        "no expired-player metric should be recorded when drain cancels eviction"
    );
}
