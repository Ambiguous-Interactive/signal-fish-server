//! Configuration module for Signal Fish.
//!
//! This module provides comprehensive configuration management with support for:
//! - JSON configuration files
//! - Environment variable overrides
//! - Stdin input
//! - Sensible defaults
//!
//! # Module Structure
//!
//! - [`crate::config::types`]: Root `Config` struct
//! - [`server`]: Server behavior configuration (rooms, players, timeouts)
//! - [`protocol`]: Protocol settings (SDK compatibility, player names)
//! - [`security`]: Security and authentication settings
//! - [`crate::config::relay`]: Relay type configuration
//! - [`logging`]: Logging configuration
//! - [`coordination`]: Cross-instance coordination settings
//! - [`metrics`]: Metrics configuration
//! - [`websocket`]: WebSocket connection settings
//! - [`crate::config::loader`]: Configuration loading functions
//! - [`crate::config::validation`]: Configuration validation functions
//! - [`crate::config::defaults`]: Default value functions

// Submodules
pub mod coordination;
pub mod defaults;
pub mod loader;
pub mod logging;
pub mod metrics;
pub mod protocol;
pub mod relay;
pub mod security;
pub mod server;
pub mod types;
pub mod validation;
pub mod websocket;

// Re-exports for convenience
pub use coordination::{CoordinationConfig, DedupCacheConfig};

pub use defaults::DashboardHistoryField;

pub use loader::load;

pub use logging::{LogFormat, LogLevel, LoggingConfig};

pub use metrics::MetricsConfig;

pub use protocol::{
    PlayerNameValidationConfig, ProtocolConfig, SdkCompatibilityConfig, SdkCompatibilityError,
    SdkCompatibilityReport,
};

pub use relay::RelayTypeConfig;

pub use security::{
    AppAuthEntry, AuthMaintenanceConfig, ClientAuthMode, SecurityConfig, TlsServerConfig,
    TokenBindingConfig, TransportSecurityConfig,
};

pub use server::{RateLimitConfig, ServerConfig};

pub use types::Config;

pub use validation::{is_production_mode, validate_config_security};

pub use websocket::WebSocketConfig;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = Config::default();

        assert_eq!(config.port, 3536);
        assert_eq!(config.server.default_max_players, 8);
        assert_eq!(config.server.ping_timeout, 30);
        assert_eq!(config.server.room_cleanup_interval, 60);
        assert_eq!(config.server.max_rooms_per_game, 1000);
        assert_eq!(config.server.empty_room_timeout, 300);
        assert_eq!(config.server.inactive_room_timeout, 3600);

        assert_eq!(config.rate_limit.max_room_creations, 5);
        assert_eq!(config.rate_limit.time_window, 60);
        assert_eq!(config.rate_limit.max_join_attempts, 20);

        assert_eq!(config.protocol.max_game_name_length, 64);
        assert_eq!(config.protocol.room_code_length, 6);
        assert_eq!(config.protocol.max_player_name_length, 32);
        assert_eq!(config.protocol.max_players_limit, 100);

        assert_eq!(config.logging.dir, "logs");
        assert_eq!(config.logging.filename, "server.log");
        assert_eq!(config.logging.rotation, "daily");
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let json = serde_json::to_string_pretty(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();

        assert_eq!(config.port, deserialized.port);
        assert_eq!(
            config.server.default_max_players,
            deserialized.server.default_max_players
        );
        assert_eq!(
            config.rate_limit.max_room_creations,
            deserialized.rate_limit.max_room_creations
        );
        assert_eq!(
            config.protocol.max_game_name_length,
            deserialized.protocol.max_game_name_length
        );
    }

    #[test]
    fn test_log_level_display() {
        assert_eq!(LogLevel::Trace.to_string(), "trace");
        assert_eq!(LogLevel::Debug.to_string(), "debug");
        assert_eq!(LogLevel::Info.to_string(), "info");
        assert_eq!(LogLevel::Warn.to_string(), "warn");
        assert_eq!(LogLevel::Error.to_string(), "error");
    }

    #[test]
    fn test_log_level_as_str() {
        assert_eq!(LogLevel::Trace.as_str(), "trace");
        assert_eq!(LogLevel::Debug.as_str(), "debug");
        assert_eq!(LogLevel::Info.as_str(), "info");
        assert_eq!(LogLevel::Warn.as_str(), "warn");
        assert_eq!(LogLevel::Error.as_str(), "error");
    }

    #[test]
    fn test_player_name_validation_config() {
        let config = PlayerNameValidationConfig::default();

        // Default allowed symbols
        assert!(config.is_allowed_symbol('-'));
        assert!(config.is_allowed_symbol('_'));
        assert!(!config.is_allowed_symbol('@'));
        assert!(!config.is_allowed_symbol('!'));

        // With additional characters
        let config_with_extra = PlayerNameValidationConfig {
            additional_allowed_characters: Some("@#".to_string()),
            ..Default::default()
        };
        assert!(config_with_extra.is_allowed_symbol('@'));
        assert!(config_with_extra.is_allowed_symbol('#'));
        assert!(!config_with_extra.is_allowed_symbol('!'));
    }

    #[test]
    fn test_sdk_compatibility_evaluate() {
        let config = SdkCompatibilityConfig::default();

        // Valid Unity SDK
        let result = config.evaluate(Some("Unity"), Some("1.11.0"));
        assert!(result.is_ok());
        let report = result.unwrap();
        assert_eq!(report.platform, Some("Unity".to_string()));
        assert!(report.capabilities.contains(&"reconnection".to_string()));

        // Version too low
        let result = config.evaluate(Some("unity"), Some("1.5.0"));
        assert!(matches!(
            result,
            Err(SdkCompatibilityError::VersionTooLow { .. })
        ));

        // Unknown platform (when enforce is true)
        let result = config.evaluate(Some("unknown-platform"), Some("1.0.0"));
        assert!(matches!(
            result,
            Err(SdkCompatibilityError::PlatformUnknown { .. })
        ));
    }
}
