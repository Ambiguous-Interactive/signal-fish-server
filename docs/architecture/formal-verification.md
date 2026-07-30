# Formal Verification and Property Testing (Protocol v3)

How the Protocol v3 session core is verified, layer by layer: what each layer
checks, how to run it, and what it can and cannot catch. The design rationale —
why TLA+/TLC, proptest on stable, and not cargo-fuzz / SMT / Kani / Loom today —
lives in [ADR-0003](../adr/0003-formal-verification-and-fuzzing.md); the
spec ⇄ code correspondence table lives in
[`formal/README.md`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/README.md).

## The six layers

| Layer                      | Verifies                                                         | Artifact                                                          |
| -------------------------- | --------------------------------------------------------------- | ---------------------------------------------------------------- |
| TLC model checking         | The session **state machine** (protocol contract, all interleavings) | `formal/tla/SignalFishSession.tla` + four `.cfg` models     |
| Trace refinement (pilot)   | Captured reliable-queue **Rust transitions** satisfy their TLA action guards | `DeliveryContractTrace.tla` + nightly generated JSONL      |
| proptest invariants        | **Real-code** selection / election / peer-list / TURN invariants | `src/server/session_policy_tests.rs::properties`                 |
| proptest wire round-trips  | v3 **wire** encode/decode fidelity (JSON + MessagePack)         | `tests/v3_wire_properties.rs`                                     |
| proptest fuzz hardening    | **Parser robustness** — no panic on hostile bytes               | `tests/protocol_fuzz_hardening.rs`                                |
| e2e / golden suites        | End-to-end **wire conformance** and the frozen v2 contract      | `tests/v3_*_e2e.rs`, `tests/v3_protocol_samples.rs`, `tests/v2_wire_golden.rs` |

The layers are complementary: TLC reasons about _event orderings_ a unit test
would never script; proptest reasons about _input distributions_ over the real
functions; the e2e suites prove the bytes on the actual WebSocket.

### TLC — the protocol state machine

`SignalFishSession.tla` models the v3 session lifecycle (finalize-time selection,
authoritative per-recipient `SessionPlan` publication, late-join / seat-fill
membership refreshes, host-failover re-planning). Each TLA+ action models one membership-touching event **atomically**
— the event handler plus all of its session side effects as one step. That is a
deliberate sequential abstraction: the server runs one event's side effects on one
task but does not serialize distinct events on the same room against each other
(see the Atomicity argument in
[`formal/README.md`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/README.md)
for what the abstraction proves and what the heal-on-next-event mechanism covers
instead). TLC then enumerates **every** reachable state of the bounded model and
checks the named invariants and action properties in each one:

- `V2Gating` — no `SessionPlan` ever reaches a sub-v3 member (Appendix K
  back-compat);
- `EmissionMatchesSessionState` / `PublicationCoverage` — every publication
  reaches exactly all current v3 members and either matches the stored sticky
  decision or explicitly resets them to the relay floor;
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
bash scripts/run-tla-model-check.sh            # every checked configuration
bash scripts/run-tla-model-check.sh --config Mesh --verbose
```

The script downloads a version-pinned, SHA256-verified `tla2tools.jar` (needs a
JRE 11+) and exits nonzero on any violation. The four
`SignalFishSession.tla` configurations cover the five capability profiles,
both desired ceilings, the WebRTC-disabled (host+direct) path, and the
all-transports-disabled relay floor (`RelayFloorOnly`: nothing is stored and
every v3 publication is an explicit relay reset); observed state spaces are
~17k–151k distinct states, ~2–7 s each. CI runs the same script via
`.github/workflows/formal-verification.yml`.

### Trace refinement — captured Rust transitions

The P10.D7 pilot connects a real-code property corpus and a v2 real-socket case
back to TLC. The feature-gated recorder captures paused-clock
`deliver_or_disconnect` decisions plus production dequeue, write, queue-close,
and finalization transitions as ordered, payload-free JSONL. A strict generator
accepts only the v2 reliable single-FIFO projection, compiles each delivery
attempt into a one-message model sender, and replays every action through
`DeliveryContractTrace.tla`. If event `i` is not enabled by the current model
state, TLC deadlocks with that trace id and index. An explicit in-flight slot
preserves dequeue-before-write-completion, while `QueueClose` separates the
channel's send-rejection boundary from final socket teardown.

The daily job is initially informational while this technique is piloted; PRs
still gate the feature build, recorder/action tests, and generator rejection
tests. The nightly wrapper also replaces the first action with an impossible
`WriterDrain` and requires TLC to deadlock at `i = 1`, proving the replay oracle
is not vacuous; a second negative proves slow-consumer close-flush is rejected.
The projection intentionally rejects overlapping enqueue/dequeue capture,
hidden untraced v2 queue occupancy, delivery classes,
generation cancellation, writer sojourn eviction, and shutdown-priority close
upgrades; the exact boundary and commands are in `formal/README.md`.

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
| trace refinement | Spec↔code action/guard divergence in captured reliable-queue executions | Uncaptured schedules; classified/lossy queues and other declared out-of-model branches |
| proptest invariants | Logic bugs in the real selection/election/peer-list/TURN code      | Orderings/interleavings (single-call properties); unsampled inputs   |
| proptest wire    | Encode/decode fidelity drift; optional-field wire regressions       | Semantic bugs above the wire; float exactness on the JSON path (documented bounded drift) |
| fuzz hardening   | Decoder panics/overflows on hostile input                           | Semantically wrong parses that still return `Ok`; coverage-guided deep paths |
| e2e / golden     | Real end-to-end wire conformance; frozen-v2 byte drift              | Exhaustive interleavings; rare input shapes                          |

The boundary TLC intentionally does not model (rate limits, opaque-payload relay,
reconnection tokens, TURN minting, multi-room, storage-error wedges) is enumerated
in
[`formal/README.md`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/README.md),
each with the suite that covers
it instead.

## Single-instance theorems

The formal suite makes the single-instance correctness boundary executable via
two seeded counterexamples (full table in
[`formal/README.md`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/README.md)):

- **`SplitBrainStampBug`** in
  [`SequencedRelay.tla`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/tla/SequencedRelay.tla)
  — a second instance stamps the same sender's stream from an independent
  counter (`counter2`); a no-affinity load balancer collapses both onto one
  recipient queue, producing interleaved duplicate/regressing `seq` values that
  violate `GapAccountable` in four actions.
- **`SplitBrainCounterBug`** in
  [`ReconnectReplay.tla`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/tla/ReconnectReplay.tla)
  — the reconnect is served by a second instance that created the room fresh
  (empty ring, zero watermark, its own `next_sequence`); the empty replay drops
  retained needed events, violating `ReplayFaithful` in three actions.
  `StatusHonest` is also violated at five actions (masked by `ReplayFaithful`
  failing first unless it is removed from `INVARIANTS`).

The composed
[`EndToEndGapAccountability.tla`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/tla/EndToEndGapAccountability.tla)
model proves these single-instance behaviors compose correctly. These
counterexamples are the formal complement to the nightly two-process test in
`tests/split_brain_two_instances_e2e.rs`.

## Additional disconnect/outage exposure bound

[`ReconnectLossBound.tla`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/tla/ReconnectLossBound.tla)
does not claim to bound all gameplay omissions visible at reconnect.
Delivery-class omissions already accounted before the cut stay outside this
model. It checks only the additional exposure introduced by abandoning the old
connection's pipeline and accepting room traffic while the recipient is absent:

```text
reconnectExposure <= QCAP + PCAP + BURST + RATE * WINDOW
```

`PCAP` covers every dequeued but client-application-unobserved stage: server
batcher, active/partial write, kernel and network buffering, and the client
receive path. It is not the configured socket-buffer byte request. `BURST` is
available immediately; each elapsed outage quantum releases at most `RATE`
more admissions. This is the discrete counterpart of the enforced arrival
curve `A(T) <= B + ceil(R*T)`, not a conclusion from an average throughput
measurement. The
[consistency and durability contract](consistency-and-durability.md#additional-disconnectoutage-exposure-bound)
gives the exact operator-facing formula and caveats.

The positive configurations are exhaustive. `_Small` exercises a full queue, a
full post-queue pipeline, an immediate burst, and two elapsed rate quanta.
`_ZeroWindow` permits the burst while proving that no steady-rate admission is
available before time advances. The CI runner also executes
`_ExpectedFailure`: `IgnorePostQueuePipelineBug` removes `PCAP`, and the run
passes only when TLC emits the exact expected
`ReconnectExposureBounded` violation (`7 > 6`). A clean run or a different
error fails CI, keeping the non-vacuity oracle live.

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
- [`formal/README.md`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/formal/README.md)
  — spec ⇄ code correspondence,
  reachability evidence, modeling boundary, state-space sizes.
- [Handoff and Topologies](handoff-and-topologies.md) — the documented protocol
  contract the spec checks.
- [Transport Fallback Contract](transport-fallback.md) — the client-side state
  machine and relay-floor guarantee.
