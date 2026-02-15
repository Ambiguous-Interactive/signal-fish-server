use crate::auth::AppInfo;

use super::EnhancedGameServer;

impl EnhancedGameServer {
    /// Determine whether the client should use relay based on configuration.
    /// Note: signal-fish-server does not include relay servers - this always returns false.
    pub fn should_use_relay(&self, _relay_type: &str) -> bool {
        false // No relay server in signal-fish-server
    }

    /// Apply per-application relay bandwidth overrides.
    /// Note: signal-fish-server does not include relay servers - this is a no-op.
    pub fn apply_app_bandwidth_policy(&self, _app_info: &AppInfo) {
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
