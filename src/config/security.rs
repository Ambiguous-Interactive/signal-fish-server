//! Security and authentication configuration types.

use super::defaults::{
    default_client_auth_mode, default_cors_origins, default_max_connections_per_ip,
    default_max_message_size, default_require_auth, default_token_binding_subprotocol,
};
use crate::security::token_binding::TokenBindingScheme;
use serde::{Deserialize, Serialize};

/// Security configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SecurityConfig {
    /// Allowed CORS origins (comma-separated, or "*" for any)
    #[serde(default = "default_cors_origins")]
    pub cors_origins: String,
    /// Enable authentication for WebSocket connections
    #[serde(default = "default_require_auth")]
    pub require_websocket_auth: bool,
    /// Enable authentication for metrics endpoint
    #[serde(default = "default_require_auth")]
    pub require_metrics_auth: bool,
    /// Authentication token for metrics endpoint (if required)
    #[serde(default)]
    pub metrics_auth_token: Option<String>,
    /// Maximum WebSocket message size in bytes
    #[serde(default = "default_max_message_size")]
    pub max_message_size: usize,
    /// Maximum connections per IP address
    #[serde(default = "default_max_connections_per_ip")]
    pub max_connections_per_ip: usize,
    /// Transport-level security configuration (TLS, mTLS, token binding scaffolding)
    #[serde(default)]
    pub transport: TransportSecurityConfig,
    /// Optional list of authorized applications.
    /// When empty and `require_websocket_auth` is false, all connections are
    /// accepted. When `require_websocket_auth` is true, only connections with
    /// an app_id matching one of these entries are accepted.
    #[serde(default)]
    pub authorized_apps: Vec<AppAuthEntry>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            cors_origins: default_cors_origins(),
            require_websocket_auth: default_require_auth(),
            require_metrics_auth: default_require_auth(),
            metrics_auth_token: None,
            max_message_size: default_max_message_size(),
            max_connections_per_ip: default_max_connections_per_ip(),
            transport: TransportSecurityConfig::default(),
            authorized_apps: Vec::new(),
        }
    }
}

/// Transport-level security configuration.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct TransportSecurityConfig {
    #[serde(default)]
    pub tls: TlsServerConfig,
    #[serde(default)]
    pub token_binding: TokenBindingConfig,
}

/// TLS server configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TlsServerConfig {
    /// Enable HTTPS/TLS termination for the HTTP + WebSocket listener.
    #[serde(default)]
    pub enabled: bool,
    /// Path to the PEM-encoded certificate chain presented to clients.
    #[serde(default)]
    pub certificate_path: Option<String>,
    /// Path to the PEM-encoded private key corresponding to the certificate chain.
    #[serde(default)]
    pub private_key_path: Option<String>,
    /// Optional path to a PEM bundle of trusted client roots when client auth is enabled.
    #[serde(default)]
    pub client_ca_cert_path: Option<String>,
    /// Whether client certificates are required.
    #[serde(default = "default_client_auth_mode")]
    pub client_auth: ClientAuthMode,
}

impl Default for TlsServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            certificate_path: None,
            private_key_path: None,
            client_ca_cert_path: None,
            client_auth: default_client_auth_mode(),
        }
    }
}

/// Optional token binding / zero-trust enforcement for WebSocket clients.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TokenBindingConfig {
    /// Enables support for token binding subprotocol negotiation.
    #[serde(default)]
    pub enabled: bool,
    /// Require clients to request/comply with the token binding subprotocol.
    #[serde(default)]
    pub required: bool,
    /// Require a verified mTLS fingerprint on every signed frame.
    #[serde(default)]
    pub require_client_fingerprint: bool,
    /// Name of the WebSocket subprotocol clients must advertise.
    #[serde(default = "default_token_binding_subprotocol")]
    pub subprotocol: String,
    /// Signing scheme used for per-frame proofs.
    #[serde(default)]
    pub scheme: TokenBindingScheme,
}

impl Default for TokenBindingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            required: false,
            require_client_fingerprint: false,
            subprotocol: default_token_binding_subprotocol(),
            scheme: TokenBindingScheme::SecWebsocketKeySha256,
        }
    }
}

/// Client authentication mode for TLS.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClientAuthMode {
    #[default]
    None,
    Optional,
    Require,
}

/// A single authorized application entry loaded from configuration.
///
/// Each entry defines the credentials and limits for one application that is
/// allowed to connect to the signaling server when `require_websocket_auth` is
/// enabled.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AppAuthEntry {
    /// Unique identifier clients send in the `Authenticate` message.
    pub app_id: String,
    /// Shared secret used for full credential validation (not required for
    /// app-id-only auth flow).
    pub app_secret: String,
    /// Human-readable name returned to the client after authentication.
    pub app_name: String,
    /// Optional maximum number of rooms this application may create.
    #[serde(default)]
    pub max_rooms: Option<u32>,
    /// Optional maximum number of players per room for this application.
    #[serde(default)]
    pub max_players_per_room: Option<u8>,
    /// Optional per-minute request rate limit for this application.
    #[serde(default)]
    pub rate_limit_per_minute: Option<u32>,
}

/// Auth maintenance configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AuthMaintenanceConfig {
    /// Interval between rate_limit_cache cleanup sweeps (seconds)
    #[serde(default = "super::defaults::default_rate_limit_cache_cleanup_interval_secs")]
    pub rate_limit_cache_cleanup_interval_secs: u64,
    /// Retention window for rate_limit_cache rows (seconds)
    #[serde(default = "super::defaults::default_rate_limit_cache_retention_secs")]
    pub rate_limit_cache_retention_secs: u64,
    /// Row-count threshold that triggers warning logs for cache drift
    #[serde(default = "super::defaults::default_rate_limit_cache_alert_rows")]
    pub rate_limit_cache_alert_rows: u64,
}

impl Default for AuthMaintenanceConfig {
    fn default() -> Self {
        Self {
            rate_limit_cache_cleanup_interval_secs:
                super::defaults::default_rate_limit_cache_cleanup_interval_secs(),
            rate_limit_cache_retention_secs:
                super::defaults::default_rate_limit_cache_retention_secs(),
            rate_limit_cache_alert_rows: super::defaults::default_rate_limit_cache_alert_rows(),
        }
    }
}
