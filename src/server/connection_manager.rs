use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::AppInfo;
use crate::coordination::MessageCoordinator;
use crate::metrics::ServerMetrics;
use crate::protocol::{GameDataEncoding, PlayerId, RoomId, ServerMessage};

use super::RegisterClientError;

#[derive(Debug, Clone)]
pub(crate) struct ClientConnection {
    pub room_id: Option<RoomId>,
    pub last_ping: Instant,
    /// Tracks when we last recorded `last_seen` for this client.
    /// Used to throttle heartbeat updates - we only record if this is older
    /// than the configured threshold (default 30 seconds).
    pub last_heartbeat_update: Option<Instant>,
    pub sender: mpsc::Sender<Arc<ServerMessage>>,
    pub client_addr: SocketAddr,
    pub game_data_format: GameDataEncoding,
    pub app_info: Option<AppInfo>,
}

pub(crate) struct ConnectionManager {
    clients: DashMap<PlayerId, ClientConnection>,
    connections_per_ip: DashMap<IpAddr, usize>,
    metrics: Arc<ServerMetrics>,
    message_coordinator: Arc<dyn MessageCoordinator>,
    max_connections_per_ip: usize,
}

impl ConnectionManager {
    pub fn new(
        max_connections_per_ip: usize,
        metrics: Arc<ServerMetrics>,
        message_coordinator: Arc<dyn MessageCoordinator>,
    ) -> Self {
        Self {
            clients: DashMap::new(),
            connections_per_ip: DashMap::new(),
            metrics,
            message_coordinator,
            max_connections_per_ip,
        }
    }

    pub async fn register_client(
        &self,
        sender: mpsc::Sender<Arc<ServerMessage>>,
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

        let player_id = Uuid::new_v4();
        let connection = ClientConnection {
            room_id: None,
            last_ping: Instant::now(),
            last_heartbeat_update: None,
            sender: sender.clone(),
            client_addr,
            game_data_format: GameDataEncoding::Json,
            app_info: None,
        };

        self.clients.insert(player_id, connection);
        self.metrics.increment_connections();

        if let Err(err) = self
            .message_coordinator
            .register_local_client(player_id, None, sender)
            .await
        {
            warn!(%player_id, %err, "Failed to register client with coordinator");
        }

        info!(%player_id, instance_id = %instance_id, client_addr = %client_addr, "Client registered");
        Ok(player_id)
    }

    pub async fn connect_test_client(
        &self,
        player_id: PlayerId,
        sender: mpsc::Sender<Arc<ServerMessage>>,
        client_addr: SocketAddr,
    ) {
        let connection = ClientConnection {
            room_id: None,
            last_ping: Instant::now(),
            last_heartbeat_update: None,
            sender: sender.clone(),
            client_addr,
            game_data_format: GameDataEncoding::Json,
            app_info: None,
        };

        self.increment_ip_slot_unbounded(client_addr.ip());
        self.clients.insert(player_id, connection);
        self.metrics.increment_connections();

        if let Err(err) = self
            .message_coordinator
            .register_local_client(player_id, None, sender)
            .await
        {
            warn!(%player_id, %err, "Failed to register test client with coordinator");
        }
    }

    pub async fn assign_client_to_room(&self, player_id: &PlayerId, room_id: RoomId) {
        if let Some(mut client) = self.clients.get_mut(player_id) {
            client.room_id = Some(room_id);
            let sender = client.sender.clone();
            drop(client);
            if let Err(err) = self
                .message_coordinator
                .register_local_client(*player_id, Some(room_id), sender)
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

    pub fn set_game_data_format(&self, player_id: &PlayerId, format: GameDataEncoding) {
        if let Some(mut connection) = self.clients.get_mut(player_id) {
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

    pub fn set_app_info(&self, player_id: &PlayerId, app_info: AppInfo) {
        if let Some(mut connection) = self.clients.get_mut(player_id) {
            connection.app_info = Some(app_info);
        }
    }

    pub fn app_info(&self, player_id: &PlayerId) -> Option<AppInfo> {
        self.clients
            .get(player_id)
            .and_then(|conn| conn.app_info.clone())
    }

    pub fn app_id(&self, player_id: &PlayerId) -> Option<Uuid> {
        self.app_info(player_id).map(|info| info.id)
    }

    pub fn clear_room_assignment(
        &self,
        player_id: &PlayerId,
    ) -> Option<mpsc::Sender<Arc<ServerMessage>>> {
        self.clients.get_mut(player_id).map(|mut client| {
            client.room_id = None;
            client.sender.clone()
        })
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
    /// This throttling mechanism reduces update overhead while maintaining
    /// the 5-minute cross-instance staleness window accuracy (30s << 5min).
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
            // Client not found, allow update (will fail at DB level anyway)
            true
        }
    }

    pub fn get_client_room(&self, player_id: &PlayerId) -> Option<RoomId> {
        self.clients
            .get(player_id)
            .and_then(|client| client.room_id)
    }

    pub fn has_client(&self, player_id: &PlayerId) -> bool {
        self.clients.contains_key(player_id)
    }

    pub fn reassign_connection(
        &self,
        current_player_id: &PlayerId,
        reconnect_player_id: &PlayerId,
        room_id: RoomId,
    ) -> bool {
        // Atomically remove the old entry (no separate get-then-remove race)
        if let Some((_, old_connection)) = self.clients.remove(current_player_id) {
            let new_client = ClientConnection {
                room_id: Some(room_id),
                last_ping: Instant::now(),
                last_heartbeat_update: None, // Reset on reconnection, will update immediately
                sender: old_connection.sender,
                client_addr: old_connection.client_addr,
                game_data_format: old_connection.game_data_format,
                app_info: old_connection.app_info,
            };

            // IP slot is already reserved from the old entry -- no need to
            // release and re-reserve for the same IP address.
            self.clients.insert(*reconnect_player_id, new_client);
            true
        } else {
            false
        }
    }

    pub fn remove_client(&self, player_id: &PlayerId) -> Option<ClientConnection> {
        self.clients.remove(player_id).map(|(_, connection)| {
            self.release_ip_slot(connection.client_addr.ip());
            connection
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

    fn try_reserve_ip_slot(&self, ip: IpAddr) -> Result<usize, usize> {
        match self.connections_per_ip.entry(ip) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                let current = *entry.get();
                if current >= self.max_connections_per_ip {
                    Err(current)
                } else {
                    let count = entry.get_mut();
                    *count += 1;
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
                *entry.get_mut() += 1;
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
                *entry.get_mut() -= 1;
            } else {
                entry.remove();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::{MembershipUpdate, MessageCoordinator};
    use crate::distributed::SequencedMessage;
    use anyhow::Result;
    use async_trait::async_trait;
    use std::net::SocketAddr;
    use tokio::sync::{mpsc, Mutex};

    #[derive(Default)]
    struct TestCoordinator {
        registrations: Mutex<Vec<(PlayerId, Option<RoomId>)>>,
        unregisters: Mutex<Vec<PlayerId>>,
    }

    #[async_trait]
    impl MessageCoordinator for TestCoordinator {
        async fn send_to_player(
            &self,
            _player_id: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
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

        async fn register_local_client(
            &self,
            player_id: PlayerId,
            room_id: Option<RoomId>,
            _sender: mpsc::Sender<Arc<ServerMessage>>,
        ) -> Result<()> {
            self.registrations.lock().await.push((player_id, room_id));
            Ok(())
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
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator: Arc<dyn MessageCoordinator> = Arc::new(TestCoordinator::default());
        ConnectionManager::new(max_connections_per_ip, metrics, coordinator)
    }

    fn channel() -> (
        mpsc::Sender<Arc<ServerMessage>>,
        mpsc::Receiver<Arc<ServerMessage>>,
    ) {
        mpsc::channel(4)
    }

    #[tokio::test]
    async fn register_client_enforces_ip_limits_and_releases_on_remove() {
        let manager = make_manager(1);
        let addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

        let (tx1, _rx1) = channel();
        let first_id = manager
            .register_client(tx1, addr, Uuid::new_v4())
            .await
            .expect("first registration succeeds");

        let (tx2, _rx2) = channel();
        let err = manager
            .register_client(tx2, addr, Uuid::new_v4())
            .await
            .expect_err("second client hits per-IP limit");
        match err {
            RegisterClientError::IpLimitExceeded { current, limit } => {
                assert_eq!(current, 1);
                assert_eq!(limit, 1);
            }
        }

        manager.remove_client(&first_id);

        let (tx3, _rx3) = channel();
        manager
            .register_client(tx3, addr, Uuid::new_v4())
            .await
            .expect("registrations resume after slot release");
    }

    #[tokio::test]
    async fn assign_client_to_room_updates_coordinator_membership() {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(TestCoordinator::default());
        let manager = ConnectionManager::new(
            4,
            metrics.clone(),
            coordinator.clone() as Arc<dyn MessageCoordinator>,
        );

        let (tx, _rx) = channel();
        let addr: SocketAddr = "127.0.0.1:6000".parse().unwrap();
        let player_id = manager
            .register_client(tx, addr, Uuid::new_v4())
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
                manager.register_client(tx, addr, Uuid::new_v4()).await
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
        let result = manager.register_client(tx, addr, Uuid::new_v4()).await;
        assert!(
            result.is_ok(),
            "Registration should succeed after all clients removed"
        );
    }

    /// D18: Reassignment does not leak IP slots.
    ///
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
            .register_client(tx, addr, Uuid::new_v4())
            .await
            .expect("registration should succeed");

        let room_id = RoomId::new_v4();
        let new_player_id = Uuid::new_v4();

        let reassigned = manager.reassign_connection(&original_id, &new_player_id, room_id);
        assert!(reassigned, "Reassignment should succeed");

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
                .register_client(tx, new_addr, Uuid::new_v4())
                .await
                .expect("should succeed within limit");
        }

        // 5th attempt from same IP should fail (already at limit)
        let (tx, _rx) = channel();
        let new_addr: SocketAddr = "10.0.0.2:10000".parse().unwrap();
        let result = manager.register_client(tx, new_addr, Uuid::new_v4()).await;
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
            .register_client(tx_verify, verify_addr, Uuid::new_v4())
            .await;
        assert!(
            result.is_ok(),
            "Registration should succeed after removing the reassigned client"
        );
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
                .register_client(tx, port_addr, Uuid::new_v4())
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
            let result = manager.register_client(tx, port_addr, Uuid::new_v4()).await;
            assert!(
                result.is_ok(),
                "Registration #{} should succeed after complete removal (no underflow)",
                i + 1
            );
        }
    }
}
