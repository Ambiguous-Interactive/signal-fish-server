# Session 073 — Rate-limit accounting and observability integrity

## Scope

Issue [#236](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/236)
is a bounded safety and observability increment under #205. The phase audits
what the server actually enforces, repairs compound accounting, and removes
configuration and telemetry that implied behavior which never existed.

## Baseline and triage

- Started from clean `main` at `60884be`, the squash merge of PR #235.
- The root formatting, check, clippy, test, policy, documentation, dependency,
  hook, and pre-push baseline was green. The checkout-only CRLF copy of
  `docker-compose.yml` made a direct local yamllint invocation fail, while the
  tracked Git blob is LF and passes; no repository change was needed.
- Main's hosted workflows were green except the still-running aggregate CI
  workflow when this phase began; that workflow subsequently completed green.
- Open work was re-triaged. #207 remains the highest gameplay-performance
  umbrella, but its measured relay-serialization allocation follow-up is larger
  than this safety phase. The bounded rate-limit defects fit #205 and became
  scoped issue #236.

## Failure-first evidence

Before production fixes:

- `room_creation_respects_creation_and_join_budgets_atomically` accepted a
  second creation after the shared join budget was exhausted.
- `room_creation_cannot_overflow_an_exhausted_join_counter` panicked on
  `u32::MAX + 1`.
- `zero_cleanup_interval_does_not_kill_the_background_task` observed Tokio's
  zero-period interval panic.

The relay-serialization allocation baseline was also preserved for a later
issue #207 increment: v3 JSON used 8–9 allocations per relay, v3 MessagePack 10–11,
and mixed MessagePack fan-out 18 allocations at two players and 37 at eight or
sixteen.

## Implementation

- Room creation now checks creation and shared join capacity before incrementing
  either counter and reports the budget that rejected it.
- Direct-library zero windows clamp to a valid enforcement window; cleanup
  threshold multiplication saturates; live subsecond retry text rounds up.
- Expired player stats synthesize zeroes without mutating the admission window.
- Room and auth cleanup tasks hold weak references. A rate-limited synchronous
  auth middleware can be constructed outside Tokio without panicking.
- Production server paths now increment auth, room-creation, join-attempt,
  signal, and detailed signal-error rejection counters. Prometheus, dashboard
  JSON, and raw snapshots derive the aggregate from the same sampled category
  values, so concurrent observations cannot disagree.
- Permanently-zero minute/hour/day/reset/cache metrics were removed.
- `AuthMaintenanceConfig`, `Config.auth`, its server-constructor argument, and
  three unused cache settings were removed. Legacy unknown `auth` input remains
  tolerated and ignored, while serialized/default config no longer advertises
  it.
- Docs now state that creation consumes both room and join budgets,
  `max_signal_errors` budgets detailed errors before generic errors, auth uses a
  sliding window, and v2 hour/day values are legacy advisory projections.

## Verification

Focused validation is green:

- 41 limiter-related unit/integration tests;
- a real `EnhancedGameServer` production-path test proving exact five-category
  metrics, aggregate conservation, Prometheus values, and absence of all
  retired series;
- paused-time tests for atomic budgets, overflow, zero windows, weak ownership,
  stats observation, cleanup saturation, and retry boundaries;
- configuration migration coverage proving legacy `auth` keys parse but are
  not serialized.

The full local gauntlet is green: formatting, check, warning-denied clippy,
all-feature tests, fuzz-target check, dependency policy, advisory audit, CI and
MSRV policy scripts, documentation consistency, Markdown lint and links, hook
readiness, and pre-push validation. The adversarial review loop also completed
without remaining implementation, compatibility, documentation, concurrency,
lifecycle, or test-design blockers.

## Publication

- Pull request:
  [#237](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/237)
- Green reviewed implementation head:
  `f81dfc6b5d03904e8ba0e9c512c77abfc4c4404c`.
- All 15 applicable hosted workflows succeeded: Advanced Safety, Browser
  Interop, CI, Documentation Validation, Formal Verification, Fortress Interop,
  Fortress WASM Interop, Fuzzing, Link Check, Markdownlint, Mutation Testing,
  Spellcheck, Unused Dependencies, Verification Nightly, and WebRTC Interop.
  Dependabot auto-merge skipped as intended for a human-authored pull request.
- The first pushed head exposed one process miss: the new ignored progress file
  was force-added only after the earlier local Markdown scan, so a wrapped
  issue reference beginning with `#` was parsed as a malformed heading in
  hosted Markdownlint. The reference was corrected, the now-tracked set of 187
  Markdown files passed locally, and the replacement head passed hosted lint.
- Cursor Bugbot reviewed the green implementation head and found no issues;
  there are zero inline review threads. Copilot was requested through both the
  reviewer API and a tagged comment after every push but reported requester
  quota exhaustion. The repository exposes no distinct human contributor who
  can review a pull request authored by its sole human contributor.
- The pull request closes #236 and references #205. The measured relay
  allocation follow-up under #207 remains separate future work rather than a
  blocker for this safety phase.
