//! Root configuration types.

use super::coordination::CoordinationConfig;
use super::defaults::default_port;
use super::logging::LoggingConfig;
use super::metrics::MetricsConfig;
use super::protocol::ProtocolConfig;
use super::relay::RelayTypeConfig;
use super::security::{AuthMaintenanceConfig, SecurityConfig};
use super::server::{RateLimitConfig, ServerConfig};
use super::websocket::WebSocketConfig;
use serde::{Deserialize, Serialize};

/// Root configuration struct for Signal Fish.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub protocol: ProtocolConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub auth: AuthMaintenanceConfig,
    #[serde(default)]
    pub coordination: CoordinationConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub relay_types: RelayTypeConfig,
    #[serde(default)]
    pub websocket: WebSocketConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: default_port(),
            server: ServerConfig::default(),
            rate_limit: RateLimitConfig::default(),
            protocol: ProtocolConfig::default(),
            logging: LoggingConfig::default(),
            security: SecurityConfig::default(),
            auth: AuthMaintenanceConfig::default(),
            coordination: CoordinationConfig::default(),
            metrics: MetricsConfig::default(),
            relay_types: RelayTypeConfig::default(),
            websocket: WebSocketConfig::default(),
        }
    }
}
