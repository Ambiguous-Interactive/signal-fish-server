//! Configuration validation functions.

use super::security::ClientAuthMode;
use super::Config;
use std::path::Path;

/// Validate configuration security and warn about potential credential leaks
pub fn validate_config_security(config: &Config) -> anyhow::Result<()> {
    let is_prod = is_production_mode();

    // Validate metrics authentication
    if config.security.require_metrics_auth {
        let token_present = config
            .security
            .metrics_auth_token
            .as_ref()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);

        if !token_present {
            anyhow::bail!(
                "\nCRITICAL: Metrics authentication is enabled but no credentials are configured!\n\
                 ===================================================================\n\
                 Configure a shared bearer token:\n\
                 export SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN=\"$(openssl rand -hex 32)\"\n\
                 \n\
                 To disable metrics auth (NOT recommended), set:\n\
                 export SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false\n\
                 ===================================================================\n"
            );
        }

        if let Some(token) = &config.security.metrics_auth_token {
            let token = token.trim();
            if token.len() < 16 {
                eprintln!(
                    "\nWARNING: Metrics auth token is very short ({} chars).\n\
                     Recommended: At least 32 characters for security.\n\
                     Generate a strong token: openssl rand -hex 32\n",
                    token.len()
                );
            }
        }
    } else if is_prod {
        eprintln!(
            "\nSECURITY WARNING: Metrics Authentication Disabled in Production!\n\
             ===================================================================\n\
             Your /metrics endpoint is publicly accessible without authentication.\n\
             This exposes sensitive application data and usage statistics.\n\
             \n\
             To enable metrics authentication:\n\
             export SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=true\n\
             export SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN=\"$(openssl rand -hex 32)\"\n\
             ===================================================================\n"
        );
    }

    for (index, app) in config.security.authorized_apps.iter().enumerate() {
        if app.app_id.trim().is_empty() {
            anyhow::bail!("security.authorized_apps[{index}].app_id must not be blank");
        }
        if app.app_secret.trim().is_empty() {
            anyhow::bail!("security.authorized_apps[{index}].app_secret must not be blank");
        }
        if app.app_name.trim().is_empty() {
            anyhow::bail!("security.authorized_apps[{index}].app_name must not be blank");
        }
    }

    // TLS validation
    if config.security.transport.tls.enabled {
        let tls = &config.security.transport.tls;
        let cert_path = tls
            .certificate_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "security.transport.tls.certificate_path must be provided when TLS is enabled"
                )
            })?;
        if !Path::new(cert_path).exists() {
            anyhow::bail!("TLS certificate file not found at {cert_path}");
        }

        let key_path = tls
            .private_key_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "security.transport.tls.private_key_path must be provided when TLS is enabled"
                )
            })?;
        if !Path::new(key_path).exists() {
            anyhow::bail!("TLS private key file not found at {key_path}");
        }

        if matches!(
            tls.client_auth,
            ClientAuthMode::Optional | ClientAuthMode::Require
        ) {
            let ca_path = tls
                .client_ca_cert_path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "security.transport.tls.client_ca_cert_path must be set when client_auth is {:?}",
                        tls.client_auth
                    )
                })?;
            if !Path::new(ca_path).exists() {
                anyhow::bail!("Client CA bundle not found at {ca_path}");
            }
        }
    }

    // Signal payload cap validation. `max_signal_bytes` larger than
    // `max_message_size` is rejected (not just warned about) because it is
    // contradictory dead config: a frame that large is rejected by the
    // `max_message_size` cap before the signal cap could ever apply, so the
    // configured value silently never takes effect.
    if config.security.max_signal_bytes == 0 {
        anyhow::bail!("security.max_signal_bytes must be greater than 0");
    }
    if config.security.max_signal_bytes > config.security.max_message_size {
        anyhow::bail!(
            "security.max_signal_bytes ({}) must not exceed security.max_message_size ({}): \
             a Signal frame that large would be rejected by the message size cap first, \
             so the configured signal cap could never take effect",
            config.security.max_signal_bytes,
            config.security.max_message_size
        );
    }

    // Background-task interval validation. These values become the period of a
    // `tokio::time::interval`, which PANICS on a zero period — killing the
    // spawned task while the process keeps serving, so the failure is silent and
    // severe. `room_cleanup_interval` drives the maintenance sweep (expired
    // rooms/clients/reconnection records/tokens/locks); losing it leaks memory
    // unboundedly. `time_window` is both the rate-limiter cleanup period and the
    // width of every rate-limit window (zero ⇒ the window resets on every check,
    // disabling the limits). `batch_interval_ms` is validated in
    // `WebSocketConfig::validate` below (it only feeds an interval when batching
    // is on). Rejecting a zero here turns an operator typo into a loud startup
    // error; the use sites below ALSO clamp defensively (the server is
    // constructible directly via the public API, bypassing this check). Loud
    // rejection is reserved for these panic-prone periods — other interval sites
    // that already clamp and therefore never panic (e.g. the dedup sweep) keep
    // their non-fatal use-site handling.
    if config.server.room_cleanup_interval == 0 {
        anyhow::bail!(
            "server.room_cleanup_interval must be greater than 0 seconds \
             (it is the period of the room/client/token cleanup task)"
        );
    }
    if config.rate_limit.time_window == 0 {
        anyhow::bail!(
            "rate_limit.time_window must be greater than 0 seconds \
             (it is the rate-limit window width and the limiter cleanup interval)"
        );
    }

    // Token binding validation
    if config.security.transport.token_binding.enabled {
        let binding = &config.security.transport.token_binding;
        if binding.required && !config.security.transport.tls.enabled {
            anyhow::bail!(
                "security.transport.token_binding.required=true requires TLS termination \
                 (set security.transport.tls.enabled=true)"
            );
        }
        if binding.require_client_fingerprint
            && !matches!(
                config.security.transport.tls.client_auth,
                ClientAuthMode::Optional | ClientAuthMode::Require
            )
        {
            anyhow::bail!(
                "security.transport.token_binding.require_client_fingerprint=true requires \
                 client certificate authentication (client_auth must be \"optional\" or \"require\")"
            );
        }
        if binding.subprotocol.trim().is_empty() {
            anyhow::bail!("security.transport.token_binding.subprotocol must not be empty");
        }
    }

    // WebSocket configuration validation
    config.websocket.validate()?;

    // Protocol version-bounds validation
    config.protocol.validate()?;

    // Session topology/transport policy validation
    config.session.validate()?;

    // TURN / STUN ICE-server policy validation
    config.turn.validate()?;

    Ok(())
}

/// Whether startup should warn that signaling is not TLS-terminated while the
/// server is actively brokering WebRTC.
///
/// Production requires `wss://` for signaling because DTLS
/// fingerprints travel inside the SDP that `Signal` relays: a plaintext `ws://`
/// signaling path lets an on-path attacker substitute fingerprints and
/// man-in-the-middle the "encrypted" peer connections. The condition is
/// TURN-specific (`turn.enabled`) because that is the deployment signal that
/// this server is brokering real WebRTC sessions. This is deliberately a
/// warning, never a hard error: terminating TLS at a reverse proxy (where
/// `security.transport.tls.enabled` stays `false`) is the most common
/// production deployment and is perfectly safe.
#[must_use]
pub fn should_warn_missing_signaling_tls(config: &Config) -> bool {
    config.turn.enabled && !config.security.transport.tls.enabled
}

/// Detect if we're running in production mode.
///
/// Checks for `SIGNAL_FISH_PRODUCTION` or generic `PRODUCTION` / `PROD` environment variables.
pub fn is_production_mode() -> bool {
    use std::env;

    // Check explicit Signal Fish environment variable
    if let Ok(mode) = env::var("SIGNAL_FISH__ENVIRONMENT") {
        return mode.to_lowercase() == "production" || mode.to_lowercase() == "prod";
    }

    // Check well-known production indicators
    env::var("SIGNAL_FISH_PRODUCTION").is_ok()
        || env::var("PRODUCTION").is_ok()
        || env::var("PROD").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppAuthEntry;

    /// Truth table for the TURN-without-TLS startup warning: it fires exactly
    /// when the server brokers WebRTC (`turn.enabled`) over a plaintext
    /// listener (`tls.enabled == false`).
    #[test]
    fn signaling_tls_warning_fires_only_for_turn_without_tls() {
        let cases = [
            (false, false, false),
            (false, true, false),
            (true, false, true),
            (true, true, false),
        ];
        for (turn_enabled, tls_enabled, expected) in cases {
            let mut config = Config::default();
            config.turn.enabled = turn_enabled;
            config.security.transport.tls.enabled = tls_enabled;
            assert_eq!(
                should_warn_missing_signaling_tls(&config),
                expected,
                "turn.enabled={turn_enabled}, tls.enabled={tls_enabled}"
            );
        }
    }

    /// The warning predicate must not be affected by unrelated security knobs.
    #[test]
    fn signaling_tls_warning_ignores_unrelated_settings() {
        let mut config = Config::default();
        config.turn.enabled = true;
        config.security.require_websocket_auth = false;
        config.security.require_metrics_auth = false;
        config.security.transport.token_binding.enabled = true;
        assert!(should_warn_missing_signaling_tls(&config));
    }

    #[test]
    fn metrics_auth_rejects_whitespace_only_token() {
        let mut config = Config::default();
        config.security.require_metrics_auth = true;
        config.security.metrics_auth_token = Some(" \t\n".to_string());

        let err = validate_config_security(&config)
            .expect_err("whitespace-only metrics token is not configured credentials");
        assert!(
            err.to_string()
                .contains("Metrics authentication is enabled but no credentials are configured"),
            "error must explain the missing metrics credentials: {err}"
        );
    }

    #[test]
    fn authorized_apps_reject_blank_required_fields_with_indexed_errors() {
        let cases: [(&str, fn(&mut AppAuthEntry)); 3] = [
            ("app_id", |app| app.app_id = " ".to_string()),
            ("app_secret", |app| app.app_secret = "\t".to_string()),
            ("app_name", |app| app.app_name = "\n".to_string()),
        ];

        for (field, mutate) in cases {
            let mut app = AppAuthEntry {
                app_id: "game".to_string(),
                app_secret: "secret".to_string(),
                app_name: "Game".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: None,
            };
            mutate(&mut app);

            let mut config = Config::default();
            config.security.require_metrics_auth = false;
            config.security.authorized_apps = vec![
                AppAuthEntry {
                    app_id: "valid".to_string(),
                    app_secret: "valid-secret".to_string(),
                    app_name: "Valid".to_string(),
                    max_rooms: None,
                    max_players_per_room: None,
                    rate_limit_per_minute: None,
                },
                app,
            ];

            let err = validate_config_security(&config)
                .expect_err("blank authorized app fields are rejected");
            let expected = format!("security.authorized_apps[1].{field}");
            assert!(
                err.to_string().contains(&expected),
                "error must point at the blank field {expected}: {err}"
            );
        }
    }

    /// A zero `server.room_cleanup_interval` is rejected: it is fed to
    /// `tokio::time::interval`, which panics on a zero period, silently killing
    /// the maintenance sweep (room/client/token/lock reaping) and leaking memory
    /// unboundedly while the process keeps serving. Fail fast at startup instead.
    #[test]
    fn zero_room_cleanup_interval_is_rejected() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.server.room_cleanup_interval = 0;

        let err = validate_config_security(&config)
            .expect_err("zero room_cleanup_interval must be rejected");
        assert!(
            err.to_string().contains("server.room_cleanup_interval"),
            "error must name the offending field: {err}"
        );
    }

    /// A zero `rate_limit.time_window` is rejected: it is the period of the
    /// rate-limiter's cleanup `interval` (panics on zero) and the width of every
    /// rate-limit window (zero ⇒ every check resets the window ⇒ limits disabled).
    #[test]
    fn zero_rate_limit_time_window_is_rejected() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.rate_limit.time_window = 0;

        let err = validate_config_security(&config)
            .expect_err("zero rate_limit.time_window must be rejected");
        assert!(
            err.to_string().contains("rate_limit.time_window"),
            "error must name the offending field: {err}"
        );
    }

    /// A zero `websocket.batch_interval_ms` is rejected ONLY when batching is
    /// enabled (the value is the flush `interval` period, which panics on zero).
    #[test]
    fn zero_batch_interval_is_rejected_when_batching_enabled() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.websocket.enable_batching = true;
        config.websocket.batch_interval_ms = 0;

        let err = validate_config_security(&config)
            .expect_err("zero batch_interval_ms with batching on must be rejected");
        assert!(
            err.to_string().contains("websocket.batch_interval_ms"),
            "error must name the offending field: {err}"
        );
    }

    /// With batching DISABLED the flush interval is never constructed, so a zero
    /// `batch_interval_ms` is harmless and must not block startup.
    #[test]
    fn zero_batch_interval_is_allowed_when_batching_disabled() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.websocket.enable_batching = false;
        config.websocket.batch_interval_ms = 0;

        assert!(
            validate_config_security(&config).is_ok(),
            "zero batch_interval_ms is harmless when batching is disabled"
        );
    }
}
