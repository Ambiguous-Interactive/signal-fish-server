#![no_main]
//! Coverage-guided fuzzing of the input-validation surface.
//!
//! The validators (`src/protocol/validation.rs`) run on every untrusted
//! `JoinRoom` / `JoinAsSpectator` field. They must total over ALL inputs:
//! return `Ok`/`Err` for any string — never panic on unicode boundaries,
//! control characters, oversized input, or empty/whitespace edge cases. This
//! target drives the config-aware validators with the default `ProtocolConfig`
//! over arbitrary UTF-8; libFuzzer flags any panic/abort.
//!
//! Run via the nightly `fuzz` CI job, never on stable.
use libfuzzer_sys::fuzz_target;
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::validation::{
    validate_game_name_with_config, validate_player_name_with_config,
    validate_room_code_with_config,
};

fuzz_target!(|data: &[u8]| {
    // Validators take &str; fuzz the realistic (UTF-8) input domain. Lossless
    // only when the bytes are valid UTF-8 — that is exactly the surface a
    // WebSocket text frame delivers.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let cfg = ProtocolConfig::default();
    let _ = validate_game_name_with_config(text, &cfg);
    let _ = validate_room_code_with_config(text, &cfg);
    let _ = validate_player_name_with_config(text, &cfg);
});
