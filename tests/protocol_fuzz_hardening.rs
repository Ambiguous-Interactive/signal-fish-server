//! Fuzz-style parser hardening for the protocol decode surface
//! (Deliverable C3).
//!
//! WHY THIS INSTEAD OF cargo-fuzz: this repository builds on a PINNED STABLE
//! toolchain (`rust-toolchain.toml`) with strict CI/local tooling parity;
//! cargo-fuzz/libFuzzer requires a nightly compiler and a separate
//! out-of-workspace fuzz crate, neither of which fits the repo policy. These
//! proptest-driven byte-level tests run in the NORMAL test suite on stable —
//! every `cargo test` and every CI job re-fuzzes the decoders — trading
//! coverage-guided depth for permanent, zero-infrastructure regression
//! pressure. The tradeoff and the conditions under which a real
//! coverage-guided fuzzer would be added later are recorded in ADR-0003
//! (docs/adr/0003-formal-verification-and-fuzzing.md).
//!
//! The hardening property is deliberately minimal: every decode returns
//! `Ok(..)` or `Err(..)` — it never panics, aborts, or overflows the stack.
//! Any panic fails the test naturally, so most assertions are simply "the
//! call returned". Where the libraries promise structured failure (recursion
//! limits, number-range checks), the `Err` is pinned explicitly so a
//! regression to a panic OR a silent acceptance is caught either way.
//!
//! DEPTH-LIMIT FINDING (investigated, not papered over): both decoders reject
//! deep-nesting bombs with `Err`, never undefined behavior —
//!   - serde_json caps recursion at 128 (`remaining_depth`, de.rs); the limit
//!     is shallow enough that deep JSON errs cleanly even on tiny stacks
//!     (measured: `Err` on a 64 KiB release thread).
//!   - rmp_serde caps recursion at 1024 (`depth: 1024`, decode.rs:213 in
//!     1.3.1); reaching the limit recurses ~1024 frames of rmp_serde driving
//!     `serde_json::Value`'s visitor. MEASURED, decoding the 100k-deep
//!     MessagePack bomb into `ClientMessage` with this repo's release
//!     profile (thin LTO, codegen-units = 1; rmp-serde 1.3.1,
//!     serde_json 1.0.150) on dedicated threads of decreasing stack size:
//!
//!     ```text
//!     release:  2 MiB Err | 1 MiB Err | 512 KiB Err | 256 KiB overflow
//!     debug:    2 MiB overflow | 4 MiB Err
//!     ```
//!
//!     The deepest ACCEPTED nesting (1021 levels: the 1024 counter admits
//!     1023 nested composites and the envelope's two maps use two of them;
//!     the 1024th composite errs) also decodes and drops within 512 KiB in
//!     release. So the release binary — what the
//!     server ships — clears tokio's default 2 MiB worker stack with ~4x
//!     margin (first failure only at 256 KiB), while debug builds'
//!     unoptimized frames exceed 2 MiB before the limit returns. That debug
//!     overflow is a build-profile + small-stack artifact, NOT a production
//!     decode vulnerability. To observe the real contract — "rmp_serde
//!     returns its depth-limit `Err`" — deterministically in BOTH profiles,
//!     the depth-bomb probes run on a dedicated generous-stack thread
//!     (`run_on_deep_stack`); the property under test is the `Err`, the big
//!     stack merely keeps the debug harness's default 2 MiB test thread from
//!     aborting before the limit is reached. The release margin itself is
//!     ENFORCED by `release_profile_depth_bomb_fits_default_worker_stack`,
//!     which is compiled only under `#[cfg(not(debug_assertions))]` — it is
//!     deliberately absent from debug CI runs (accepted: debug measurably
//!     cannot fit the limit in 2 MiB) and runs whenever release tests run:
//!     `cargo test --release --test protocol_fuzz_hardening`. The same
//!     numbers are recorded in ADR-0003.
//!
//! Probed surfaces — ONE production decode site plus three deliberate
//! defense-in-depth decoders:
//!   - PRODUCTION: untrusted client input is protocol-decoded at exactly one
//!     site — JSON text frames into `ClientMessage` via serde_json in
//!     `parse_client_message` (src/websocket/token_binding.rs), reached only
//!     behind the app-level `max_message_size` cap (64 KiB default,
//!     src/websocket/connection.rs) and bounded by serde_json's 128-deep
//!     recursion limit. Inbound binary frames are size-capped opaque
//!     game-data relays (src/server/game_data.rs) never decoded into a
//!     protocol enum; their JSON-fallback transcode decodes only into
//!     `serde_json::Value` (src/websocket/sending.rs), behind the same size
//!     cap and the same library depth limits this suite pins.
//!   - DEFENSE-IN-DEPTH / SDK PARITY: `rmp_serde` into `ClientMessage` (no
//!     production path today; hardens any future binary protocol frame
//!     ahead of need) and both encodings into `ServerMessage` (server
//!     frames are decoded SDK-side, outside this repo; fuzzing the shared
//!     message shapes here is free parity coverage). These three are fuzzed
//!     on purpose and labeled as such — they are NOT production entry
//!     points.
//!
//! Inputs: arbitrary byte slices; structured single-byte mutations,
//! truncations, and duplications of the canonical v3 protocol samples
//! (.llm/code-samples/protocol/v3-*.jsonl); deep-nesting bombs (10k-deep
//! arrays in the opaque `signal` field for JSON, 100k-deep MessagePack array
//! headers); oversized strings; out-of-range numbers; invalid UTF-8.
//!
//! All proptest tests carry `#[cfg_attr(miri, ignore)]` per repository policy
//! (tests/ci_config_tests.rs::test_proptest_tests_ignored_under_miri).

use proptest::prelude::*;
use signal_fish_server::protocol::{ClientMessage, ServerMessage};

/// Run `body` on a thread with a generous (32 MiB) stack and return its
/// result. Used only for the deep-nesting probes so the rmp_serde / serde_json
/// recursion limit (`Err`) is reached deterministically in DEBUG builds, where
/// unoptimized stack frames would otherwise exhaust the cargo-test harness's
/// 2 MiB thread before the limit returns (see the module DEPTH-LIMIT FINDING).
/// Production is release-built, where the limit fits a default 2 MiB worker
/// stack with measured ~4x margin — enforced by
/// `release_profile_depth_bomb_fits_default_worker_stack`.
fn run_on_deep_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .name("fuzz-deep-stack".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(body)
        .expect("spawn deep-stack probe thread")
        .join()
        .expect("deep-stack probe thread must return (not overflow)")
}

/// Feed one byte slice to all four fuzzed decoders (the single production
/// decode site plus the three defense-in-depth/SDK-parity decoders — see the
/// module header); assert only that each returns (any panic/abort fails the
/// test naturally). A successful parse must additionally survive
/// re-serialization to both wire encodings without panicking (errors are
/// acceptable; aborts are not).
fn assert_decoders_return(bytes: &[u8]) {
    if let Ok(message) = serde_json::from_slice::<ClientMessage>(bytes) {
        let _ = serde_json::to_string(&message);
        let _ = rmp_serde::to_vec_named(&message);
    }
    if let Ok(message) = serde_json::from_slice::<ServerMessage>(bytes) {
        let _ = serde_json::to_string(&message);
        let _ = rmp_serde::to_vec_named(&message);
    }
    if let Ok(message) = rmp_serde::from_slice::<ClientMessage>(bytes) {
        let _ = serde_json::to_string(&message);
        let _ = rmp_serde::to_vec_named(&message);
    }
    if let Ok(message) = rmp_serde::from_slice::<ServerMessage>(bytes) {
        let _ = serde_json::to_string(&message);
        let _ = rmp_serde::to_vec_named(&message);
    }
}

// ---------------------------------------------------------------------------
// Arbitrary bytes
// ---------------------------------------------------------------------------

proptest! {
    /// Arbitrary byte slices never panic any decoder (JSON or MessagePack,
    /// client or server enum), and accidental valid parses re-serialize
    /// without panicking.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn arbitrary_bytes_never_panic_decoders(
        bytes in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        assert_decoders_return(&bytes);
    }

    /// ASCII-biased byte soup reaches deeper into the JSON lexer than raw
    /// random bytes (which usually fail on the first byte), so probe it
    /// separately: printable chars + JSON structural characters.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn json_flavored_bytes_never_panic_decoders(
        text in r#"[ -~\{\}\[\]",:0-9eE.+-]{0,512}"#,
    ) {
        assert_decoders_return(text.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// Structured mutations of the canonical v3 samples
// ---------------------------------------------------------------------------

/// Load the non-blank lines of a canonical sample file (same anchoring as
/// tests/v3_protocol_samples.rs).
fn sample_lines(relative: &str) -> Vec<Vec<u8>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read sample file {}: {err}", path.display()));
    let lines: Vec<Vec<u8>> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.as_bytes().to_vec())
        .collect();
    assert!(!lines.is_empty(), "{relative} has no sample lines");
    lines
}

#[derive(Debug, Clone)]
enum Mutation {
    /// Overwrite one byte with an arbitrary value.
    ReplaceByte { position: usize, value: u8 },
    /// Truncate the line at a position.
    Truncate { position: usize },
    /// Duplicate the byte at a position (insertion).
    DuplicateByte { position: usize },
    /// Duplicate a whole prefix slice onto the front (structural repetition).
    DuplicatePrefix { length: usize },
}

fn arb_mutation() -> impl Strategy<Value = Mutation> {
    prop_oneof![
        (any::<proptest::sample::Index>(), any::<u8>()).prop_map(|(idx, value)| {
            Mutation::ReplaceByte {
                position: idx.index(usize::MAX),
                value,
            }
        }),
        any::<proptest::sample::Index>().prop_map(|idx| Mutation::Truncate {
            position: idx.index(usize::MAX),
        }),
        any::<proptest::sample::Index>().prop_map(|idx| Mutation::DuplicateByte {
            position: idx.index(usize::MAX),
        }),
        any::<proptest::sample::Index>().prop_map(|idx| Mutation::DuplicatePrefix {
            length: idx.index(usize::MAX),
        }),
    ]
}

fn apply_mutation(line: &[u8], mutation: &Mutation) -> Vec<u8> {
    let mut mutated = line.to_vec();
    if mutated.is_empty() {
        return mutated;
    }
    match mutation {
        Mutation::ReplaceByte { position, value } => {
            let position = position % mutated.len();
            mutated[position] = *value;
        }
        Mutation::Truncate { position } => {
            let position = position % mutated.len();
            mutated.truncate(position);
        }
        Mutation::DuplicateByte { position } => {
            let position = position % mutated.len();
            let byte = mutated[position];
            mutated.insert(position, byte);
        }
        Mutation::DuplicatePrefix { length } => {
            let length = (length % mutated.len()).max(1);
            let prefix: Vec<u8> = mutated[..length].to_vec();
            mutated.splice(0..0, prefix);
        }
    }
    mutated
}

proptest! {
    /// Random single-byte mutations / truncations / duplications of every
    /// canonical v3 sample line parse without panicking; lines that still
    /// parse re-serialize without panicking. Two stacked mutations probe
    /// compound corruption.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn mutated_canonical_samples_never_panic(
        line_choice in any::<proptest::sample::Index>(),
        first in arb_mutation(),
        second in proptest::option::of(arb_mutation()),
    ) {
        // Client and server samples are mutated and fed to BOTH enums via
        // assert_decoders_return: cross-feeding (a server frame into the
        // client decoder) is itself a hostile input worth probing.
        let client_lines = sample_lines(".llm/code-samples/protocol/v3-client-messages.jsonl");
        let server_lines = sample_lines(".llm/code-samples/protocol/v3-server-messages.jsonl");
        let all_lines: Vec<&Vec<u8>> = client_lines.iter().chain(server_lines.iter()).collect();
        let line = all_lines[line_choice.index(all_lines.len())];

        let mut mutated = apply_mutation(line, &first);
        if let Some(second) = second {
            mutated = apply_mutation(&mutated, &second);
        }
        assert_decoders_return(&mutated);

        // Also probe the mutated text reinterpreted as MessagePack bytes and
        // vice versa is already covered by assert_decoders_return (it feeds
        // every decoder); finally probe the untouched line for a baseline.
        assert_decoders_return(line);
    }
}

// ---------------------------------------------------------------------------
// Deterministic adversarial probes
// ---------------------------------------------------------------------------

/// 10k-deep arrays inside the opaque `signal` field: serde_json's recursion
/// limit (128) must reject this with `Err`, NOT a stack overflow. Run on the
/// deep-stack thread so a debug build cannot abort before the limit returns
/// (serde_json's limit is shallow, but the rule is applied uniformly to every
/// deep-nesting probe).
#[test]
fn json_deep_nesting_in_signal_errs_without_overflow() {
    run_on_deep_stack(|| {
        let depth = 10_000;
        let payload = format!(
            r#"{{"type":"Signal","data":{{"to":"00000000-0000-0000-0000-000000000001","signal":{}{}}}}}"#,
            "[".repeat(depth),
            "]".repeat(depth)
        );

        let client = serde_json::from_slice::<ClientMessage>(payload.as_bytes());
        assert!(
            client.is_err(),
            "10k-deep JSON must hit serde_json's recursion limit, got Ok"
        );
        // The raw Value parse must be depth-limited the same way.
        let value = serde_json::from_slice::<serde_json::Value>(payload.as_bytes());
        assert!(value.is_err(), "raw Value parse must also be depth-limited");

        // Deeply nested objects are limited identically.
        let object_payload = format!(
            r#"{{"type":"Signal","data":{{"to":"00000000-0000-0000-0000-000000000001","signal":{}null{}}}}}"#,
            r#"{"k":"#.repeat(depth),
            "}".repeat(depth)
        );
        let nested_object = serde_json::from_slice::<ClientMessage>(object_payload.as_bytes());
        assert!(nested_object.is_err(), "10k-deep JSON objects must err too");
    });
}

/// Hand-rolled MessagePack `{"type":"Signal","data":{"to":<bin8 uuid>,
/// "signal":` + `depth` nested fixarray-of-1 headers + `nil}}`. Hand-rolled
/// because the `rmp` crate is not a direct dependency and valid encoders
/// refuse to build hostile depth. Shared by the deep-stack depth-bomb probe
/// and the release-profile worker-stack margin probe.
fn msgpack_signal_depth_bomb(depth: usize) -> Vec<u8> {
    fn push_fixstr(buf: &mut Vec<u8>, s: &str) {
        assert!(s.len() < 32, "fixstr only");
        buf.push(0xa0 | s.len() as u8);
        buf.extend_from_slice(s.as_bytes());
    }

    let mut bytes = Vec::new();
    bytes.push(0x82); // fixmap, 2 entries
    push_fixstr(&mut bytes, "type");
    push_fixstr(&mut bytes, "Signal");
    push_fixstr(&mut bytes, "data");
    bytes.push(0x82); // fixmap, 2 entries
    push_fixstr(&mut bytes, "to");
    bytes.extend_from_slice(&[0xc4, 16]); // bin8, 16 bytes (uuid wire form)
    bytes.extend_from_slice(&[0u8; 16]);
    push_fixstr(&mut bytes, "signal");
    // `depth` fixarray-of-1 headers (0x91): one nesting level each.
    bytes.extend(std::iter::repeat_n(0x91u8, depth));
    bytes.push(0xc0); // nil innermost
    bytes
}

/// MessagePack depth bomb: 100k nested array headers aimed at the opaque
/// `signal` field. rmp_serde must reject with its depth limit (`Err`), not
/// overflow the stack while driving `serde_json::Value`'s recursive visitor.
/// Runs on the deep-stack thread so the depth-limit `Err` is observed in both
/// debug and release builds (see the module DEPTH-LIMIT FINDING — release fits
/// a default worker stack with measured margin; debug needs more headroom to
/// reach the 1024-deep limit before its larger frames exhaust a 2 MiB harness
/// thread).
#[test]
fn msgpack_depth_bomb_errs_without_overflow() {
    run_on_deep_stack(|| {
        let bytes = msgpack_signal_depth_bomb(100_000);
        let client = rmp_serde::from_slice::<ClientMessage>(&bytes);
        assert!(
            client.is_err(),
            "100k-deep MessagePack must hit rmp_serde's depth limit, got Ok"
        );

        // The bare nested-array bomb (no envelope) must be limited identically
        // when decoded straight into a serde_json::Value.
        let mut bare: Vec<u8> = std::iter::repeat_n(0x91u8, 100_000).collect();
        bare.push(0xc0);
        let value = rmp_serde::from_slice::<serde_json::Value>(&bare);
        assert!(value.is_err(), "bare MessagePack depth bomb must err");
    });
}

/// RELEASE-PROFILE ENFORCEMENT of the measured stack margin (module
/// DEPTH-LIMIT FINDING): on a thread with exactly tokio's default 2 MiB
/// worker stack, the worst-case-depth MessagePack bomb must come back as
/// rmp_serde's depth-limit `Err` (the thread returning at all proves no
/// overflow), and the deepest nesting the decoder ACCEPTS (1021 levels) must
/// decode and drop successfully — the deepest stack the success path can
/// ever use. Compiled out under debug_assertions on purpose: debug frames
/// measurably need more than 2 MiB to reach the limit, which is exactly why
/// `run_on_deep_stack` exists. Runs whenever release tests run:
/// `cargo test --release --test protocol_fuzz_hardening`.
#[cfg(not(debug_assertions))]
#[test]
fn release_profile_depth_bomb_fits_default_worker_stack() {
    const TOKIO_DEFAULT_WORKER_STACK: usize = 2 * 1024 * 1024;
    let probe = std::thread::Builder::new()
        .name("release-depth-margin-probe".to_string())
        .stack_size(TOKIO_DEFAULT_WORKER_STACK)
        .spawn(|| {
            let bomb = msgpack_signal_depth_bomb(100_000);
            assert!(
                rmp_serde::from_slice::<ClientMessage>(&bomb).is_err(),
                "100k-deep MessagePack must hit rmp_serde's depth limit on a \
                 2 MiB stack in release"
            );

            // Deepest accepted nesting: the 1024 counter admits 1023 nested
            // composites, the envelope's two maps use two of them (the
            // 1024th composite errs). Decoding AND dropping the resulting
            // recursive Value must also fit the worker stack.
            let deepest_ok = msgpack_signal_depth_bomb(1021);
            let decoded = rmp_serde::from_slice::<ClientMessage>(&deepest_ok);
            assert!(
                decoded.is_ok(),
                "1021-level nesting is within rmp_serde's depth limit: {:?}",
                decoded.err()
            );
            drop(decoded);
        })
        .expect("spawn 2 MiB release margin probe thread");
    probe
        .join()
        .expect("release-profile decode must fit a default 2 MiB worker stack (no overflow)");
}

/// Oversized strings parse (the protocol layer has no JSON-level string cap —
/// frame-size limits live in the WebSocket layer) and re-serialize without
/// panicking; out-of-range numbers are rejected with `Err`.
#[test]
fn oversized_strings_and_huge_numbers_return() {
    // 2 MiB string in the opaque signal field: must parse (no panic) and
    // re-serialize.
    let big = "x".repeat(2 * 1024 * 1024);
    let payload = format!(
        r#"{{"type":"Signal","data":{{"to":"00000000-0000-0000-0000-000000000001","signal":"{big}"}}}}"#
    );
    let parsed = serde_json::from_slice::<ClientMessage>(payload.as_bytes())
        .expect("oversized string is structurally valid JSON");
    let reserialized = serde_json::to_string(&parsed).expect("re-serializes");
    assert!(reserialized.len() > big.len());

    // Out-of-range numbers: serde_json pins these as Err (number out of
    // range), exponent overflow and 400-digit integer literals alike.
    for huge in ["1e999", "-1e999", &format!("{}9", "9".repeat(400))] {
        let payload = format!(
            r#"{{"type":"Signal","data":{{"to":"00000000-0000-0000-0000-000000000001","signal":{huge}}}}}"#
        );
        let result = serde_json::from_slice::<ClientMessage>(payload.as_bytes());
        assert!(
            result.is_err(),
            "out-of-range number {huge} must be rejected, got Ok"
        );
    }

    // Largest finite f64 exponent still parses fine.
    let payload =
        r#"{"type":"Signal","data":{"to":"00000000-0000-0000-0000-000000000001","signal":1e308}}"#;
    assert!(serde_json::from_slice::<ClientMessage>(payload.as_bytes()).is_ok());
}

/// Invalid UTF-8 byte sequences fed to the text-frame decoder return `Err`
/// (from_slice validates UTF-8 before lexing); MessagePack decoders also
/// return on the same bytes.
#[test]
fn invalid_utf8_returns_err() {
    let probes: [&[u8]; 4] = [
        b"\xff\xfe{\"type\":\"Ping\"}",
        b"{\"type\":\"Ping\xc3\"}",
        b"{\"type\":\"Ping\"}\xf0\x28\x8c\x28",
        b"\xed\xa0\x80",
    ];
    for probe in probes {
        assert!(
            serde_json::from_slice::<ClientMessage>(probe).is_err(),
            "invalid UTF-8 must be rejected by the JSON decoder"
        );
        assert_decoders_return(probe);
    }
}

/// Protocol v3 `GameData.seq` boundary values on BOTH wire encodings: absent,
/// 0 (never produced by the server, whose first stamp is 1, but must decode),
/// 1, and u64::MAX all decode, round-trip exactly, and never panic. Also pins
/// the rejection (not panic) of out-of-domain seq values on the text path.
#[test]
fn game_data_seq_boundary_values_decode_on_both_paths() {
    let uuid = "00000000-0000-0000-0000-000000000001";

    // Absent seq: decodes to None (the entire pre-v3 wire).
    let bare = format!(r#"{{"type":"GameData","data":{{"from_player":"{uuid}","data":{{}}}}}}"#);
    match serde_json::from_str::<ServerMessage>(&bare).expect("bare GameData decodes") {
        ServerMessage::GameData { seq, .. } => assert_eq!(seq, None),
        other => panic!("expected GameData, got {other:?}"),
    }

    for boundary in [0u64, 1, u64::MAX] {
        // Text path.
        let json = format!(
            r#"{{"type":"GameData","data":{{"from_player":"{uuid}","data":{{}},"seq":{boundary}}}}}"#
        );
        let decoded = serde_json::from_str::<ServerMessage>(&json)
            .unwrap_or_else(|err| panic!("seq {boundary} must decode from JSON: {err}"));
        match &decoded {
            ServerMessage::GameData { seq, .. } => assert_eq!(*seq, Some(boundary)),
            other => panic!("expected GameData, got {other:?}"),
        }
        // Exact JSON round-trip (skip_serializing_if must not eat a Some).
        assert_eq!(
            serde_json::to_string(&decoded).expect("re-serialize"),
            json,
            "seq {boundary} must round-trip byte-identically"
        );

        // Binary path (enveloped enum via to_vec_named, the production
        // MessagePack convention).
        let mp = rmp_serde::to_vec_named(&decoded).expect("msgpack encode");
        match rmp_serde::from_slice::<ServerMessage>(&mp)
            .unwrap_or_else(|err| panic!("seq {boundary} must decode from MessagePack: {err}"))
        {
            ServerMessage::GameData { seq, .. } => assert_eq!(seq, Some(boundary)),
            other => panic!("expected GameData, got {other:?}"),
        }

        // Feed both encodings through every fuzzed decoder for the
        // never-panic property.
        assert_decoders_return(json.as_bytes());
        assert_decoders_return(&mp);
    }

    // Out-of-domain seq values: rejected with Err (never a panic, never a
    // silent wrap).
    for hostile in ["-1", "18446744073709551616", "1.5", "\"1\"", "null"] {
        let json = format!(
            r#"{{"type":"GameData","data":{{"from_player":"{uuid}","data":{{}},"seq":{hostile}}}}}"#
        );
        // `null` IS in-domain for an Option field; everything else must err.
        let result = serde_json::from_str::<ServerMessage>(&json);
        if hostile == "null" {
            match result.expect("explicit null seq decodes as None") {
                ServerMessage::GameData { seq, .. } => assert_eq!(seq, None),
                other => panic!("expected GameData, got {other:?}"),
            }
        } else {
            assert!(
                result.is_err(),
                "out-of-domain seq {hostile} must be rejected, got Ok"
            );
        }
        assert_decoders_return(json.as_bytes());
    }
}

/// Truncations of VALID MessagePack encodings (the highest-value corruption:
/// every prefix is reachable from a real frame cut mid-flight).
#[test]
fn truncated_valid_msgpack_never_panics() {
    let message = ClientMessage::Signal {
        to: uuid::Uuid::from_u128(7),
        signal: serde_json::json!({"Offer": "v=0 mock sdp", "nested": [1, 2.5, {"deep": true}]}),
    };
    let encoded = rmp_serde::to_vec_named(&message).expect("encodes");
    for cut in 0..encoded.len() {
        assert_decoders_return(&encoded[..cut]);
    }

    let server = ServerMessage::NewPeer {
        peer_id: uuid::Uuid::from_u128(9),
        you_initiate: true,
    };
    let encoded = rmp_serde::to_vec_named(&server).expect("encodes");
    for cut in 0..encoded.len() {
        assert_decoders_return(&encoded[..cut]);
    }
}
