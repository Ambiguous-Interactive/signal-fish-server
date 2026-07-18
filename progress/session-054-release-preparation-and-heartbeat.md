# Session 054 — release preparation and heartbeat liveness

## Scope

Repair the two independent failure classes exposed by generated release PR
PR #182, make both classes deterministic under local and CI validation, and carry
the remediation through an exact-head green pull request and reviewer closure.

## Baseline evidence

PR #182 was generated from `main` for version 0.4.1 and changes only the six
canonical release files. Its MSRV, Linux/macOS/Windows Nextest, coverage, and
AddressSanitizer jobs all fail the same documentation-consistency test because
the generated `[0.4.1]` comparison starts at `v0.4.0`, while the repository has
no corresponding dated changelog section. The dedicated documentation job
passes only because it checks out full history and tags; normal shallow
checkouts do not. The checker therefore has topology-dependent behavior.

The Real-World Scenario Profiles job fails independently in the H10 reliable
asymmetric-bandwidth scenario. Under sustained application backpressure, the
receive task awaits message delivery and cannot promptly read the peer's Pong;
the separate heartbeat task then closes the otherwise-active connection with
code 4003. The same failure is reproducible on `main`, so the generated release
diff did not introduce it.

## Approved invariants

- Reconstruct missing 0.1.1, 0.3.0, and 0.4.0 changelog releases from immutable
  tag snapshots and leave only post-v0.4.0 changes under Unreleased.
- Make changelog comparison validation depend only on repository files, never
  on whether tags happened to be fetched.
- Make release preparation reject an invalid baseline before mutating files.
- Probe only otherwise-idle WebSocket connections. Any valid inbound frame
  proves liveness immediately; stale or mismatched Pongs never satisfy a probe.
- Preserve bounded, sequential application processing and the existing timeout
  defaults rather than masking the race with a larger timeout.
- Add deterministic regression tests, observability, and durable LLM guidance.

## Red/green log

### Release history and preparation

Red evidence:

- A fixture whose `[0.2.0]` link skipped the missing `0.1.1` section passed when
  a local `v0.1.1` tag existed and failed without it.
- A fixture with Cargo version `1.2.3` and latest dated changelog release `1.1.0`
  was mutated successfully instead of failing preflight.

Green implementation:

- Reconstructed `0.1.1` (2026-02-23), `0.3.0` (2026-06-20), and `0.4.0`
  (2026-07-12) from annotated tag history. A one-time top-level Markdown bullet
  inventory proved conservation of all 132 existing blocks: 12 remain
  Unreleased, 43 belong to 0.4.0, and 77 belong to 0.3.0. The sole additional
  0.1.1 retrospective note is supported by its three-commit release range.
- The checker now validates exactly one first-position Unreleased section,
  strict unique descending semver headings, real calendar dates, one contiguous
  adjacent comparison chain, and a direct oldest-release link using files only.
- Release preparation now rejects Cargo/changelog drift, a missing/lightweight/
  non-ancestor baseline tag, an existing target tag/section, and lockfile drift
  before editing the six output files. It validates both the baseline and result.
- Data-driven checker cases, topology parity, byte-identical preflight failure
  assertions, and an integrated prepare-then-real-checker fixture are green.

Targeted evidence:

- `cargo test --test release_prepare_tests -- --nocapture`: 7 passed.
- `cargo test --test doc_consistency_script_tests -- --nocapture`: 5 passed.
- `cargo test --test doc_consistency_policy_tests -- --nocapture`: 10 passed.
- `bash scripts/check-doc-consistency.sh --skip-changelog-gate`: passed.

### Idle-only WebSocket probes

Red evidence:

- `inbound_activity_skips_probes_until_the_connection_becomes_idle` failed in
  1.23 seconds because the old fixed Pong-only loop emitted an RFC Ping while
  application Ping frames were arriving every 200 ms.
- The historical H10 failure consistently reached roughly 22.43 seconds in its
  first reliable backpressure cycle before the sender closed `4003`, proving
  the receive-loop head-of-line window exceeded the 10s + 5s probe contract.

Green implementation:

- One O(1) Tokio watch state tracks inbound generation, serial handler activity,
  and at most one nonce/evidence record. A decoded non-Pong frame publishes
  activity before parsing or any await; active processing keeps probes skipped.
- Writer-side generation recheck closes the command/write race. Exact matching
  Pongs record RTT; first post-write non-Pong activity cancels; wrong/stale Pongs
  do nothing; genuine silence still closes 4003. Disabled probes avoid receive-
  side state mutations.
- Added skipped/cancelled activity counters to JSON snapshots and Prometheus,
  documented the idle-only tradeoff and nearly `2 × interval + timeout`
  fixed-tick worst case, and replaced the LLM heartbeat sample that taught the
  head-of-line bug.
- Strengthened cancellation coverage beyond the original deadline and proved a
  later idle probe still succeeds. The state-table tests cover writer-race skip,
  active-handler skip, first-evidence wins, wrong nonce, inclusive deadline,
  late evidence, cancellation, and silence.

Real-world evidence:

- Five complete serialized H10 repetitions passed. Each ran two ~22.4s reliable
  backpressure cycles plus the 60s volatile phase, conserved every volatile
  offer as delivered or exact-reported dropped, recorded zero ping timeouts,
  and exercised activity-based skipping.
- The fifth attempted repetition exposed a separate observation flake: an
  already-congested Linux TCP path can reset after the server's slow-consumer
  decision before tungstenite surfaces the semantic Close. The test now accepts
  only the two exact reset representations and only after both the server-side
  slow-consumer metric and healthy-watcher departure independently prove the
  intended cause. The replacement full repetition exercised this path and
  passed reconnect, delivery, and liveness assertions.
- Focused WebSocket unit tests: 3 passed; `server_ping_e2e`: 16 passed;
  Prometheus renderer tests: 5 passed; targeted all-feature Clippy: passed.

Adversarial audit:

- Confirmed bounded race-safe state and no timeout inflation or unbounded queue.
- Its actionable findings (post-deadline cancellation survival, fixed-tick bound,
  non-Pong wording, and disabled-mode overhead) were implemented and retested.

## Full-project verification

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- `cargo test --locked --all-features`: the first compile attempt exhausted the
  runner's per-process allocation while writing an incremental dependency graph;
  the coverage-equivalent bounded rerun with `CARGO_BUILD_JOBS=2` and
  `CARGO_INCREMENTAL=0` passed every executed unit, integration, policy, and
  documentation test. Only explicitly ignored nightly/on-demand tests skipped.
- `cargo deny --all-features check`: advisories, bans, licenses, and sources
  passed; only the repository's acknowledged duplicate-version warnings remain.
- `bash scripts/check-ci-config.sh`, `check-doc-consistency.sh --changed-files`,
  `check-workflow-hygiene.sh`, `check-llm-file-sizes.sh`,
  `check-llm-example-files.sh`, and `check-msrv-consistency.sh`: passed. Reported
  warnings are pre-existing advisory items outside this change's scope.
- `pwsh ... scripts/check-hook-readiness.ps1`: passed with zero warnings.
- Worktree pre-commit and pre-push PowerShell preflights: passed. The profiled
  pre-commit run took 1,288 ms and reported its non-fatal 1,000 ms budget warning.
- The first published exact-head Markdownlint run exposed two reconstructed
  changelog double blanks and a paragraph-leading `#182` parsed as a heading.
  Those three formatting defects were reproduced from the job log, corrected,
  and `bash scripts/check-markdown.sh` passed locally before republishing.
- The next macOS Nextest run exposed a Bash 3.2 portability difference: under
  `set -u`, expanding a declared-but-empty array fails before the first release
  is appended. The checker now cardinality-guards its only pre-population array
  expansion and removes the unused parallel date array; the macOS CI lane is
  the permanent regression oracle for the platform's system Bash.
- `git diff --check`: passed.
