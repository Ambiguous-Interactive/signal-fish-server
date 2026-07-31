# Session 071 — Control deadlines and healthcheck audit

## Objective

Advance the highest-value ready in-repository work while carrying the merged
session-070 state forward to one green pull request.

## Baseline and prioritization

- `main` and `origin/main` began at session 070's merge commit `664f415`.
- The connected GitHub repository had no open pull request.
- Open issues were #204, #205, #206, #207, #213, #220, and #226.
- Main's merge commit was fully green across all 14 applicable workflows.
- Two independent audits selected #226 as the highest-value fully specified
  task. A gameplay audit then found a production deadline race in the same
  delivery boundary P24 had just hardened, so that complete failure class took
  priority within this session.

## Issue #226 — multiline Docker healthcheck parsing

### Red evidence

`scripts/check-ci-config.sh` exited successfully but warned
`No HEALTHCHECK directive found in Dockerfile.` The production Dockerfile has a
live `HEALTHCHECK` split across two physical lines, while the audit searched
only a single line containing both `HEALTHCHECK` and `localhost`.

### Fix

The audit now assembles Dockerfile logical instructions without shell
evaluation. It:

- honors default backslash continuations plus CRLF;
- removes full-line comments before instruction assembly;
- rejects RUN/COPY/ADD heredoc input, including ONBUILD-wrapped forms, that it
  cannot safely inspect;
- resets healthcheck state at each `FROM`, so only the final runtime stage
  counts; and
- accepts only the repository's real `curl` probe of the exact
  `http://localhost:PORT/v2/health` endpoint.

Missing, disabled, malformed, invalid-host, wrong-path, wrong-command,
wrong-port, heredoc-only, or builder-stage-only probes are blocking errors.
The pre-existing `EXPOSE` and authentication checks remain unchanged.
Black-box tests execute the actual script with stubbed cargo-deny tooling
across the fixture table and against the checked-in repository.

## Strict control-capacity deadline sweep

### Red evidence

The ordinary relay wait added in P24 already used an absolute timer-first
deadline because Tokio's `Timeout` polls its inner future before its timer.
Three control-message paths did not:

1. initial room-join/reconnect transition reservation used
   `tokio::time::timeout`;
2. conditional single-recipient delivery used an unbiased `select!`; and
3. conditional reservation for lifecycle broadcasts, tailored session plans,
   replans, and two-phase room transactions used another unbiased `select!`.

Paused-time data-driven regressions fill the control queue, poll each wait
pending, advance to or beyond the deadline without polling it, return capacity,
and then poll again. The original initial-transition implementation returned
`Delivered`, proving that expired capacity waits could revive.

### Fix

Every affected full-queue path now uses an explicit biased selection:

1. drain completion, where applicable, preserves cancellation precedence;
2. closed queues and stale classified generations retain their terminal
   `ChannelClosed`/`Canceled` classification;
3. the absolute deadline resolves an otherwise-live, still-eligible wait as
   `SlowConsumer`; and
4. only unexpired capacity may reserve the queue.

The matrix covers all three primitives, legacy and classified queues, and exact
and post-deadline boundaries. It proves no late enqueue, the requested
`SlowConsumer` close reason, exact accounting, closure precedence, and
generation-fence precedence. Existing positive capacity recovery,
drain-cancellation, phased-transaction, and replacement-connection tests remain
the complementary behavior gates.

## Scope audit

The control-capacity sweep is complete. Adjacent socket-write, authentication,
and idle-read deadlines also use Tokio timeout/select boundaries, but they own
different I/O and security policies rather than queue admission. They require
an explicit inclusive/exclusive contract and deterministic write/read seams;
issue #233 tracks that broader #205 audit instead of silently expanding P25.

## Changelog classification

The control deadline correction changes observable runtime reliability and is
recorded under `[Unreleased] / Fixed`. The healthcheck parser is internal CI
tooling and is not separately release-noted.

## Adversarial review

The first healthcheck review rejected the draft because the repository test
depended on cargo-deny being installed, builder-stage healthchecks leaked into
the runtime result, invalid grammar and inert comments could satisfy the port
substring, hostname boundaries were loose, and the Docker escape directive was
ignored. The first four failure modes are fixed and fixture-covered. General
escape-directive support was deliberately excluded when the implementation was
narrowed to issue #226's default-backslash contract; non-default continuations
cannot satisfy the audit.

The gameplay audit independently found the three expired-capacity paths,
verified their production callers, and confirmed that the revised
drain-deadline-capacity order closes the class without changing cancellation
semantics.

The full adversarial pass then challenged parser-directive and heredoc edges,
first-stage `EXPOSE` leakage, arbitrary commands mentioning the URL, terminal
queue-state precedence at expiry, missing exact/classified/close-reason
oracles, an overbroad changelog claim, and an incorrect session-070 reviewed
head. Dedicated healthcheck and runtime passes evaluated every finding. The
healthcheck implementation was ultimately narrowed back to issue #226's
verified contract instead of introducing a partial Docker grammar or new
final-stage `EXPOSE`/`ENV` compatibility policy.

After the final EOF-continuation and documentation corrections, the independent
healthcheck reviewer, runtime reviewer, and complete-diff reviewer all returned
explicit zero-revision verdicts.

Hosted Cursor Bugbot subsequently identified one additional bypass: `ADD` and
`ONBUILD ADD` heredoc bodies could still impersonate `FROM` or `HEALTHCHECK`
instructions because the fail-closed matcher covered only `RUN` and `COPY`.
The finding was accepted. The matcher now covers `RUN`, `COPY`, and `ADD`, plus
their `ONBUILD` wrappers, and a compact opener matrix verifies that fake stage
and healthcheck instructions in every covered heredoc body remain blocking.
A hosted re-review of that follow-up diff was pending at that point, so the log
did not yet claim a zero-revision verdict for the updated hosted-review state.

Cursor Bugbot's hosted re-review of implementation head `2fd062e` found no new
issues. The accepted finding's inline thread is resolved, and the final thread
audit found no unresolved feedback.

## Verification

- The new control-capacity regression failed red on the original implementation
  and passes after the three-site sweep.
- All 20 message-coordinator tests pass with every feature enabled.
- All healthcheck audit fixtures and the checked-in repository audit pass.
- `cargo fmt --all -- --check`, strict all-target/all-feature clippy, and the
  complete `cargo test --locked --all-features` suite pass on the production
  implementation.
- After the hosted follow-ups, strict clippy and the focused healthcheck fixture
  suite pass; hosted nextest passes on Linux, macOS, and Windows.
- Shellcheck, `cargo deny --all-features check`, and `git diff --check` pass.
- Detailed follow-up issue #233 records the adjacent strict-deadline audit.
- The repository documentation, MSRV, workflow-hygiene, markdown, link-text,
  hook-readiness, pre-commit, and pre-push policy gauntlet passes.
- The first hosted head exposed a Windows-only denied unused-variable lint in
  the test helper. Separate Unix/non-Unix definitions preserve chmod/no-op
  behavior and the corrected Windows clippy lane passes.

## Publication

- Pull request:
  [#234](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/234)
- Green reviewed implementation head:
  `2fd062e9448430884ae8a30f45afd590109f5cc7`.
- All 11 applicable workflows succeeded: Advanced Safety, Browser Interop, CI,
  Documentation Validation, Fortress Interop, Fortress WASM Interop, Link
  Check, Markdownlint, Spellcheck, Unused Dependencies, and WebRTC Interop.
  Dependabot auto-merge skipped as intended for the human-authored pull
  request.
- Cursor Bugbot found no new issues on the green implementation head, the
  accepted earlier finding is resolved, and no inline review threads remain.
  Copilot was triggered through both the review API and tagged comments after
  each push but reported requester quota exhaustion. The repository has no
  distinct human contributor available to review a pull request authored by
  its sole human contributor.
- The pull request closes #226 and references #205 plus the new detailed
  follow-up issue #233.
