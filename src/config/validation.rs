//! Configuration validation functions.

use super::security::ClientAuthMode;
use super::Config;
use std::path::Path;

/// Validate configuration security and warn about potential credential leaks
pub fn validate_config_security(config: &Config) -> anyhow::Result<()> {
    let is_prod = is_production_mode();

    crate::security::OriginPolicy::parse(&config.security.cors_origins)?;

    if config.server.event_buffer_size > super::server::MAX_EVENT_BUFFER_SIZE {
        anyhow::bail!(
            "server.event_buffer_size must not exceed {} (configured: {})",
            super::server::MAX_EVENT_BUFFER_SIZE,
            config.server.event_buffer_size
        );
    }

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

    let mut app_ids = std::collections::HashSet::new();
    for (index, app) in config.security.allowed_apps.iter().enumerate() {
        if app.app_id.trim().is_empty() {
            anyhow::bail!("security.allowed_apps[{index}].app_id must not be blank");
        }
        if !app_ids.insert(app.app_id.as_str()) {
            anyhow::bail!(
                "security.allowed_apps[{index}].app_id duplicates an earlier entry: {:?}",
                app.app_id
            );
        }
        if app.app_name.trim().is_empty() {
            anyhow::bail!("security.allowed_apps[{index}].app_name must not be blank");
        }
    }

    // TLS validation
    if config.security.transport.tls.enabled {
        if !cfg!(feature = "tls") {
            anyhow::bail!(
                "security.transport.tls.enabled=true requires a binary compiled with the `tls` \
                 Cargo feature; this binary cannot serve HTTPS (rebuild with `--features tls`)"
            );
        }
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

    // Message size cap validation. A zero cap would reject every inbound
    // frame AND derive a zero transport-layer cap on the WebSocket upgrade
    // (`2 * max_message_size`), so no message could ever be admitted. The
    // signal-cap checks below happen to reject this transitively
    // (`max_signal_bytes > 0` and `<= max_message_size` force a nonzero
    // message cap), but the direct check keeps the diagnostic precise and the
    // invariant independent of that coupling.
    if config.security.max_message_size == 0 {
        anyhow::bail!(
            "security.max_message_size must be greater than 0: a zero cap rejects every \
             WebSocket message before it can be processed"
        );
    }

    if config.security.max_outbound_message_size == 0 {
        anyhow::bail!(
            "security.max_outbound_message_size must be greater than 0: a zero cap rejects every server message"
        );
    }
    if config.security.max_outbound_message_size
        > crate::config::defaults::MAX_OUTBOUND_MESSAGE_SIZE
    {
        anyhow::bail!(
            "security.max_outbound_message_size ({}) must not exceed the portable protocol maximum ({})",
            config.security.max_outbound_message_size,
            crate::config::defaults::MAX_OUTBOUND_MESSAGE_SIZE,
        );
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
    if config.security.transport.token_binding.required
        && !config.security.transport.token_binding.enabled
    {
        anyhow::bail!(
            "security.transport.token_binding.required=true requires \
             security.transport.token_binding.enabled=true"
        );
    }
    if config.security.transport.token_binding.enabled {
        let binding = &config.security.transport.token_binding;
        if binding.scheme
            == crate::security::token_binding::TokenBindingScheme::SecWebsocketKeySha256
        {
            anyhow::bail!(
                "security.transport.token_binding.scheme=sec_websocket_key_sha256 is protocol-v1 \
                 compatibility syntax and cannot be enabled because it lacks server freshness; use \
                 server_nonce_hkdf_sha256"
            );
        }
        if binding.required && !built_in_tls_active(config, cfg!(feature = "tls")) {
            anyhow::bail!(
                "security.transport.token_binding.required=true requires active built-in TLS \
                 (set security.transport.tls.enabled=true and compile with `--features tls`)"
            );
        }
        if binding.subprotocol.trim().is_empty() {
            anyhow::bail!("security.transport.token_binding.subprotocol must not be empty");
        }
        if !crate::security::token_binding::token_binding_subprotocol_is_v2_compatible(
            &binding.subprotocol,
        ) {
            anyhow::bail!(
                "security.transport.token_binding.subprotocol={} uses Signal Fish's reserved \
                 protocol namespace but does not name the v2 wire contract; use {} or a custom \
                 non-reserved alias",
                binding.subprotocol,
                crate::security::token_binding::TOKEN_BINDING_SUBPROTOCOL_V2
            );
        }
        if binding.require_client_fingerprint {
            if !binding.required {
                anyhow::bail!(
                    "security.transport.token_binding.require_client_fingerprint=true requires \
                     security.transport.token_binding.required=true so clients cannot bypass \
                     certificate binding by omitting the subprotocol"
                );
            }
            if !built_in_tls_active(config, cfg!(feature = "tls")) {
                anyhow::bail!(
                    "security.transport.token_binding.require_client_fingerprint=true requires \
                     active built-in TLS so the fingerprint comes from an authenticated rustls \
                     peer certificate"
                );
            }
            if matches!(
                config.security.transport.tls.client_auth,
                ClientAuthMode::None
            ) {
                anyhow::bail!(
                    "security.transport.token_binding.require_client_fingerprint=true requires \
                     security.transport.tls.client_auth to be `optional` or `require`"
                );
            }
        }
    } else if config
        .security
        .transport
        .token_binding
        .require_client_fingerprint
    {
        anyhow::bail!(
            "security.transport.token_binding.require_client_fingerprint=true requires \
             security.transport.token_binding.enabled=true"
        );
    }

    // Cross-field: the room-activity refresh piggybacks on the per-player
    // heartbeat throttle (`maybe_update_last_seen`), so a room with active
    // members has its `last_activity` refreshed at most once per
    // `heartbeat_throttle_secs`. If that throttle is >= `inactive_room_timeout`,
    // steady ping/GameData/Signal traffic can leave `last_activity` stale past
    // the reaper deadline and GC an occupied room while its members are still
    // connected (BUG-1 reintroduced by misconfiguration). Require the throttle
    // to stay strictly below the inactive-room deadline. (`heartbeat_throttle_secs
    // == 0` disables throttling — every heartbeat refreshes — so it is always
    // safe and exempt.)
    if config.server.heartbeat_throttle_secs > 0
        && config.server.heartbeat_throttle_secs >= config.server.inactive_room_timeout
    {
        anyhow::bail!(
            "server.heartbeat_throttle_secs ({}) must be less than \
             server.inactive_room_timeout ({}): the room-activity refresh is throttled on \
             heartbeat_throttle_secs, so a throttle at or above the inactive-room deadline \
             lets GC reap an occupied room whose members are still active",
            config.server.heartbeat_throttle_secs,
            config.server.inactive_room_timeout
        );
    }

    // WebSocket configuration validation
    config.websocket.validate()?;

    // Cross-field: the slow-consumer grace period must be shorter than the
    // activity-reaper deadline (BUG-2 / timeout inversion). A message handler
    // records sender activity at dispatch, then parks on the broadcast
    // `join_all` while a slow recipient drains — up to `slow_consumer_timeout_ms`.
    // If that park can outlast `server.ping_timeout`, the reaper evicts the
    // HEALTHY sender (close 4003) before its own slow recipient is ever
    // disconnected: a legal config gets a healthy player kicked. Require the
    // grace period to be strictly less than the ping deadline so the sender's
    // recorded activity is always still fresh when its park ends. (Guarded on
    // `ping_timeout > 0`: a zero deadline disables the reaper, so no inversion
    // exists to prevent.)
    //
    // `formal/tla/SenderPacingReaper.tla` derives this strict `<` as
    // the NECESSARY floor. It models the pre-park delay `d` — the
    // `maybe_update_last_seen` throttle-boundary DB write + `rooms` write-lock
    // that runs after the activity record and before the park — and TLC shows
    // that `slow >= ping` is unsafe (the peak reaper-visible gap `d + slow`
    // exceeds the deadline), which is EXACTLY the region rejected here. The
    // `<` is not, however, proven SUFFICIENT: the model bounds `d` to one
    // tick, but the lock/DB delay is unbounded under contention, so a config
    // with a thin margin can still invert if `d` exceeds that margin. Full
    // safety is an operator sizing concern — keep the margin
    // `ping_timeout * 1000 - slow_consumer_timeout_ms` (both in ms; note
    // `ping_timeout` is seconds, `slow_consumer_timeout_ms` is milliseconds)
    // above the worst-case pre-park delay (the default 30000 - 5000 = 25000 ms
    // dwarfs it). This check is the guardrail against the provable inversion
    // region, not a liveness proof under unbounded load.
    if config.server.ping_timeout > 0 {
        let ping_timeout_ms = config.server.ping_timeout.saturating_mul(1000);
        if config.websocket.slow_consumer_timeout_ms >= ping_timeout_ms {
            anyhow::bail!(
                "websocket.slow_consumer_timeout_ms ({}) must be less than \
                 server.ping_timeout ({} s = {} ms): a slow-consumer park that can \
                 outlast the ping deadline lets the activity reaper evict the HEALTHY \
                 sender (close 4003) before its slow recipient is disconnected \
                 (timeout inversion)",
                config.websocket.slow_consumer_timeout_ms,
                config.server.ping_timeout,
                ping_timeout_ms
            );
        }
    }

    // Protocol bounds plus generated-room-code closure: every automatically
    // generated code must be admissible through the explicit join path.
    config
        .protocol
        .validate_room_code_generation(config.server.room_code_prefix.as_deref())?;

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
    config.turn.enabled && !built_in_tls_active(config, cfg!(feature = "tls"))
}

fn built_in_tls_active(config: &Config, tls_feature_compiled: bool) -> bool {
    config.security.transport.tls.enabled && tls_feature_compiled
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
    use crate::config::AppRegistrationEntry;

    /// Truth table for the TURN-without-TLS startup warning: it fires exactly
    /// when the server brokers WebRTC (`turn.enabled`) without an effective
    /// built-in TLS listener. Configuration alone must not hide the warning
    /// when the binary was compiled without the `tls` feature.
    #[test]
    fn signaling_tls_warning_fires_only_for_turn_without_tls() {
        let cases = [
            (false, false, false, false),
            (false, true, false, false),
            (true, false, false, true),
            (true, true, false, true),
            (true, true, true, false),
        ];
        for (turn_enabled, tls_enabled, tls_feature_compiled, expected) in cases {
            let mut config = Config::default();
            config.turn.enabled = turn_enabled;
            config.security.transport.tls.enabled = tls_enabled;
            assert_eq!(
                turn_enabled && !built_in_tls_active(&config, tls_feature_compiled),
                expected,
                "turn.enabled={turn_enabled}, tls.enabled={tls_enabled}, \
                 tls_feature_compiled={tls_feature_compiled}"
            );
        }
    }

    #[cfg(not(feature = "tls"))]
    #[test]
    fn tls_enabled_is_rejected_when_the_binary_lacks_tls_support() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.security.transport.tls.enabled = true;

        let error = validate_config_security(&config)
            .expect_err("a non-TLS binary must never accept an HTTPS configuration");
        assert!(error
            .to_string()
            .contains("compiled with the `tls` Cargo feature"));
    }

    #[cfg(not(feature = "tls"))]
    #[test]
    fn client_fingerprint_binding_is_rejected_without_tls_support() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.security.transport.token_binding.enabled = true;
        config.security.transport.token_binding.required = true;
        config
            .security
            .transport
            .token_binding
            .require_client_fingerprint = true;

        let error = validate_config_security(&config)
            .expect_err("a binary without TLS cannot derive authenticated certificate identity");
        assert!(error.to_string().contains("requires active built-in TLS"));
    }

    #[test]
    fn required_token_binding_cannot_be_disabled() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.security.transport.token_binding.required = true;

        let error = validate_config_security(&config)
            .expect_err("required token binding must not be disabled");
        assert!(error
            .to_string()
            .contains("requires security.transport.token_binding.enabled=true"));
    }

    #[test]
    fn enabled_token_binding_rejects_reserved_non_v2_subprotocols() {
        for subprotocol in [
            "signalfish.tokenbinding.v1",
            "Signalfish.TokenBinding.V2",
            "signalfish.tokenbinding.v3",
        ] {
            let mut config = Config::default();
            config.security.require_metrics_auth = false;
            config.security.transport.token_binding.enabled = true;
            config.security.transport.token_binding.subprotocol = subprotocol.to_string();

            let error = validate_config_security(&config)
                .expect_err("reserved protocol versions must match the v2 wire contract");
            assert!(error.to_string().contains("reserved protocol namespace"));
            assert!(error.to_string().contains("signalfish.tokenbinding.v2"));
        }
    }

    #[test]
    fn token_binding_subprotocol_alias_and_disabled_legacy_value_remain_compatible() {
        for (enabled, subprotocol) in [
            (true, "example.game.token-binding"),
            (false, "signalfish.tokenbinding.v1"),
        ] {
            let mut config = Config::default();
            config.security.require_metrics_auth = false;
            config.security.transport.token_binding.enabled = enabled;
            config.security.transport.token_binding.subprotocol = subprotocol.to_string();
            assert!(
                validate_config_security(&config).is_ok(),
                "enabled={enabled}, subprotocol={subprotocol}"
            );
        }
    }

    #[cfg(feature = "tls")]
    #[test]
    fn client_fingerprint_binding_accepts_optional_or_required_mtls() {
        for client_auth in [ClientAuthMode::Optional, ClientAuthMode::Require] {
            let mut config = Config::default();
            config.security.require_metrics_auth = false;
            config.security.transport.tls.enabled = true;
            config.security.transport.tls.certificate_path =
                Some("tests/fixtures/tls/cert.pem".to_string());
            config.security.transport.tls.private_key_path =
                Some("tests/fixtures/tls/key.pem".to_string());
            config.security.transport.tls.client_ca_cert_path =
                Some("tests/fixtures/tls/cert.pem".to_string());
            config.security.transport.tls.client_auth = client_auth;
            config.security.transport.token_binding.enabled = true;
            config.security.transport.token_binding.required = true;
            config
                .security
                .transport
                .token_binding
                .require_client_fingerprint = true;

            let result = validate_config_security(&config);
            assert!(
                result.is_ok(),
                "{client_auth:?} mTLS must support verified fingerprint binding: {result:?}"
            );
        }
    }

    #[cfg(feature = "tls")]
    #[test]
    fn client_fingerprint_binding_rejects_disabled_client_auth() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.security.transport.tls.enabled = true;
        config.security.transport.tls.certificate_path =
            Some("tests/fixtures/tls/cert.pem".to_string());
        config.security.transport.tls.private_key_path =
            Some("tests/fixtures/tls/key.pem".to_string());
        config.security.transport.token_binding.enabled = true;
        config.security.transport.token_binding.required = true;
        config
            .security
            .transport
            .token_binding
            .require_client_fingerprint = true;

        let error = validate_config_security(&config)
            .expect_err("client-auth none cannot produce a peer certificate");
        assert!(error.to_string().contains("client_auth"));
    }

    #[test]
    fn client_fingerprint_binding_requires_token_binding() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config
            .security
            .transport
            .token_binding
            .require_client_fingerprint = true;

        let error = validate_config_security(&config)
            .expect_err("fingerprint binding has no effect while token binding is disabled");
        assert!(error.to_string().contains("token_binding.enabled=true"));
    }

    #[test]
    fn client_fingerprint_binding_requires_mandatory_subprotocol() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.security.transport.token_binding.enabled = true;
        config
            .security
            .transport
            .token_binding
            .require_client_fingerprint = true;

        let error = validate_config_security(&config)
            .expect_err("a client must not opt out of configured fingerprint binding");
        assert!(error.to_string().contains("token_binding.required=true"));
        assert!(error.to_string().contains("omitting the subprotocol"));
    }

    /// The warning predicate must not be affected by unrelated security knobs.
    #[test]
    fn signaling_tls_warning_ignores_unrelated_settings() {
        let mut config = Config::default();
        config.turn.enabled = true;
        config.security.enforce_app_id_allowlist = false;
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

    /// A zero `max_message_size` is contradictory dead config (every frame
    /// rejected, zero transport cap derived on the upgrade) and must fail
    /// startup with a diagnostic that names THIS knob — not a downstream
    /// consequence like the signal-cap comparison.
    #[test]
    fn zero_max_message_size_is_rejected_with_a_direct_diagnostic() {
        let mut config = Config::default();
        // Quiet the unrelated metrics-credentials check so the failure below
        // can only come from the knob under test.
        config.security.require_metrics_auth = false;
        config.security.max_message_size = 0;

        let err = validate_config_security(&config)
            .expect_err("max_message_size = 0 must be rejected at startup");
        assert!(
            err.to_string()
                .contains("security.max_message_size must be greater than 0"),
            "error must name security.max_message_size directly: {err}"
        );
    }

    #[test]
    fn allowed_apps_reject_blank_required_fields_with_indexed_errors() {
        let cases: [(&str, fn(&mut AppRegistrationEntry)); 2] = [
            ("app_id", |app| app.app_id = " ".to_string()),
            ("app_name", |app| app.app_name = "\n".to_string()),
        ];

        for (field, mutate) in cases {
            let mut app = AppRegistrationEntry {
                app_id: "game".to_string(),
                app_name: "Game".to_string(),
                max_rooms: None,
                max_players_per_room: None,
                rate_limit_per_minute: None,
            };
            mutate(&mut app);

            let mut config = Config::default();
            config.security.require_metrics_auth = false;
            config.security.allowed_apps = vec![
                AppRegistrationEntry {
                    app_id: "valid".to_string(),
                    app_name: "Valid".to_string(),
                    max_rooms: None,
                    max_players_per_room: None,
                    rate_limit_per_minute: None,
                },
                app,
            ];

            let err = validate_config_security(&config)
                .expect_err("blank allowed app fields are rejected");
            let expected = format!("security.allowed_apps[1].{field}");
            assert!(
                err.to_string().contains(&expected),
                "error must point at the blank field {expected}: {err}"
            );
        }
    }

    #[test]
    fn allowed_apps_reject_duplicate_public_ids_before_policy_construction() {
        let entry = AppRegistrationEntry {
            app_id: "duplicate".to_string(),
            app_name: "First".to_string(),
            max_rooms: Some(1),
            max_players_per_room: Some(2),
            rate_limit_per_minute: Some(3),
        };
        let mut conflicting = entry.clone();
        conflicting.app_name = "Conflicting".to_string();
        conflicting.max_rooms = Some(99);

        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.security.allowed_apps = vec![entry, conflicting];

        let error = validate_config_security(&config)
            .expect_err("duplicate public app IDs must not use last-entry-wins semantics");
        assert!(error.to_string().contains("allowed_apps[1].app_id"));
        assert!(error.to_string().contains("duplicates an earlier entry"));
    }

    #[test]
    fn room_code_generation_config_rejects_unjoinable_codes() {
        struct Case {
            name: &'static str,
            length: usize,
            prefix: Option<&'static str>,
            expected: &'static str,
        }

        let cases = [
            Case {
                name: "zero total length",
                length: 0,
                prefix: None,
                expected: "protocol.room_code_length must be greater than 0",
            },
            Case {
                name: "blank configured prefix",
                length: 6,
                prefix: Some(" \t"),
                expected: "server.room_code_prefix must not be blank",
            },
            Case {
                name: "punctuation in prefix",
                length: 6,
                prefix: Some("EU-"),
                expected: "server.room_code_prefix must contain only ASCII alphanumeric",
            },
            Case {
                name: "non-ASCII prefix",
                length: 6,
                prefix: Some("ÉU"),
                expected: "server.room_code_prefix must contain only ASCII alphanumeric",
            },
            Case {
                name: "prefix consumes entire code",
                length: 6,
                prefix: Some("ABCDEF"),
                expected: "server.room_code_prefix must be shorter than protocol.room_code_length",
            },
            Case {
                name: "prefix exceeds code",
                length: 6,
                prefix: Some("ABCDEFG"),
                expected: "server.room_code_prefix must be shorter than protocol.room_code_length",
            },
        ];

        for case in cases {
            let mut config = Config::default();
            config.security.require_metrics_auth = false;
            config.protocol.room_code_length = case.length;
            config.server.room_code_prefix = case.prefix.map(ToString::to_string);

            let error = match validate_config_security(&config) {
                Ok(()) => panic!("{} must fail startup validation", case.name),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains(case.expected),
                "{}: expected `{}`, got `{error}`",
                case.name,
                case.expected
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

    /// Timeout inversion (BUG-2): the slow-consumer grace period must be
    /// strictly less than the activity-reaper deadline. Data-driven over the
    /// boundary. `ping_timeout` default is 30 s (30000 ms). This asserts the
    /// check's NECESSARY floor — it rejects `slow >= ping` (the provable
    /// inversion region derived by `formal/tla/SenderPacingReaper.tla`)
    /// and accepts `slow < ping`. Passing the floor is not a safety
    /// certificate: a thin margin can still invert under load, which is an
    /// operator sizing concern (see `validate_config_security`).
    #[test]
    fn slow_consumer_timeout_must_be_below_ping_deadline() {
        // (slow_consumer_timeout_ms, ping_timeout_secs, expect_ok)
        let cases = [
            (5_000_u64, 30_u64, true), // default: well below
            (29_999, 30, true),        // just below the 30000 ms deadline
            (30_000, 30, false),       // equal: reaper could win → rejected
            (60_000, 30, false),       // above: the classic inversion → rejected
            (60_000, 0, true),         // ping_timeout 0 disables the reaper: no inversion
            (10_000, 5, false),        // 10000 ms >= 5000 ms deadline → rejected
        ];

        for (slow_ms, ping_s, expect_ok) in cases {
            let mut config = Config::default();
            config.security.require_metrics_auth = false;
            config.websocket.slow_consumer_timeout_ms = slow_ms;
            config.server.ping_timeout = ping_s;

            let result = validate_config_security(&config);
            assert_eq!(
                result.is_ok(),
                expect_ok,
                "slow_consumer_timeout_ms={slow_ms}, ping_timeout={ping_s}s"
            );
            if !expect_ok {
                let err = result.unwrap_err().to_string();
                assert!(
                    err.contains("slow_consumer_timeout_ms") && err.contains("ping_timeout"),
                    "rejection must name both offending fields: {err}"
                );
            }
        }
    }

    /// The heartbeat throttle must stay below the inactive-room deadline, or the
    /// throttled room-activity refresh can let GC reap an occupied room (BUG-1
    /// via misconfiguration). Data-driven over the boundary.
    #[test]
    fn heartbeat_throttle_must_be_below_inactive_room_timeout() {
        // (heartbeat_throttle_secs, inactive_room_timeout_secs, expect_ok)
        let cases = [
            (30_u64, 3600_u64, true), // defaults: well below
            (3599, 3600, true),       // just below
            (3600, 3600, false),      // equal: refresh can lag the reaper → rejected
            (7200, 3600, false),      // above: the misconfig bugbot flagged → rejected
            (0, 3600, true),          // 0 disables throttling (refresh every heartbeat) → safe
        ];

        for (throttle, inactive, expect_ok) in cases {
            let mut config = Config::default();
            config.security.require_metrics_auth = false;
            config.server.heartbeat_throttle_secs = throttle;
            config.server.inactive_room_timeout = inactive;

            let result = validate_config_security(&config);
            assert_eq!(
                result.is_ok(),
                expect_ok,
                "heartbeat_throttle_secs={throttle}, inactive_room_timeout={inactive}"
            );
            if !expect_ok {
                let err = result.unwrap_err().to_string();
                assert!(
                    err.contains("heartbeat_throttle_secs")
                        && err.contains("inactive_room_timeout"),
                    "rejection must name both offending fields: {err}"
                );
            }
        }
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

    #[test]
    fn extreme_event_buffer_size_is_rejected_before_server_startup() {
        let mut config = Config::default();
        config.security.require_metrics_auth = false;
        config.server.event_buffer_size = usize::MAX;

        let error = validate_config_security(&config)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(error.contains("server.event_buffer_size must not exceed"));
    }
}
