//! Configuration loading and environment parsing.

use super::validation::validate_config_security;
use super::Config;
use serde_json::Value;
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
/// Additionally, individual fields can be overridden by environment variables with prefix SIGNAL_FISH
/// using "__" as a nested separator, e.g. `SIGNAL_FISH__PORT=8080` or `SIGNAL_FISH__LOGGING__LEVEL=debug`.
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

    // 1) Inline JSON via env var
    if let Ok(json) = env::var("SIGNAL_FISH_CONFIG_JSON") {
        if let Some(value) = parse_json_document(&json, "SIGNAL_FISH_CONFIG_JSON") {
            merge_values(&mut merged, value);
        }
    }

    // 2) JSON from STDIN (opt-in)
    if let Ok(val) = env::var("SIGNAL_FISH_CONFIG_STDIN") {
        if env_var_truthy(&val) {
            let mut buf = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                eprintln!("Failed to read config from stdin: {e}");
            } else if let Some(value) = parse_json_document(&buf, "stdin") {
                merge_values(&mut merged, value);
            }
        }
    }

    // 3) Explicit path via env var
    if let Ok(path) = env::var("SIGNAL_FISH_CONFIG_PATH") {
        let path = PathBuf::from(path);
        merge_file_source(&mut merged, &path);
    }

    // 4) config.json in CWD
    merge_file_source(&mut merged, &PathBuf::from("config.json"));

    // 5) config.json next to executable
    if let Ok(exe_path) = env::current_exe() {
        if let Some(mut exe_dir) = exe_path.parent().map(std::path::Path::to_path_buf) {
            exe_dir.push("config.json");
            merge_file_source(&mut merged, &exe_dir);
        }
    }

    // Environment overrides with prefix SIGNAL_FISH and nested separator __
    apply_env_overrides(&mut merged);

    let config = match serde_json::from_value::<Config>(merged) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Failed to deserialize config; using defaults: {e}");
            defaults
        }
    };

    // Security validation for sensitive fields — intentional warn-only behaviour;
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
        Ok(value) => Some(value),
        Err(err) => {
            eprintln!("Failed to parse config from {label}: {err}");
            None
        }
    }
}

fn merge_file_source(target: &mut Value, path: &Path) {
    if path.as_os_str().is_empty() || !path.exists() {
        return;
    }

    match fs::read_to_string(path) {
        Ok(contents) => {
            if let Some(value) = parse_json_document(&contents, &format!("file {}", path.display()))
            {
                merge_values(target, value);
            }
        }
        Err(err) => {
            eprintln!("Failed to read config from {}: {}", path.display(), err);
        }
    }
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
    for (key, raw_value) in std::env::vars() {
        let Some(stripped) = key.strip_prefix("SIGNAL_FISH__") else {
            continue;
        };

        let segments: Vec<String> = stripped
            .split("__")
            .filter(|segment| !segment.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();

        if segments.is_empty() {
            continue;
        }

        let value = parse_env_value(&raw_value);
        set_nested_value(root, &segments, value);
    }
}

fn env_var_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes"
    )
}

fn parse_env_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.contains(',') {
        let items = trimmed
            .split(',')
            .map(|segment| parse_scalar(segment.trim()))
            .collect::<Vec<_>>();
        return Value::Array(items);
    }

    parse_scalar(trimmed)
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

    if segments.len() == 1 {
        let map = ensure_object(target);
        // SAFETY: Length is checked to be exactly 1 on the line above.
        #[allow(clippy::indexing_slicing)]
        map.insert(segments[0].clone(), value);
        return;
    }

    let map = ensure_object(target);
    // SAFETY: segments.len() > 1 (len 0 and len 1 are handled above), so
    // index 0 and the [1..] slice are both in bounds.
    #[allow(clippy::indexing_slicing)]
    let key = segments[0].clone();
    let entry = map
        .entry(key)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    #[allow(clippy::indexing_slicing)]
    let rest = &segments[1..];
    set_nested_value(entry, rest, value);
}

fn ensure_object(value: &mut Value) -> &mut serde_json::Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(serde_json::Map::new());
    }

    // SAFETY: The branch above guarantees `value` is a `Value::Object`, so
    // `as_object_mut()` will always return `Some`.
    #[allow(clippy::expect_used)]
    value
        .as_object_mut()
        .expect("value should be coerced into an object")
}
