# Session 104 — Live-interop failure diagnostics

## Scope and priority

Session 103 / PR #306 merged green with no open pull request or dependency
update remaining. P53 and P56 still require their pre-registered hosted sample
windows, so the next actionable gameplay-path issue was #301: one Windows live
WebRTC cell and one Fortress WASM released-graph cell had failed
nondeterministically, then passed on identical-tree reruns.

The exact staged inverse of merged PR #306 was present when the session began.
The index and worktree matched `HEAD^` byte-for-byte, including the `lru`
downgrade and restoration of obsolete mock-only performance tests. It was
checkout residue rather than forward work and was restored to authoritative
merged `HEAD` before this session's branch was created.

## Native selected-pair evidence

PR #299 had made a missing event print the complete event-tag order, but the
cross-platform selected-candidate assertion still received only a drained
event slice. `ClientProcess` had captured stderr and checked the exit code, then
discarded that outcome before `ScenarioRun` performed the assertion.

The scenario now retains every reaped client exit code. A missing
`selected_candidate_pair` failure prints the exit code, complete tag order,
advertised local candidate set, recent JSON events, and captured stderr. A
deterministic regression was observed RED before the formatting seam existed
and now pins all four diagnostic categories without reproducing the hosted
transport failure.

## Fortress stall classification

The failed run's retained reports disprove a correctness regression and do not
support the issue's specific runner-I/O/download hypothesis. The joiner recorded
one denied `advance_frame` call in 602 active callbacks (0.166%) at Fortress's
eight-frame prediction boundary, yet reached current frame 601 / confirmed
frame 600, completed 1,317 messages in 10.698 seconds (123.1/s), drained a queue
whose peak was 8 and oldest age 17.5 ms, matched all nine checksums and both
1,327-message direction ledgers, and reported zero waits, loss, overflow,
retries, malformed input, or runtime errors. Its callback mean/p99/max were a
smooth 17.510/17.9/19.5 ms. The creator passed every gate, the identical-tree
rerun passed, and the observed base rate was one failure in 25 runs.

The WASM report now declares `max_stall_count: 1`. Zero and one recovered
prediction-window denial pass; two, a negative count, or a fractional count
fail the executable harness self-test. Wait recommendations and all prior
progress, throughput, cadence, queue, lag, rollback, checksum, conservation,
and exact-zero error gates remain unchanged. Native P12 remains zero-stall.
Because that threshold expands the exact report shape, the coupled Rust report,
browser configuration, room-ready signal, GDScript bootstrap-error report, and
harness validator now use schema v3; stale v2 exports fail closed.

## Changelog classification

These changes affect interoperability-test evidence and CI acceptance rather
than server, client, protocol, configuration, or runtime behavior. The
repository's path classifier nevertheless treats the standalone fixture and
native interop test as release-note scope, so `CHANGELOG.md` records the
maintainer-visible failure evidence and the exact one-stall boundary without
implying a shipped server behavior change.

## Validation

- Native diagnostic regression: pass.
- Native all-target strict Clippy: pass.
- Fortress harness syntax and 0/1/2/invalid self-test: pass.
- Focused Fortress structural policy regression: pass.
- Root all-feature tests (including 759 library tests and 311 CI-policy tests),
  all-target strict Clippy, formatting, Cargo deny, CI configuration, workflow
  hygiene, documentation, Markdown, MSRV, tooling-parity, hook-readiness, and
  worktree hook preflights: pass.
- Current-checkout native two-peer live WebRTC exchange: pass.
- Two adversarial review rounds: the first found the truncated diagnostic
  source, formatter-only regression, and stale schema version; an independent
  evaluator fixed all three, and the second round reported zero implementation
  findings.
- The local Fortress host test is unavailable because this environment has no
  Godot 4.5 executable for the fixture's `api-custom` build. The pinned hosted
  Godot/Emscripten/Chromium workflow is the authoritative remaining evidence.
- Hosted-CI and PR review evidence will be recorded before handoff.

## Follow-up boundary

P53 and P56 remain open until their registered hosted sample sizes are met; no
threshold is weakened and no completion is inferred here. PR #306 also requires
a fresh minor-bump Prepare Release run for 0.6.0 after this single session PR is
merged; it must not be stacked with the present issue work. Issue #307 records
that release follow-up with its version, diff-scope, validation, review, and
publication boundaries.
