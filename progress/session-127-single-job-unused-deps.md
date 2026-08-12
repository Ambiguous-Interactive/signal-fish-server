# Session 127 — Redundant CI runner removal

## Scope and prioritization

The session began from clean `main` at merge commit `5349551`. There were no
open or draft pull requests and no open Dependabot updates to incorporate.
P53/#274 and P56/#290 remain hosted-evidence phases under their unchanged
20-attempt cohorts; the concrete production fixes behind both have already
landed, so an unrelated source change would not advance their acceptance
evidence. The broad safety, resilience, formal-method, and performance issues
remain intentionally open-ended. Issue #345 was the highest-confidence
actionable CI-cost item, but only after proving that its legacy status alias had
no external merge-policy consumer.

The latest retained scheduled H14 manifest (run `31561156680`) is eligible,
complete, and green, advancing P56 from the stale 6/20 count to 7/20 without
changing its workload or cohort identity. It records 5,000/5,000 compatible
deliveries, exact fallback accounting, 0.01x amplification, approximately
32,739 B/s compatible throughput, and zero slow-consumer evictions. P53 remains
at 6/20 per operating system.

## Repository policy audit

The connected GitHub app and public repository endpoints supplied the effective
repository-policy state:

- `main` is protected, but its effective classic branch protection reports an
  empty `required_status_checks.contexts` list and an empty
  `required_status_checks.checks` list.
- The repository has one active ruleset, `Copilot review for default branch`
  (ID `14802448`). Its complete rule list contains deletion protection,
  non-fast-forward protection, and Copilot code review; it contains no required
  status-check rule.
- The effective rules endpoint for `main` reports the same three rules and no
  status requirement.

This proves that `Unused Dependencies / Check Unused Features` is historical
compatibility output rather than a required branch gate. Removing it cannot
leave a pull request waiting for an expected status.

## CI speed and cost

The `unused-deps.yml` workflow now has one hosted job. The retained
`Check Unused Dependencies` job still runs cargo-machete and cargo-udeps behind
one checkout, pinned nightly toolchain, Rust cache, and checksum-verified binary
installation. Cargo-machete remains gating. Cargo-udeps remains informational
and still runs after a machete failure unless the workflow is cancelled.

Only the setup-free `Check Unused Features` compatibility job was removed.
Issue #345's baseline attributes about 21.6 seconds / 0.36 raw runner-minutes per
trigger to its redundant runner allocation. With one pull-request run and one
main push per merged change, 100 merged changes avoid about 72 raw
runner-minutes before counting synchronization pushes. The configuration regression
now requires exactly one workflow job and rejects reintroduction of the
obsolete alias while preserving the analyzer setup, order, commands, and
failure policy.

The workflow audit found four more avoidable runner allocations on ordinary
push and pull-request runs:

- Documentation validation's inline-code job checked out the repository and
  printed that its future validation was skipped. Removing it preserves all
  four required documentation gates plus the auxiliary shellcheck job and saves
  one Linux runner allocation on matching changes.
- Fortress, Fortress WASM, and native WebRTC interop each repeated cargo-deny on
  one standalone graph. Central `CI / Dependency Audit` already checks all five
  tracked Cargo graphs on every push, pull request, and daily schedule, and its
  dynamic inventory test fails when any graph is absent. The interop-local
  audits remain enabled only for `workflow_dispatch`, because central CI has no
  manual trigger and a selected unmerged ref still needs dependency-policy
  coverage. The three PR runs (`31559147837`, `31559147843`, and `31559147808`)
  displayed 34, 62, and 39 seconds—2.25 raw and four per-job-rounded Linux
  runner-minutes per typical source PR. A policy regression now permits the
  known local cargo-deny action or direct-command forms only behind that exact
  dispatch-only condition.

Larger possible savings—fuzz build reuse, link-check consolidation, and moving
the separate documentation shellcheck into an existing job—remain deferred
until their cache, trigger, and failure-semantics boundaries are measured.

## Changelog classification

This does not change the server binary, public API, wire protocol,
configuration, or gameplay behavior. It does change contributor-visible CI
execution and touches the native client's dependency-policy surface, so the
`Unreleased / Changed` section records the preserved trigger coverage and
runner-allocation reduction.

## Verification and publication

The exact commit candidate passes the complete mandatory local suite:

- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `cargo test --locked --all-features` (including an isolated rerun of all nine
  mTLS process tests after a deliberately parallel local Clippy/test run
  contended over the shared Cargo target)
- `cargo deny --all-features check` and `scripts/check-ci-config.sh`
- documentation consistency, workflow hygiene, tooling parity, Markdown, LLM
  file/example, and hook-readiness policy checks
- worktree pre-commit (964 ms profiled) and pre-push (595 ms) preflights
- both documentation policy test binaries and all 320 CI configuration tests
  (319 passed, one intentionally ignored)

Four adversarial review rounds found and closed stale branch-protection claims,
manual-dispatch coverage, dependency-audit regression, timing/accounting, and
documentation precision gaps. The final exact-diff review reported zero
findings. Pull-request publication, review state, and hosted status evidence
remain to be recorded.
