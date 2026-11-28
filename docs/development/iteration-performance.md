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

## Routine PR CI bound (#953)

Issue #953 required a repeated routine-PR bound without removing checks. The
2026-11-28 cohort covers twelve consecutive merged PR heads in the Linux
`Nextest (ubuntu-latest)` job, plus one same-head repeat. The workflow,
toolchain, all-feature nextest command, allocation ceilings, non-TLS control,
and H14 control are unchanged. Cache state comes from the retained
`Swatinem/rust-cache` log line. `none` means no exact cache key existed; those
jobs compiled the full graph.

| PR | Head | Cache | Job | Nextest step | Execution | Tests |
| --- | --- | --- | ---: | ---: | ---: | ---: |
| [#1817](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/45959806511/job/140374870424) | `1c3a6c603` | hit | 578 s | 519.8 s | 375.575 s | 3,261 |
| [#1819](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/45987646725/job/140467490957) | `eedcfc37a` | none | 559 s | 501.0 s | 361.418 s | 3,261 |
| [#1820](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/45991637413/job/140480848138) | `8ed871496` | none | 728 s | 614.0 s | 376.388 s | 3,261 |
| [#1821](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46041593673/job/140650557306) | `4a75e42fd` | none | 663 s | 558.3 s | 369.920 s | 3,261 |
| [#1822](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46046061580/job/140665878841) | `e7a8ceb86` | none | 584 s | 520.7 s | 363.600 s | 3,261 |
| [#1824](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46055104900/job/140695731478) | `6bd1abf86` | none | 645 s | 548.4 s | 369.553 s | 3,262 |
| [#1825](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46059833716/job/140711459536) | `7f4551317` | none | 675 s | 601.1 s | 374.483 s | 3,262 |
| [#1827](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46068436351/job/140739678424) | `57211f6e9` | none | 629 s | 555.7 s | 368.309 s | 3,269 |
| [#1828](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46078381083/job/140771503054) | `1bd23d2a0` | hit | 570 s | 481.0 s | 368.355 s | 3,269 |
| [#1829](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46085083623/job/140792819634) | `ca369a82e` | none | 715 s | 603.6 s | 375.148 s | 3,269 |
| [#1830](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46090004406/job/140808616082) | `dd62e6d92` | hit | 523 s | 467.3 s | 364.325 s | 3,269 |
| [#1831](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46105113007/job/140856127919) | `c0e787645` | hit | 537 s | 454.4 s | 363.768 s | 3,272 |
| [#1831 attempt 2](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/46105113007/job/140885803873) | `c0e787645` | hit | 551 s | 489.6 s | 370.236 s | 3,272 |

Every sample stays below the original 824-second job reference. The largest
job is 728 seconds; the largest nextest step is 614.0 seconds, below the
815-second #952 observation; the largest execution is 376.388 seconds, below
the 453.599-second #950 observation. All samples ran at least 3,261 tests with
47 skipped. The earlier reference heads ran 3,221–3,228 eligible tests, so this
bound does not come from a narrower suite. The same-head repeat moved the job
from 537 to 551 seconds, so the result is not a single fast sample.

Limits: all samples use shared GitHub-hosted runners and do not expose outer
CPU quotas. The heads differ within the comparable PR cohort, so this is not a
broad same-source experiment. It bounds only the Linux Nextest job; macOS,
Windows, coverage, and deep verification stay separate. Raw logs and extracted
metadata remain under the ignored `progress/session-953-ci-runtime/` path.

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
measurements. The 10-second target was missed for raw root unit rebuilds on the benchmark artifact filesystem.
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

Filesystem probes explained extra warm-up rebuilds: the Windows-backed 9p
workspace truncated explicit nanosecond timestamp updates, while generated
files retained fractional timestamps. Cargo reported generated `private.rs`
newer than its fingerprint. The root target directory is already an ext4
named volume; nested package defaults were still on the workspace mount.
The runner now inherits the root Cargo target directory for tooling and proof
batches, with a function-local export. Explicit target overrides remain intact.
A real default-cache core check passed with a 3.35-second first build.

The controlled paired benchmark deliberately retains the same explicit
Windows-backed target override for both heads because native storage was
nearly full. Its raw-unit figures must not be generalized to default native
storage. Its measured routing comparison remains separate from default-cache
validation. Only inactive Cargo CI-profile output was cleaned: 7,470 rebuildable
files, 2.9 GiB. Session history, source files, user files, and other caches were
preserved. Raw timestamp and Cargo fingerprint evidence remains in the local
iteration-performance record.

## Controlled runner results

Twenty alternating matched edit pairs per case completed with all 160 selected
regressions passing. Baseline: `ce9df996`; runner candidate: `dcd182d0`.
Sources used isolated native temporary worktrees; both artifact caches used the
same explicit Windows-backed 9p override. Later default-cache and native-client
fixes do not run on these selected, explicitly overridden paths. Do not pool
these results with the earlier raw-unit experiment or claim a default-native
storage percentile from them.

| Existing regression scope | Baseline p95 | Candidate p95 | Change |
| --- | ---: | ---: | ---: |
| Pure protocol decision | 27.072 s | 1.766 s | 93.48% faster |
| Public server contract | 18.800 s | 19.395 s | 3.16% slower |
| Protocol integration | 8.088 s | 8.501 s | 5.12% slower |
| TOML tooling check | 6.437 s | 2.133 s | 66.87% faster |

These are actual runner invocations, including selection, compilation, metadata
validation, and execution. Core and tooling isolation improve those loops.
The server compilation bottleneck remains, and the local 10-second target is
not universal. Retain that finding under the existing crate-boundary and CI
measurement work, issues #642 and #953.

Raw local commands, outputs, source hashes, invalid prewarm attempts, filesystem
probes, and summaries remain under `progress/iteration-performance/`.
The completed raw report is `matched-wrapper-20.json`; its SHA-256 is
`cba3d25b3e1ec4041023250601c490c1520080722142aa0dfc2d84a0b65b934a`.

## Runtime crate decision (#642)

Defer a full runtime crate split. Use the existing proof-core and tooling
boundaries, and public server contracts for routine server edits. A runtime
split adds publishing, feature, visibility, and mutation-coverage work. The
measured public-contract path does not currently justify that cost. Accept
slower private unit-test edits as a known exception. Revisit this decision if
a required server workflow repeatedly exceeds the target on native storage.

Session 708 measured 20 warm edits to `src/server.rs` with
`bash scripts/dev-loop.sh =test_multi_player_room_scenario`. All 20 selected
regressions passed. The default artifact directory used ext4, with no target
override or concurrent Cargo process. The host was Linux aarch64 with Rust
1.94.0. Median elapsed time was 6.430 seconds; nearest-rank p95 was 7.126
seconds. Two separate warm-ups took 8.587 and 1.584 seconds.

The elapsed samples, in execution order, were:

```text
6.439 6.388 6.452 6.500 6.239 6.811 6.421 6.661 6.233 6.541
7.170 7.126 6.909 6.463 6.280 6.135 6.211 6.294 6.156 6.264
```

These are current-source measurements, not a matched comparison with the
older 9p cohort. Comment edits force runtime-crate recompilation but do not
represent every patch. Elapsed time includes selection, build, metadata,
and execution. Internal compiler, linker, and scheduler costs were not
separately traced. This decision resolves the crate-split choice in #642;
it does not prove the private root-unit floor, every edit path, or CI p95.
The wider performance and CI campaigns remain open. Raw commands, outputs,
source identity, and full-precision samples remain locally in
`progress/session-708/native-loop-final.json`.

An earlier 20-edit cohort measured an intermediate endpoint-refresh candidate
with an owned endpoint retained across storage. It remains separately recorded
in `progress/session-708/native-loop.json`; no samples were pooled or discarded.
