//! Security and application-identification configuration types.

use super::defaults::{
    default_client_auth_mode, default_cors_origins, default_max_connections_per_ip,
    default_max_message_size, default_max_outbound_message_size, default_max_signal_bytes,
    default_require_auth, default_token_binding_subprotocol,
};
use crate::security::token_binding::TokenBindingScheme;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Security configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SecurityConfig {
    /// Allowed HTTP CORS and browser WebSocket origins (comma-separated, or
    /// "*" for any). Origin-less native WebSocket clients remain compatible.
    #[serde(default = "default_cors_origins")]
    pub cors_origins: String,
    /// Require the public client `app_id` to match a configured application.
    ///
    /// This is an access/accounting allowlist, not credential authentication:
    /// shipped clients disclose their app IDs and no client secret is checked.
    #[serde(default = "default_require_auth")]
    #[serde(alias = "require_websocket_auth")]
    pub enforce_app_id_allowlist: bool,
    /// Enable authentication for metrics endpoint
    #[serde(default = "default_require_auth")]
    pub require_metrics_auth: bool,
    /// Authentication token for metrics endpoint (if required)
    #[serde(default)]
    pub metrics_auth_token: Option<String>,
    /// Maximum inbound WebSocket message size in bytes.
    ///
    /// Enforced twice, at two layers: messages over this value get a polite
    /// `MessageTooLarge` error frame from the application-level check, and the
    /// WebSocket upgrade itself caps frames/messages at `2 *
    /// max_message_size` so grossly oversized frames are killed at the
    /// transport layer before the server buffers them (the 2x headroom keeps
    /// the polite path the authority near the limit). Must be greater than 0.
    #[serde(default = "default_max_message_size")]
    pub max_message_size: usize,
    /// Maximum aggregate application payload, in bytes, of one outbound
    /// WebSocket text or binary message after protocol encoding and before
    /// WebSocket framing.
    ///
    /// The complete encoded message is checked before it is handed to the
    /// WebSocket sink, so transport fragmentation cannot bypass this limit.
    /// An oversized message is never partially written: the affected
    /// connection closes with WebSocket code 1009 instead. Must be in
    /// `1..=67108864` so every advertised value is portable.
    #[serde(default = "default_max_outbound_message_size")]
    pub max_outbound_message_size: usize,
    /// Maximum serialized size in bytes of a v3 `Signal` payload (the opaque
    /// `signal` value, measured as canonical JSON).
    ///
    /// Lives in `[security]` beside `max_message_size` because it bounds
    /// attacker-controlled payload *bytes* (a size cap), not a request *rate* —
    /// the per-connection signal rate limit stays in `[rate_limit].max_signals`.
    /// Must be `> 0` and must not exceed `max_message_size` (a larger value is
    /// dead config: such a frame is rejected by the frame cap first).
    #[serde(default = "default_max_signal_bytes")]
    pub max_signal_bytes: usize,
    /// Maximum concurrent connections per IP address.
    ///
    /// Sized to cover a full client behind one NAT egress (a 16-player
    /// session plus spectators and reconnect churn). Must be `> 0`: a zero
    /// cap rejects every registration with `IpLimitExceeded` (there is no
    /// "unlimited" convention here; a deliberate lockdown is expressed by
    /// shutting the listener down), and startup validation rejects it.
    #[serde(default = "default_max_connections_per_ip")]
    pub max_connections_per_ip: usize,
    /// Transport-level security configuration (TLS, mTLS, token binding scaffolding)
    #[serde(default)]
    pub transport: TransportSecurityConfig,
    /// Optional list of application IDs allowed to identify themselves.
    /// When empty and `enforce_app_id_allowlist` is false, all connections are
    /// accepted. When `enforce_app_id_allowlist` is true, only connections with
    /// an app_id matching one of these entries are accepted.
    #[serde(default)]
    #[serde(alias = "authorized_apps")]
    pub allowed_apps: Vec<AppRegistrationEntry>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            cors_origins: default_cors_origins(),
            enforce_app_id_allowlist: default_require_auth(),
            require_metrics_auth: default_require_auth(),
            metrics_auth_token: None,
            max_message_size: default_max_message_size(),
            max_outbound_message_size: default_max_outbound_message_size(),
            max_signal_bytes: default_max_signal_bytes(),
            max_connections_per_ip: default_max_connections_per_ip(),
            transport: TransportSecurityConfig::default(),
            allowed_apps: Vec::new(),
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
    /// Requires [`Self::enabled`] to be true.
    #[serde(default)]
    pub required: bool,
    /// Bind every proof to the authenticated mTLS leaf-certificate SHA-256 fingerprint.
    /// Requires mandatory token binding, built-in TLS, and optional or required TLS client auth.
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
            scheme: TokenBindingScheme::ServerNonceHkdfSha256,
        }
    }
}

/// Client authentication mode for TLS.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClientAuthMode {
    #[default]
    None,
    Optional,
    Require,
}

impl ClientAuthMode {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Optional => "optional",
            Self::Require => "require",
        }
    }
}

impl<'de> Deserialize<'de> for ClientAuthMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let token = raw.trim();

        if token.eq_ignore_ascii_case("none") {
            Ok(Self::None)
        } else if token.eq_ignore_ascii_case("optional") {
            Ok(Self::Optional)
        } else if token.eq_ignore_ascii_case("require") {
            Ok(Self::Require)
        } else {
            Err(serde::de::Error::custom(format!(
                "invalid client auth mode '{raw}', expected one of: none, optional, require"
            )))
        }
    }
}

impl fmt::Display for ClientAuthMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single application registration loaded from configuration.
///
/// Each entry defines the public identifier and limits for one application
/// allowed to connect when `enforce_app_id_allowlist` is enabled. It contains
/// no client credential and does not establish hostile-client identity.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AppRegistrationEntry {
    /// Unique identifier clients send in the `Authenticate` message.
    pub app_id: String,
    /// Human-readable name returned to the client after app identification.
    pub app_name: String,
    /// Optional maximum number of rooms this application may create. Must be
    /// `> 0` when set: a zero cap rejects every creation for this app
    /// (startup validation rejects it).
    #[serde(default)]
    pub max_rooms: Option<u32>,
    /// Optional maximum number of players per room for this application. Must
    /// be `> 0` when set: a zero cap rejects every join for this app (startup
    /// validation rejects it).
    #[serde(default)]
    pub max_players_per_room: Option<u8>,
    /// Optional per-minute request rate limit for this application. Must be
    /// `> 0` when set: a zero budget rejects every `Authenticate` for this
    /// app (startup validation rejects it).
    ///
    /// Enforced as two sliding windows: the application-wide ceiling, plus a
    /// per-source (IP) share of half the ceiling (at least one) so a single
    /// source cannot continuously exhaust the app's budget and lock out
    /// legitimate handshakes (issue #502).
    #[serde(default)]
    pub rate_limit_per_minute: Option<u32>,
}
