use super::*;
use crate::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig, TurnConfig,
};
use crate::coordination::{ClientDeliveryHandle, MessageCoordinator};
use crate::database::{create_database, DatabaseConfig, GameDatabase, RoomCleanupOutcome};
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

    let connection_manager = ConnectionManager::new(
        config.max_connections_per_ip,
        Arc::clone(&metrics),
        Arc::clone(&message_coordinator),
        config.websocket_config.delivery_stats_interval_secs > 0,
    );
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
        active_session_plans: DashMap::new(),
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

    async fn transition_room_to_waiting(&self, room_id: &RoomId) -> anyhow::Result<()> {
        self.inner.transition_room_to_waiting(room_id).await
    }

    async fn toggle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
    ) -> anyhow::Result<Option<(LobbyState, Vec<PlayerId>, bool)>> {
        self.inner.toggle_player_ready(room_id, player_id).await
    }

    async fn finalize_room_game(&self, room_id: &RoomId) -> anyhow::Result<()> {
        self.inner.finalize_room_game(room_id).await
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
}

struct DrainTriggerCoordinator {
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
            .is_some_and(|handle| handle.sender.try_send(message).is_ok())
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
            let _ = handle.sender.try_send(Arc::clone(&message));
        }
        Ok(true)
    }

    async fn broadcast_to_room(
        &self,
        room_id: &RoomId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()> {
        for handle in self.recipients_for(room_id, None).await {
            let _ = handle.sender.try_send(Arc::clone(&message));
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
            let _ = handle.sender.try_send(Arc::clone(&message));
        }
        Ok(())
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
async fn draining_cleanup_task_exits_without_activity_timeout_eviction() {
    let server = create_test_server_with_config(ServerConfig {
        ping_timeout: Duration::ZERO,
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
            ping_timeout: Duration::ZERO,
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
            ping_timeout: Duration::ZERO,
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
