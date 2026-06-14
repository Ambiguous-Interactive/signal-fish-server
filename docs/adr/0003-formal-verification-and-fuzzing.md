# Formal Verification and Fuzzing for the Protocol v3 Session Core

## Status

ADR-0003 - Accepted (amended — see [Update: production-readiness additions](#update-production-readiness-additions))

## Context

Protocol v3 ([ADR-0001](0001-protocol-v3-two-axis.md)) added a small but
safety-critical state machine to the server: per-room session-plan selection (the
`mesh+webrtc → host+webrtc → host+direct → relay` ladder), per-recipient
`SessionPlan` emission, late-join / seat-fill pairing, and host-failover
re-planning. Its correctness rests on cross-cutting invariants that example-based
tests can demonstrate but not exhaust:

- a v2 (or v3 relay-only) member must **never** observe a v3 control message
  (Appendix K back-compat);
- a stored `host` plan must **always** name a current, session-capable host (the
  self-heal contract);
- topology/transport are **sticky** once stored; only the host is re-elected;
- peer lists are **capability-filtered on both sides**, and mesh glare is
  antisymmetric;
- the decode surface (`Signal`'s opaque payload, both JSON and MessagePack
  frames) must never panic on hostile input.

The codebase is also constrained: a **pinned stable toolchain**
(`rust-toolchain.toml`), strict zero-warning / zero-production-panic policy, and
CI/local tooling parity (every check runs the same way locally and in CI). Any
verification we add must run on stable, in the normal test suite, with no
out-of-workspace tooling.

## Decision

Adopt a **three-layer verification stack**, each layer matched to what it is good
at, all runnable on the pinned stable toolchain:

### 1. TLA+ / TLC for the session state machine

Model the v3 session lifecycle in TLA+ (`formal/tla/SignalFishSession.tla`) and
exhaustively model-check it with TLC across four configurations
(`desired = mesh`, `desired = host`, `host` with WebRTC disabled, and the
relay-floor model with both upgrade transports disabled).

Model checking fits because the protocol core is a **small, finite,
non-deterministic state machine**: a handful of players with fixed capability
profiles, a bounded churn budget, and a few actions (join, depart, grant
authority, finalize). TLC enumerates _every_ interleaving of those whole events
and checks the named invariants in every reachable state — the exhaustive
coverage example tests cannot give. The spec mirrors the implementation
action-for-action, with each TLA+ action modeling one event plus all of its
session side effects as a single atomic step. That atomicity is a deliberate
sequential abstraction: the server runs one event's side effects on one task
but does not serialize distinct events on the same room against each other, so
invariants like host validity are theorems of the abstraction — the running
system holds them between heals and repairs transient violations at the next
membership-touching event (the widened `host_invalid` trigger exists for
exactly this; see the Atomicity argument in
[`formal/README.md`](../../formal/README.md)). A TLC violation is therefore a
real contract violation, not an idealization gap — in the other direction, the
transient inter-event windows are covered by unit tests, not by TLC. The
spec/code correspondence and the "intentionally not modeled" boundary are
documented in [`formal/README.md`](../../formal/README.md). CI runs TLC on any
change to the spec or to the modeled source files (`session_policy.rs`,
`signaling.rs`, `room_service.rs`, `reconnection_service.rs`,
`database/mod.rs`, `protocol/room_state.rs`) via path filters.

### 2. proptest for code-level invariants and wire round-trips (on stable)

Property tests drive the **real code** with randomized inputs:

- selection-core invariants (legal pair, ceiling respected, first-fit ladder
  semantics cross-checked by independent recomputation, member-order
  independence, subset monotonicity, glare antisymmetry, star shape,
  capability filtering) — `src/server/session_policy_tests.rs::properties`;
- wire round-trips for the v3 payloads through both encodings the server speaks
  (JSON text + `rmp_serde::to_vec_named` MessagePack), opaque-`Signal` payload
  fidelity, `Authenticate` optional-field presence, and TURN credential
  determinism cross-checked against an independent HMAC recomputation —
  `tests/v3_wire_properties.rs`.

proptest runs on stable, integrates with `cargo test`/nextest, and shrinks
counterexamples automatically. Per repo policy every proptest test carries
`#[cfg_attr(miri, ignore)]` (enforced by `tests/ci_config_tests.rs`).

### 3. proptest-driven fuzz hardening for the parser surface (on stable)

`tests/protocol_fuzz_hardening.rs` throws arbitrary bytes, structured mutations
of the canonical v3 samples, deep-nesting bombs, oversized strings, out-of-range
numbers, and invalid UTF-8 at four decoders
(`serde_json` / `rmp_serde` × `ClientMessage` / `ServerMessage`) and asserts the
single property **"decode returns `Ok` or `Err`, never a panic/abort/overflow"**,
pinning the libraries' structured-failure behavior (recursion limits, number
ranges) where it is the active defense.

Of those four, exactly **one** is a production decode of untrusted client
input: JSON text frames into `ClientMessage` via serde_json
(`parse_client_message`, `src/websocket/token_binding.rs`), reached only behind
the app-level `max_message_size` cap (64 KiB default,
`src/websocket/connection.rs`) and bounded by serde_json's 128-deep recursion
limit. Inbound binary frames are size-capped opaque game-data relays
(`src/server/game_data.rs`) never decoded into a protocol enum (their
JSON-fallback transcode decodes only into `serde_json::Value`, behind the same
caps), and `ServerMessage` decoding happens SDK-side, outside this repo. The
other three decoders are fuzzed deliberately as defense-in-depth and SDK-parity
hardening — labeled as such in the test module — not because production reaches
them.

Depth-bomb evidence (measured with the repo's release profile — thin LTO,
`codegen-units = 1` — on rmp-serde 1.3.1 / serde_json 1.0.150): decoding a
100k-deep MessagePack bomb into `ClientMessage` returns rmp_serde's depth-limit
`Err` (its budget is 1024, `decode.rs:213`) on 2 MiB, 1 MiB, and 512 KiB
threads, and first overflows at 256 KiB — roughly 4x margin under tokio's
default 2 MiB worker stack; the deepest accepted nesting (1021 levels) also
decodes and drops within 512 KiB. Debug builds overflow a 2 MiB thread before
the limit returns (passing at 4 MiB), which is why the depth probes run on a
dedicated big-stack thread. serde_json's 128-deep limit errs cleanly even on a
64 KiB stack. The release margin is enforced by a release-profile-only test
(`release_profile_depth_bomb_fits_default_worker_stack`,
`#[cfg(not(debug_assertions))]` — compiled out of debug CI by design) that runs
under `cargo test --release --test protocol_fuzz_hardening`.

#### Why not cargo-fuzz / libFuzzer (now)

`cargo-fuzz` is the standard coverage-guided fuzzer, but it **requires a nightly
compiler** (it depends on `-Z` flags and a libFuzzer sanitizer runtime) and lives
in a **separate out-of-workspace crate**. Both conflict with this repo's pinned
stable toolchain and CI/local tooling-parity policy: a nightly-only job runs
differently locally than in CI and bypasses the stable gate. The proptest
hardening tests instead run inside the _normal_ stable test suite — every
`cargo test` and every CI job re-fuzzes the decoders — trading coverage-guided
depth for permanent, zero-infrastructure regression pressure.

**What would justify adding cargo-fuzz later:** a hand-written or `unsafe` parser
(today we lean entirely on `serde_json` / `rmp_serde`, which are themselves
heavily fuzzed upstream), a measured need for coverage-guided exploration of a
state-rich decoder, or a dedicated nightly fuzzing lane that does not weaken the
stable gate (e.g. an out-of-band scheduled job feeding a corpus, kept clearly
separate from the required checks). If added, the existing proptest corpus and
the canonical samples seed it directly.

### Why not SMT (z3) for this layer

The v3 properties are **state-machine reachability** facts (ladder selection,
glare antisymmetry, the self-heal invariant over event sequences), not
data-constraint satisfaction problems. TLC's explicit-state enumeration checks
them exhaustively over the bounded model and produces a concrete counter-example
_trace_ on failure — far more useful here than an UNSAT core. The arithmetic in
the system is trivial (topology ranks 0/1/2, a UUID total order), so an SMT
encoding would add a heavyweight dependency and a translation gap for no
coverage gain. SMT would fit a different problem (e.g. proving a numeric/crypto
routine correct over all inputs); this layer is not that.

### Why not Kani / Loom (now)

- **Loom** exhaustively explores orderings of _hand-rolled_ concurrency
  primitives (custom atomics, lock-free structures). This code deliberately uses
  `tokio` mpsc channels and `DashMap` rather than bespoke synchronization. Each
  event's session side effects run within one task, but distinct events on the
  same room are not serialized against each other — the TLA+ model's atomic
  events are a sequential abstraction, and the inter-event races it collapses
  (concurrent departures, the reconnect-failure rollback) are by-design healed
  at the next membership-touching event via the widened `host_invalid` trigger
  (see the Atomicity argument in `formal/README.md`), with the heal arms covered
  by unit tests. Loom would still add nothing here: there is no hand-rolled
  primitive for it to explore, and it needs a model of the async runtime it does
  not have.
- **Kani** is a bounded model checker for proving `unsafe`-free / arithmetic
  properties of Rust functions (absence of overflow, panics, UB on all inputs in
  a bound). It would be a reasonable future addition for a numeric or `unsafe`
  routine, but the session core's logic is exhaustively covered by TLC (protocol
  state) plus proptest (code-level invariants), and Kani also currently leans on
  nightly-adjacent tooling. The panic-freedom property Kani would give for the
  parser is already covered, for the realistic input space, by the stable fuzz
  hardening tests.

## Consequences

### Positive

- **Exhaustive protocol coverage:** TLC checks every interleaving of the bounded
  model — the back-compat, sticky-pair, self-heal, glare, and star invariants are
  proven over the whole reachable state space, not sampled.
- **Real-code invariants:** proptest exercises the actual `session_policy` /
  `signaling` / TURN code and the real wire encoders, cross-checked against
  independent recomputations.
- **Permanent parser pressure on stable:** the decode surface is fuzzed on every
  `cargo test` and CI run with no nightly job and no extra infrastructure.
- **Correspondence is enforced:** a PR touching the modeled source files triggers
  the formal-verification workflow, so the spec cannot silently drift from the
  code.

### Negative

- **Two artifacts to keep in sync:** the TLA+ spec must be updated deliberately
  when the protocol contract changes; the correspondence table and the CI path
  filter mitigate drift but do not eliminate the maintenance.
- **Bounded models:** TLC checks small player/churn bounds and proptest samples a
  finite slice of the input space — neither is a whole-program proof. The bounds
  are chosen to cover every documented rung, downgrade, and failover (see the
  reachability probes in `formal/README.md`).
- **No coverage-guided fuzzing yet:** structured proptest mutation is shallower
  than libFuzzer's coverage feedback; acceptable while the parsers are entirely
  third-party and upstream-fuzzed.

### Mitigations

- `formal/README.md` carries the spec ⇄ code correspondence table, the
  reachability-probe evidence, and the "not modeled" boundary.
- The formal-verification workflow's path filter forces a TLC run whenever the
  modeled source changes.
- The fuzz suite seeds from the canonical `.llm/code-samples/protocol/v3-*.jsonl`
  samples, so it tracks the documented wire shapes.

## Alternatives Considered

### 1. Example-based tests only

**Rejected:** the v3 invariants are universally quantified over member sets,
configs, capability profiles, and event interleavings; example tables (which the
repo already has and keeps) demonstrate specific rungs but cannot exhaust the
back-compat / self-heal / glare space. TLC + proptest close that gap.

### 2. cargo-fuzz / libFuzzer now

**Rejected for now:** requires nightly and an out-of-workspace crate, breaking the
pinned-stable and CI/local-parity policies. Revisit if a hand-rolled or `unsafe`
parser appears, or a separate nightly lane is justified that does not weaken the
stable gate.

### 3. SMT (z3) for the selection logic

**Rejected:** the properties are reachability over a state machine, not
data-constraint satisfaction; TLC checks them exhaustively and yields concrete
failure traces, with no heavyweight solver dependency or encoding gap.

### 4. Kani / Loom

**Rejected for now:** Loom targets hand-rolled concurrency primitives this code
avoids (it uses tokio/DashMap with per-task serialization); Kani targets bounded
numeric/`unsafe` proofs not central to this state-machine-shaped logic, and the
parser panic-freedom it would provide is already covered on stable.

## Update: production-readiness additions

The "rejected for now" stances on **coverage-guided fuzzing** and **SMT** above
were revisited for the v2/v3 production-readiness pass and **both were added** —
each as exactly the _non-stable-gate-weakening_ lane the original ADR anticipated.
The original reasoning is preserved above as the at-the-time record; this section
is the amendment.

### Coverage-guided fuzzing (cargo-fuzz / libFuzzer) — added

The ADR named the condition: "a dedicated nightly fuzzing lane that does not
weaken the stable gate (e.g. an out-of-band scheduled job feeding a corpus, kept
clearly separate from the required checks)." That is precisely what was built:

- An **out-of-workspace** crate ([`fuzz/`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/fuzz/README.md))
  with an empty `[workspace]` table, so it never perturbs the pinned-stable build.
- Targets `decode_protocol` and `validate_inputs`, mirroring the stable
  proptest surfaces, seeded from the canonical `.llm/code-samples/protocol/*.jsonl`.
- A **nightly-only** CI job (`.github/workflows/fuzz.yml`) on a pinned nightly,
  separate from the required stable checks. The stable proptest suite remains the
  always-on gate; any reproducible fuzz finding is added back to
  `tests/protocol_fuzz_hardening.rs` so it is caught on every `cargo test`.

The stable proptest fuzzer is **still primary**; cargo-fuzz adds coverage-guided
depth without moving the gate, so the original parity concern is fully preserved.

### SMT proofs (z3) for the pure decision functions — added

The ADR rejected SMT because the v3 properties are _state-machine reachability_
facts that TLC checks exhaustively with concrete traces. That remains true — and
TLC is still the primary check for the lifecycle. The added Z3 layer
([`formal/z3/protocol_invariants.py`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/z3/protocol_invariants.py))
targets a **complementary** layer TLC cannot reach: the _pure decision functions_
(ladder selector, `all_support` relay-floor invariant, glare antisymmetry, host
election) proven over **unbounded** inputs — any member count, any capability mix,
any id space — where TLC only samples a bounded model. The encoding gap the ADR
warned about is mitigated by a **self-checking harness** (a deliberately wrong
selector must produce a `sat` counterexample, so a `PASS` is never vacuous) and a
dedicated CI job (`.github/workflows/formal-verification.yml`, `z3` job). The
accepted tradeoff is the dual maintenance of the SMT model alongside the Rust
source — bounded by keeping the proofs scoped to the small, stable pure-logic
surface and anchored to function names in `src/server/session_policy.rs` /
`signaling.rs`.

See [`formal/README.md`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/README.md#z3-proofs)
for the proof catalogue and the TLA+ ⇄ Z3 division of labour.

## References

- [Protocol v3 Two-Axis (ADR-0001)](0001-protocol-v3-two-axis.md) — the invariants this layer verifies
- [Matchbox Compatibility (ADR-0002)](0002-matchbox-compatibility.md) — the opaque-signal payload contract
  the fuzz suite probes
- [`formal/README.md`](../../formal/README.md) — spec ⇄ code correspondence, reachability evidence, modeling boundary
- [Formal Verification architecture](../architecture/formal-verification.md) — how the layers fit and what
  each can/cannot catch
- TLA+ / TLC: <https://lamport.azurewebsites.net/tla/tla.html>
