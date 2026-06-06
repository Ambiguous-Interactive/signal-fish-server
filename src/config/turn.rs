//! TURN / STUN ICE-server configuration (Protocol v3, PLAN §P4, Appendix C/F).
//!
//! Drives the ICE servers emitted into a WebRTC `SessionPlan`. Two minting modes
//! are supported via [`TurnMode`]:
//!
//! - [`TurnMode::StaticSecret`] — the server self-mints short-lived, per-player
//!   coturn REST credentials from [`static_auth_secret`](TurnConfig::static_auth_secret)
//!   (coturn `--use-auth-secret`). The secret never leaves the server; clients
//!   receive only the expiring username/credential pair (see
//!   [`crate::security::mint_turn_credentials`]).
//! - [`TurnMode::Managed`] — defers to a managed provider (Cloudflare/Twilio/
//!   Metered) via [`managed_provider`](TurnConfig::managed_provider) /
//!   [`managed_api_token`](TurnConfig::managed_api_token). In P4 this is a STUN-only
//!   stub (no outbound HTTP dependency is added); provider minting is deferred.
//!
//! The whole block is inert when [`enabled`](TurnConfig::enabled) is `false`
//! (default): only the configured public `stun_urls` are advertised, keeping the
//! zero-dependency, relay-floor posture out of the box.

use serde::{Deserialize, Serialize};

/// How TURN credentials are obtained.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnMode {
    /// Self-mint coturn REST credentials from [`TurnConfig::static_auth_secret`]
    /// (wire token `"static_secret"`).
    StaticSecret,
    /// Fetch credentials from a managed provider (wire token `"managed"`). P4 stub:
    /// STUN-only, no provider call.
    Managed,
}

/// Default TURN mode: self-minted static-secret credentials.
const fn default_turn_mode() -> TurnMode {
    TurnMode::StaticSecret
}

/// Default public STUN servers advertised even when TURN is disabled.
fn default_stun_urls() -> Vec<String> {
    vec!["stun:stun.l.google.com:19302".to_string()]
}

/// Default TURN credential lifetime (1 hour).
const fn default_credential_ttl_secs() -> u64 {
    3600
}

/// ICE / TURN configuration (Appendix C `[turn]`).
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TurnConfig {
    /// Whether TURN credentials are minted and advertised. When `false` (default),
    /// only [`stun_urls`](Self::stun_urls) are advertised and no secret is required.
    #[serde(default)]
    pub enabled: bool,
    /// How TURN credentials are obtained (`static_secret` | `managed`).
    #[serde(default = "default_turn_mode")]
    pub mode: TurnMode,
    /// coturn `--static-auth-secret` (server-only). Required when
    /// [`enabled`](Self::enabled) and `mode == static_secret`; never sent to clients.
    #[serde(default)]
    pub static_auth_secret: String,
    /// TURN server URLs, e.g. `["turn:turn.example.com:3478"]`. Required (non-empty)
    /// when [`enabled`](Self::enabled) and `mode == static_secret`.
    #[serde(default)]
    pub urls: Vec<String>,
    /// Public STUN URLs advertised regardless of [`enabled`](Self::enabled).
    #[serde(default = "default_stun_urls")]
    pub stun_urls: Vec<String>,
    /// Lifetime (seconds) of a minted TURN credential. Must be `> 0` when enabled.
    #[serde(default = "default_credential_ttl_secs")]
    pub credential_ttl_secs: u64,
    /// Managed-mode provider name (e.g. `"cloudflare"`). Required when
    /// `mode == managed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_provider: Option<String>,
    /// Managed-mode API token. Required when `mode == managed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_api_token: Option<String>,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: default_turn_mode(),
            static_auth_secret: String::new(),
            urls: Vec::new(),
            stun_urls: default_stun_urls(),
            credential_ttl_secs: default_credential_ttl_secs(),
            managed_provider: None,
            managed_api_token: None,
        }
    }
}

impl TurnConfig {
    /// Validate the TURN configuration.
    ///
    /// URL hygiene is checked unconditionally: every entry in
    /// [`urls`](Self::urls) and [`stun_urls`](Self::stun_urls) must be non-blank
    /// (whitespace-only is rejected with an indexed message), because the URLs are
    /// propagated verbatim to clients and a blank entry would break client-side
    /// `RTCIceServer` parsing.
    ///
    /// When [`enabled`](Self::enabled):
    /// - [`credential_ttl_secs`](Self::credential_ttl_secs) must be `> 0` (a
    ///   zero-lifetime credential is dead on arrival);
    /// - `mode == static_secret` requires a non-blank
    ///   [`static_auth_secret`](Self::static_auth_secret) **and** at least one
    ///   [`urls`](Self::urls) entry (a TURN deployment needs both);
    /// - `mode == managed` requires a non-blank
    ///   [`managed_provider`](Self::managed_provider) **and** a non-blank
    ///   [`managed_api_token`](Self::managed_api_token) (both are needed to call
    ///   the provider).
    ///
    /// When disabled the block is inert; only the URL-hygiene checks apply (so a
    /// stray blank URL in a disabled block is still flagged).
    #[must_use = "validation result must be checked; a malformed TURN config is an error"]
    pub fn validate(&self) -> anyhow::Result<()> {
        for (index, url) in self.urls.iter().enumerate() {
            if url.trim().is_empty() {
                anyhow::bail!("turn.urls[{index}] must not be blank");
            }
        }
        for (index, url) in self.stun_urls.iter().enumerate() {
            if url.trim().is_empty() {
                anyhow::bail!("turn.stun_urls[{index}] must not be blank");
            }
        }

        if !self.enabled {
            return Ok(());
        }

        if self.credential_ttl_secs == 0 {
            anyhow::bail!("turn.credential_ttl_secs must be greater than 0 when turn is enabled");
        }

        match self.mode {
            TurnMode::StaticSecret => {
                if self.static_auth_secret.trim().is_empty() {
                    anyhow::bail!(
                        "turn.static_auth_secret must be set when turn is enabled with \
                         mode = \"static_secret\" (it is the coturn --static-auth-secret)"
                    );
                }
                if self.urls.is_empty() {
                    anyhow::bail!(
                        "turn.urls must list at least one TURN server when turn is enabled \
                         with mode = \"static_secret\""
                    );
                }
            }
            TurnMode::Managed => {
                let provider_set = self
                    .managed_provider
                    .as_ref()
                    .is_some_and(|p| !p.trim().is_empty());
                if !provider_set {
                    anyhow::bail!(
                        "turn.managed_provider must be set when turn is enabled with \
                         mode = \"managed\""
                    );
                }
                let token_set = self
                    .managed_api_token
                    .as_ref()
                    .is_some_and(|t| !t.trim().is_empty());
                if !token_set {
                    anyhow::bail!(
                        "turn.managed_api_token must be set when turn is enabled with \
                         mode = \"managed\" (it is required to call the provider)"
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_disabled_with_public_stun() {
        let cfg = TurnConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.mode, TurnMode::StaticSecret);
        assert!(cfg.static_auth_secret.is_empty());
        assert!(cfg.urls.is_empty());
        assert_eq!(cfg.stun_urls, vec!["stun:stun.l.google.com:19302"]);
        assert_eq!(cfg.credential_ttl_secs, 3600);
        assert!(cfg.managed_provider.is_none());
        assert!(cfg.managed_api_token.is_none());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn serde_default_from_empty_object() {
        let cfg: TurnConfig = serde_json::from_str("{}").expect("empty object uses defaults");
        assert!(!cfg.enabled);
        assert_eq!(cfg.mode, TurnMode::StaticSecret);
        assert_eq!(cfg.stun_urls, vec!["stun:stun.l.google.com:19302"]);
        assert_eq!(cfg.credential_ttl_secs, 3600);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn parses_full_turn_block_and_round_trips() {
        let json = r#"{
            "enabled": true,
            "mode": "static_secret",
            "static_auth_secret": "super-secret",
            "urls": ["turn:turn.example.com:3478"],
            "stun_urls": ["stun:stun.l.google.com:19302"],
            "credential_ttl_secs": 1800
        }"#;
        let cfg: TurnConfig = serde_json::from_str(json).expect("valid turn block");
        assert!(cfg.enabled);
        assert_eq!(cfg.mode, TurnMode::StaticSecret);
        assert_eq!(cfg.static_auth_secret, "super-secret");
        assert_eq!(cfg.urls, vec!["turn:turn.example.com:3478"]);
        assert_eq!(cfg.credential_ttl_secs, 1800);
        assert!(cfg.validate().is_ok());

        // Round-trip through JSON preserves every field.
        let serialized = serde_json::to_string(&cfg).expect("serialize");
        let reparsed: TurnConfig = serde_json::from_str(&serialized).expect("re-parse");
        assert_eq!(reparsed.enabled, cfg.enabled);
        assert_eq!(reparsed.mode, cfg.mode);
        assert_eq!(reparsed.static_auth_secret, cfg.static_auth_secret);
        assert_eq!(reparsed.urls, cfg.urls);
        assert_eq!(reparsed.stun_urls, cfg.stun_urls);
        assert_eq!(reparsed.credential_ttl_secs, cfg.credential_ttl_secs);
    }

    #[test]
    fn wire_tokens_for_mode() {
        let cfg = TurnConfig {
            mode: TurnMode::Managed,
            ..TurnConfig::default()
        };
        let json = serde_json::to_value(&cfg).unwrap();
        assert_eq!(json["mode"], "managed");

        let static_cfg = TurnConfig::default();
        let json = serde_json::to_value(&static_cfg).unwrap();
        assert_eq!(json["mode"], "static_secret");
    }

    #[test]
    fn managed_fields_omitted_when_none() {
        let cfg = TurnConfig::default();
        let json = serde_json::to_value(&cfg).unwrap();
        assert!(json.get("managed_provider").is_none());
        assert!(json.get("managed_api_token").is_none());
    }

    fn enabled_static() -> TurnConfig {
        TurnConfig {
            enabled: true,
            mode: TurnMode::StaticSecret,
            static_auth_secret: "secret".to_string(),
            urls: vec!["turn:turn.example.com:3478".to_string()],
            ..TurnConfig::default()
        }
    }

    #[test]
    fn enabled_static_secret_with_secret_and_urls_is_ok() {
        assert!(enabled_static().validate().is_ok());
    }

    #[test]
    fn enabled_static_secret_missing_secret_is_err() {
        let cfg = TurnConfig {
            static_auth_secret: String::new(),
            ..enabled_static()
        };
        let err = cfg.validate().expect_err("missing secret is rejected");
        assert!(err.to_string().contains("static_auth_secret"));
    }

    #[test]
    fn enabled_static_secret_blank_secret_is_err() {
        let cfg = TurnConfig {
            static_auth_secret: "   ".to_string(),
            ..enabled_static()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn enabled_static_secret_empty_urls_is_err() {
        let cfg = TurnConfig {
            urls: Vec::new(),
            ..enabled_static()
        };
        let err = cfg.validate().expect_err("empty urls is rejected");
        assert!(err.to_string().contains("turn.urls"));
    }

    #[test]
    fn enabled_managed_missing_provider_is_err() {
        let cfg = TurnConfig {
            enabled: true,
            mode: TurnMode::Managed,
            managed_provider: None,
            managed_api_token: Some("token".to_string()),
            ..TurnConfig::default()
        };
        let err = cfg.validate().expect_err("missing provider is rejected");
        assert!(err.to_string().contains("managed_provider"));
    }

    #[test]
    fn enabled_managed_blank_provider_is_err() {
        let cfg = TurnConfig {
            enabled: true,
            mode: TurnMode::Managed,
            managed_provider: Some("  ".to_string()),
            managed_api_token: Some("token".to_string()),
            ..TurnConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn enabled_managed_missing_token_is_err() {
        let cfg = TurnConfig {
            enabled: true,
            mode: TurnMode::Managed,
            managed_provider: Some("cloudflare".to_string()),
            managed_api_token: None,
            ..TurnConfig::default()
        };
        let err = cfg.validate().expect_err("missing token is rejected");
        assert!(err.to_string().contains("managed_api_token"));
    }

    #[test]
    fn enabled_managed_with_provider_and_token_is_ok() {
        let cfg = TurnConfig {
            enabled: true,
            mode: TurnMode::Managed,
            managed_provider: Some("cloudflare".to_string()),
            managed_api_token: Some("token".to_string()),
            ..TurnConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn enabled_zero_ttl_is_err() {
        let cfg = TurnConfig {
            credential_ttl_secs: 0,
            ..enabled_static()
        };
        let err = cfg
            .validate()
            .expect_err("zero ttl is rejected when enabled");
        assert!(err.to_string().contains("credential_ttl_secs"));
    }

    #[test]
    fn disabled_zero_ttl_is_ok() {
        // A disabled block is inert: ttl is not validated.
        let cfg = TurnConfig {
            enabled: false,
            credential_ttl_secs: 0,
            ..TurnConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn disabled_with_empty_everything_is_ok() {
        let cfg = TurnConfig {
            enabled: false,
            static_auth_secret: String::new(),
            urls: Vec::new(),
            stun_urls: Vec::new(),
            ..TurnConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn blank_turn_url_is_err_with_indexed_message() {
        let cfg = TurnConfig {
            urls: vec!["turn:turn.example.com:3478".to_string(), "   ".to_string()],
            ..enabled_static()
        };
        let err = cfg.validate().expect_err("blank url is rejected");
        assert!(
            err.to_string().contains("turn.urls[1]"),
            "error must point at the offending index: {err}"
        );
    }

    #[test]
    fn blank_stun_url_is_err_with_indexed_message() {
        let cfg = TurnConfig {
            enabled: false,
            stun_urls: vec!["stun:stun.l.google.com:19302".to_string(), String::new()],
            ..TurnConfig::default()
        };
        let err = cfg.validate().expect_err("blank stun url is rejected");
        assert!(
            err.to_string().contains("turn.stun_urls[1]"),
            "error must point at the offending index: {err}"
        );
    }

    #[test]
    fn blank_url_rejected_even_when_disabled() {
        // URL hygiene applies regardless of `enabled`.
        let cfg = TurnConfig {
            enabled: false,
            urls: vec!["  ".to_string()],
            ..TurnConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_non_ascii_whitespace_urls() {
        // `str::trim` strips NBSP and ideographic space; pin that.
        for blank in ["\t", "\n", "\u{00A0}", "\u{3000}"] {
            let cfg = TurnConfig {
                urls: vec![blank.to_string()],
                ..enabled_static()
            };
            assert!(
                cfg.validate().is_err(),
                "whitespace-only URL {blank:?} must be rejected"
            );
        }
    }
}
