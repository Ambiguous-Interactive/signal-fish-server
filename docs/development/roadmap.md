# Development roadmap

GitHub issues own requirements, state, evidence, blockers, and closure decisions.
This index owns outcome groups and the ordered ready queue. Refresh links when work changes.
Keep GOAL.md and PLAN.md local and ignored. They are not durable backlogs.

## Baseline and reconciliation

The 2026-11-15 baseline contains 42 open issues at main `8135716b`.
The GitHub issue listing ended with `hasNextPage=false`.
All 42 bodies and complete comment histories were fetched. The rows below record remaining work.
They are dated reconciliation notes, not live status. Read each issue before execution.
No acceptance criterion or historical failure is closed by this inventory.
No duplicate cause was established. Similar symptoms remain separate.

Existing evidence inventories remain authoritative:

- [Production correctness audit](production-correctness-audit.md).
- [ARM capacity audit](arm-capacity-audit.md).
- [Source-linked proof obligations](../../formal/source-linked/obligations.md).
- [Iteration measurements](iteration-performance.md).

## Ready queue

Resume an unfinished task before selecting a new one. One owner claims one issue at a time.
These are bounded investigation tasks; ready does not mean a production defect is confirmed.
Refresh issue comments and dependencies before claiming. Blocked tasks return to triage.

Slot 8 records the bounded A01 nested operation-result experiment under
[#826][i826]. Exact nested reachability and all 15 representative result shapes
qualify; exhaustive fields, version eligibility and runtime projections remain open.
The issue owns delivery state. Refresh existing debt before selecting the next task.

Correctness precedes usability, then measured performance. Within a tier, prefer dependency
unblocking, then the oldest actionable issue. A confirmed urgent defect can change this order.
Record the reason. Use parent/sub-issue relationships for decomposition and blocked-by relationships
only for demonstrated prerequisites. Do not infer dependencies from a shared campaign reference.

Existing #1244 requires a changed historical-input experiment before re-entering this queue.
Existing #1264 lacks the original terminal-event identity; its current typed controls already exist.
Recover that evidence or define a discriminating pinned-source recurrence before scheduling it again.
Existing #1441 has new normal-workload measurements below; historical timing outliers remain unresolved.
Existing #1426 needs a correlated Windows panic capture before an unchanged rerun can resolve attribution.
Existing #775 refuses its upper retained-record shape before allocation. Largest-cell qualification needs
streaming work or a provisioned host; the current 31 GiB host cannot hold its required recipient rows.
Existing #1501 has an upstream assessment below. Re-entry requires a changed warning-path experiment,
not another unchanged passing run.

## Original cohort

### Gameplay correctness

| Issue | Remaining work at baseline |
| --- | --- |
| [#826][i826] | Bounded histories and repairs exist; complete the A01–A22 coverage ledger before closure. |
| [#1120][i1120] | Reactor diagnostics exist; the historical reciprocal-reset timeout remains unexplained. |
| [#1244][i1244] | Fresh-link stall repairs pass; the historical Godot 4.3 reliable startup gap remains unattributed. |
| [#1264][i1264] | Transport controls exist; distinguish Engine budget rejection from closure in the original event path. |
| [#1426][i1426] | A span-across-await defect was found; the historical missing-span panic is not attributed. |

### Client compatibility

| Issue | Remaining work at baseline |
| --- | --- |
| [#853][i853] | New readiness repairs exist; the original browser/native establishment cause remains unknown. |
| [#870][i870] | Admission controls have advanced; finish the issue-specific resource-bound audit and qualification. |
| [#889][i889] | Two-endpoint diagnostics improved; the original three-player missing receipt is unexplained. |
| [#1240][i1240] | Negotiated-channel support exists; finish linked native/Web Godot and service qualification. |
| [#1319][i1319] | Windows pacing exposed a separate fixture defect; original saturation setup still needs attribution. |
| [#1565][i1565] | Same-head retry passes; the unnamed original TURN event remains unexplained. |
| [#1616][i1616] | Restart qualification passes on retry; original time-sync recommendation remains unexplained. |
| [#1639][i1639] | Bounded two-endpoint SCTP diagnostics exist; original unreliable receipt loss remains unattributed. |

### Capacity and performance

| Issue | Remaining work at baseline |
| --- | --- |
| [#636][i636] | Campaign remains broad; define finite capacity obligations before more optional allocation work. |
| [#775][i775] | One-room long cell qualifies; bounded memory growth and the upper workload remain unproved. |
| [#795][i795] | Functional fixture budget correction is separate from the original macOS generator pause. |
| [#956][i956] | Matched overlap controls pass; they do not attribute the historical relay latency failure. |
| [#1009][i1009] | A fresh long cell passes; neither original Linux stall has a demonstrated cause. |
| [#1127][i1127] | Controls pass; the historical macOS lossy generator delay remains unattributed. |
| [#1307][i1307] | Sanitizer generator lag has no demonstrated physical cause; retain exact accounting checks. |
| [#1352][i1352] | Stage and poll diagnostics exist; matched coverage controls pass without reproducing the original wait. |
| [#1359][i1359] | Registry warm-up qualifies in the loaded suite; isolated macOS and historical call-site attribution remain. |
| [#1436][i1436] | First-use 64-byte difference is isolated; allocation-site attribution remains open. |
| [#1650][i1650] | Refused-start macOS allocation mismatch still needs source-level attribution. |

### Delivery tooling

| Issue | Remaining work at baseline |
| --- | --- |
| [#811][i811] | Passed-but-slow hooks remain; isolate avoidable work from filesystem stalls. |
| [#953][i953] | Iteration work and repeated controls exist; matched historical CI cost attribution remains incomplete. |
| [#1224][i1224] | Existing vendored test lint findings still need scoped remediation. |
| [#1226][i1226] | Analyzer qualification has advanced; the original canceled Miri run remains unexplained. |
| [#1290][i1290] | Executed-sibling controls exclude persistent inheritance; transient ownership and historical timeout remain open. |
| [#1341][i1341] | Changed browser setup qualifies; original package-install cancellation remains unexplained. |
| [#1363][i1363] | The scheduled native-suite cancellation still needs actor or timeout attribution. |
| [#1365][i1365] | Internal observer and tooling-only changes still need narrow changelog classification. |
| [#1441][i1441] | Rustdoc classification improved; representative hook runtime and remaining criteria remain open. |
| [#1464][i1464] | Pilot and helper proofs exist; required caller, state and concurrent correspondence remain open. |
| [#1486][i1486] | Exact-blob refactor checks pass slowly; isolate and remove proven overhead without weakening policy. |
| [#1501][i1501] | Miri warning needs a bounded upstream provenance assessment; no runtime defect is established. |
| [#1504][i1504] | Checksum portability repaired; broader cold/warm bootstrap and cache assessment remains. |
| [#1608][i1608] | Same-head fuzz retry passes; original build timeout lacks internal logs and artifacts. |

### Upstream dependencies

| Issue | Remaining work at baseline |
| --- | --- |
| [#731][i731] | Upstream release and allowlist removal remain pending; recheck availability before scheduling. |
| [#756][i756] | Upstream version constraints and acceptance removal remain pending; recheck both chains. |
| [#912][i912] | Vendored repairs qualify selected paths; upstream replacements still need full compatibility qualification. |
| [#1111][i1111] | Available upgrades were refreshed; remaining exact upstream constraints need release checks. |

## Session contract

Before implementation, add this contract to the selected existing issue. Link current evidence.
Keep the issue's original acceptance criteria. Claim ownership explicitly in the issue.

```text
Outcome:
Remaining acceptance criteria:
Owner and start time:
Hypothesis and alternatives:
Pinned source, environment, workload, reproduction command:
Experiment, expected observation, and negative control:
Dependencies or missing evidence:
Next action:
Closure evidence required:
```

Observe, predict, demonstrate red (or a matched performance baseline), change, then demonstrate green.
Use scripts/dev-loop.sh for scoped iteration. Run the required publication checks once.
Capture exact test selection, source/binary identity, controls, timings, failures, and rejected hypotheses.
An intended assertion failure counts as red; a build or tool failure does not.
Keep essential reproduction steps and results in the issue or tracked evidence inventory.
Ignored progress files may retain raw logs but must not hold the only evidence.

Search existing issues before recording a finding. Update the matching issue first.
Create an issue only for distinct uncovered work. Create a child only for separate ownership,
delivery, or dependency tracking. Keep discoveries outside the selected task from expanding its PR.
Sweep for related instances, then record out-of-bound repairs against existing issues.
Campaigns execute finite named obligations. Completing one obligation does not close the campaign.

A passing rerun or added diagnostics does not establish historical cause.
Keep historical failures open until their cause is demonstrated and acceptance criteria are met.
Record missing evidence and its recovery action. After two sessions without a discriminating result,
return the issue to triage with a changed experiment or concrete blocker. Do not repeat unchanged runs.
Use Fixes only for complete acceptance; use Refs for partial progress.
Report task acceptance separately from PR delivery. Never count diagnostic work as a repaired incident.

## Trial slot 1 evidence

On 2026-11-15, existing #1244 received the experiment contract before execution.
Four native Godot 4.3 pairings passed: both offer roles, first without added workers,
then with four CPU workers. Each case checked two physical generations and both directions.
All 2,048 required reliable snapshot receipts and 512 startup receipts passed.
An independent raw-byte audit checked all 1,024 Godot-to-native snapshot callbacks.
Removing sequence zero or reversing the first two callbacks failed the audit in all four cases.

This accepts current-source qualification only. It does not reproduce the historical missing prefix.
Chromium 156 differs from the historical Chromium 154. The driver is pinned to f3d3f86;
server source is 1f8638fd. No receipt gate, deadline, or production source changed.
The issue retains the commands, identities, result hashes, and remaining acceptance criteria.
See [the runtime reproduction notes](../../clients/native/tests/fixtures/README.md#trial-slot-1-godot-43-prefix-comparison).

Task claim: 22:38 UTC; owner acceptance after independent audit: 22:47 UTC on 2026-11-15.
Elapsed task time is about nine minutes at minute precision, including tool waits.
Active effort and blocked time were not separately instrumented; do not infer them from elapsed time.
PR #1672 records delivery independently from this task acceptance.

The original cohort remains 42 open issues. New issues: zero. Reopened issues: zero.
Duplicates closed: zero. Existing issues closed: zero. Accepted runtime qualification tasks: one.
No historical root-cause criterion is satisfied by this experiment.
Next #1244 work must recover a pinned historical comparison or a new correlated failing history;
do not repeat this unchanged four-worker matrix. #1264 is the next independent ready investigation.

## Trial slot 2 evidence

Slot 2 selects existing #1441 after a source/artifact audit of #1264.
The only retained original #1264 artifact explicitly omits the event identity.
No unchanged saturation rerun was started. The issue records the evidence needed to resume.
The local host is Linux; #1426 needs a correlated Windows panic capture.
The #1441 hook cost blocks documentation work and has an executable local reproduction.

Two changes reduce confirmed overhead: copy ordinary Rust spans together when masking literals/comments,
and skip worktree/index comparison when no staged check uses worktree text.
Rust classification still reads exact Git blobs. Relevant mixed changes still compare all staged paths.
The existing Rustdoc corpus preserves staged/worktree positives, executable negatives, strings,
attributes, macros, and revision ranges. Independent review found no semantic mismatch in 4,000
baseline/candidate comparisons, including malformed input and scoped ranges.

Twenty alternating pairs using one temporary index give these hook-timer results on Linux ARM64:

| Hook timing | Baseline | Candidate |
| --- | ---: | ---: |
| Median | 1,016 ms | 827.5 ms |
| Nearest-rank p95 | 1,039 ms | 877 ms |
| Maximum | 1,051 ms | 915 ms |

Every verdict passes. Candidate process-wall p95 is 1,163.675 ms; it is not sub-second.
An earlier lexer-only candidate improved median but missed the target at p95 1,023 ms.
That cohort and all failures remain retained. Its final pairs overlapped owner readiness probes;
the final cohort did not. Shared-host external load was not controlled.

Reproduce the seven-file index by adding two method-doc lines before these anchors at baseline a48657dd:
`room_exists` in database/mod.rs, `decision_with` in server/session_policy.rs, `instance_id` in server.rs,
`validate_game_name_with_config` in protocol/validation.rs, `handle_player_ready` in server/ready_state.rs,
`sorted_routed_player_ids` in server/signaling.rs, and `relay_serialization_runtime` in its benchmark.
The first six paths are under src/; the benchmark is under benches/.
Use an alternate Git index, never the user's real index. Alternate baseline and candidate hook processes
on that same index and filesystem with SIGNAL_FISH_HOOK_PROFILE=1. Retain hook and process-wall time,
exit status, source/loader/helper hashes, and all attempts. The existing issue retains exact manifests.

Committed source 7fd1f131 then ran the same seven-file edits in a linked worktree with its actual index.
All verdicts pass, but staged hook times are 1,051/1,013/996/1,051/1,042 ms and worktree times are
1,207/1,204/1,258/1,211/1,190 ms. This misses the target and prevents issue completion.
The temporary-index comparison does not establish actual-index or linked-worktree timing.
The full original worktree also passes slowly at 1,193 ms while checking unrelated untracked skills;
the generated catalog change was restored and user files were preserved.

This delivers a measured reduction in two avoidable costs, with remaining timing work explicit.
It does not explain the retained 14-second filesystem/discovery observation or prove a universal
runtime bound. Keep #1441 open. Next profile actual-index discovery and blob reads; do not rerun
unchanged temporary-index comparisons as closure evidence.
New, reopened, closed, and duplicate-disposition issue counts are zero for this slot.
No server API, runtime dependency, client protocol, or hook timing boundary changes.
Claim: 23:08 UTC; partial source-cost improvement accepted after review at 23:25 UTC on 2026-11-15.
Elapsed task time is about 17 minutes at minute precision, including tool waits.
Active effort and blocked time were not separately instrumented. Overall issue acceptance remains incomplete.
PR #1673 records delivery separately.

## Trial slot 3 evidence

Slot 3 selects existing #1224 after checking #775's largest-cell prerequisites.
At source 16fe2b75, the existing admission control refuses all 11,048,652,288 planned upper-shape
send/delivery records before artifacts or connections. Even five u64 fields per recipient row require
over 414 GB, excluding all other fields and process state. This host has 32,541,140 KiB total memory.
The source gate and lower-bound arithmetic justify deferring that full live shape without overriding budgets.
This selection audit consumes no delivery slot and does not complete the broader #775 work.

The exact strict vendored SCTP command reproduces all 61 existing #1224 findings.
Four test-only files now use equivalent initializers, payload iterators, patterns and conversions,
fixed-size expectations, and explicit panic branches. Every protocol assertion and test name remains.
No lint suppression, production source, package metadata, license, or upstream author changes.
The copy remains published rtc-sctp 0.21.1 with the provenance in
[the existing patch notes](../../third_party/rtc-sctp/PATCH.md).

Strict all-target vendored lint now passes. All 278 selected association/endpoint/chunk/queue tests pass;
59 other library tests are explicitly unselected. The native startup-prefix control also passes both
physical generations. Root formatting and strict all-target/all-feature lint pass.
Intermediate loop-count and mutability repair attempts remain retained; neither is counted as green.
A forced absent INIT downcast fails at the repaired panic; restoring the source passes the same selector.
Independent review found no weakened assertion or changed protocol behavior.

Gate decision: run strict vendored-test lint inside the existing native interoperability script and job,
using its target cache and setup. Dependency lint alone does not check the standalone package's tests.
No separate workflow or job is added. The corrected local lint run takes 5.94 seconds; that one sample
does not establish hosted CI cost. Existing reset, queue, shutdown, and live interop selections stay intact.

Original #1224 closed as completed through PR #1674. Its 27 checks were terminal: 19 successful
and eight policy skips. CodeRabbit approved the verified head. Local main synced to merge
`e6e7a379`. The original issue records acceptance and timing. No replacement issue was created.
Raw diagnostics, all attempts, lint logs, test inventories and source identities remain under
progress/backlog-trial/slot-3/. Essential commands and outcomes are recorded in the original issue.

## Trial slot 4 evidence

Existing #1501 now has a bounded upstream provenance assessment. The original hosted log confirms
the core 0.9.12 WordLock cast warning through DashMap during the auth race test. The lane passed
459 tests with 16 existing ignores. That pass does not establish strict provenance.

Published core 0.10.0 uses pointer-preserving operations. Its verified release requires Rust 1.95;
the project pins 1.94. DashMap 6.2.1 and parking_lot 0.12.5 require the 0.9 series. The dry-run
resolver rejects 0.10.0 at DashMap's requirement and leaves the lockfile unchanged.
Existing #1111 owns the compatible-release watch. No dependency override or new issue was added.

The pinned nightly's isolated auth control passes without the warning on ARM and x86 interpreter
targets. The ARM 16-seed control also passes without it. All 475 historical names were recovered
and verified against current and original-source listings. Both exact-scope replays pass the auth
group without the warning; their unrelated tails were stopped and are not full-lane green results.
The original source's higher-preemption and strict-mode controls also pass without reaching the cast.
These negative reproduction results do not qualify the warned path. Original #1501 stays open.

The CI testing guide records the analyzer limit, unchanged diagnostic policy, upstream constraint,
current exact-name lane selection and artifact names. Tests, assertions, ignores and CI flags remain
unchanged. Independent review corrected the helper name and found no other blocker.
All attempts, source identities, release checksums and logs remain under progress/backlog-trial/slot-4/.
The original issue records commands and unmet acceptance; delivery does not close that criterion.
PR #1675 merged at `c7241ff0` with 16 successful checks, six policy skips and a neutral
CodeRabbit rate-limit result. Independent review found no blocker. Original #1501 stays open.

## Trial slot 5 evidence

GOAL.md requires resuming relevant open dependency work before a new task. Existing PR #1653
had a failing cross-language reconciliation after prost/prost-derive 0.14.4 changed to 0.14.6.
This exploration slot resumed that PR under existing #1111; the queued A01 task remains unclaimed.
No replacement PR or issue was created.

The matched control isolates the lockfile. Both November 12 records pass with the baseline lock
and reject the candidate lock. Every other source hash matches. The guard correctly refuses
old measurements as current-source evidence; no codec disagreement follows from that failure.

Two new seeded records retain the unchanged corpus, harnesses, schemas, generated bindings,
assertions and negative controls. Each passes 20 cells, eight Rust reference wires and 15 negative
controls. The independent verifier reconciles raw streams and all 64 order comparisons, including
six negative controls. Both source manifests and CSV summaries reconcile. Rust wire exports remain
byte-identical to the historical exports. Five owning Rust corpus tests and strict lint pass.

The workflow points its existing reconciliation steps to the new records. The verifier and all
historical records remain unchanged. The shared-host measurements establish no speed improvement,
stable codec ranking or production encoding adoption. The
[current record](../../benches/encoding/results/2026-11-16-cross-language-aarch64/README.md)
retains methods, limits and raw data. Existing #1111 remains open for incompatible upstream work.
The issue records commands, acceptance and delivery. Local control logs remain under
progress/backlog-trial/slot-5/.

## Ten-session trial

Setup is not a trial delivery session. Sessions 1–4 and 6–9 address existing debt.
Sessions 5 and 10 allow exploration. Urgent correctness work may displace exploration; record why.
One task and one PR remain active. A resumed PR keeps its original slot, not a new session credit.
Keep the two-hour session limit and reserve the final 30 minutes for delivery.

Update the trial table after each delivery. Record issue links, PR, accepted criteria, remaining work,
new/reopened issues, start/end times, blocked time, and investigation effort in the linked issue.
Keep cycle time from first task claim through acceptance, including waits; record active effort separately.
Compare local-loop and CI timings only for equivalent environments and workloads.

| Slot | Allocation | Issue / PR | Accepted outcome and remaining work |
| --- | --- | --- | --- |
| 1 | Debt | [#1244][i1244] / [#1672](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1672) | Four current-source Godot 4.3 controls pass; historical cause remains open. |
| 2 | Debt | [#1441][i1441] / [#1673](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1673) | Temporary-index p95 1,039→877 ms; actual-index timing still misses. |
| 3 | Debt | [#1224][i1224] / [#1674](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1674) | Existing issue completed: 61 lint findings fixed, owning tests and hosted controls pass. |
| 4 | Debt | [#1501][i1501] / [#1675](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1675) | Upstream repair and constraints assessed; original warning remains unreproduced. |
| 5 | Exploration | [#1111][i1111] / [#1653](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1653) | Current-source prost 0.14.6 records qualify; PR merged and Docker publication passed. |
| 6 | Debt | [#826][i826] / [#1677](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1677) | Exact inbound unions and existing join null values qualify; broader audit remains open. |
| 7 | Debt | [#826][i826] / [#1678](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1678) | Exact outbound reachability and corrected reconnect samples qualify; broader audit remains open. |
| 8 | Debt | [#826][i826] | All 15 nested result variants qualify; omission and unsupported-event faults fail; broader audit remains open. |
| 9 | Debt | Pending | Not started |
| 10 | Exploration | Pending | Not started |

At slot 10, compare the original cohort's completed issues and remaining obligations with this baseline.
Report new issues, reopened issues, duplicates, accepted product outcomes, task age, cycle time,
blocked time, and comparable developer/CI timings separately. Explain unresolved debt with evidence.
Change one workflow constraint at a time. Do not promise zero issues or use closure counts as a quota.
Do not add a Project board, dashboard, enforcement hook, or stale-closure automation during this trial.

Adoption checks: select an existing issue without replacement; keep partial repairs open;
close only complete evidence-backed repairs; retain historical failures after passing reruns;
keep campaigns open after child completion; link new blockers without expanding scope.

[i636]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/636
[i731]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/731
[i756]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/756
[i775]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/775
[i795]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/795
[i811]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/811
[i826]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/826
[i853]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/853
[i870]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/870
[i889]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/889
[i912]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912
[i953]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/953
[i956]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/956
[i1009]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1009
[i1111]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1111
[i1120]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1120
[i1127]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1127
[i1224]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1224
[i1226]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1226
[i1240]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1240
[i1244]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1244
[i1264]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1264
[i1290]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1290
[i1307]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1307
[i1319]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1319
[i1341]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1341
[i1352]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1352
[i1359]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1359
[i1363]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1363
[i1365]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1365
[i1426]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1426
[i1436]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1436
[i1441]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1441
[i1464]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1464
[i1486]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1486
[i1501]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1501
[i1504]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1504
[i1565]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1565
[i1608]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1608
[i1616]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1616
[i1639]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1639
[i1650]: https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1650
