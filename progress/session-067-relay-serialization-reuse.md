# Session 067 — Relay socket serialization measurement and reuse

**Branch:** `agent/session-067-relay-serialization-reuse`
**Base:** `6343151` (PR #223, session 066)

## Objective

Complete issue #222: extend the relay measurement through the real socket-frame
serialization boundary, establish a non-vacuous allocation and runtime
baseline, and reuse compatible-recipient results only if the same harness
proves a material improvement without changing wire output or delivery
accounting.

## GitHub and dependency state at start

- `main` had no failing workflow; the exact base push exposed all 15 expected
  workflows.
- Issue #222 was the highest-gameplay-impact actionable follow-up. Issues #210,
  #213, and #220 remain broader research/static-analysis/formal-method work.
- Dependabot PR #224 re-proposed four declarations already rejected with
  current measured evidence: `tokio-tungstenite` 0.30 duplicates Axum's 0.29
  stack, `base64` 0.23 duplicates the existing graph, `serial_test` 4.0.1
  requires rustc 1.93.1 against the repository's 1.89.0 MSRV, and `syn` 3
  removes `Arm::guard` while syn 2 remains transitively required. The PR was
  closed, and exact version-scoped Dependabot holds now prevent repeated churn
  without disabling advisory scanning.

## Measurement design

The new serialization benchmarks drive production
`broadcast_to_room_except_with_message`, generation-scoped classified queues,
and the exact synchronous projector immediately upstream of each Axum socket
write. The socket itself is replaced with an in-memory frame ledger; no
benchmark-only serializer exists.

Each sample relays 1,024 distinct, prebuilt ~1 KiB messages through room sizes
2, 8, and 16 in three scenarios:

1. protocol-v3 JSON text to homogeneous JSON recipients;
2. protocol-v3 MessagePack binary to homogeneous MessagePack recipients; and
3. a MessagePack source to recipients cycling through v2/JSON, v3/JSON,
   v2/MessagePack, and v3/MessagePack.

Every cell proves delivery attempts, successful enqueues, dequeues,
materialized frames, simulated successful-write accounting, text/binary cohort
counts, wire bytes, codec operations, empty queues, and a SHA-256 output digest.
It does not claim to exercise kernel/socket error or cancellation behavior;
those remain covered by the production send/accounting tests. Allocation
samples run five exact repeats under `stats_alloc`. Runtime samples use
uninstrumented Criterion because the instrumented allocator performs a
sequentially consistent atomic operation on every allocation.

The benchmark seam and production projector refactor were committed before the
optimization as `6703c81`, preserving a reviewable and reproducible baseline.

## Pre-change baseline

Allocation operations and bytes are per relay:

| scenario | room | recipients | codec work / relay | allocation ops | bytes |
| --- | ---: | ---: | --- | ---: | ---: |
| v3 JSON text | 2 | 1 | 1 JSON encode | 11.001 | 3,202 |
| v3 JSON text | 8 | 7 | 7 JSON encodes | 41.001 | 20,254 |
| v3 JSON text | 16 | 15 | 15 JSON encodes | 81.001 | 42,990 |
| v3 MessagePack binary | 2 | 1 | 1 MP encode | 13.001 | 3,201 |
| v3 MessagePack binary | 8 | 7 | 7 MP encodes | 55.001 | 20,249 |
| v3 MessagePack binary | 16 | 15 | 15 MP encodes | 111.001 | 42,979 |
| mixed MP source | 2 | 1 | 1 decode + 1 JSON encode | 20.001 | 5,017 |
| mixed MP source | 8 | 7 | 4 decodes + 4 JSON + 3 MP encodes | 79.001 | 25,327 |
| mixed MP source | 16 | 15 | 8 decodes + 8 JSON + 7 MP encodes | 159.001 | 53,134 |

Criterion median times were 2.169 ms, 14.693 ms, and 30.614 ms for JSON;
1.742 ms, 10.766 ms, and 23.184 ms for binary; and 2.688 ms, 15.003 ms, and
31.536 ms for mixed traffic at room sizes 2, 8, and 16 respectively.

## Implementation

- Introduce a `DeliveryMessage` carrier that atomically binds the logical
  `Arc<ServerMessage>` to its optional relay-wide cache. The whole carrier is
  returned on a full reliable queue and survives the parked retry, so cached
  frames cannot drift onto a different message.
- Create one cache only when a game-data fan-out has more than one recipient.
  Direct one-recipient delivery retains the uncached path.
- Lazily initialize six exact cohorts: text v2/v3, direct binary v2/v3, and
  binary-to-text fallback v2/v3. `OnceLock` allows concurrent socket writers to
  initialize each immutable result at most once.
- Share the decoded JSON tree for MessagePack fallback across its v2 and v3
  text cohorts, then serialize borrowed envelopes so sharing does not require a
  payload-sized clone.
- Keep queue classification, metadata validation, delivery deadlines,
  cancellation-safe terminal accounting, unsupported-format reports, and
  socket failure handling per recipient.

## Result

Every post-change frame digest and wire-byte total is identical to the matching
baseline cell. Codec work collapses to one operation per present cohort (and
one shared MessagePack decode):

| scenario | room | codec work / relay | allocation ops | bytes | ops change |
| --- | ---: | --- | ---: | ---: | ---: |
| v3 JSON text | 8 | 1 JSON encode | 12.001 | 8,370 | -70.7% |
| v3 JSON text | 16 | 1 JSON encode | 12.001 | 14,258 | -85.2% |
| v3 MessagePack binary | 8 | 1 MP encode | 14.001 | 8,369 | -74.5% |
| v3 MessagePack binary | 16 | 1 MP encode | 14.001 | 14,257 | -87.4% |
| mixed MP source | 8 | 1 decode + 2 JSON + 2 MP encodes | 40.001 | 15,649 | -49.4% |
| mixed MP source | 16 | 1 decode + 2 JSON + 2 MP encodes | 40.001 | 21,537 | -74.8% |

Criterion comparison against `issue222-pre`:

| scenario | room 8 median change | room 16 median change |
| --- | ---: | ---: |
| v3 JSON text | -25.6% | -30.5% |
| v3 MessagePack binary | -8.9% | -12.6% |
| mixed MessagePack source | -14.3% | -25.9% |

All six multi-recipient cells improved with `p < 0.05`. The room-size-two cells
showed no statistically significant runtime change, matching the design choice
not to allocate a cache when there is only one recipient.

## Verification

- Exact five-repeat pre/post allocation measurements across all nine cells
- Criterion pre/post comparison across all nine cells
- Exact pre/post output SHA-256 and wire-byte equality in every cell
- Focused production projector and six-cohort cache unit tests
- Full classified outbound-queue unit suite
- Exact 345-mutant inventory and unchanged 36-shard feasibility
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `cargo test --locked --all-features`
- `cargo deny` for the root, native, Fortress, and Fortress WASM graphs
- Optional-feature matrix, MSRV, doc consistency, workflow/tooling,
  Dependabot, LLM policy, and PowerShell worktree hook checks
- Full hosted-CI and reviewer evidence is recorded on the session PR

## Follow-ups

The dependency/CI audit identified separate maintenance opportunities that do
not belong in the relay hot-path patch: extend dependency automation to the
standalone client/fuzz packages and make the fuzz lockfile reproducible
(issue #225); repair the multiline Docker `HEALTHCHECK` parser warning
(issue #226).
