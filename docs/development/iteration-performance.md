# Development iteration and CI

## Evidence and root causes

The November 6–14, 2026 local archive contains 23 distinct PR heads. Sixteen
have completed observed workflows. Their slowest workflow completion proxy
has a median of 23.69 minutes and p95 of 27.38 minutes. The proxy uses
`run_started_at` to `updated_at`; it is not an exact merge critical path.
Snapshots are incomplete and do not establish a population-wide percentile.

The final session 702 snapshot records 50 jobs across 15 workflows. Excluding
skipped jobs and invalid timestamp intervals gives 258.67 observed job-minutes.
The previous substantial head records 240.05 job-minutes. These are durations,
not billed minutes. Source evidence remains in ignored session records; the
hosted runs provide durable references:

- [Session 702 PR CI](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/43641731685)
- [Same-head manual CI](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/43641884441)
- [Delivered change](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1649)

The causes are independent and compound:

| Cause | Observation | Fix |
| --- | --- | --- |
| Duplicate verification | Ubuntu ran in PR and manual CI for the same head | Normal delivery uses PR CI |
| Nested builds | Package and privacy checks consumed 265 + 172 seconds serially in session 701 macOS tests | Separate packaging qualification |
| Broad proof triggers | Four Lean fault shards consumed about 78 job-minutes in session 702 | Positive proofs for affected source; deep fault campaigns separately |
| Coupled test graphs | 110 integration targets; CI configuration tests exceeded 36,000 lines | Independent repository-check package |
| Repeated compilation | Quick Check type-checked all targets before lint and tests | Cheap prerequisite and parallel compilation |
| Unbounded coordination | Repeated consensus and late unrelated fixes restarted CI | One task owner and bounded review |
| Ignored archive traversal | Several local Markdown guard scans each exceeded 90 seconds walking session history | Enumerate tracked and non-ignored files with Git first |
| False green selection | Helper names could select zero tests with a warning | Fail empty requested selections |

Earlier experiments constrain the approach. Session 267 measured roughly
10.5 seconds for a warm library edit: macro expansion and type checking
outweighed the 1.2-second link. Mold changed 10.6 to 10.8 seconds; parallel
frontends changed 10.4 to 13 seconds. Neither improved that case. Session 433
removed save-time Clippy contention; session 651 reduced warm policy scans
from 9.368 to 2.788 seconds. Preserve these improvements. Do not assume a new
cache or linker solves repeated work.

No collected evidence proves a production capability unused. Remove features
only with usage or ownership evidence. Preserve active Rust, .NET, and Godot
contracts and distinguish those clients from repository reference fixtures.

## Daily workflow

Use `bash scripts/dev-loop.sh <test-name>` for red-green iteration. Use
`bash scripts/dev-loop.sh --clippy --changed` for changed-target validation.
Resolve shared helpers to every owning target. The four feature-independent
core modules use the existing proof-core package, which imports their actual
production source. Keep server behavior checks on named public integration
contracts where possible; root unit filtering still compiles the large unit
binary. Full CI retains root unit and integration coverage. Empty requested
selections must fail. One process owns a shared Cargo target directory at a time.

The default `scripts/test.sh` and `scripts/run-local-ci.sh` commands use the
scoped loop. Their `--deep` option retains broad local checks for diagnosis.
Pre-publication work runs the required local lint gate once. Hosted PR checks
own expensive tests, documentation builds, and dependency analysis.
Do not dispatch manual full CI merely to complete a routine PR.

Keep three verification cohorts:

- Routine PR: Linux runtime regressions, strict lint, protocol fixtures and
  short delivery, reconnect, and backpressure contracts.
- Affected changes: repository checks, packaging, privacy, feature combinations,
  client/transport contracts, documentation, and source-linked positive proofs.
- Deep verification: full platform suites, coverage, long faults and soak tests,
  complete proof fault campaigns, and broad compatibility sweeps.

Unknown changes select conservative coverage. Keep every target assigned.
Required verdicts must distinguish intentionally unselected jobs from failed,
cancelled, or missing selected work. Never hide a failure with a retry policy.

## Task and agent protocol

Start with one hypothesis, a reproduction or baseline, and observable acceptance.
Make one change. Capture red and green evidence, negative controls, environment,
revision, and discarded hypotheses. Accept, reject, or defer from the results.

Use one owner and up to two independent workers. Each worker gets ownership,
exclusions, commands, and expected evidence. Use one independent review, then
revisit concrete unresolved findings. Escalate unresolved correctness findings.
Do not repeat reviews to obtain unanimous agreement on every preference.

Coordination sessions resolve dependencies across components. Implementation
sessions execute a bounded task. Handoffs retain only revision, next action,
reproduction, results, and blockers. Concurrent implementation uses separate
worktrees and build directories; research and review need no competing build.

Measure accepted-task duration, rework, and compute before increasing team size.
[Anthropic's research experience](https://www.anthropic.com/engineering/multi-agent-research-system)
supports independent parallel exploration; it does not prove faster coupled coding.

For this networking server, use short gameplay contracts: join, ready, negotiate,
exchange, fallback, reconnect, and authority transfer. Reuse deterministic clocks
and seeded faults when they preserve the behavior under test. Large player
matrices and long network faults belong to deep verification. Use actual engine
headless checks for demonstrated integration gaps; avoid adding an engine runtime
to the server's ordinary loop.
[Godot supports headless execution](https://docs.godotengine.org/en/stable/tutorials/editor/command_line_tutorial.html).

## Measurement and acceptance

Targets are p95 warm scoped edit-test at most 10 seconds and routine required
PR verification at most 10 minutes. Neither historical data nor a passing build
proves that these targets have been achieved.

For local comparisons, use matched baseline and candidate worktrees, identical
hardware, toolchain, lockfiles, feature flags, and test selectors. Prewarm each
case. Alternate at least 20 real small source edits per case: protocol, server,
integration test, and tooling test. Record selection, lock wait, compilation,
linking, execution, exit status, CPU budget, and selected test inventory. A
no-change run measures only the warm floor; label it separately. Keep cold runs
separate. Use nearest-rank p95 and report sample count and failures.

For CI, retain job and step timestamps for 20 routine PR heads after rollout.
Separate queue delay, runner execution, and total job-minutes. Compare equivalent
cohorts and report cold cache cases. Audit the union of PR, affected, and deep
inventories against the full test inventory. Seed defects into selection fixtures
and contract tests to prove defects still fail the responsible cohort.

Enable replacement checks before removing required old names. Preserve deep
verification visibility. Report remaining misses rather than declaring success
from a single fast sample. Consider crate splitting or build archives only if
residual measurements justify their maintenance cost.

## Initial implementation measurement

On the existing aarch64 development container, 20 warm semantic-neutral edits
of the TOML script test source passed through the independent tooling package.
Median elapsed time was 2.567 seconds; nearest-rank p95 was 3.826 seconds.
The source bytes were restored after the experiment. Raw commands, outputs,
and timings are retained locally in
`progress/iteration-performance/tooling-changed-source-20.json`.
This is one candidate tooling case, not a matched baseline comparison or a
measurement of server, protocol, or integration edits. Full root strict Clippy
completed in 26.35 seconds. The broader local and hosted p95 targets remain
unverified until the specified samples are collected.

The affected CI configuration suite also exposed ignored-directory traversal.
Its old run was cancelled after 175.56 seconds with eight scans still running.
After Git-based inventory discovery, the final suite completed in 26.529 seconds
with 384 passing tests and one intentional skip. This is a bounded
before/after observation, not a repeated-run speed percentile. Added controls
exercise ignored directories and operation outside a Git checkout.

A hosted regression exposed Cargo's directory-style test discovery. Comparing
actual baseline and candidate Cargo metadata found 110 original integration
targets, including `tests/capacity_runner/main.rs`. The restored explicit
inventory preserves all 110 and adds one ownership guard. The guard covers
both top-level test files and directory-style `main.rs` targets.

The first corrected hosted head (`8e01ce11`) passed 50 checks with success or
applicable skips. Root runtime verification still took 14 minutes 10 seconds:
229 seconds compilation and 539.202 seconds test execution, plus setup and
controls. Its capacity runner consumed 183.595 test-seconds in the serialized
process group, including a 52.809-second payload case. This observation
motivated an affected capacity job and full deep coverage instead of routine
PR execution. It does not establish a new CI percentile.

The initial matched raw-unit experiment also rejected the assumption that
tooling isolation improves root unit compilation: eleven completed protocol
pairs had medians 27.596 seconds baseline and 26.853 seconds candidate. These
partial samples and interrupted attempts remain separate from final workflow
measurements. The 10-second target was missed for raw root unit rebuilds.
The next experiment reuses the existing proof-core test package for the same
production decision modules and public integration contracts for server work.
It introduces no production interface or new test framework.

Within each named-test invocation, the runner now builds once and uses native
nextest metadata for per-pattern validation and execution. Metadata is fresh
for every invocation and removed afterwards, so a later edit cannot reuse a
stale binary through this path. One unchanged-root probe measured 1.430 seconds
with reuse versus 1.705 seconds with repeated Cargo discovery. This single
sample is not a performance bound.

Hook profiling found another obsolete path: body-only skill changes searched
for legacy headings before rebuilding the full catalog. The replacement uses
the existing metadata parser and bounded batched Git blobs. Name, title,
description, file-list, mode, index, and generator changes retain full checks.
An isolated project-filesystem worktree profile passed in 664 milliseconds;
small staged body-only cases measured 225–234 milliseconds versus 291 before.
These fixture observations do not erase the recorded 1.2–1.3-second broad
commits. Normal source commits skip catalog work.
