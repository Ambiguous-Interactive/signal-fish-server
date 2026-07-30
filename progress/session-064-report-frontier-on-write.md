# Session 064 — Confirm the delivery-report frontier on write

**Branch:** `dev/wallstop/session-064-report-frontier-on-write`
**Base:** `bfd9e2c` (PR #216, session 063)

## Objective

Advance `PLAN.md` to a green PR by carrying forward the highest-priority open
delivery-correctness defect, issue #218. A queued `DeliveryReport` moved the
successful socket-write counter frontier when it was popped, even though the
socket send/flush could still be cancelled or fail.

## GitHub state at start

- No open or draft PRs.
- No open Dependabot PR required incorporation. Dependency PR #215 was already
  incorporated into #216; #214 was rejected on measured graph/MSRV evidence.
- PR #216's exact final head was green across all 18 applicable workflows. The
  new `main` push at `bfd9e2c` was still running with no observed failures when
  audited.
- Gameplay-impact order: #218, #217, #211, then the research/tooling and design
  issues.

## Red evidence

The regression assertion
`a_queued_report_never_advances_the_unsupported_frontier` inspected
`wire_counters` immediately after popping a queued report. It failed:

```text
left:  latest.dropped_full = 1
right: latest.dropped_full = 0
popping a report must not advance the frontier before its write succeeds
```

No socket write had occurred. This proves the queue state described a frame the
socket sink had not successfully sent and flushed.

## Implementation

- `OutboundReceiver::prepare_report_for_wire` stamps a queued report against
  the last successful socket-write frontier immediately before the writer
  sends it. It is a read-only peek.
- `OutboundReceiver::confirm_report_written` advances the frontier only after
  the socket sink's send/flush future returns success.
- `write_queued_report`, the production seam used by `send_queued`, keeps
  prepare/send/confirm indivisible. A deterministic controlled-future
  regression proves the frontier stays unchanged while the send is pending,
  after send failure, a non-written disposition, and cancellation, then
  advances on successful send/flush.
- `pending_unsupported_report` builds directly on that one confirmed
  `wire_counters` value.
- `commit_pending_unsupported_report` retains its existing
  peek/write/commit behavior.
- The compensating `confirmed_wire_counters` field and pop-time
  `prepare_for_wire` mutation are removed.

## Verification

- Red regression reproduced before the fix.
- Focused queue and production-writer regressions pass after the fix.
- `cargo test --locked --all-features --lib`: 627 passed.
- `cargo fmt --check` and
  `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- `cargo test --locked --all-features`: passed across all non-ignored unit,
  integration, and documentation tests.
- The ignored H14 causal experiment passed with all 5,000 messages accounted
  for, four advisories, two fallback transmissions, and no slow-consumer
  eviction.
- `cargo deny --all-features check`, CI/MSRV/document/workflow/LLM policy
  scripts, hook-readiness, worktree pre-commit/pre-push checks, hook policy
  tests, and all 289 applicable `ci_config_tests` passed.
- Adversarial review initially required a production send/flush seam and
  deterministic pending/failure/cancellation coverage. The revised code added
  both; re-review reported zero code, test, or wording findings.

The remaining PR/reviewer and hosted-CI evidence will be recorded before the
session is complete.

## Next gameplay work

Issue #217 is next: use a paced-sender experiment to distinguish a genuinely
stalled recipient from a weak but steadily progressing one, then either make
that distinction explicit in production behavior or document the measured
operator-sizing decision.
