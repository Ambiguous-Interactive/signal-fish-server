//! Configuration loading and environment parsing.

use super::validation::validate_config_security;
use super::Config;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Load configuration with the following precedence (highest first):
/// 1) `SIGNAL_FISH_CONFIG_JSON` env var containing raw JSON
/// 2) If `SIGNAL_FISH_CONFIG_STDIN=true/1`, read JSON from stdin
/// 3) File pointed by `SIGNAL_FISH_CONFIG_PATH` env var
/// 4) config.json in current working directory
/// 5) config.json next to the executable (application directory)
/// 6) Defaults compiled into the binary
///
/// Additionally, individual fields can be overridden by environment variables
/// with the `SIGNAL_FISH__` prefix, e.g. `SIGNAL_FISH__PORT=8080` or
/// `SIGNAL_FISH__LOGGING__LEVEL=debug`.
/// Any errors while reading/parsing are printed to stderr and defaults are used.
///
/// **Note:** Validation errors from [`validate_config_security`] are logged to stderr but are
/// *not* propagated — `load()` always returns a `Config`. Callers who need hard failure
/// should call [`validate_config_security()`](super::validation::validate_config_security)
/// on the returned config and handle the error themselves.
#[must_use]
pub fn load() -> Config {
    use std::env;
    use std::io::Read;
    use std::path::PathBuf;

    let defaults = Config::default();
    let mut merged =
        serde_json::to_value(&defaults).unwrap_or_else(|_| Value::Object(serde_json::Map::new()));

    let inline_source = env::var("SIGNAL_FISH_CONFIG_JSON")
        .ok()
        .and_then(|json| parse_json_document(&json, "SIGNAL_FISH_CONFIG_JSON"));
    let stdin_source = if let Ok(val) = env::var("SIGNAL_FISH_CONFIG_STDIN") {
        if env_var_truthy(&val) {
            let mut buf = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                eprintln!("Failed to read config from stdin: {e}");
                None
            } else {
                parse_json_document(&buf, "stdin")
            }
        } else {
            None
        }
    } else {
        None
    };

    let explicit_source = env::var("SIGNAL_FISH_CONFIG_PATH")
        .ok()
        .and_then(|path| read_file_source(&PathBuf::from(path)));
    let cwd_source = read_file_source(&PathBuf::from("config.json"));
    let executable_source = env::current_exe().ok().and_then(|exe_path| {
        exe_path.parent().and_then(|exe_dir| {
            let mut path = exe_dir.to_path_buf();
            path.push("config.json");
            read_file_source(&path)
        })
    });

    // `merge_values` gives the later value precedence, so apply the documented
    // JSON sources from lowest to highest priority.
    merge_sources_low_to_high(
        &mut merged,
        [
            executable_source,
            cwd_source,
            explicit_source,
            stdin_source,
            inline_source,
        ],
    );

    // Environment overrides with prefix SIGNAL_FISH__ and nested separator __
    apply_env_overrides(&mut merged);

    let config = match serde_json::from_value::<Config>(merged) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Failed to deserialize config; using defaults: {e}");
            defaults
        }
    };

    // Security validation for sensitive fields — intentional warn-only behavior;
    // main.rs calls validate_config_security() again and propagates errors properly.
    if let Err(e) = validate_config_security(&config) {
        eprintln!("Configuration validation error: {e}");
    }

    config
}

fn parse_json_document(raw: &str, label: &str) -> Option<Value> {
    if raw.trim().is_empty() {
        return None;
    }

    match serde_json::from_str(raw) {
        Ok(mut value) => match normalize_legacy_app_access_config(&mut value, label) {
            Ok(()) => Some(value),
            Err(error) => {
                eprintln!("Invalid config from {label}: {error}");
                Some(fail_closed_app_access_source())
            }
        },
        Err(err) => {
            eprintln!("Failed to parse config from {label}: {err}");
            None
        }
    }
}

fn read_file_source(path: &Path) -> Option<Value> {
    if path.as_os_str().is_empty() || !path.exists() {
        return None;
    }

    match fs::read_to_string(path) {
        Ok(contents) => parse_json_document(&contents, &format!("file {}", path.display())),
        Err(err) => {
            eprintln!("Failed to read config from {}: {}", path.display(), err);
            None
        }
    }
}

fn merge_sources_low_to_high<I>(target: &mut Value, sources: I)
where
    I: IntoIterator<Item = Option<Value>>,
{
    for source in sources.into_iter().flatten() {
        merge_values(target, source);
    }
}

fn fail_closed_app_access_source() -> Value {
    serde_json::json!({
        "security": {
            "enforce_app_id_allowlist": true,
            "allowed_apps": []
        }
    })
}

fn merge_values(target: &mut Value, source: Value) {
    match (target, source) {
        (Value::Object(target_map), Value::Object(source_map)) => {
            for (key, value) in source_map {
                match target_map.get_mut(&key) {
                    Some(existing) => merge_values(existing, value),
                    None => {
                        target_map.insert(key, value);
                    }
                }
            }
        }
        (target_slot, source_value) => {
            *target_slot = source_value;
        }
    }
}

fn apply_env_overrides(root: &mut Value) {
    apply_env_overrides_from_iter(root, std::env::vars());
}

fn apply_env_overrides_from_iter<I, K, V>(root: &mut Value, vars: I)
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut overrides: BTreeMap<Vec<String>, (bool, Value)> = BTreeMap::new();

    for (key, raw_value) in vars {
        let Some(stripped) = key.as_ref().strip_prefix("SIGNAL_FISH__") else {
            continue;
        };

        let mut segments: Vec<String> = stripped
            .split("__")
            .filter(|segment| !segment.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();

        if segments.is_empty() {
            continue;
        }

        let legacy_name = normalize_legacy_app_access_env_path(&mut segments);
        let mut value = parse_env_value(&segments, raw_value.as_ref());
        if segments.as_slice() == ["security", "allowed_apps"] {
            discard_legacy_app_secrets(&mut value, key.as_ref());
        }

        match overrides.entry(segments) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((legacy_name, value));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let (existing_legacy, _) = entry.get();
                eprintln!(
                    "Conflicting canonical and legacy app-access environment overrides; \
                     the canonical name takes precedence"
                );
                if *existing_legacy && !legacy_name {
                    entry.insert((false, value));
                }
            }
        }
    }

    for (segments, (_, value)) in overrides {
        set_nested_value(root, &segments, value);
    }
}

fn normalize_legacy_app_access_config(value: &mut Value, label: &str) -> Result<(), String> {
    let Some(security) = value.get_mut("security").and_then(Value::as_object_mut) else {
        return Ok(());
    };

    normalize_legacy_key(
        security,
        "require_websocket_auth",
        "enforce_app_id_allowlist",
        label,
    )?;
    normalize_legacy_key(security, "authorized_apps", "allowed_apps", label)?;

    if let Some(apps) = security.get_mut("allowed_apps") {
        discard_legacy_app_secrets(apps, label);
    }
    Ok(())
}

fn normalize_legacy_key(
    object: &mut serde_json::Map<String, Value>,
    legacy: &str,
    canonical: &str,
    label: &str,
) -> Result<(), String> {
    let Some(value) = object.remove(legacy) else {
        return Ok(());
    };
    if object.contains_key(canonical) {
        return Err(format!(
            "{label} contains both security.{canonical} and deprecated security.{legacy}"
        ));
    }
    eprintln!("Deprecated config key security.{legacy} in {label}; use security.{canonical}");
    object.insert(canonical.to_string(), value);
    Ok(())
}

fn normalize_legacy_app_access_env_path(segments: &mut [String]) -> bool {
    if segments.len() != 2 || segments.first().map(String::as_str) != Some("security") {
        return false;
    }
    let Some(setting) = segments.get_mut(1) else {
        return false;
    };
    match setting.as_str() {
        "require_websocket_auth" => {
            *setting = "enforce_app_id_allowlist".to_string();
            true
        }
        "authorized_apps" => {
            *setting = "allowed_apps".to_string();
            true
        }
        _ => false,
    }
}

fn discard_legacy_app_secrets(value: &mut Value, label: &str) {
    let Some(apps) = value.as_array_mut() else {
        return;
    };
    let mut discarded = false;
    for app in apps {
        if let Some(app) = app.as_object_mut() {
            discarded |= app.remove("app_secret").is_some();
        }
    }
    if discarded {
        eprintln!(
            "Deprecated app_secret in {label} was ignored; clients send only public app_id values"
        );
    }
}

fn env_var_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes"
    )
}

fn parse_env_value(segments: &[String], raw: &str) -> Value {
    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return value;
    }

    if is_legacy_comma_array_override(segments) && trimmed.contains(',') {
        let items = trimmed
            .split(',')
            .map(|segment| parse_scalar(segment.trim()))
            .collect::<Vec<_>>();
        return Value::Array(items);
    }

    parse_scalar(trimmed)
}

fn is_legacy_comma_array_override(segments: &[String]) -> bool {
    const LEGACY_COMMA_ARRAY_PATHS: &[&[&str]] = &[
        &["protocol", "player_name_validation", "allowed_symbols"],
        &["metrics", "dashboard_cache_history_fields"],
    ];

    LEGACY_COMMA_ARRAY_PATHS
        .iter()
        .any(|path| segments.iter().map(String::as_str).eq(path.iter().copied()))
}

fn parse_scalar(raw: &str) -> Value {
    if raw.is_empty() {
        return Value::String(String::new());
    }

    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn set_nested_value(target: &mut Value, segments: &[String], value: Value) {
    if segments.is_empty() {
        *target = value;
        return;
    }

    let Some((first, rest)) = segments.split_first() else {
        return;
    };

    if rest.is_empty() {
        let map = ensure_object(target);
        map.insert(first.clone(), value);
        return;
    }

    let map = ensure_object(target);
    let entry = map
        .entry(first.clone())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    set_nested_value(entry, rest, value);
}

fn ensure_object(value: &mut Value) -> &mut serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => {
            *other = Value::Object(serde_json::Map::new());
            ensure_object(other)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::defaults::DashboardHistoryField;
    use crate::config::logging::LogFormat;

    fn config_with_env(vars: &[(&str, &str)]) -> Config {
        let mut merged =
            serde_json::to_value(Config::default()).expect("default config serializes");
        apply_env_overrides_from_iter(&mut merged, vars.iter().copied());
        serde_json::from_value(merged).expect("env overrides deserialize as Config")
    }

    #[test]
    fn field_overrides_require_canonical_double_underscore_prefix() {
        let config = config_with_env(&[("SIGNAL_FISH_LOGGING__FORMAT", "text")]);
        assert_eq!(
            config.logging.format,
            LogFormat::Json,
            "single-underscore SIGNAL_FISH_LOGGING__FORMAT must not override config"
        );

        let config = config_with_env(&[("SIGNAL_FISH__LOGGING__FORMAT", "text")]);
        assert_eq!(
            config.logging.format,
            LogFormat::Text,
            "canonical SIGNAL_FISH__LOGGING__FORMAT must override config"
        );
    }

    #[test]
    fn removed_auth_maintenance_environment_keys_are_tolerated_and_ignored() {
        let config = config_with_env(&[
            (
                "SIGNAL_FISH__AUTH__RATE_LIMIT_CACHE_CLEANUP_INTERVAL_SECS",
                "1",
            ),
            ("SIGNAL_FISH__AUTH__RATE_LIMIT_CACHE_RETENTION_SECS", "2"),
            ("SIGNAL_FISH__AUTH__RATE_LIMIT_CACHE_ALERT_ROWS", "3"),
        ]);

        let serialized = serde_json::to_value(config).expect("config serializes");
        assert!(serialized.get("auth").is_none());
    }

    #[test]
    fn env_overrides_preserve_comma_strings_and_parse_json_values() {
        let config = config_with_env(&[
            (
                "SIGNAL_FISH__SECURITY__CORS_ORIGINS",
                "https://game.example,https://beta.example",
            ),
            (
                "SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOWED_SYMBOLS",
                r##"["#","@"]"##,
            ),
            (
                "SIGNAL_FISH__METRICS__DASHBOARD_CACHE_HISTORY_FIELDS",
                r#"["active_rooms","rooms_created"]"#,
            ),
            (
                "SIGNAL_FISH__SECURITY__ALLOWED_APPS",
                r#"[
                    {
                        "app_id": "game-one",
                        "app_name": "Game One"
                    },
                    {
                        "app_id": "game-two",
                        "app_name": "Game Two"
                    }
                ]"#,
            ),
        ]);

        assert_eq!(
            config.security.cors_origins,
            "https://game.example,https://beta.example"
        );
        assert_eq!(
            config.protocol.player_name_validation.allowed_symbols,
            vec!['#', '@']
        );
        assert_eq!(
            config.metrics.dashboard_cache_history_fields,
            vec![
                DashboardHistoryField::ActiveRooms,
                DashboardHistoryField::RoomsCreated
            ]
        );
        assert_eq!(config.security.allowed_apps.len(), 2);
        assert_eq!(config.security.allowed_apps[0].app_id, "game-one");
        assert_eq!(config.security.allowed_apps[1].app_name, "Game Two");
    }

    #[test]
    fn legacy_app_access_env_keys_override_canonical_defaults_without_retaining_secrets() {
        let config = config_with_env(&[
            ("SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH", "false"),
            (
                "SIGNAL_FISH__SECURITY__AUTHORIZED_APPS",
                r#"[{
                    "app_id": "legacy-game",
                    "app_secret": "must-not-be-retained",
                    "app_name": "Legacy Game"
                }]"#,
            ),
        ]);

        assert!(!config.security.enforce_app_id_allowlist);
        assert_eq!(config.security.allowed_apps.len(), 1);
        assert_eq!(config.security.allowed_apps[0].app_id, "legacy-game");

        let serialized = serde_json::to_string(&config).expect("config serializes");
        assert!(!serialized.contains("must-not-be-retained"));
        assert!(!serialized.contains("app_secret"));
    }

    #[test]
    fn canonical_app_access_env_keys_win_over_legacy_keys_in_either_order() {
        for vars in [
            [
                ("SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH", "true"),
                ("SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST", "false"),
            ],
            [
                ("SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST", "false"),
                ("SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH", "true"),
            ],
        ] {
            let config = config_with_env(&vars);
            assert!(!config.security.enforce_app_id_allowlist);
        }
    }

    #[test]
    fn canonical_app_list_env_wins_over_legacy_list_in_either_order() {
        const CANONICAL: (&str, &str) = (
            "SIGNAL_FISH__SECURITY__ALLOWED_APPS",
            r#"[{"app_id":"canonical","app_name":"Canonical"}]"#,
        );
        const LEGACY: (&str, &str) = (
            "SIGNAL_FISH__SECURITY__AUTHORIZED_APPS",
            r#"[{"app_id":"legacy","app_secret":"discard-me","app_name":"Legacy"}]"#,
        );

        for vars in [[LEGACY, CANONICAL], [CANONICAL, LEGACY]] {
            let config = config_with_env(&vars);
            assert_eq!(config.security.allowed_apps.len(), 1);
            assert_eq!(config.security.allowed_apps[0].app_id, "canonical");
            let serialized = serde_json::to_string(&config).expect("config serializes");
            assert!(!serialized.contains("discard-me"));
            assert!(!serialized.contains("app_secret"));
        }
    }

    #[test]
    fn legacy_json_source_normalizes_before_merging_and_discards_secrets() {
        let mut merged =
            serde_json::to_value(Config::default()).expect("default config serializes");
        let source = parse_json_document(
            r#"{
                "security": {
                    "require_websocket_auth": false,
                    "authorized_apps": [{
                        "app_id": "legacy-game",
                        "app_secret": "must-not-be-retained",
                        "app_name": "Legacy Game"
                    }]
                }
            }"#,
            "test source",
        )
        .expect("legacy source normalizes");
        merge_values(&mut merged, source);

        let config: Config = serde_json::from_value(merged).expect("merged config deserializes");
        assert!(!config.security.enforce_app_id_allowlist);
        assert_eq!(config.security.allowed_apps[0].app_id, "legacy-game");
        let serialized = serde_json::to_string(&config).expect("config serializes");
        assert!(!serialized.contains("must-not-be-retained"));
        assert!(!serialized.contains("app_secret"));
    }

    #[test]
    fn rejected_mixed_alias_source_overrides_lower_sources_with_fail_closed_policy() {
        let mut merged = serde_json::json!({
            "security": {
                "enforce_app_id_allowlist": false,
                "allowed_apps": [{"app_id": "lower", "app_name": "Lower"}]
            }
        });
        let rejected = parse_json_document(
            r#"{
                "security": {
                    "enforce_app_id_allowlist": false,
                    "require_websocket_auth": true
                }
            }"#,
            "test source",
        )
        .expect("a rejected security source produces a fail-closed override");
        merge_sources_low_to_high(&mut merged, [Some(rejected)]);

        let config: Config = serde_json::from_value(merged).expect("config deserializes");
        assert!(config.security.enforce_app_id_allowlist);
        assert!(config.security.allowed_apps.is_empty());
    }

    #[test]
    fn later_json_sources_have_higher_precedence_after_legacy_normalization() {
        let defaults = serde_json::to_value(Config::default()).expect("defaults serialize");
        let executable = parse_json_document(
            r#"{"security":{"enforce_app_id_allowlist":false}}"#,
            "executable config",
        );
        let inline = parse_json_document(
            r#"{
                "security": {
                    "require_websocket_auth": true,
                    "authorized_apps": [{
                        "app_id": "inline",
                        "app_secret": "discard-me",
                        "app_name": "Inline"
                    }]
                }
            }"#,
            "SIGNAL_FISH_CONFIG_JSON",
        );
        let mut merged = defaults;
        merge_sources_low_to_high(&mut merged, [executable, inline]);

        let config: Config = serde_json::from_value(merged).expect("config deserializes");
        assert!(config.security.enforce_app_id_allowlist);
        assert_eq!(config.security.allowed_apps[0].app_id, "inline");
        let serialized = serde_json::to_string(&config).expect("config serializes");
        assert!(!serialized.contains("discard-me"));
    }

    #[test]
    fn env_overrides_keep_legacy_comma_lists_type_scoped() {
        let config = config_with_env(&[
            (
                "SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOWED_SYMBOLS",
                "#,@",
            ),
            (
                "SIGNAL_FISH__METRICS__DASHBOARD_CACHE_HISTORY_FIELDS",
                "active_rooms,rooms_created",
            ),
        ]);

        assert_eq!(
            config.protocol.player_name_validation.allowed_symbols,
            vec!['#', '@']
        );
        assert_eq!(
            config.metrics.dashboard_cache_history_fields,
            vec![
                DashboardHistoryField::ActiveRooms,
                DashboardHistoryField::RoomsCreated
            ]
        );
    }
}
