#![no_main]
//! Coverage-guided fuzzing of the protocol decode surface.
//!
//! The libFuzzer counterpart to `assert_decoders_return` in
//! `tests/protocol_fuzz_hardening.rs`: feed an arbitrary byte slice to the one
//! production decode site (JSON text frame -> `ClientMessage`) plus the three
//! defense-in-depth / SDK-parity decoders (rmp -> `ClientMessage`, and both
//! encodings -> `ServerMessage`), and require that every decode returns
//! `Ok`/`Err` — never panics, aborts, or overflows the stack. A successful
//! parse must additionally survive re-serialization to both wire encodings.
//!
//! The property is the absence of a crash: libFuzzer treats any panic/abort as
//! a finding. Seed the corpus from `.llm/code-samples/protocol/*.jsonl` (see
//! fuzz/README.md). Run via the nightly `fuzz` CI job, never on stable.
use libfuzzer_sys::fuzz_target;
use signal_fish_server::protocol::{ClientMessage, ServerMessage};

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = serde_json::from_slice::<ClientMessage>(data) {
        let _ = serde_json::to_string(&message);
        let _ = rmp_serde::to_vec_named(&message);
    }
    if let Ok(message) = serde_json::from_slice::<ServerMessage>(data) {
        let _ = serde_json::to_string(&message);
        let _ = rmp_serde::to_vec_named(&message);
    }
    if let Ok(message) = rmp_serde::from_slice::<ClientMessage>(data) {
        let _ = serde_json::to_string(&message);
        let _ = rmp_serde::to_vec_named(&message);
    }
    if let Ok(message) = rmp_serde::from_slice::<ServerMessage>(data) {
        let _ = serde_json::to_string(&message);
        let _ = rmp_serde::to_vec_named(&message);
    }
});
