use crate::protocol::{PlayerId, ServerMessage};
use std::sync::Arc;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Handle ping with coordination.
    ///
    /// Records the in-memory ping timestamp (always) for disconnect detection and
    /// replies `Pong`. The throttled `last_seen` + room-activity refresh is done
    /// once per inbound message by the router (`handle_client_message` →
    /// `maybe_update_last_seen`), so `Ping` needs no separate refresh here.
    pub async fn handle_ping(&self, player_id: &PlayerId) {
        // Always record the ping in memory for disconnect detection
        self.connection_manager.record_ping(player_id);

        let _ = self
            .message_coordinator
            .send_to_player(player_id, Arc::new(ServerMessage::Pong))
            .await;
    }

    /// Conditionally updates `last_seen` if the throttle threshold has elapsed.
    /// This reduces update overhead while maintaining cross-instance staleness accuracy.
    pub(super) async fn maybe_update_last_seen(&self, player_id: &PlayerId) {
        let threshold = self.config.heartbeat_throttle;

        // If throttle is disabled (Duration::ZERO), always update
        let should_update = threshold.is_zero()
            || self
                .connection_manager
                .should_update_last_seen(player_id, threshold);

        if should_update {
            self.metrics.increment_heartbeat_updates();
            if let Err(e) = self.database.update_player_last_seen(player_id).await {
                tracing::warn!(%player_id, "Failed to update player last_seen: {}", e);
            }
            // Keep the player's ROOM alive too. The activity reaper for a room
            // with players keys off `last_activity`, which is otherwise written
            // only at creation — so a room whose members are actively pinging,
            // relaying GameData, or exchanging WebRTC Signals would still be
            // GC'd `inactive_room_timeout` after creation, mid-game (BUG-1).
            // This method is the single throttled liveness-refresh, invoked once
            // per inbound message by `handle_client_message` (text frames) and
            // `handle_game_data_binary` (binary frames), so it covers every
            // message type uniformly.
            //
            // `update_room_activity` takes the global `rooms` write lock, so this
            // is deliberately gated behind the per-player heartbeat throttle
            // (`should_update`, default 30 s cadence): the relay hot path
            // (potentially every frame) acquires the lock at most once per player
            // per throttle window, not per message. A room needs only one refresh
            // per `inactive_room_timeout` (default 1 h) to stay alive, and startup
            // validation forces `heartbeat_throttle_secs < inactive_room_timeout`,
            // so the throttled cadence can never starve the reaper. If this ever
            // shows up in profiling, promote `last_activity` to an atomic so it can
            // be bumped under the shared read lock.
            if let Some(room_id) = self.connection_manager.get_client_room(player_id) {
                if let Err(e) = self.database.update_room_activity(&room_id).await {
                    tracing::warn!(%player_id, %room_id, "Failed to update room activity: {}", e);
                }
            }
        } else {
            self.metrics.increment_heartbeat_skipped();
            tracing::trace!(%player_id, "Skipped last_seen update (throttled)");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{
        AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
        SessionConfig, TransportSecurityConfig, TurnConfig,
    };
    use crate::database::DatabaseConfig;
    use crate::protocol::ServerMessage;
    use crate::server::{EnhancedGameServer, ServerConfig};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration as StdDuration;
    use tokio::sync::mpsc;
    use tokio::time::{sleep, timeout, Duration};

    async fn create_test_server() -> Arc<EnhancedGameServer> {
        EnhancedGameServer::new(
            ServerConfig {
                max_connections_per_ip: 32,
                ..ServerConfig::default()
            },
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

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn handle_ping_resets_timeout_and_replies() {
        let server = create_test_server().await;
        let (sender, mut receiver) = mpsc::channel(4);
        let addr: SocketAddr = "127.0.0.1:45000".parse().unwrap();

        let player_id = server
            .connection_manager
            .register_client(
                sender,
                crate::coordination::ConnectionCloseSignal::detached(),
                addr,
                server.instance_id,
            )
            .await
            .expect("client registration");

        sleep(Duration::from_millis(25)).await;
        let expired_before = server
            .connection_manager
            .collect_expired_clients(StdDuration::from_millis(5));
        assert_eq!(
            expired_before,
            vec![player_id],
            "player should look expired before ping"
        );

        server.handle_ping(&player_id).await;

        let msg = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("channel still open")
            .expect("message present");
        assert!(
            matches!(*msg, ServerMessage::Pong),
            "server responds with Pong"
        );

        let expired_after = server
            .connection_manager
            .collect_expired_clients(StdDuration::from_millis(5));
        assert!(
            expired_after.is_empty(),
            "ping refresh should remove player from expired set"
        );
    }
}
