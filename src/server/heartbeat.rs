use crate::protocol::{PlayerId, ServerMessage};
use std::sync::Arc;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Handle ping with coordination.
    ///
    /// Updates the in-memory ping timestamp (always) and the `last_seen` timestamp
    /// (throttled based on `heartbeat_throttle` configuration to reduce processing overhead).
    pub async fn handle_ping(&self, player_id: &PlayerId) {
        // Always record the ping in memory for disconnect detection
        self.connection_manager.record_ping(player_id);

        // Only update last_seen if enough time has passed since last update (throttled)
        self.maybe_update_last_seen(player_id).await;

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
        TransportSecurityConfig,
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
    async fn handle_ping_resets_timeout_and_replies() {
        let server = create_test_server().await;
        let (sender, mut receiver) = mpsc::channel(4);
        let addr: SocketAddr = "127.0.0.1:45000".parse().unwrap();

        let player_id = server
            .connection_manager
            .register_client(sender, addr, server.instance_id)
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
