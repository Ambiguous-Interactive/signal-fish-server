//! Configuration loading and HTTP endpoint integration tests.
//!
//! Covers:
//! - Config loading from JSON (`SIGNAL_FISH_CONFIG_JSON`)
//! - Environment variable overrides (`SIGNAL_FISH__*`)
//! - Health endpoint (`/health`)
//! - Metrics endpoint (`/metrics`)

mod test_helpers;

use axum::routing::get;
use regex::Regex;
use signal_fish_server::config::{
    AppAuthEntry, ClientAuthMode, Config, DashboardHistoryField, LogFormat,
};
use signal_fish_server::security::token_binding::TokenBindingScheme;
use signal_fish_server::websocket::{
    create_router, create_standalone_router, websocket_handler_v3,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
use test_helpers::{create_test_server, test_server_config};

fn repo_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn collect_markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()))
    {
        let path = entry
            .unwrap_or_else(|err| panic!("failed to read entry in {}: {err}", dir.display()))
            .path();
        if path.is_dir() {
            collect_markdown_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

#[derive(Clone, Copy)]
struct ConfigReferenceRow {
    env: &'static str,
    path: &'static str,
    default: Option<&'static str>,
}

const CONFIG_REFERENCE_ROWS: &[ConfigReferenceRow] = &[
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PORT",
        path: "port",
        default: Some("3536"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__DEFAULT_MAX_PLAYERS",
        path: "server.default_max_players",
        default: Some("8"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__PING_TIMEOUT",
        path: "server.ping_timeout",
        default: Some("30"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__ROOM_CLEANUP_INTERVAL",
        path: "server.room_cleanup_interval",
        default: Some("60"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__MAX_ROOMS_PER_GAME",
        path: "server.max_rooms_per_game",
        default: Some("1000"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__EMPTY_ROOM_TIMEOUT",
        path: "server.empty_room_timeout",
        default: Some("300"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__INACTIVE_ROOM_TIMEOUT",
        path: "server.inactive_room_timeout",
        default: Some("3600"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__RECONNECTION_WINDOW",
        path: "server.reconnection_window",
        default: Some("300"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__EVENT_BUFFER_SIZE",
        path: "server.event_buffer_size",
        default: Some("100"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__ENABLE_RECONNECTION",
        path: "server.enable_reconnection",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__HEARTBEAT_THROTTLE_SECS",
        path: "server.heartbeat_throttle_secs",
        default: Some("30"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__REGION_ID",
        path: "server.region_id",
        default: Some("default"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SERVER__ROOM_CODE_PREFIX",
        path: "server.room_code_prefix",
        default: Some("null"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__RATE_LIMIT__MAX_ROOM_CREATIONS",
        path: "rate_limit.max_room_creations",
        default: Some("5"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__RATE_LIMIT__TIME_WINDOW",
        path: "rate_limit.time_window",
        default: Some("60"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__RATE_LIMIT__MAX_JOIN_ATTEMPTS",
        path: "rate_limit.max_join_attempts",
        default: Some("20"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__RATE_LIMIT__MAX_SIGNALS",
        path: "rate_limit.max_signals",
        default: Some("600"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__RATE_LIMIT__MAX_SIGNAL_ERRORS",
        path: "rate_limit.max_signal_errors",
        default: Some("60"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__MAX_GAME_NAME_LENGTH",
        path: "protocol.max_game_name_length",
        default: Some("64"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__ROOM_CODE_LENGTH",
        path: "protocol.room_code_length",
        default: Some("6"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__MAX_PLAYER_NAME_LENGTH",
        path: "protocol.max_player_name_length",
        default: Some("32"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__MAX_PLAYERS_LIMIT",
        path: "protocol.max_players_limit",
        default: Some("100"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__ENABLE_MESSAGE_PACK_GAME_DATA",
        path: "protocol.enable_message_pack_game_data",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__MIN_PROTOCOL_VERSION",
        path: "protocol.min_protocol_version",
        default: Some("2"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__MAX_PROTOCOL_VERSION",
        path: "protocol.max_protocol_version",
        default: Some("3"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__ENFORCE",
        path: "protocol.sdk_compatibility.enforce",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__MINIMUM_VERSIONS",
        path: "protocol.sdk_compatibility.minimum_versions",
        default: None,
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__RECOMMENDED_VERSIONS",
        path: "protocol.sdk_compatibility.recommended_versions",
        default: None,
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__CAPABILITIES",
        path: "protocol.sdk_compatibility.capabilities",
        default: None,
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__NOTES",
        path: "protocol.sdk_compatibility.notes",
        default: None,
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOW_UNICODE_ALPHANUMERIC",
        path: "protocol.player_name_validation.allow_unicode_alphanumeric",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOW_SPACES",
        path: "protocol.player_name_validation.allow_spaces",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOW_LEADING_TRAILING_WHITESPACE",
        path: "protocol.player_name_validation.allow_leading_trailing_whitespace",
        default: Some("false"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOWED_SYMBOLS",
        path: "protocol.player_name_validation.allowed_symbols",
        default: Some(r#"["-","_"]"#),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ADDITIONAL_ALLOWED_CHARACTERS",
        path: "protocol.player_name_validation.additional_allowed_characters",
        default: Some("null"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__LOGGING__DIR",
        path: "logging.dir",
        default: Some("logs"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__LOGGING__FILENAME",
        path: "logging.filename",
        default: Some("server.log"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__LOGGING__ROTATION",
        path: "logging.rotation",
        default: Some("daily"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__LOGGING__LEVEL",
        path: "logging.level",
        default: Some("null"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__LOGGING__ENABLE_FILE_LOGGING",
        path: "logging.enable_file_logging",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__LOGGING__FORMAT",
        path: "logging.format",
        default: Some("json"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__CORS_ORIGINS",
        path: "security.cors_origins",
        default: Some("http://localhost:3000,http://localhost:5173"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH",
        path: "security.require_websocket_auth",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH",
        path: "security.require_metrics_auth",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN",
        path: "security.metrics_auth_token",
        default: Some("null"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__MAX_MESSAGE_SIZE",
        path: "security.max_message_size",
        default: Some("65536"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__MAX_SIGNAL_BYTES",
        path: "security.max_signal_bytes",
        default: Some("16384"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__MAX_CONNECTIONS_PER_IP",
        path: "security.max_connections_per_ip",
        default: Some("10"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED",
        path: "security.transport.tls.enabled",
        default: Some("false"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CERTIFICATE_PATH",
        path: "security.transport.tls.certificate_path",
        default: Some("null"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__PRIVATE_KEY_PATH",
        path: "security.transport.tls.private_key_path",
        default: Some("null"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CLIENT_CA_CERT_PATH",
        path: "security.transport.tls.client_ca_cert_path",
        default: Some("null"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CLIENT_AUTH",
        path: "security.transport.tls.client_auth",
        default: Some("none"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__ENABLED",
        path: "security.transport.token_binding.enabled",
        default: Some("false"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRED",
        path: "security.transport.token_binding.required",
        default: Some("false"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRE_CLIENT_FINGERPRINT",
        path: "security.transport.token_binding.require_client_fingerprint",
        default: Some("false"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__SUBPROTOCOL",
        path: "security.transport.token_binding.subprotocol",
        default: Some("signalfish.tokenbinding.v1"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__SCHEME",
        path: "security.transport.token_binding.scheme",
        default: Some("sec_websocket_key_sha256"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__AUTHORIZED_APPS",
        path: "security.authorized_apps",
        default: Some("[]"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__AUTH__RATE_LIMIT_CACHE_CLEANUP_INTERVAL_SECS",
        path: "auth.rate_limit_cache_cleanup_interval_secs",
        default: Some("300"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__AUTH__RATE_LIMIT_CACHE_RETENTION_SECS",
        path: "auth.rate_limit_cache_retention_secs",
        default: Some("172800"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__AUTH__RATE_LIMIT_CACHE_ALERT_ROWS",
        path: "auth.rate_limit_cache_alert_rows",
        default: Some("100000"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__COORDINATION__DEDUP_CACHE__CAPACITY",
        path: "coordination.dedup_cache.capacity",
        default: Some("100000"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__COORDINATION__DEDUP_CACHE__TTL_SECS",
        path: "coordination.dedup_cache.ttl_secs",
        default: Some("60"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__COORDINATION__DEDUP_CACHE__CLEANUP_INTERVAL_SECS",
        path: "coordination.dedup_cache.cleanup_interval_secs",
        default: Some("30"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__COORDINATION__MEMBERSHIP_SNAPSHOT_INTERVAL_SECS",
        path: "coordination.membership_snapshot_interval_secs",
        default: Some("30"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__METRICS__DASHBOARD_CACHE_REFRESH_INTERVAL_SECS",
        path: "metrics.dashboard_cache_refresh_interval_secs",
        default: Some("5"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__METRICS__DASHBOARD_CACHE_TTL_SECS",
        path: "metrics.dashboard_cache_ttl_secs",
        default: Some("30"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__METRICS__DASHBOARD_CACHE_HISTORY_WINDOW_SECS",
        path: "metrics.dashboard_cache_history_window_secs",
        default: Some("300"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__METRICS__DASHBOARD_CACHE_HISTORY_FIELDS",
        path: "metrics.dashboard_cache_history_fields",
        default: Some(
            r#"["active_rooms","rooms_by_game","player_percentiles","game_percentiles"]"#,
        ),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__RELAY_TYPES__DEFAULT_RELAY_TYPE",
        path: "relay_types.default_relay_type",
        default: Some("matchbox"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__RELAY_TYPES__GAME_RELAY_MAPPINGS",
        path: "relay_types.game_relay_mappings",
        default: Some("{}"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SESSION__DEFAULT_TOPOLOGY",
        path: "session.default_topology",
        default: Some("relay"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SESSION__GAME_TOPOLOGY_MAPPINGS",
        path: "session.game_topology_mappings",
        default: Some("{}"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SESSION__ENABLE_WEBRTC",
        path: "session.enable_webrtc",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SESSION__ENABLE_DIRECT",
        path: "session.enable_direct",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SESSION__ENABLE_ICE_PREGATHER",
        path: "session.enable_ice_pregather",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SESSION__ICE_SERVERS",
        path: "session.ice_servers",
        default: Some("[]"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__TURN__ENABLED",
        path: "turn.enabled",
        default: Some("false"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__TURN__MODE",
        path: "turn.mode",
        default: Some("static_secret"),
    },
    ConfigReferenceRow {
        // Empty string by default; not asserted against Config::default() here
        // (an empty default cell is awkward in the table), but its presence and
        // path are still validated.
        env: "SIGNAL_FISH__TURN__STATIC_AUTH_SECRET",
        path: "turn.static_auth_secret",
        default: None,
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__TURN__URLS",
        path: "turn.urls",
        default: Some("[]"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__TURN__STUN_URLS",
        path: "turn.stun_urls",
        default: Some("[\"stun:stun.l.google.com:19302\"]"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__TURN__CREDENTIAL_TTL_SECS",
        path: "turn.credential_ttl_secs",
        default: Some("3600"),
    },
    ConfigReferenceRow {
        // `Option`, skipped from serialization when None, so no default is
        // asserted against Config::default(); documented as `null` (unset).
        env: "SIGNAL_FISH__TURN__MANAGED_PROVIDER",
        path: "turn.managed_provider",
        default: None,
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__TURN__MANAGED_API_TOKEN",
        path: "turn.managed_api_token",
        default: None,
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__WEBSOCKET__ENABLE_BATCHING",
        path: "websocket.enable_batching",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__WEBSOCKET__BATCH_SIZE",
        path: "websocket.batch_size",
        default: Some("10"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__WEBSOCKET__BATCH_INTERVAL_MS",
        path: "websocket.batch_interval_ms",
        default: Some("16"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__WEBSOCKET__AUTH_TIMEOUT_SECS",
        path: "websocket.auth_timeout_secs",
        default: Some("10"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__WEBSOCKET__IDLE_TIMEOUT_SECS",
        path: "websocket.idle_timeout_secs",
        default: Some("300"),
    },
];

const README_REQUIRED_CONFIG_ROWS: &[ConfigReferenceRow] = &[
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PORT",
        path: "port",
        default: Some("3536"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__ENABLE_MESSAGE_PACK_GAME_DATA",
        path: "protocol.enable_message_pack_game_data",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__MIN_PROTOCOL_VERSION",
        path: "protocol.min_protocol_version",
        default: Some("2"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__PROTOCOL__MAX_PROTOCOL_VERSION",
        path: "protocol.max_protocol_version",
        default: Some("3"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__LOGGING__LEVEL",
        path: "logging.level",
        default: Some("null"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__LOGGING__ENABLE_FILE_LOGGING",
        path: "logging.enable_file_logging",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__LOGGING__FORMAT",
        path: "logging.format",
        default: Some("json"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__CORS_ORIGINS",
        path: "security.cors_origins",
        default: Some("http://localhost:3000,http://localhost:5173"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH",
        path: "security.require_websocket_auth",
        default: Some("true"),
    },
    ConfigReferenceRow {
        env: "SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH",
        path: "security.require_metrics_auth",
        default: Some("true"),
    },
];

#[derive(Debug, PartialEq, Eq)]
struct ParsedConfigRow {
    env: String,
    path: String,
    default: String,
}

fn strip_markdown_code(value: &str) -> String {
    value.trim().trim_matches('`').trim().to_string()
}

fn parse_config_reference_rows(content: &str) -> Vec<ParsedConfigRow> {
    content
        .lines()
        .filter_map(|line| {
            let cells = line
                .split('|')
                .skip(1)
                .map(strip_markdown_code)
                .collect::<Vec<_>>();
            if cells.len() < 3 || !cells[0].starts_with("SIGNAL_FISH__") {
                return None;
            }
            Some(ParsedConfigRow {
                env: cells[0].clone(),
                path: cells[1].clone(),
                default: cells[2].clone(),
            })
        })
        .collect()
}

fn config_value_at_path<'a>(
    root: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    path.split('.')
        .try_fold(root, |value, segment| value.get(segment))
}

fn render_doc_default(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::Array(_)
        | serde_json::Value::Object(_) => value.to_string(),
    }
}

fn assert_config_reference_rows(
    path: &Path,
    required_rows: &[ConfigReferenceRow],
    require_all_rows: bool,
) {
    let content = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    let parsed_rows = parse_config_reference_rows(&content);
    let mut violations = Vec::new();

    for expected in required_rows {
        match parsed_rows.iter().find(|row| row.env == expected.env) {
            Some(actual) if actual.path != expected.path => violations.push(format!(
                "{} documents {} as `{}`, expected `{}`",
                path.display(),
                expected.env,
                actual.path,
                expected.path
            )),
            Some(actual) => {
                if let Some(default) = expected.default {
                    if actual.default != default {
                        violations.push(format!(
                            "{} documents {} default as `{}`, expected `{}`",
                            path.display(),
                            expected.env,
                            actual.default,
                            default
                        ));
                    }
                }
            }
            None => violations.push(format!(
                "{} is missing required config reference row for `{}` ({})",
                path.display(),
                expected.env,
                expected.path
            )),
        }
    }

    if require_all_rows {
        for actual in parsed_rows {
            if !required_rows.iter().any(|row| row.env == actual.env) {
                violations.push(format!(
                    "{} documents unexpected config env var `{}`",
                    path.display(),
                    actual.env
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "config reference table drift found:\n{}",
        violations.join("\n")
    );
}

fn json_fence_blocks(content: &str) -> Vec<(usize, String)> {
    let mut blocks = Vec::new();
    let mut in_json_block = false;
    let mut start_line = 0;
    let mut block = String::new();

    for (line_index, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_json_block {
                blocks.push((start_line, std::mem::take(&mut block)));
                in_json_block = false;
            } else if trimmed
                .trim_start_matches('`')
                .trim_start()
                .starts_with("json")
            {
                in_json_block = true;
                start_line = line_index + 1;
            }
            continue;
        }

        if in_json_block {
            block.push_str(line);
            block.push('\n');
        }
    }

    blocks
}

fn collect_config_enum_token_violations(
    label: &str,
    value: &serde_json::Value,
    violations: &mut Vec<String>,
) {
    for (pointer, allowed) in [
        ("/logging/format", &["json", "text"][..]),
        (
            "/security/transport/tls/client_auth",
            &["none", "optional", "require"][..],
        ),
        (
            "/security/transport/token_binding/scheme",
            &["sec_websocket_key_sha256"][..],
        ),
    ] {
        let Some(value) = value.pointer(pointer) else {
            continue;
        };
        let Some(token) = value.as_str() else {
            violations.push(format!("{label} has non-string `{pointer}` value: {value}"));
            continue;
        };
        if !allowed.contains(&token) {
            violations.push(format!(
                "{label} documents non-canonical `{pointer}` value `{token}`; expected one of: {}",
                allowed.join(", ")
            ));
        }
    }

    if let Some(fields) = value.pointer("/metrics/dashboard_cache_history_fields") {
        let Some(fields) = fields.as_array() else {
            violations.push(format!(
                "{label} has non-array `/metrics/dashboard_cache_history_fields` value: {fields}"
            ));
            return;
        };
        let allowed = [
            "active_rooms",
            "rooms_by_game",
            "player_percentiles",
            "game_percentiles",
            "active_connections",
            "rooms_created",
        ];
        for field in fields {
            let Some(token) = field.as_str() else {
                violations.push(format!(
                    "{label} has non-string dashboard history field value: {field}"
                ));
                continue;
            };
            if !allowed.contains(&token) {
                violations.push(format!(
                    "{label} documents non-canonical dashboard history field `{token}`; expected one of: {}",
                    allowed.join(", ")
                ));
            }
        }
    }
}

// ===========================================================================
// Config loading tests
// ===========================================================================

#[test]
fn test_config_default_values() {
    let config = Config::default();

    assert_eq!(config.port, 3536);
    assert_eq!(config.server.default_max_players, 8);
    assert_eq!(config.server.ping_timeout, 30);
    assert_eq!(config.server.room_cleanup_interval, 60);
    assert_eq!(config.server.max_rooms_per_game, 1000);
    assert_eq!(config.server.empty_room_timeout, 300);
    assert_eq!(config.server.inactive_room_timeout, 3600);
    assert_eq!(config.protocol.room_code_length, 6);
    assert_eq!(config.protocol.max_game_name_length, 64);
    assert_eq!(config.protocol.max_player_name_length, 32);
    assert_eq!(config.protocol.max_players_limit, 100);
    assert!(config.security.require_websocket_auth);
    assert!(config.security.require_metrics_auth);
}

#[test]
fn test_config_roundtrip_serialization() {
    let config = Config::default();
    let json = serde_json::to_string_pretty(&config).expect("serialization should succeed");
    let deserialized: Config = serde_json::from_str(&json).expect("deserialization should succeed");

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
fn test_config_from_json_string() {
    let json = r#"{
        "port": 9999,
        "server": {
            "default_max_players": 16,
            "region_id": "us-east-1"
        },
        "protocol": {
            "room_code_length": 8
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.port, 9999);
    assert_eq!(config.server.default_max_players, 16);
    assert_eq!(config.server.region_id, "us-east-1");
    assert_eq!(config.protocol.room_code_length, 8);
    // Non-specified fields should remain at defaults
    assert_eq!(config.server.ping_timeout, 30);
}

#[test]
fn test_config_partial_json_uses_defaults_for_missing_fields() {
    let json = r#"{ "port": 4000 }"#;
    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.port, 4000);
    // All other fields should be defaults
    assert_eq!(config.server.default_max_players, 8);
    assert_eq!(config.protocol.room_code_length, 6);
    assert_eq!(config.logging.dir, "logs");
}

#[test]
fn test_config_security_authorized_apps_deserialization() {
    let json = r#"{
        "security": {
            "require_websocket_auth": true,
            "authorized_apps": [
                {
                    "app_id": "my-game",
                    "app_secret": "my-secret",
                    "app_name": "My Game",
                    "max_rooms": 100,
                    "max_players_per_room": 4,
                    "rate_limit_per_minute": 30
                }
            ]
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert!(config.security.require_websocket_auth);
    assert_eq!(config.security.authorized_apps.len(), 1);

    let app = &config.security.authorized_apps[0];
    assert_eq!(app.app_id, "my-game");
    assert_eq!(app.app_secret, "my-secret");
    assert_eq!(app.app_name, "My Game");
    assert_eq!(app.max_rooms, Some(100));
    assert_eq!(app.max_players_per_room, Some(4));
    assert_eq!(app.rate_limit_per_minute, Some(30));
}

#[test]
fn test_config_rate_limit_section() {
    let json = r#"{
        "rate_limit": {
            "max_room_creations": 20,
            "time_window": 120,
            "max_join_attempts": 50,
            "max_signals": 700,
            "max_signal_errors": 70
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.rate_limit.max_room_creations, 20);
    assert_eq!(config.rate_limit.time_window, 120);
    assert_eq!(config.rate_limit.max_join_attempts, 50);
    assert_eq!(config.rate_limit.max_signals, 700);
    assert_eq!(config.rate_limit.max_signal_errors, 70);
}

#[test]
fn test_config_example_includes_all_rate_limit_fields() {
    let path = repo_path("config.example.json");
    let content = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    let value: serde_json::Value =
        serde_json::from_str(&content).expect("config.example.json must be valid JSON");
    let rate_limit = value
        .get("rate_limit")
        .and_then(serde_json::Value::as_object)
        .expect("config.example.json must include a rate_limit object");

    for key in [
        "max_room_creations",
        "time_window",
        "max_join_attempts",
        "max_signals",
        "max_signal_errors",
    ] {
        assert!(
            rate_limit.contains_key(key),
            "config.example.json rate_limit must document `{key}`"
        );
    }

    let config: Config =
        serde_json::from_str(&content).expect("config.example.json must parse as Config");
    assert_eq!(config.rate_limit.max_signals, 600);
    assert_eq!(config.rate_limit.max_signal_errors, 60);
    assert_eq!(config.logging.format, LogFormat::Json);
    assert_eq!(
        value.pointer("/logging/format"),
        Some(&serde_json::json!("json")),
        "config.example.json must use the canonical lowercase logging format token"
    );

    // Protocol v3 session block (P3): the example must document every field.
    let session = value
        .get("session")
        .and_then(serde_json::Value::as_object)
        .expect("config.example.json must include a session object");
    for key in [
        "default_topology",
        "game_topology_mappings",
        "enable_webrtc",
        "enable_direct",
        "enable_ice_pregather",
        "ice_servers",
    ] {
        assert!(
            session.contains_key(key),
            "config.example.json session must document `{key}`"
        );
    }
    assert_eq!(
        config.session.default_topology,
        signal_fish_server::protocol::Topology::Relay,
        "session.default_topology must default to the relay floor"
    );
    assert!(config.session.enable_webrtc);
    assert!(config.session.enable_direct);
    assert!(config.session.enable_ice_pregather);
    assert_eq!(
        value.pointer("/session/default_topology"),
        Some(&serde_json::json!("relay")),
        "config.example.json must use the canonical lowercase topology token"
    );
    // The example config must pass session validation.
    config
        .session
        .validate()
        .expect("config.example.json session block must validate");

    // Protocol v3 TURN block (P4): the example must document every field.
    let turn = value
        .get("turn")
        .and_then(serde_json::Value::as_object)
        .expect("config.example.json must include a turn object");
    for key in [
        "enabled",
        "mode",
        "static_auth_secret",
        "urls",
        "stun_urls",
        "credential_ttl_secs",
    ] {
        assert!(
            turn.contains_key(key),
            "config.example.json turn must document `{key}`"
        );
    }
    assert_eq!(
        value.pointer("/turn/mode"),
        Some(&serde_json::json!("static_secret")),
        "config.example.json must use the canonical snake_case TURN mode token"
    );
    assert_eq!(
        value.pointer("/turn/enabled"),
        Some(&serde_json::json!(false)),
        "config.example.json must ship with TURN disabled (relay-floor posture)"
    );
    // The example config must pass TURN validation.
    config
        .turn
        .validate()
        .expect("config.example.json turn block must validate");
}

#[test]
fn test_config_enum_tokens_are_canonical_and_backward_compatible() {
    assert_eq!(
        serde_json::to_string(&LogFormat::Json).unwrap(),
        r#""json""#
    );
    assert_eq!(
        serde_json::to_string(&LogFormat::Text).unwrap(),
        r#""text""#
    );
    assert_eq!(
        serde_json::from_str::<LogFormat>(r#""json""#).unwrap(),
        LogFormat::Json
    );
    assert_eq!(
        serde_json::from_str::<LogFormat>(r#""Json""#).unwrap(),
        LogFormat::Json
    );
    assert_eq!(
        serde_json::from_str::<LogFormat>(r#"" text ""#).unwrap(),
        LogFormat::Text
    );
    assert!(
        serde_json::from_str::<LogFormat>(r#""yaml""#).is_err(),
        "unknown logging formats must fail with a diagnostic instead of falling back silently"
    );

    assert_eq!(
        serde_json::to_string(&ClientAuthMode::None).unwrap(),
        r#""none""#
    );
    assert_eq!(
        serde_json::to_string(&ClientAuthMode::Optional).unwrap(),
        r#""optional""#
    );
    assert_eq!(
        serde_json::to_string(&ClientAuthMode::Require).unwrap(),
        r#""require""#
    );
    assert_eq!(
        serde_json::from_str::<ClientAuthMode>(r#""require""#).unwrap(),
        ClientAuthMode::Require
    );
    assert_eq!(
        serde_json::from_str::<ClientAuthMode>(r#""Require""#).unwrap(),
        ClientAuthMode::Require
    );
    assert_eq!(
        serde_json::from_str::<ClientAuthMode>(r#"" optional ""#).unwrap(),
        ClientAuthMode::Optional
    );
    assert!(
        serde_json::from_str::<ClientAuthMode>(r#""mutual_tls""#).is_err(),
        "unknown TLS client auth modes must not deserialize"
    );
    assert!(
        serde_json::from_str::<ClientAuthMode>(r#""re-quire""#).is_err(),
        "punctuated TLS client auth tokens must not be normalized into valid tokens"
    );

    assert_eq!(
        serde_json::to_string(&TokenBindingScheme::SecWebsocketKeySha256).unwrap(),
        r#""sec_websocket_key_sha256""#
    );
    assert_eq!(
        serde_json::from_str::<TokenBindingScheme>(r#""sec_websocket_key_sha256""#).unwrap(),
        TokenBindingScheme::SecWebsocketKeySha256
    );
    assert_eq!(
        serde_json::from_str::<TokenBindingScheme>(r#""SecWebsocketKeySha256""#).unwrap(),
        TokenBindingScheme::SecWebsocketKeySha256
    );
    assert!(
        serde_json::from_str::<TokenBindingScheme>(r#""unknown_scheme""#).is_err(),
        "unknown token binding schemes must not deserialize"
    );
    assert!(
        serde_json::from_str::<TokenBindingScheme>(r#""sec-websocket-key-sha256""#).is_err(),
        "punctuated token binding scheme tokens must not be normalized into valid tokens"
    );

    for (field, canonical, legacy) in [
        (
            DashboardHistoryField::ActiveRooms,
            "active_rooms",
            "ActiveRooms",
        ),
        (
            DashboardHistoryField::RoomsByGame,
            "rooms_by_game",
            "RoomsByGame",
        ),
        (
            DashboardHistoryField::PlayerPercentiles,
            "player_percentiles",
            "PlayerPercentiles",
        ),
        (
            DashboardHistoryField::GamePercentiles,
            "game_percentiles",
            "GamePercentiles",
        ),
        (
            DashboardHistoryField::ActiveConnections,
            "active_connections",
            "ActiveConnections",
        ),
        (
            DashboardHistoryField::RoomsCreated,
            "rooms_created",
            "RoomsCreated",
        ),
    ] {
        assert_eq!(
            serde_json::to_string(&field).unwrap(),
            format!(r#""{canonical}""#)
        );
        assert_eq!(
            serde_json::from_str::<DashboardHistoryField>(&format!(r#""{canonical}""#)).unwrap(),
            field
        );
        assert_eq!(
            serde_json::from_str::<DashboardHistoryField>(&format!(r#""{legacy}""#)).unwrap(),
            field
        );
    }
    assert!(
        serde_json::from_str::<DashboardHistoryField>(r#""latency_p99""#).is_err(),
        "unknown dashboard history fields must not deserialize"
    );
    assert!(
        serde_json::from_str::<DashboardHistoryField>(r#""active-rooms""#).is_err(),
        "hyphenated dashboard history fields must not be normalized into valid tokens"
    );
    assert!(
        serde_json::from_str::<DashboardHistoryField>(r#"" Active Rooms ""#).is_err(),
        "space-separated dashboard history fields must not be normalized into valid tokens"
    );
}

#[test]
fn test_config_reference_expected_defaults_match_code() {
    let config = serde_json::to_value(Config::default()).expect("default config serializes");
    let mut violations = Vec::new();

    for row in CONFIG_REFERENCE_ROWS {
        let Some(expected_default) = row.default else {
            continue;
        };
        let Some(value) = config_value_at_path(&config, row.path) else {
            violations.push(format!(
                "{} points to missing Config::default() path `{}`",
                row.env, row.path
            ));
            continue;
        };
        let actual_default = render_doc_default(value);
        if actual_default != expected_default {
            violations.push(format!(
                "{} ({}) expected default `{}` but Config::default() serializes `{}`",
                row.env, row.path, expected_default, actual_default
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "config reference defaults drifted from Config::default():\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_config_reference_tables_are_complete_and_current() {
    assert_config_reference_rows(
        &repo_path("docs/configuration.md"),
        CONFIG_REFERENCE_ROWS,
        true,
    );
    assert_config_reference_rows(&repo_path("README.md"), README_REQUIRED_CONFIG_ROWS, false);
}

#[test]
fn test_docs_use_canonical_config_tokens_and_env_prefixes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![root.join("README.md")];
    collect_markdown_files(&root.join("docs"), &mut files);
    let env_var_regex = Regex::new(r"\bSIGNAL_FISH_[A-Z0-9_]*\b").expect("valid env regex");
    let allowed_single_underscore_vars = [
        "SIGNAL_FISH_CONFIG_JSON",
        "SIGNAL_FISH_CONFIG_STDIN",
        "SIGNAL_FISH_CONFIG_PATH",
        "SIGNAL_FISH_PRODUCTION",
        "SIGNAL_FISH_HOOK_PROFILE",
        "SIGNAL_FISH_WARM_CARGO_CHECK",
    ];

    let mut violations = Vec::new();
    for file in files {
        let content = fs::read_to_string(&file)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", file.display()));
        for found in env_var_regex.find_iter(&content) {
            let env_var = found.as_str();
            if env_var.starts_with("SIGNAL_FISH__")
                || allowed_single_underscore_vars.contains(&env_var)
            {
                continue;
            }
            violations.push(format!(
                "{} contains stale single-underscore field override `{env_var}`; use `SIGNAL_FISH__...`",
                file.display()
            ));
        }

        for (start_line, block) in json_fence_blocks(&content) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&block) else {
                continue;
            };
            collect_config_enum_token_violations(
                &format!("{}:{start_line}", file.display()),
                &value,
                &mut violations,
            );
        }
    }

    let example = repo_path("config.example.json");
    let content = fs::read_to_string(&example)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", example.display()));
    let value = serde_json::from_str::<serde_json::Value>(&content)
        .expect("config.example.json must parse as JSON");
    collect_config_enum_token_violations(&example.display().to_string(), &value, &mut violations);

    assert!(
        violations.is_empty(),
        "stale config documentation found:\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_websocket_module_does_not_reexport_binary_wire_internals() {
    let mod_rs = fs::read_to_string(repo_path("src/websocket/mod.rs"))
        .expect("src/websocket/mod.rs must be readable");
    let sending_rs = fs::read_to_string(repo_path("src/websocket/sending.rs"))
        .expect("src/websocket/sending.rs must be readable");
    let public_frame_regex =
        Regex::new(r"(?m)^\s*pub(?:\([^)]*\))?\s+struct\s+BinaryGameDataFrame\b")
            .expect("valid public frame regex");
    let public_encoder_regex =
        Regex::new(r"(?m)^\s*pub(?:\s+|\(crate\)\s+)fn\s+encode_binary_game_data\b")
            .expect("valid public encoder regex");

    assert!(
        !mod_rs.contains("BinaryGameDataFrame"),
        "BinaryGameDataFrame must stay private to websocket::sending"
    );
    assert!(
        !mod_rs.contains("encode_binary_game_data"),
        "encode_binary_game_data must stay private to websocket::sending"
    );
    assert!(
        !mod_rs.contains("pub mod sending"),
        "websocket::sending must not be exported as a public module"
    );
    assert!(
        !mod_rs.contains("pub use sending::"),
        "websocket::sending internals must not be re-exported"
    );
    assert!(
        !public_frame_regex.is_match(&sending_rs),
        "BinaryGameDataFrame must stay private to websocket::sending"
    );
    assert!(
        !public_encoder_regex.is_match(&sending_rs),
        "encode_binary_game_data must not become public crate API"
    );
}

#[test]
fn test_config_region_id_and_room_code_prefix() {
    let json = r#"{
        "server": {
            "region_id": "eu-west-2",
            "room_code_prefix": "EU"
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.server.region_id, "eu-west-2");
    assert_eq!(config.server.room_code_prefix, Some("EU".to_string()));
}

#[test]
fn test_config_heartbeat_throttle() {
    let json = r#"{
        "server": {
            "heartbeat_throttle_secs": 15
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert_eq!(config.server.heartbeat_throttle_secs, 15);
}

#[test]
fn test_config_websocket_section() {
    let json = r#"{
        "websocket": {
            "enable_batching": true,
            "batch_size": 64,
            "batch_interval_ms": 50,
            "auth_timeout_secs": 15
        }
    }"#;

    let config: Config = serde_json::from_str(json).expect("parse should succeed");

    assert!(config.websocket.enable_batching);
    assert_eq!(config.websocket.batch_size, 64);
    assert_eq!(config.websocket.batch_interval_ms, 50);
    assert_eq!(config.websocket.auth_timeout_secs, 15);
}

// ===========================================================================
// Health endpoint tests
// ===========================================================================

#[tokio::test]
async fn test_health_endpoint_returns_ok() {
    let server = create_test_server().await;
    let app = create_router("*").with_state(server);

    let test_server = axum_test::TestServer::new(app);
    let response = test_server.get("/health").await;

    response.assert_status_ok();
    response.assert_text("OK");
}

// ===========================================================================
// Metrics endpoint tests
// ===========================================================================

#[tokio::test]
async fn test_metrics_endpoint_no_auth_required() {
    // Create server with metrics auth disabled
    let mut config = test_server_config();
    config.require_metrics_auth = false;

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app);

    let response = test_server.get("/metrics").await;
    response.assert_status_ok();

    // Should return JSON with expected structure
    let json: serde_json::Value = response.json();
    assert!(
        json.get("timeRange").is_some(),
        "metrics should contain timeRange"
    );
    assert!(
        json.get("serverMetrics").is_some(),
        "metrics should contain serverMetrics"
    );
}

#[tokio::test]
async fn test_metrics_endpoint_requires_auth_when_configured() {
    // Create server with metrics auth enabled
    let mut config = test_server_config();
    config.require_metrics_auth = true;
    config.metrics_auth_token = Some("test-metrics-token".to_string());

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app);

    // Without auth header, should fail
    let response = test_server.get("/metrics").await;
    response.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_metrics_endpoint_accepts_valid_bearer_token() {
    let mut config = test_server_config();
    config.require_metrics_auth = true;
    config.metrics_auth_token = Some("valid-token".to_string());

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app);

    let response = test_server
        .get("/metrics")
        .add_header(
            axum::http::header::AUTHORIZATION,
            "Bearer valid-token"
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
        )
        .await;

    response.assert_status_ok();
}

#[tokio::test]
async fn test_metrics_endpoint_rejects_invalid_bearer_token() {
    let mut config = test_server_config();
    config.require_metrics_auth = true;
    config.metrics_auth_token = Some("correct-token".to_string());

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app);

    let response = test_server
        .get("/metrics")
        .add_header(
            axum::http::header::AUTHORIZATION,
            "Bearer wrong-token"
                .parse::<axum::http::HeaderValue>()
                .unwrap(),
        )
        .await;

    response.assert_status(axum::http::StatusCode::UNAUTHORIZED);
}

// ===========================================================================
// Prometheus metrics endpoint tests
// ===========================================================================

#[tokio::test]
async fn test_prometheus_metrics_endpoint_returns_text() {
    let mut config = test_server_config();
    config.require_metrics_auth = false;

    let server = test_helpers::create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    let app = create_router("*").with_state(server);
    let test_server = axum_test::TestServer::new(app);

    let response = test_server.get("/metrics/prom").await;
    response.assert_status_ok();

    // Prometheus format should contain standard HELP and TYPE annotations
    let body = response.text();
    assert!(body.contains("# HELP"), "Should contain HELP comment lines");
    assert!(body.contains("# TYPE"), "Should contain TYPE annotations");
}

// ===========================================================================
// Router structure tests
// ===========================================================================

#[tokio::test]
async fn test_websocket_route_exists() {
    let server = create_test_server().await;
    let app = create_router("*").with_state(server);

    let test_server = axum_test::TestServer::new(app);

    // GET /ws without WebSocket upgrade should not return 404
    // (It will return 400 or similar since there's no upgrade header, but NOT 404)
    let response = test_server.get("/ws").await;
    let status = response.status_code();
    assert_ne!(
        status,
        axum::http::StatusCode::NOT_FOUND,
        "/ws route should exist"
    );
}

#[tokio::test]
async fn test_production_style_router_has_top_level_v3_only() {
    let server = create_test_server().await;
    let enhanced_router = create_router("*").with_state(server.clone());
    let app = axum::Router::new()
        .nest("/v2", enhanced_router)
        .route("/v3/ws", get(websocket_handler_v3))
        .with_state(server);

    let test_server = axum_test::TestServer::new(app);

    let v2_response = test_server.get("/v2/ws").await;
    assert_ne!(
        v2_response.status_code(),
        axum::http::StatusCode::NOT_FOUND,
        "/v2/ws route should exist"
    );

    let v3_response = test_server.get("/v3/ws").await;
    assert_ne!(
        v3_response.status_code(),
        axum::http::StatusCode::NOT_FOUND,
        "/v3/ws route should exist at the top level"
    );

    let nested_v3_response = test_server.get("/v2/v3/ws").await;
    nested_v3_response.assert_status(axum::http::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_standalone_router_exposes_v3_alias() {
    let server = create_test_server().await;
    let app = create_standalone_router("*").with_state(server);

    let test_server = axum_test::TestServer::new(app);
    let response = test_server.get("/v3/ws").await;
    assert_ne!(
        response.status_code(),
        axum::http::StatusCode::NOT_FOUND,
        "standalone router should expose /v3/ws"
    );
}

#[tokio::test]
async fn test_unknown_route_returns_404() {
    let server = create_test_server().await;
    let app = create_router("*").with_state(server);

    let test_server = axum_test::TestServer::new(app);
    let response = test_server.get("/nonexistent").await;
    response.assert_status(axum::http::StatusCode::NOT_FOUND);
}

// ===========================================================================
// CORS configuration tests
// ===========================================================================

#[tokio::test]
async fn test_permissive_cors_with_wildcard() {
    let server = create_test_server().await;
    let app = create_router("*").with_state(server);

    let test_server = axum_test::TestServer::new(app);
    let response = test_server.get("/health").await;
    response.assert_status_ok();
}

#[tokio::test]
async fn test_specific_cors_origins() {
    let server = create_test_server().await;
    let app = create_router("http://localhost:3000,http://example.com").with_state(server);

    let test_server = axum_test::TestServer::new(app);
    let response = test_server.get("/health").await;
    response.assert_status_ok();
}

// ===========================================================================
// Config validation tests
// ===========================================================================

use signal_fish_server::config::validate_config_security;

/// A config-validation test scenario: (name, config_modifier, expected_ok).
type ValidationScenario = (&'static str, Box<dyn Fn(&mut Config)>, bool);

/// The default Config has `require_metrics_auth = true` but no
/// `metrics_auth_token`, so validation MUST fail.  This is the exact
/// scenario that caused the Docker CI startup failure.
#[test]
fn test_default_config_fails_validation() {
    let config = Config::default();

    // Preconditions: confirm the defaults that cause the failure
    assert!(
        config.security.require_metrics_auth,
        "default_require_auth() should be true"
    );
    assert!(
        config.security.metrics_auth_token.is_none(),
        "default metrics_auth_token should be None"
    );

    let result = validate_config_security(&config);
    assert!(
        result.is_err(),
        "default config should fail validation because metrics auth is required but no token is set"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Metrics authentication is enabled but no credentials are configured"),
        "error message should mention missing credentials, got: {err_msg}"
    );
}

/// Data-driven test covering a matrix of auth configuration scenarios.
#[test]
fn test_config_validation_scenarios() {
    // (name, config_modifier, expected_ok)
    let scenarios: Vec<ValidationScenario> = vec![
        (
            "default config → fails (require_metrics_auth=true, no token)",
            Box::new(|_c: &mut Config| {
                // leave defaults untouched
            }),
            false,
        ),
        (
            "auth disabled → passes",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
            }),
            true,
        ),
        (
            "auth enabled with valid long token → passes",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = true;
                c.security.metrics_auth_token =
                    Some("a-strong-token-that-is-at-least-32-chars!!".to_string());
            }),
            true,
        ),
        (
            "auth enabled with short token → passes (warning only)",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = true;
                c.security.metrics_auth_token = Some("short".to_string());
            }),
            true,
        ),
        (
            "auth enabled with empty token → fails",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = true;
                c.security.metrics_auth_token = Some(String::new());
            }),
            false,
        ),
        (
            "auth enabled with whitespace-only token → fails",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = true;
                c.security.metrics_auth_token = Some(" \t\n".to_string());
            }),
            false,
        ),
        (
            "both auth fields disabled → passes (Docker/CI scenario)",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.security.require_websocket_auth = false;
            }),
            true,
        ),
        (
            "metrics auth enabled, websocket auth disabled → fails without token",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = true;
                c.security.require_websocket_auth = false;
                c.security.metrics_auth_token = None;
            }),
            false,
        ),
        (
            "metrics auth disabled, websocket auth enabled → passes",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.security.require_websocket_auth = true;
            }),
            true,
        ),
        (
            "authorized app with whitespace-only app_id → fails",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.security.authorized_apps = vec![AppAuthEntry {
                    app_id: " ".to_string(),
                    app_secret: "secret".to_string(),
                    app_name: "Game".to_string(),
                    max_rooms: None,
                    max_players_per_room: None,
                    rate_limit_per_minute: None,
                }];
            }),
            false,
        ),
        (
            "max_signal_bytes of 0 → fails",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.security.max_signal_bytes = 0;
            }),
            false,
        ),
        (
            "max_signal_bytes above max_message_size → fails (dead config)",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.security.max_signal_bytes = c.security.max_message_size + 1;
            }),
            false,
        ),
        (
            "max_signal_bytes equal to max_message_size → passes (boundary)",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.security.max_signal_bytes = c.security.max_message_size;
            }),
            true,
        ),
        (
            "idle timeout of 0 (disabled) → passes",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.websocket.idle_timeout_secs = 0;
            }),
            true,
        ),
        (
            "aggressive 1s idle timeout → passes (any positive value is valid)",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.websocket.idle_timeout_secs = 1;
            }),
            true,
        ),
        (
            "very large idle timeout → passes",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.websocket.idle_timeout_secs = 86_400;
            }),
            true,
        ),
    ];

    for (name, modifier, expected_ok) in &scenarios {
        let mut config = Config::default();
        modifier(&mut config);

        let result = validate_config_security(&config);
        assert_eq!(
            result.is_ok(),
            *expected_ok,
            "scenario \"{name}\": expected {}, got {:?}",
            if *expected_ok { "Ok" } else { "Err" },
            result,
        );
    }
}

/// Regression test: the Docker container disables both auth flags.
/// This configuration MUST pass validation.
#[test]
fn test_docker_default_config_passes_validation() {
    let mut config = Config::default();
    config.security.require_metrics_auth = false;
    config.security.require_websocket_auth = false;
    config.security.metrics_auth_token = None;

    let result = validate_config_security(&config);
    assert!(
        result.is_ok(),
        "Docker-style config (auth disabled) must pass validation, but got: {:?}",
        result.unwrap_err(),
    );
}

/// Data-driven test for TLS validation edge cases.
#[test]
fn test_validate_config_security_tls_validation() {
    let scenarios: Vec<ValidationScenario> = vec![
        (
            "TLS enabled but no cert path → fails",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.security.transport.tls.enabled = true;
                c.security.transport.tls.certificate_path = None;
                c.security.transport.tls.private_key_path = Some("/tmp/key.pem".to_string());
            }),
            false,
        ),
        (
            "TLS enabled but no key path → fails",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.security.transport.tls.enabled = true;
                c.security.transport.tls.certificate_path = Some("/tmp/cert.pem".to_string());
                c.security.transport.tls.private_key_path = None;
            }),
            false,
        ),
        (
            "TLS disabled → passes regardless of cert/key",
            Box::new(|c: &mut Config| {
                c.security.require_metrics_auth = false;
                c.security.transport.tls.enabled = false;
                c.security.transport.tls.certificate_path = None;
                c.security.transport.tls.private_key_path = None;
            }),
            true,
        ),
    ];

    for (name, modifier, expected_ok) in &scenarios {
        let mut config = Config::default();
        modifier(&mut config);

        let result = validate_config_security(&config);
        assert_eq!(
            result.is_ok(),
            *expected_ok,
            "TLS scenario \"{name}\": expected {}, got {:?}",
            if *expected_ok { "Ok" } else { "Err" },
            result,
        );
    }
}

/// `--print-config` must never leak credential material: every *set* secret in
/// the printed JSON is replaced with the [`REDACTED_SECRET`] marker. This
/// drives the REAL compiled binary (`CARGO_BIN_EXE_signal-fish-server`)
/// end-to-end through `Config::redacted_for_display`, pinning the operator
/// contract the unit tests in `src/config/types.rs` only cover in-process.
///
/// [`REDACTED_SECRET`]: signal_fish_server::config::REDACTED_SECRET
#[test]
fn test_print_config_redacts_secrets_end_to_end() {
    use signal_fish_server::config::REDACTED_SECRET;

    const SENTINEL_SECRET: &str = "super-secret-turn-key-do-not-print";

    // Per-test temp config file + working directory (no stray repo
    // `config.json` can interfere), mirroring `tests/v3_multiprocess_e2e.rs`.
    let workdir = tempfile::tempdir().expect("create temp workdir");
    let config_path = workdir.path().join("config.json");
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "turn": {
                "enabled": true,
                "mode": "static_secret",
                "static_auth_secret": SENTINEL_SECRET,
                "urls": ["turn:turn.example.com:3478"]
            }
        }))
        .expect("serialize test config"),
    )
    .expect("write test config");

    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-server"));
    command
        .arg("--print-config")
        .current_dir(workdir.path())
        .stdin(std::process::Stdio::null());
    // Scrub every inherited SIGNAL_FISH* variable: env overrides are applied
    // last by the loader and would silently override the temp config.
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(|key| key.starts_with("SIGNAL_FISH"))
        {
            command.env_remove(&key);
        }
    }
    command.env("SIGNAL_FISH_CONFIG_PATH", &config_path);

    let output = command
        .output()
        .expect("run signal-fish-server --print-config");
    assert!(
        output.status.success(),
        "--print-config exited with {:?}; stderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let printed: serde_json::Value =
        serde_json::from_str(&stdout).expect("--print-config stdout is a single JSON document");

    // The set secret is replaced by the redaction marker...
    assert_eq!(
        printed["turn"]["static_auth_secret"],
        serde_json::Value::String(REDACTED_SECRET.to_string()),
        "expected turn.static_auth_secret to print as the redaction marker"
    );
    // ...and the raw credential appears nowhere in the output.
    assert!(
        !stdout.contains(SENTINEL_SECRET),
        "raw secret leaked into --print-config output:\n{stdout}"
    );
    // Non-secret TURN fields stay visible so operators can audit the config.
    assert_eq!(
        printed["turn"]["urls"][0], "turn:turn.example.com:3478",
        "non-secret turn.urls should print unredacted"
    );
}
