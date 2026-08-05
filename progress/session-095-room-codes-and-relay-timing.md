# Session 095 — Room-code closure and relay timing observations (P52/P53)

## Scope and prioritization

Remote triage found no open pull requests or dependency updates. Main was at
`ec3be74` (PR #282); the previous main revision was fully green, while the newest
revision's CI, Advanced Safety, and Docker Publish runs were still in progress
with no failures at session start. Issue #250 remains the highest raw
gameplay/security concern, but changing the authentication trust boundary needs
an explicit compatibility and credential decision. The other open safety,
resilience, optimization, analyzer, formal-methods, and design-system issues are
broad campaigns or off the immediate gameplay path.

Two bounded gaps were actionable. Audit found that accepted room-code settings
could create rooms no second player could join; this became #283 and the complete
P52 fix. Issue #274's remaining timing/lane question cannot be decided from the
two recorded macOS outliers, so P53 starts unbiased hosted evidence collection
without claiming that the decision is complete. Generated-code collision retry
behavior is related but separable and is tracked in #284.

## Failure-first evidence

Before P52, `ProtocolConfig::validate` accepted `room_code_length = 0`, and the
server accepted prefixes such as `EU-` or values at least as long as the total
code length. Generation could therefore return an empty or punctuated code (or
silently ignore an oversized prefix), while the explicit join validator required
an exact-length ASCII-alphanumeric code. A focused test first failed to compile
because no generation/admission invariant existed.

Before P53, the ordinary matrix either enforced a platform wall-clock limit or,
on macOS, merely printed one process's timing. It did not preserve a repeated,
machine-readable hosted distribution, and using the gated selector for evidence
would have censored the very outliers under investigation.

## Implementation

- `ProtocolConfig::validate_room_code_generation` composes the protocol checks
  with the server prefix and rejects blank, non-ASCII-alphanumeric, full-length,
  and oversized prefixes. Both top-level config validation and direct server
  construction call it before accepting work.
- Table/property tests cover invalid settings and accepted generator-to-validator
  closure. The live E2E path creates a room with a lowercase configured prefix,
  then has a second client join using the exact normalized generated code.
- Documentation now describes the invariant and corrects the previously invalid
  hyphenated region-prefix example.
- `Relay Timing Observations` runs five all-feature six-cell matrix repetitions
  on each hosted desktop OS, writes versioned JSONL plus raw logs and an attempt
  manifest, and uploads them even on failure. The dedicated ignored selector
  keeps every semantic oracle but disables the wall-clock gate so the sample is
  not selection-biased.
- The pre-registered #274 decision uses the first 20 consecutive scheduled,
  first-attempt allocations per OS under one workload/toolchain cohort. The
  GitHub run ledger keeps RED, cancelled, missing, and incomplete attempts in
  the denominator. Manual/PR/rerun observations are diagnostic only.
- Dedicated-process evidence can justify only an equivalently isolated,
  all-feature PR timing job. The candidate 250 ms ceiling requires every eligible
  observation below it and a maximum at or below 125 ms; otherwise that platform
  remains correctness-only.

## Red-green evidence

The focused room-code test was RED before the validation API existed, then the
invalid-configuration table, generator/join closure property, direct-constructor
guard, and two-client generated-code join all passed. The relay observation
workflow's configuration test was RED while the workflow was absent, then passed
after the workflow and exact selector were added. A local diagnostic run produced
all six JSONL rows while preserving delivery, conformance, zero-backpressure, and
zero-eviction assertions.

## Validation and review

The first independent adversarial pass found three evidence-integrity gaps in
P53: feature-minimal isolated observations could not justify the broad
all-feature Nextest lane, complete-only samples allowed survivorship bias, and a
substring policy test could pass with dead or weakened workflow text. The lane
now runs all features, scopes any future threshold to an equivalently isolated
PR job, records explicit attempt outcomes, and pre-registers the GitHub run
ledger as the denominator. Its structural YAML test pins the live job, matrix,
steps, failure semantics, artifacts, and each exact Cargo invocation. A second
pass found that the all-feature marker could still be borrowed from the unit
guard; exact logical-command comparison closed that bypass. The third pass and
the independent production/config review both reported zero findings.

The authoritative local validation completed successfully with:

- formatting plus strict all-target, all-feature Clippy with warnings denied;
- `cargo test --locked --all-features`, including 736 library tests and all
  integration/policy suites;
- focused invalid-config, generator/join closure, direct-constructor, live
  two-client join, JSONL, semantic-profile, and workflow-policy tests;
- `cargo deny --all-features check` (only the repository's accepted duplicate
  dependency/license-not-encountered advisories);
- CI config, documentation, MSRV, workflow hygiene, LLM size/example, tooling
  parity, Actionlint, and YAML lint checks; and
- hook readiness plus worktree pre-commit and pre-push preflights. The first
  profiled worktree pre-commit was cold at 2.8 seconds; a warm rerun reduced this
  to 1.8 seconds, with unchanged-file discovery and the Rust panic scan accounting
  for 1.38 seconds. The actual staged hook reduced discovery to 129 ms and passed
  every check, but still completed in 1.6 seconds because the unchanged Rust
  panic scan took 882 ms plus PowerShell/check overhead. No hook or hook-policy
  path changed in this session; the over-target warning is recorded rather than
  expanding the gameplay/workflow PR into an unrelated hook rewrite.

## Publication and hosted failure analysis

Pull request #285 published the two scoped commits at `ecef57f`. Eighteen
hosted workflows completed successfully, including CI, Advanced Safety,
Fuzzing, every interop lane, and the new three-platform Relay Timing
Observations run; both Dependabot-only invocations skipped as designed. Cursor
was explicitly triggered on the exact head. Copilot reached its repository/user
review quota and returned no code review, and no independent human reviewer is
available beyond the PR author.

Verification Nightly's Real-World Scenario Profiles job was the sole failure.
Every preceding profile passed, then H10 completed both expected reliable
slow-consumer/reconnect cycles before its volatile terminal-accounting frontier
missed the 60-second post-fault ceiling by less than one second. The exact
hosted selector reproduced under `taskset -c 0`: it preserved both `4002`
cycles, stayed connected throughout the volatile phase, conserved every offer,
and passed in 158.399 seconds, but the healthy path's maximum interarrival gap
reached 52.657 seconds and accounting completed almost exactly at the old
ceiling. This classifies the failure as a loaded-runner sender-ingress backlog,
not a production relay stall or lost terminal outcome.

The accounting condition remains event-driven and unchanged. Its ceiling now
uses H10's existing 90-second phase budget, giving the 60-second offered phase
bounded scheduling headroom without weakening any eviction, reconnect,
delivery, gap, liveness, or conservation oracle. A dedicated waiter also emits
the target and complete per-class counter snapshot if the frontier genuinely
fails again. Adversarial review found that an early victim termination could
otherwise leave the two-recipient target unreachable for the entire wider
ceiling; the waiter now races the observer and immediately reports its exact
Close, transport-error, or EOF classification. The constrained exact Nextest
selector passes at the reproduced 158-second boundary, and the follow-up head
is subject to the same full hosted gate and reviewer loop.

Cursor's delayed first-head review found that Verification Nightly's broad
`--run-ignored all` selector also picked up the new observation-only test. That
duplicated the six-cell clean matrix in a contended job without recording its
results. The nightly command now explicitly skips the dedicated observation
selector while retaining its existing ordinary, fault, knee, and diagnostic
coverage; the parsed workflow policy test pins that separation.

Cursor's delayed second-head review also tightened the new H10 early-terminal
diagnostic. It now states that termination happened during the accounting wait
and preserves the same slow-consumer, ping, probe, activity-reaper,
healthy-watcher, delivery, and proxy-termination evidence as the experiment's
later terminal branches.

P53 itself intentionally stays open after publication while the pre-registered
scheduled cohort accumulates.
