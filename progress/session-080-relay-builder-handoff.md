# Session 080 — Allocation-free relay builder handoff

## Scope

Complete P37 as the next measured increment of issue #207: remove the
one-shot relay builder's heap allocation without weakening the ordered
relay-stamp / recipient-snapshot boundary, and prove the result for real JSON
and binary ingress.

## Implementation and semantics

The object-safe `MessageCoordinator` seam retains its public boxed `FnOnce`
method for downstream compatibility and adds a borrowed `FnMut` hot-path
method. The private game-data helper is generic over `FnOnce` and adapts it on
the stack through `Option::take`.
The first call consumes the builder, maps a built message into its shared
`Arc`, and preserves `None` when relay-stamp allocation finds that the sender
is no longer routed. A defensive later call returns `None` without rebuilding
or allocating another stamp. Dropping the adapter before or after invocation
releases captured payload state exactly once.

The routing and connection read guards still cover both recipient snapshotting
and the single builder invocation. Existing terminal-tail and reconnect
ordering tests therefore retain the same concurrency proof.

## Measurement

The allocation harness now runs separate JSON and MessagePack-binary
production-ingress-to-classified-queue cells. Runtime, routing, queues,
caller-owned payloads, and warm-up are constructed outside the measured
region; the production cells include the real message-envelope and `Arc`
construction but avoid background task and payload-creation noise. Each cell relays 4,096 messages at
2, 8, and 16 players across five byte-for-byte identical allocator samples,
with non-vacuous delivery-attempt, enqueue, dequeue, and queue-drain checks.

Both production ingress kinds produce the same steady-state operation result:

| Room size | Production operations / bytes per relay |
| --- | --- |
| 2 | 3 / 696 |
| 8 | 4 / 1,616 |
| 16 | 4 / 1,936 |

An additional isolated coordinator-handoff cell retains the historical minimal
envelope so the removed box is compared like-for-like:

| Room size | Before operations / bytes per relay | After operations / bytes per relay |
| --- | --- | --- |
| 2 | 3 / 424 | 2 / 400 |
| 8 | 4 / 1,344 | 3 / 1,320 |
| 16 | 4 / 1,664 | 3 / 1,640 |

The checked-in operation ceilings are 2/3/3. Conservative byte ceilings are
416/1,336/1,656, exactly eight bytes below the former ceilings, even though the
current compiler's observed samples are another 16 bytes lower. This is an
allocation-free builder handoff result only; no relay-latency improvement is
claimed without a separate timing measurement.

## Validation and review

Focused adapter, cancellation, missing-stamp, terminal-unroute, reconnect
ordering, and allocation tests pass. `cargo fmt`, all-target/all-feature clippy,
the full all-feature test suite, `cargo deny`, CI/MSRV/workflow/document/LLM
policy scripts, hook readiness, and both worktree hook preflights pass. The
pre-commit hook reports a non-blocking 1,669 ms profile warning (531 ms changed
file discovery plus 809 ms Rust panic scanning); no hook implementation changed
in this session. Hosted PR review is recorded after publication.
