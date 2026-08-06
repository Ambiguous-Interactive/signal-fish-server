# Session 100 — H14 hosted evidence integrity

## Scope and prioritization

Remote triage found no open or dependency pull requests. Exact `main` head
`72313f0` had newly started push workflows, and six setup jobs failed while
GitHub could not resolve action download metadata. Those hosted failures are
infrastructure errors rather than evidence for a repository code change, but
the exact head is not called green until its reruns finish. Failed-job reruns
were requested for all 11 affected workflow runs because 15 dependent jobs had
been cancelled; attempt 2 showed no new failures at the last pre-publication
snapshot.

P53 and P56 each have one of 20 eligible scheduled first attempts. Issue #290
was accidentally closed when PR #292's prose said it "does not close #290";
the issue was reopened because its explicit hosted acceptance gate remains
incomplete. P56 was the highest-impact bounded work: its H14 selector ran third
inside a broad nightly job, so an unrelated earlier failure could skip the
attempt, and it retained neither a machine-readable outcome nor raw evidence.

## Failure-first evidence

A parser-based workflow guard first failed because `Verification Nightly` had
no stable H14 contract identifier, setup-aware execution condition, exact named
selector, attempt manifest, or artifact step. The existing P53 guard separately
failed after authoritative hosted logs showed that its requested 90-day
artifact retention is capped to 30 days by repository settings. The first P53
scheduled run succeeded on Linux, Windows, and macOS, but all three artifacts
expire after 30 days, contradicting PLAN and development documentation.

## Evidence contract

- `h14-capacity-v1` preserves the existing Nextest `profile ci`, all-feature
  binary, hosted job, precursor context, fixed workload, and every semantic
  oracle. An equality filter pins the one H14 test and fails if it selects no
  test.
- H14 runs after unrelated scenario/matrix failures only when checkout,
  toolchain, and Nextest setup succeeded and the job was not cancelled. Its
  exit code is captured and re-propagated, so a RED test still fails the job.
- An always-run manifest records event, run, attempt, SHA, runner image,
  toolchain, contract version, H14 outcome/exit, raw-log presence and size,
  evidence completeness, pass state, and eligibility. Eligibility depends only
  on a scheduled first attempt; RED, skipped, cancelled, incomplete, and
  missing attempts cannot disappear from the denominator.
- Manifest and raw log upload immediately after H14, before the later H10
  experiment can fail or consume the job timeout. Thirty days is the effective
  repository maximum and covers the 20-allocation cohort plus a 10-day margin
  at nominal daily cadence. The cohort is audited incrementally; artifacts must
  be downloaded before expiry if missed schedules stretch collection beyond
  30 days.
- Scheduled run `31070254464`, manually audited before the manifest existed,
  remains observation 1/20 because instrumentation changes no production,
  workload, runner, or oracle context. A future causal fix or workload change
  requires a documented cohort version bump.

## Validation and publication

The failure-first H14 and P53 retention guards are green after the workflow
changes. Local validation passed:

- the exact H14 Nextest selector under the CI profile;
- successful and upstream-skipped manifest smoke fixtures, including JSON
  parsing and eligibility/completeness assertions;
- `actionlint`, workflow hygiene, CI configuration, Markdown, documentation,
  LLM policy, and hook-readiness checks;
- worktree pre-commit and pre-push policy checks;
- locked all-target/all-feature Clippy with warnings denied; and
- the full locked all-feature test suite.

Adversarial review, publication, and exact-head hosted CI/reviewer evidence are
recorded before this session closes. The implementation review found and closed
three gaps before publication: retention language now distinguishes nominal
daily cadence from scheduled allocations, empty logs cannot be complete or
passing evidence, and executable fixtures cover success after an unrelated job
failure, H14 failure, upstream skip, and an empty-log false positive. A second
adversarial pass reported no remaining blocking, important, or moderate
findings.

This is an internal CI/evidence change. It changes no server API,
configuration, wire behavior, runtime behavior, performance, or security
contract, so no changelog entry is required.
