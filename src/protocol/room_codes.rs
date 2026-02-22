use crate::config::ProtocolConfig;
use rand::RngExt;

/// Generate alphanumeric room code with configurable length
/// Uses uppercase letters and numbers for easy communication
pub fn generate_room_code_with_config(config: &ProtocolConfig) -> String {
    const ALPHANUMERIC_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut rng = rand::rng();
    (0..config.room_code_length)
        .map(|_| {
            let idx = rng.random_range(0..ALPHANUMERIC_CHARS.len());
            // SAFETY: `idx` is produced by `random_range(0..len)`, so it is
            // always within [0, len).
            #[allow(clippy::indexing_slicing)]
            let ch = ALPHANUMERIC_CHARS[idx] as char;
            ch
        })
        .collect()
}

/// Generate room code avoiding confusing characters (0, O, I, 1) with configurable length
pub fn generate_clean_room_code_with_config(config: &ProtocolConfig) -> String {
    generate_clean_room_code_of_length(config.room_code_length)
}

/// Generate a clean room code of the requested length.
pub fn generate_clean_room_code_of_length(length: usize) -> String {
    const CLEAN_CHARS: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    if length == 0 {
        return String::new();
    }
    let mut rng = rand::rng();
    (0..length)
        .map(|_| {
            let idx = rng.random_range(0..CLEAN_CHARS.len());
            // SAFETY: `idx` is produced by `random_range(0..len)`, so it is
            // always within [0, len).
            #[allow(clippy::indexing_slicing)]
            let ch = CLEAN_CHARS[idx] as char;
            ch
        })
        .collect()
}

/// Generate a region-aware room code by prepending an optional prefix.
pub fn generate_region_room_code(config: &ProtocolConfig, region_prefix: Option<&str>) -> String {
    let normalized_prefix = region_prefix
        .map(|p| p.trim().to_ascii_uppercase())
        .filter(|p| !p.is_empty());

    if let Some(prefix) = normalized_prefix {
        let prefix_len = prefix.chars().count();
        if prefix_len >= config.room_code_length {
            tracing::warn!(
                prefix_len,
                room_code_length = config.room_code_length,
                "Room code prefix is greater than or equal to total room code length; falling back to random codes without prefix"
            );
            return generate_clean_room_code_with_config(config);
        }

        let random_len = config.room_code_length - prefix_len;
        let random_segment = generate_clean_room_code_of_length(random_len);
        format!("{prefix}{random_segment}")
    } else {
        generate_clean_room_code_with_config(config)
    }
}

/// Generate a 6-character alphanumeric room code (legacy)
/// Uses uppercase letters and numbers for easy communication
#[allow(dead_code)]
pub fn generate_room_code() -> String {
    // Delegate to config-based generator with default config
    let cfg = ProtocolConfig::default();
    generate_room_code_with_config(&cfg)
}

/// Generate room code avoiding confusing characters (0, O, I, 1) - legacy 6-char version
pub fn generate_clean_room_code() -> String {
    let cfg = ProtocolConfig::default();
    generate_clean_room_code_with_config(&cfg)
}
