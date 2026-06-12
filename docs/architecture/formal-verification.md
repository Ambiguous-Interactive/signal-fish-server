# Formal Verification and Property Testing (Protocol v3)

How the Protocol v3 session core is verified, layer by layer: what each layer
checks, how to run it, and what it can and cannot catch. The design rationale —
why TLA+/TLC, proptest on stable, and not cargo-fuzz / SMT / Kani / Loom today —
lives in [ADR-0003](../adr/0003-formal-verification-and-fuzzing.md); the
spec ⇄ code correspondence table lives in
[`formal/README.md`](../../formal/README.md).

## The five layers

| Layer                      | Verifies                                                         | Artifact                                                          |
| -------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------- |
| TLC model checking         | The session **state machine** (protocol contract, all interleavings) | `formal/tla/SignalFishSession.tla` + four `.cfg` models     |
| proptest invariants        | **Real-code** selection / election / peer-list / TURN invariants | `src/server/session_policy_tests.rs::properties`                 |
| proptest wire round-trips  | v3 **wire** encode/decode fidelity (JSON + MessagePack)         | `tests/v3_wire_properties.rs`                                     |
| proptest fuzz hardening    | **Parser robustness** — no panic on hostile bytes               | `tests/protocol_fuzz_hardening.rs`                                |
| e2e / golden suites        | End-to-end **wire conformance** and the frozen v2 contract      | `tests/v3_*_e2e.rs`, `tests/v3_protocol_samples.rs`, `tests/v2_wire_golden.rs` |

The layers are complementary: TLC reasons about _event orderings_ a unit test
would never script; proptest reasons about _input distributions_ over the real
functions; the e2e suites prove the bytes on the actual WebSocket.

### TLC — the protocol state machine

`SignalFishSession.tla` models the v3 session lifecycle (finalize-time selection,
per-recipient `SessionPlan` emission, late-join / seat-fill pairing, host-failover
re-planning). Each TLA+ action models one membership-touching event **atomically**
— the event handler plus all of its session side effects as one step. That is a
deliberate sequential abstraction: the server runs one event's side effects on one
task but does not serialize distinct events on the same room against each other
(see the Atomicity argument in [`formal/README.md`](../../formal/README.md) for
what the abstraction proves and what the heal-on-next-event mechanism covers
instead). TLC then enumerates **every** reachable state of the bounded model and
checks the named invariants and action properties in each one:

- `V2Gating` — no `SessionPlan` / `NewPeer` ever reaches (or names) a sub-v3
  member (Appendix K back-compat);
- `HostValid` — a stored `host` plan always names a current, capable host, in
  _every_ reachable state of the model (a theorem of the atomic-event
  abstraction; the running system's contract is eventually-healed validity);
- `PlanLegality` / `CeilingRespected` — only legal ladder rungs are stored, never
  above the desired ceiling;
- `MeshPlanExactness` / `GlareAntisymmetry` / `StarProperty` — exact peer lists
  and a single offerer per pair;
- `StickyPairProperty` / `HostDepartureHealedSameStep` — topology/transport never
  change once stored; a departing host is re-elected or the entry dropped in the
  same step.

Run it:

```bash
bash scripts/run-tla-model-check.sh            # all four models
bash scripts/run-tla-model-check.sh --config Mesh --verbose
```

The script downloads a version-pinned, SHA256-verified `tla2tools.jar` (needs a
JRE 11+) and exits nonzero on any violation. The four models cover the five
capability profiles, both desired ceilings, the WebRTC-disabled (host+direct)
path, and the all-transports-disabled relay floor (`RelayFloorOnly`: nothing is
ever stored or emitted); observed state spaces are ~4k–57k distinct states,
~1–2 s each. CI runs the same script via
`.github/workflows/formal-verification.yml`.

### proptest — real-code invariants

`session_policy_tests::properties` drives randomized member sets (capability
profiles, join times, authority), randomized `SessionConfig`s, and randomized
stored plans through `choose_session_plan`, `elect_host`, the replan-site
capability filter, and `plan_for`, asserting each contract by **recomputing the
expectation independently** from member/config data — never by calling the code
under test a second way. `tests/v3_wire_properties.rs` checks the wire payloads.

### proptest — wire round-trips

Randomized `Signal` payloads (arbitrary bounded-depth JSON), `SessionPlanPayload`
/ `SessionPeer` / `IceServer`, the capability enums, and `Authenticate`
optional-field presence round-trip through both encodings the server speaks
(`serde_json` text and `rmp_serde::to_vec_named` MessagePack). TURN minting is
checked for determinism, per-player distinctness, and equality with an
independently recomputed `base64(HMAC-SHA1(secret, username))`.

### proptest — fuzz hardening

`tests/protocol_fuzz_hardening.rs` feeds arbitrary bytes, mutated canonical
samples, deep-nesting bombs, oversized strings, out-of-range numbers, and invalid
UTF-8 to four decoders and asserts only that each returns `Ok`/`Err` — never a
panic, abort, or stack overflow. Exactly one of the four is a production decode
of untrusted input: JSON text frames into `ClientMessage`
(`parse_client_message`, `src/websocket/token_binding.rs`), behind the app-level
64 KiB `max_message_size` cap and serde_json's 128-deep recursion limit; inbound
binary frames are size-capped opaque relays that are never decoded into a
protocol enum, and `ServerMessage` is decoded SDK-side. The other three decoders
(MessagePack→`ClientMessage`, both encodings→`ServerMessage`) are fuzzed as
deliberate defense-in-depth and SDK-parity hardening. A release-profile-only
probe additionally enforces the measured stack margin of the MessagePack depth
limit on a default 2 MiB worker stack (numbers in
[ADR-0003](../adr/0003-formal-verification-and-fuzzing.md)). The suite runs in
the normal stable test suite (see
[ADR-0003](../adr/0003-formal-verification-and-fuzzing.md) for why this replaces
cargo-fuzz here).

## What each layer can and cannot catch

| Layer            | Catches                                                              | Does **not** catch                                                  |
| ---------------- | ------------------------------------------------------------------- | ------------------------------------------------------------------- |
| TLC              | Contract violations across any event interleaving in the bounded model | Bugs outside the model boundary (rate limits, ICE minting, the actual Rust); behavior beyond the player/churn bounds |
| proptest invariants | Logic bugs in the real selection/election/peer-list/TURN code      | Orderings/interleavings (single-call properties); unsampled inputs   |
| proptest wire    | Encode/decode fidelity drift; optional-field wire regressions       | Semantic bugs above the wire; float exactness on the JSON path (documented bounded drift) |
| fuzz hardening   | Decoder panics/overflows on hostile input                           | Semantically wrong parses that still return `Ok`; coverage-guided deep paths |
| e2e / golden     | Real end-to-end wire conformance; frozen-v2 byte drift              | Exhaustive interleavings; rare input shapes                          |

The boundary TLC intentionally does not model (rate limits, opaque-payload relay,
reconnection tokens, TURN minting, multi-room, storage-error wedges) is enumerated
in [`formal/README.md`](../../formal/README.md), each with the suite that covers
it instead.

## Correspondence-maintenance rule

The TLA+ spec mirrors `src/server/session_policy.rs`, `src/server/signaling.rs`,
and `src/server/room_service.rs`. **A PR that changes the behavior of those files
must consider the spec**: either the invariants still hold (re-run TLC) or the
contract moved and the spec/invariants are updated deliberately. This is enforced
mechanically — `.github/workflows/formal-verification.yml` triggers on changes to
those source files (and to `formal/**` and the runner script), so the model check
runs on every such PR. When a property test or TLC invariant fails, treat it as a
real finding: determine whether it is a spec bug or an implementation bug before
weakening either.

## See also

- [ADR-0003: Formal verification and fuzzing](../adr/0003-formal-verification-and-fuzzing.md)
  — the decision record and tool-choice rationale.
- [`formal/README.md`](../../formal/README.md) — spec ⇄ code correspondence,
  reachability evidence, modeling boundary, state-space sizes.
- [Handoff and Topologies](handoff-and-topologies.md) — the documented protocol
  contract the spec checks.
- [Transport Fallback Contract](transport-fallback.md) — the client-side state
  machine and relay-floor guarantee.
