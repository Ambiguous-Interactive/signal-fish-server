# Coverage-guided fuzzing (cargo-fuzz / libFuzzer)

This crate is the **nightly, coverage-guided** extension of the **stable,
in-suite** proptest fuzzer in
[`tests/protocol_fuzz_hardening.rs`](../tests/protocol_fuzz_hardening.rs).

[ADR-0003](../docs/adr/0003-formal-verification-and-fuzzing.md) explains why the
_primary_ fuzzer is proptest-on-stable — the repository pins a stable toolchain
(`rust-toolchain.toml`) with strict CI/local parity, and libFuzzer needs a
nightly compiler and a separate out-of-workspace crate. That ADR named the
conditions for adding a coverage-guided fuzzer later; this crate is that
addition. It lives in its own package with an empty `[workspace]` table, so it
**never** perturbs the pinned-stable build of the main crate, and it is run only
on the nightly toolchain (locally or in the dedicated `fuzz` CI job).

## Targets

| Target | Surface | Stable counterpart |
| ------ | ------- | ------------------ |
| `decode_protocol` | `ClientMessage` / `ServerMessage` decode (serde_json + rmp-serde), then re-serialize | `assert_decoders_return` |
| `validate_inputs` | `validate_{game_name,room_code,player_name}_with_config` over arbitrary UTF-8 | name/room-code validation property tests |

The invariant is the same as the stable suite's: **every decode/validate
returns `Ok`/`Err` and never panics, aborts, or overflows the stack.** libFuzzer
reports any crash as a finding.

## Running

Requires the nightly toolchain and `cargo-fuzz` (both provided in the dev
container; install with `rustup toolchain install nightly` and
`cargo install cargo-fuzz`).

```bash
# Build all targets (instrumented):
cargo +nightly fuzz build

# Fuzz one target for 60s (CI smoke uses -max_total_time):
cargo +nightly fuzz run decode_protocol -- -max_total_time=60
cargo +nightly fuzz run validate_inputs -- -max_total_time=60
```

## Seeding the corpus

Seed `decode_protocol` from the canonical wire samples so libFuzzer starts from
valid structures and mutates outward:

```bash
mkdir -p corpus/decode_protocol
i=0
for f in ../.agents/skills/websocket-protocol/references/*.jsonl; do
  while IFS= read -r line; do
    printf '%s' "$line" > "corpus/decode_protocol/seed_$i"
    i=$((i + 1))
  done < "$f"
done
```

## Triaging a finding

A crash artifact lands in `artifacts/<target>/`. Reproduce and minimize:

```bash
cargo +nightly fuzz run decode_protocol "artifacts/decode_protocol/crash-<hash>"
cargo +nightly fuzz tmin decode_protocol "artifacts/decode_protocol/crash-<hash>"
```

Any reproducible finding should be added as a regression case to the **stable**
suite (`tests/protocol_fuzz_hardening.rs`) so it is caught on every `cargo test`,
not only on the nightly fuzz job.
