# Session 128 — Atomic reconnect-claim lifecycle proof

## Scope and prioritization

PR #350 remained the only open pull request. Its 45 repository-owned checks
were green with five intentional skips and no unresolved review thread; the one
red check was the service-owned Copilot reviewer, whose requesting user had
exhausted quota. GitHub rejected a targeted rerun. No new scheduled nightly had
occurred, so P53 remained 6/20 per operating system and P56 remained 7/20.

The next correctness-ranked open item was #206. Its acceptance had already been
delivered by closed issue #210 / P23: the consistency and durability contract,
ADR-0008's single-home decision and technique survey, the bounded reconnect-loss
model with a seeded counterexample, and the real two-process split-brain catalog.
The session recorded that evidence on #206 and closed the completed broad
research issue without changing production behavior.

Issue #220 remained open. Comparing the current model inventory with gameplay-
critical production transitions found one concrete gap: Rust tests covered
individual reconnection-claim races, but no model composed duplicate valid
claimants, invalid certificate identity, stale claim handles, expiry cleanup,
failed restoration, and successful one-time consumption.

## Model and production correspondence

`formal/tla/ReconnectionClaimLifecycle.tla` models one disconnected-player
record and three sockets: two sharing the valid credential and one with an
invalid identity. Globally increasing claim epochs stand in for UUID claim IDs;
callers retain old handles so TLC can attempt a stale release or completion
after another socket owns a retry.

The positive configuration exhaustively interleaves claim, release, complete,
restore failure, expiry, and cleanup. Its invariants require:

- exact ownership between `Claimed` state and one active claimant/epoch pair;
- invalid credentials never reserve the record;
- released and completed claim generations remain disjoint, with at most one
  completion;
- expiry cleanup never removes an active claim;
- restore failure releases instead of consumes; and
- a retained stale handle cannot mutate a later reservation.

Four expected-failure configurations independently remove the identity check,
claim-ID check, claimed-record cleanup exclusion, and restore-failure release.
Each is registered with its exact invariant diagnostic. The first focused TLC
runs explored 90 distinct positive states and observed every expected failure.

The implementation already compares the active UUID before release/completion,
but no Rust regression exercised a retained old handle after a retry. The new
unit test proves both stale operations return false, preserve the pending active
record, and leave its rightful handle able to complete. The formal workflow now
triggers on `src/reconnection.rs`, with the exact path set enforced by
`ci_config_tests`.

## Changelog classification

This phase changes no binary behavior, public API, protocol, configuration, or
operator-visible output. The repository's changelog gate nevertheless
classifies the formal model and Rust correspondence test as notable non-internal
verification work, so `CHANGELOG.md` records it under Unreleased / Added.

## Validation and publication

Local validation completed:

- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `cargo test --locked --all-features` — green, including 835 library tests
- `cargo test --locked --lib reconnection::tests` — 30/30
- `cargo test --locked --test ci_config_tests` — 319 passed, one intentional
  expensive ignore
- `cargo test --locked --test ci_config_script_tests` — 5/5
- `cargo test --locked --test doc_consistency_policy_tests --test
  doc_consistency_script_tests` — 16/16
- `bash scripts/run-tla-model-check.sh` — the complete suite passed; the new
  positive configuration explored 181 generated / 90 distinct states and all
  four negative configurations observed their exact registered invariant
  violation
- `bash scripts/run-z3-proofs.sh`
- `cargo deny --all-features check`
- Actionlint, Markdownlint, workflow hygiene, CI configuration, documentation
  consistency, tooling parity, LLM policy, and all 1,001 internal-link checks
- hook readiness plus worktree pre-commit/pre-push; the profiled warm
  pre-commit completed in 971 ms
- `git diff --check`

Publication and hosted validation completed:

- commits `2a0f4fb` and `5ad313d` were pushed to the existing PR #350, keeping
  the session in one unstacked pull request;
- the implementation head `5ad313d5a78be6b31ce29276d92962e0a3ea8fd3`
  completed with 51 successful checks and five intentional skips, including a
  green TLA+ model check, all platform Nextest/lint lanes, coverage, MSRV, Miri,
  ASan, fuzz, and native/browser/Godot interoperability;
- final thread-aware review inspection found no unresolved thread or requested
  change, and GitHub reports the PR mergeable with a `clean` merge state;
- Copilot submitted another quota-exhausted COMMENTED notice. It created no
  failing check and is not actionable code feedback; and
- the exact-head proof evidence was recorded on issue #220, which remains open
  for future bounded model gaps.

The final PLAN/progress bookkeeping commit is documentation-only; its own
exact-head documentation checks are recorded on PR #350.
