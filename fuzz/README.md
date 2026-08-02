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

## Dependency policy

`fuzz/Cargo.lock` is committed because this is an executable tooling graph, not
a published library. Stable CI checks every target with `--locked`, and the
nightly fuzz job runs a locked metadata preflight before invoking `cargo-fuzz`.
The preflight is necessary because cargo-fuzz 0.13 does not accept `--locked`.
Dependabot monitors this standalone package independently from the root server
package.

## Targets

| Target | Surface | Stable counterpart |
| ------ | ------- | ------------------ |
| `decode_protocol` | `ClientMessage` / `ServerMessage` decode (serde_json + rmp-serde), then re-serialize | `assert_decoders_return` |
| `validate_inputs` | `validate_{game_name,room_code,player_name}_with_config` over arbitrary UTF-8 | name/room-code validation property tests |
| `fuzz_session_machine` | Structured room/session operation sequences against the in-process server | model-based state-machine tests |
| `fuzz_reconnect_tokens` | Structured reconnect-token issue/claim/reuse/expiry sequences | reconnection property and integration tests |

Every target must avoid panics, aborts, and stack overflows. The protocol and
validation targets must also return `Ok`/`Err` for arbitrary input; the two
state-machine targets assert their modeled room/session and reconnect-token
invariants. libFuzzer reports any crash or failed assertion as a finding.

## Running

Requires the pinned `nightly-2026-08-01` toolchain and `cargo-fuzz` (both
provided in the dev container; install with
`rustup toolchain install nightly-2026-08-01` and
`cargo install cargo-fuzz`).

```bash
# cargo-fuzz may itself be MUSL-linked, so always override its inferred target
# with the pinned compiler's GNU host triple.
FUZZ_TARGET="$(rustc +nightly-2026-08-01 -vV | sed -n 's/^host: //p')"

# Build all targets (instrumented):
cargo +nightly-2026-08-01 fuzz build --target "$FUZZ_TARGET"

# Fuzz one target for 60s (CI smoke uses -max_total_time):
cargo +nightly-2026-08-01 fuzz run decode_protocol --target "$FUZZ_TARGET" -- -max_total_time=60
cargo +nightly-2026-08-01 fuzz run validate_inputs --target "$FUZZ_TARGET" -- -max_total_time=60
```

## Seeding the corpus

Seed `decode_protocol` from the canonical wire samples so libFuzzer starts from
valid structures and mutates outward:

```bash
mkdir -p corpus/decode_protocol
i=0
for f in ../.llm/code-samples/protocol/*.jsonl; do
  while IFS= read -r line; do
    printf '%s' "$line" > "corpus/decode_protocol/seed_$i"
    i=$((i + 1))
  done < "$f"
done
```

## Triaging a finding

A crash artifact lands in `artifacts/<target>/`. Reproduce and minimize:

```bash
FUZZ_TARGET="$(rustc +nightly-2026-08-01 -vV | sed -n 's/^host: //p')"
cargo +nightly-2026-08-01 fuzz run decode_protocol --target "$FUZZ_TARGET" "artifacts/decode_protocol/crash-<hash>"
cargo +nightly-2026-08-01 fuzz tmin decode_protocol --target "$FUZZ_TARGET" "artifacts/decode_protocol/crash-<hash>"
```

Any reproducible finding should be added as a regression case to the **stable**
suite (`tests/protocol_fuzz_hardening.rs`) so it is caught on every `cargo test`,
not only on the nightly fuzz job.
