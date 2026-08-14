//! TURN / STUN ICE-server configuration (Protocol v3).
//!
//! Drives the ICE servers emitted into a WebRTC `SessionPlan`. TURN is **fully
//! self-hosted**: the server self-mints short-lived, per-player coturn REST
//! credentials from [`static_auth_secret`](TurnConfig::static_auth_secret)
//! (coturn `--use-auth-secret`). The secret never leaves the server; clients
//! receive only the expiring username/credential pair (see
//! [`crate::security::mint_turn_credentials`]). No third-party cloud is ever
//! contacted and no external credentials are required — the operator runs their
//! own coturn (see `docs/deployment-turn.md`).
//!
//! The whole block is inert when [`enabled`](TurnConfig::enabled) is `false`
//! (default): only the configured public `stun_urls` are advertised, keeping the
//! zero-dependency, relay-floor posture out of the box.

use serde::{Deserialize, Serialize};

use super::ice_url;

/// Default public STUN servers advertised even when TURN is disabled.
fn default_stun_urls() -> Vec<String> {
    vec!["stun:stun.l.google.com:19302".to_string()]
}

/// Default TURN credential lifetime (1 hour).
const fn default_credential_ttl_secs() -> u64 {
    3600
}

/// ICE / TURN configuration (`[turn]` in `docs/configuration.md`).
///
/// Self-hosted only: when [`enabled`](Self::enabled), the server mints coturn
/// REST credentials for an operator-run TURN server. There is no managed /
/// third-party-cloud mode.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TurnConfig {
    /// Whether TURN credentials are minted and advertised. When `false` (default),
    /// only [`stun_urls`](Self::stun_urls) are advertised and no secret is required.
    #[serde(default)]
    pub enabled: bool,
    /// coturn `--static-auth-secret` (server-only). Required when
    /// [`enabled`](Self::enabled); never sent to clients.
    #[serde(default)]
    pub static_auth_secret: String,
    /// TURN server URLs, e.g. `["turn:turn.example.com:3478"]`. Required (non-empty)
    /// when [`enabled`](Self::enabled).
    #[serde(default)]
    pub urls: Vec<String>,
    /// Public STUN URLs advertised regardless of [`enabled`](Self::enabled).
    #[serde(default = "default_stun_urls")]
    pub stun_urls: Vec<String>,
    /// Lifetime (seconds) of a minted TURN credential. Must be `> 0` when enabled.
    #[serde(default = "default_credential_ttl_secs")]
    pub credential_ttl_secs: u64,
}

impl Default for TurnConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            static_auth_secret: String::new(),
            urls: Vec::new(),
            stun_urls: default_stun_urls(),
            credential_ttl_secs: default_credential_ttl_secs(),
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
    /// `RTCIceServer` parsing. For the same reason, schemes are checked
    /// unconditionally too: `urls` entries must start with `turn:`/`turns:` and
    /// `stun_urls` entries with `stun:`/`stuns:` — case-insensitively (URI
    /// schemes are case-insensitive, RFC 3986 §3.1) and with a non-whitespace
    /// remainder after the colon. Exact-duplicate URLs anywhere in the block
    /// only warn (clients tolerate repeated entries — the warn-but-succeed
    /// stance).
    ///
    /// When [`enabled`](Self::enabled):
    /// - [`credential_ttl_secs`](Self::credential_ttl_secs) must be `> 0` (a
    ///   zero-lifetime credential is dead on arrival);
    /// - a non-blank [`static_auth_secret`](Self::static_auth_secret) **and** at
    ///   least one [`urls`](Self::urls) entry are required (a self-hosted TURN
    ///   deployment needs both — the secret to mint credentials, the URL to reach
    ///   coturn).
    ///
    /// When disabled the block is inert; only the URL-hygiene checks apply (so a
    /// stray blank URL in a disabled block is still flagged).
    #[must_use = "validation result must be checked; a malformed TURN config is an error"]
    pub fn validate(&self) -> anyhow::Result<()> {
        for (index, url) in self.urls.iter().enumerate() {
            if url.trim().is_empty() {
                anyhow::bail!("turn.urls[{index}] must not be blank");
            }
            if let Err(reason) = ice_url::check_url_scheme(url, ice_url::TURN_SCHEMES) {
                anyhow::bail!("turn.urls[{index}] {reason}");
            }
        }
        for (index, url) in self.stun_urls.iter().enumerate() {
            if url.trim().is_empty() {
                anyhow::bail!("turn.stun_urls[{index}] must not be blank");
            }
            if let Err(reason) = ice_url::check_url_scheme(url, ice_url::STUN_SCHEMES) {
                anyhow::bail!("turn.stun_urls[{index}] {reason}");
            }
        }
        ice_url::warn_on_duplicate_urls(
            "turn",
            self.urls
                .iter()
                .chain(self.stun_urls.iter())
                .map(String::as_str),
        );

        if !self.enabled {
            return Ok(());
        }

        if self.credential_ttl_secs == 0 {
            anyhow::bail!("turn.credential_ttl_secs must be greater than 0 when turn is enabled");
        }

        if self.static_auth_secret.trim().is_empty() {
            anyhow::bail!(
                "turn.static_auth_secret must be set when turn is enabled \
                 (it is the coturn --static-auth-secret)"
            );
        }
        if self.urls.is_empty() {
            anyhow::bail!("turn.urls must list at least one TURN server when turn is enabled");
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
        assert!(cfg.static_auth_secret.is_empty());
        assert!(cfg.urls.is_empty());
        assert_eq!(cfg.stun_urls, vec!["stun:stun.l.google.com:19302"]);
        assert_eq!(cfg.credential_ttl_secs, 3600);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn serde_default_from_empty_object() {
        let cfg: TurnConfig = serde_json::from_str("{}").expect("empty object uses defaults");
        assert!(!cfg.enabled);
        assert_eq!(cfg.stun_urls, vec!["stun:stun.l.google.com:19302"]);
        assert_eq!(cfg.credential_ttl_secs, 3600);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn parses_full_turn_block_and_round_trips() {
        let json = r#"{
            "enabled": true,
            "static_auth_secret": "super-secret",
            "urls": ["turn:turn.example.com:3478"],
            "stun_urls": ["stun:stun.l.google.com:19302"],
            "credential_ttl_secs": 1800
        }"#;
        let cfg: TurnConfig = serde_json::from_str(json).expect("valid turn block");
        assert!(cfg.enabled);
        assert_eq!(cfg.static_auth_secret, "super-secret");
        assert_eq!(cfg.urls, vec!["turn:turn.example.com:3478"]);
        assert_eq!(cfg.credential_ttl_secs, 1800);
        assert!(cfg.validate().is_ok());

        // Round-trip through JSON preserves every field.
        let serialized = serde_json::to_string(&cfg).expect("serialize");
        let reparsed: TurnConfig = serde_json::from_str(&serialized).expect("re-parse");
        assert_eq!(reparsed.enabled, cfg.enabled);
        assert_eq!(reparsed.static_auth_secret, cfg.static_auth_secret);
        assert_eq!(reparsed.urls, cfg.urls);
        assert_eq!(reparsed.stun_urls, cfg.stun_urls);
        assert_eq!(reparsed.credential_ttl_secs, cfg.credential_ttl_secs);
    }

    /// Legacy configs may still carry the removed `mode` / `managed_*` keys.
    /// Serde ignores unknown fields (no `deny_unknown_fields`), so such a block
    /// must still parse and validate as a plain self-hosted block.
    #[test]
    fn ignores_removed_legacy_managed_keys() {
        let json = r#"{
            "enabled": true,
            "mode": "managed",
            "managed_provider": "cloudflare",
            "managed_api_token": "token",
            "static_auth_secret": "super-secret",
            "urls": ["turn:turn.example.com:3478"]
        }"#;
        let cfg: TurnConfig = serde_json::from_str(json).expect("legacy keys are ignored");
        assert!(cfg.enabled);
        assert_eq!(cfg.static_auth_secret, "super-secret");
        assert!(cfg.validate().is_ok());
        // The removed keys never round-trip back out.
        let reserialized = serde_json::to_value(&cfg).expect("serialize");
        assert!(reserialized.get("mode").is_none());
        assert!(reserialized.get("managed_provider").is_none());
    }

    fn enabled_static() -> TurnConfig {
        TurnConfig {
            enabled: true,
            static_auth_secret: "secret".to_string(),
            urls: vec!["turn:turn.example.com:3478".to_string()],
            ..TurnConfig::default()
        }
    }

    #[test]
    fn enabled_with_secret_and_urls_is_ok() {
        assert!(enabled_static().validate().is_ok());
    }

    #[test]
    fn enabled_missing_secret_is_err() {
        let cfg = TurnConfig {
            static_auth_secret: String::new(),
            ..enabled_static()
        };
        let err = cfg.validate().expect_err("missing secret is rejected");
        assert!(err.to_string().contains("static_auth_secret"));
    }

    #[test]
    fn enabled_blank_secret_is_err() {
        let cfg = TurnConfig {
            static_auth_secret: "   ".to_string(),
            ..enabled_static()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn enabled_empty_urls_is_err() {
        let cfg = TurnConfig {
            urls: Vec::new(),
            ..enabled_static()
        };
        let err = cfg.validate().expect_err("empty urls is rejected");
        assert!(err.to_string().contains("turn.urls"));
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

    // -----------------------------------------------------------------------
    // URL scheme validation: `urls` must be turn:/turns:, `stun_urls` must be
    // stun:/stuns: (case-insensitive, non-empty remainder). Like the blank
    // check, scheme hygiene applies regardless of `enabled`.
    // -----------------------------------------------------------------------

    #[test]
    fn turn_urls_accept_turn_schemes_case_insensitively() {
        for url in [
            "turn:turn.example.com:3478",
            "turns:turn.example.com:5349",
            "TURN:turn.example.com:3478",
            "TURNS:turn.example.com:5349",
            "turn:turn.example.com:3478?transport=udp",
            "turn:[2001:db8::1]:3478",
        ] {
            let cfg = TurnConfig {
                urls: vec![url.to_string()],
                ..enabled_static()
            };
            assert!(cfg.validate().is_ok(), "{url:?} must be accepted");
        }
    }

    #[test]
    fn stun_urls_accept_stun_schemes_case_insensitively() {
        for url in [
            "stun:stun.l.google.com:19302",
            "stuns:stun.example.com:5349",
            "STUN:stun.l.google.com:19302",
            "STUNS:stun.example.com:5349",
        ] {
            let cfg = TurnConfig {
                enabled: false,
                stun_urls: vec![url.to_string()],
                ..TurnConfig::default()
            };
            assert!(cfg.validate().is_ok(), "{url:?} must be accepted");
        }
    }

    #[test]
    fn turn_urls_reject_non_turn_schemes_with_indexed_message() {
        // A STUN url in `turn.urls` is also wrong: that list feeds the
        // credentialed TURN `IceServer` entry.
        for url in [
            "http://example.com",
            "relay:foo",
            "stun:stun.l.google.com:19302",
            "turn:",
            "turn: ",
            "turn :host",
            "no-colon-at-all",
        ] {
            let cfg = TurnConfig {
                urls: vec!["turn:turn.example.com:3478".to_string(), url.to_string()],
                ..enabled_static()
            };
            let err = cfg
                .validate()
                .expect_err(&format!("{url:?} must be rejected in turn.urls"));
            assert!(
                err.to_string().contains("turn.urls[1]"),
                "error must point at the offending index: {err}"
            );
        }
    }

    #[test]
    fn stun_urls_reject_non_stun_schemes_with_indexed_message() {
        for url in [
            "http://example.com",
            "relay:foo",
            "turn:turn.example.com:3478",
            "stun:",
            "stun:\t",
            "stun :host",
        ] {
            let cfg = TurnConfig {
                enabled: false,
                stun_urls: vec!["stun:stun.l.google.com:19302".to_string(), url.to_string()],
                ..TurnConfig::default()
            };
            let err = cfg
                .validate()
                .expect_err(&format!("{url:?} must be rejected in turn.stun_urls"));
            assert!(
                err.to_string().contains("turn.stun_urls[1]"),
                "error must point at the offending index: {err}"
            );
        }
    }

    #[test]
    fn bad_scheme_rejected_even_when_disabled() {
        // Scheme hygiene applies regardless of `enabled`, exactly like the
        // blank-URL check: a disabled block's URLs still reach clients via
        // `stun_urls`, and a stray malformed `urls` entry should fail fast.
        let cfg = TurnConfig {
            enabled: false,
            urls: vec!["http://example.com".to_string()],
            ..TurnConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn duplicate_urls_warn_but_validate_ok() {
        // Exact duplicates (within `urls`, within `stun_urls`, or across the
        // whole `[turn]` block) only warn via `tracing::warn!` — clients
        // tolerate repeated entries, so this is a misconfiguration, not an
        // error (the warn-but-succeed precedent). Asserting Ok pins that.
        let cfg = TurnConfig {
            urls: vec![
                "turn:turn.example.com:3478".to_string(),
                "turn:turn.example.com:3478".to_string(),
            ],
            stun_urls: vec![
                "stun:stun.l.google.com:19302".to_string(),
                "stun:stun.l.google.com:19302".to_string(),
            ],
            ..enabled_static()
        };
        assert!(cfg.validate().is_ok());
    }
}
