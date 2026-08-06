use crate::auth::AppContext;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Determine whether the client should use relay based on configuration.
    /// Note: signal-fish-server does not include relay servers - this always returns false.
    pub fn should_use_relay(&self, _relay_type: &str) -> bool {
        false // No relay server in signal-fish-server
    }

    /// Apply per-application relay bandwidth overrides.
    /// Note: signal-fish-server does not include relay servers - this is a no-op.
    pub fn apply_app_bandwidth_policy(&self, _app_context: &AppContext) {
        // No-op: no relay server in signal-fish-server
    }

    /// Resolve the relay type for a game based on configuration.
    /// This is used for protocol labeling even without a relay server.
    pub(crate) fn resolve_relay_type(&self, game_name: &str) -> String {
        self.relay_type_config
            .game_relay_mappings
            .get(game_name)
            .cloned()
            .unwrap_or_else(|| self.relay_type_config.default_relay_type.clone())
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{
        CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
        TransportSecurityConfig, TurnConfig,
    };
    use crate::database::DatabaseConfig;
    use crate::rate_limit::RateLimitConfig;
    use crate::server::{EnhancedGameServer, ServerConfig};
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Build a minimal in-memory server with the given relay-type config.
    async fn server_with_relay_config(relay: RelayTypeConfig) -> Arc<EnhancedGameServer> {
        let config = ServerConfig {
            rate_limit_config: RateLimitConfig::default(),
            ..ServerConfig::default()
        };
        EnhancedGameServer::new(
            config,
            ProtocolConfig::default(),
            relay,
            SessionConfig::default(),
            TurnConfig::default(),
            DatabaseConfig::InMemory,
            MetricsConfig::default(),
            CoordinationConfig::default(),
            TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("failed to construct test server")
    }

    /// `should_use_relay` must return `false` for any input: signal-fish-server
    /// ships no relay server. Kills the `should_use_relay -> true` mutant
    /// (relay_policy.rs:9), which would flip this to `true`.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn should_use_relay_is_always_false() {
        let server = server_with_relay_config(RelayTypeConfig::default()).await;
        assert!(
            !server.should_use_relay("matchbox"),
            "should_use_relay must be false: there is no relay server"
        );
        assert!(
            !server.should_use_relay(""),
            "should_use_relay must be false for any relay type"
        );
        assert!(
            !server.should_use_relay("unity_netcode"),
            "should_use_relay must be false even for a mapped relay type"
        );
    }

    /// `resolve_relay_type` returns the explicit per-game mapping when present
    /// and the configured default otherwise. Asserting the EXACT non-empty
    /// strings kills both the `String::new()` (empty) and `"xyzzy".into()`
    /// mutants at relay_policy.rs:21.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn resolve_relay_type_returns_mapping_then_default() {
        let mut mappings = HashMap::new();
        mappings.insert("Chess".to_string(), "unity_netcode".to_string());
        let relay = RelayTypeConfig {
            game_relay_mappings: mappings,
            default_relay_type: "matchbox".to_string(),
        };
        let server = server_with_relay_config(relay).await;

        // Explicitly mapped game: the mapped value, verbatim. Not "" and not
        // "xyzzy".
        assert_eq!(
            server.resolve_relay_type("Chess"),
            "unity_netcode",
            "a mapped game must resolve to its configured relay type"
        );

        // Unmapped game: falls back to the configured default. Also not "" and
        // not "xyzzy".
        assert_eq!(
            server.resolve_relay_type("Unmapped"),
            "matchbox",
            "an unmapped game must resolve to the default relay type"
        );
    }
}
