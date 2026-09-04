//! Root configuration types.

use super::coordination::CoordinationConfig;
use super::defaults::default_port;
use super::logging::LoggingConfig;
use super::metrics::MetricsConfig;
use super::protocol::ProtocolConfig;
use super::relay::RelayTypeConfig;
use super::security::SecurityConfig;
use super::server::{RateLimitConfig, ServerConfig};
use super::session::SessionConfig;
use super::turn::TurnConfig;
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
    pub coordination: CoordinationConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub relay_types: RelayTypeConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub turn: TurnConfig,
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
            coordination: CoordinationConfig::default(),
            metrics: MetricsConfig::default(),
            relay_types: RelayTypeConfig::default(),
            session: SessionConfig::default(),
            turn: TurnConfig::default(),
            websocket: WebSocketConfig::default(),
        }
    }
}

/// Marker substituted for every *set* secret value by
/// [`Config::redacted_for_display`].
pub const REDACTED_SECRET: &str = "<redacted>";

impl Config {
    /// Clone of this configuration safe for display (`--print-config`): every
    /// **set** secret value is replaced with [`REDACTED_SECRET`], while unset
    /// secrets (`None` or empty string) are left as-is so operators can still
    /// tell "configured" apart from "missing".
    ///
    /// Redacted fields (keep in sync with `redaction_covers_every_secret_field`
    /// in this module's tests when adding credential-like config):
    /// - `security.metrics_auth_token`
    /// - `session.ice_servers[*].credential` (static TURN credentials)
    /// - `turn.static_auth_secret`
    ///
    /// TLS *paths* (`security.transport.tls.*_path`) are file locations, not
    /// secrets, and stay visible.
    #[must_use]
    pub fn redacted_for_display(&self) -> Self {
        let mut redacted = self.clone();
        redact_optional(&mut redacted.security.metrics_auth_token);
        for ice_server in &mut redacted.session.ice_servers {
            redact_optional(&mut ice_server.credential);
        }
        redact(&mut redacted.turn.static_auth_secret);
        redacted
    }
}

/// Replace a set (non-empty) secret with [`REDACTED_SECRET`]; leave empty
/// values alone so "unset" stays distinguishable from "set".
fn redact(value: &mut String) {
    if !value.is_empty() {
        REDACTED_SECRET.clone_into(value);
    }
}

/// Optional-field variant of [`redact`]: `None` and `Some("")` are unset and
/// stay as-is.
fn redact_optional(value: &mut Option<String>) {
    if let Some(inner) = value {
        redact(inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppRegistrationEntry;
    use crate::protocol::IceServer;

    /// Sentinel secret literals that must never survive redaction.
    const SECRETS: [&str; 3] = [
        "metrics-token-sentinel",
        "ice-credential-sentinel",
        "turn-static-secret-sentinel",
    ];

    /// A config with every known secret field populated with a sentinel value.
    fn config_with_all_secrets() -> Config {
        let mut config = Config::default();
        config.security.metrics_auth_token = Some(SECRETS[0].to_string());
        config.security.allowed_apps = vec![AppRegistrationEntry {
            app_id: "app-id-not-secret".to_string(),
            app_name: "Test App".to_string(),
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
            max_relay_bytes: None,
        }];
        config.session.ice_servers = vec![IceServer {
            urls: vec!["turn:turn.example.com:3478".to_string()],
            username: Some("ice-username".to_string()),
            credential: Some(SECRETS[1].to_string()),
        }];
        config.turn.enabled = true;
        config.turn.static_auth_secret = SECRETS[2].to_string();
        config.turn.urls = vec!["turn:turn.example.com:3478".to_string()];
        config
    }

    #[test]
    fn redaction_removes_every_secret_literal_from_serialized_output() {
        let json = serde_json::to_string_pretty(&config_with_all_secrets().redacted_for_display())
            .expect("redacted config serializes");

        for secret in SECRETS {
            assert!(
                !json.contains(secret),
                "secret literal {secret:?} leaked into --print-config output:\n{json}"
            );
        }
        assert!(
            json.contains(REDACTED_SECRET),
            "set secrets must be visibly marked as redacted:\n{json}"
        );
    }

    #[test]
    fn redaction_replaces_each_secret_field_with_the_marker() {
        let redacted = config_with_all_secrets().redacted_for_display();

        assert_eq!(
            redacted.security.metrics_auth_token.as_deref(),
            Some(REDACTED_SECRET)
        );
        assert_eq!(
            redacted.session.ice_servers[0].credential.as_deref(),
            Some(REDACTED_SECRET)
        );
        assert_eq!(redacted.turn.static_auth_secret, REDACTED_SECRET);
    }

    #[test]
    fn redaction_preserves_non_secret_fields() {
        let original = config_with_all_secrets();
        let redacted = original.redacted_for_display();

        assert_eq!(redacted.port, original.port);
        assert_eq!(
            redacted.security.allowed_apps[0].app_id,
            "app-id-not-secret"
        );
        assert_eq!(redacted.security.allowed_apps[0].app_name, "Test App");
        assert_eq!(
            redacted.session.ice_servers[0].urls,
            original.session.ice_servers[0].urls
        );
        // The ICE username is the public half of the credential pair.
        assert_eq!(
            redacted.session.ice_servers[0].username.as_deref(),
            Some("ice-username")
        );
        assert_eq!(redacted.turn.urls, original.turn.urls);
    }

    #[test]
    fn redaction_leaves_unset_secrets_unset() {
        // Defaults: every secret is None / empty. None of them may be falsely
        // marked as redacted, so operators can tell unset from set.
        let redacted = Config::default().redacted_for_display();

        assert!(redacted.security.metrics_auth_token.is_none());
        assert!(redacted.security.allowed_apps.is_empty());
        assert!(redacted.turn.static_auth_secret.is_empty());

        let json = serde_json::to_string(&redacted).expect("default config serializes");
        assert!(
            !json.contains(REDACTED_SECRET),
            "an all-unset config must not show the redaction marker:\n{json}"
        );

        // Empty-but-present values also stay as-is ("" is unset, not a secret).
        let mut config = Config::default();
        config.security.metrics_auth_token = Some(String::new());
        let redacted = config.redacted_for_display();
        assert_eq!(redacted.security.metrics_auth_token.as_deref(), Some(""));
    }

    /// Future-proofing sweep: walk the serialized JSON of a fully-populated,
    /// redacted config and require that every *string* leaf stored under a
    /// credential-suggesting key is either empty or the redaction marker. A
    /// newly added secret-bearing config field whose name follows the
    /// conventional patterns will fail this test until it is added to
    /// [`Config::redacted_for_display`].
    #[test]
    fn redaction_covers_every_secret_field() {
        const SECRET_KEY_MARKERS: [&str; 5] =
            ["secret", "token", "password", "credential", "api_key"];
        // Keys that *match* a marker but are known non-secret scalars. Keep this
        // list short and well-justified.
        const ALLOWED_KEYS: [&str; 0] = [];

        fn walk(key_path: &str, value: &serde_json::Value, hits: &mut Vec<(String, String)>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, child) in map {
                        let child_path = if key_path.is_empty() {
                            key.clone()
                        } else {
                            format!("{key_path}.{key}")
                        };
                        let key_lower = key.to_lowercase();
                        let looks_secret = SECRET_KEY_MARKERS
                            .iter()
                            .any(|marker| key_lower.contains(marker))
                            && !ALLOWED_KEYS.contains(&key_lower.as_str());
                        if looks_secret {
                            if let serde_json::Value::String(text) = child {
                                hits.push((child_path.clone(), text.clone()));
                            }
                        }
                        walk(&child_path, child, hits);
                    }
                }
                serde_json::Value::Array(items) => {
                    for (index, item) in items.iter().enumerate() {
                        walk(&format!("{key_path}[{index}]"), item, hits);
                    }
                }
                _ => {}
            }
        }

        let redacted = serde_json::to_value(config_with_all_secrets().redacted_for_display())
            .expect("redacted config serializes");
        let mut hits = Vec::new();
        walk("", &redacted, &mut hits);

        assert!(
            hits.len() >= SECRETS.len(),
            "the sweep must visit every known secret field (found only {hits:?})"
        );
        for (path, value) in hits {
            assert!(
                value.is_empty() || value == REDACTED_SECRET,
                "credential-like config field `{path}` leaked a value through \
                 redacted_for_display(): {value:?}"
            );
        }
    }
}
