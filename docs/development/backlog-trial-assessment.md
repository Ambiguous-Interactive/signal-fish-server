# Final backlog trial cohort assessment

Issue-state snapshot: 2026-11-16 04:49 UTC. Baseline: 42 original open issues at main 8135716b.
All 42 current issue bodies and all 946 comments were fetched with the connected GitHub tools.
Comment pagination included second pages for #636/#775/#826. All original issue event pages were fetched.
The only cohort closure since setup is #1224, completed through PR #1674.
41 original issues remain open. No original issue was reopened or marked duplicate during the trial.
No trial-created replacement or follow-up issue was found. User issue #1676 was created at 01:59:38 UTC;
it is new external scope, not a trial-generated outcome. Thus repository open count stays 42 = 41 original + 1 new.
The trial has seven selected original issues (including slot 10); 35 received no new bounded experiment.
Ten bounded task outcomes are recorded across those seven issues; only one issue meets full closure criteria.
F379 corrects four existing nullable join-schema fields. F380 corrects two reconnect samples.
These are schema/example repairs, not evidence of a runtime incident repair.
Accepted criteria below are not closure counts. Historical passing repeats establish no causal repair.

## Accepted criteria and delivery

| Slot | Issue / PR | Accepted bounded outcome | Original issue complete? |
| --- | --- | --- | --- |
| 1 | #1244 / #1672 | Four current-source Godot 4.3 isolated/loaded and role controls; 2,048 snapshot + 512 startup receipts; independent prefix audit and negative controls. | No |
| 2 | #1441 / #1673 | Two source-cost reductions; 4,000 differential comparisons; matched temporary-index measurements. Actual-index target misses. | No |
| 3 | #1224 / #1674 | All 61 findings repaired without weakened protocol assertions; 278 owning tests, native controls and hosted strict gate pass. | Yes |
| 4 | #1501 / #1675 | Original warning source and compatible upstream/MSRV bounds assessed; no fresh cast-path reproduction or strict-provenance qualification. | No |
| 5 | #1111 / #1653 | prost 0.14.6 accepted by two unchanged-corpus seeds, 20 cells each, eight reference wires, 15 negatives; raw streams and old wire bytes reconcile. | No |
| 6 | #826 / #1677 | Exact 15/11 inbound union sets, representative shapes, 48 optional-field cells; F379 repairs four existing null values in the schema. | No |
| 7 | #826 / #1678 | Exact 32 outbound variants plus extension; 44 samples cover 31 JSON variants; F380 corrects two reconnect examples; negative controls discriminate. | No |
| 8 | #826 / #1679 | All 15 nested results, 21 wire vectors, malformed/unsupported and reference-omission controls; no production defect established. | No |
| 9 | #826 / #1680 | Six populated versioned nested player-snapshot vectors, cross-version/hybrid/replay negatives and two marker fault pairs; no production defect established. | No |
| 10 | [#1464](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1464) | 64 startup bounds pairs; three retained configurations; 196,608 explicit-client negotiations and three defaults; two compiled assertion-failing source faults, restored green. | No |

Nine predecessor PRs are merged. The final slot delivers this assessment and the
startup control in one PR. [Issue #1464](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1464)
records its final reviewed head, checks, merge, sync and timing. Publication does
not change the acceptance limits below. Stop after this slot and assessment.
The eight debt/two exploration allocation is preserved: exploration slots 5 and 10.
Slot 5 resumed existing #1653 before selecting new work; its original PR-open age differs from session cycle.

## Cycle, effort and waits

Claim-to-local-acceptance includes local waits. Claim-to-merge includes CI/review waits.
Minute-only claims produce approximate durations. Local acceptance is a bounded task result, not issue closure.

| Slot | Claim UTC | Bounded local acceptance UTC | Merge UTC | Claim-to-merge | Effort / wait evidence |
| --- | --- | --- | --- | --- | --- |
| 1 | Nov 15 22:38 | 22:47 | 23:05:12 | ~27m12s | Active effort/blocked time uninstrumented; local acceptance ~9m. |
| 2 | Nov 15 23:08 | 23:25 | 23:37:57 | ~29m57s | Active effort/blocked time uninstrumented; partial local acceptance ~17m. |
| 3 | Nov 15 23:43 | 23:56 | Nov 16 00:11:21 | ~28m21s | Full CI acceptance 00:10:23, claim-to-CI 27m23s; publication-to-green ~12m36s. Active effort uninstrumented. |
| 4 | Nov 16 00:14 | 00:50 | 01:04:56 | ~50m56s | Claim-to-required-CI 49m50s; PR-open-to-CI 12m46s; active effort uninstrumented. |
| 5 | Nov 16 01:12 | 01:25 | 01:38:03 | ~26m03s | Claim-to-PR-CI 23m21s; workflow start-to-required 9m57s; active effort uninstrumented. PR opened Nov 15 01:50:29 (~23h47m34s before merge). |
| 6 | Nov 16 01:51 | Results at 02:03:17; final acceptance at merge | 02:18:34 | ~27m34s | Estimated active effort ~18m, not instrumented; final CI wait 12m44s overlaps audit/handoff. |
| 7 | Nov 16 03:26 | Reviewed/publication ~03:33 | 03:44:38 | ~18m38s | Estimated active effort ~10m, not instrumented; CI 10m23s overlaps review/audit/handoff. |
| 8 | Nov 16 04:01:54 | Results at 04:05:39; final acceptance at merge | 04:19:41 | 17m47s | Estimated active effort ~8m, not instrumented; CI 11m28s overlaps monitoring/handoff. |
| 9 | Nov 16 04:25:23 | Results before publication; final acceptance at merge | 04:42:08 | 16m45s | Estimated active effort ~5m, not instrumented; CI 12m28s overlaps monitoring/handoff. |
| 10 | Nov 16 04:47:29 | Restored results 04:49; final review/checks recorded in #1464 | See #1464 | See #1464 | Active effort and blocked time are not separately instrumented. CI/review waits overlap assessment work. Initial 05:00 claim text was corrected. |

Direct PR metadata corrects #1441's body estimate: actual merge 23:37:57, not 23:38:21.
Do not sum overlapping CI, review and active-work durations. No complete instrumented effort/blocked-time series exists.
There is no matched pretrial task-cycle cohort, so no cycle-time improvement or team-size benefit is established.

## Comparable measurements and limits

The valid within-trial matched performance result is slot 2: 20 alternating seven-file temporary-index pairs.
Hook median/p95/max changes from 1,016/1,039/1,051 ms to 827.5/877/915 ms, with all verdicts passing.
Candidate process-wall p95 is 1,163.675 ms. Actual-index staged samples remain 1,051/1,013/996/1,051/1,042 ms;
worktree samples remain 1,207/1,204/1,258/1,211/1,190 ms. This does not meet the actual-index sub-second criterion.
Slot 10 restored control runs in 0.022 s after a 9.41 s build; the earlier green
runs in 0.023 s after a 3.46 s build. These single scoped samples are not a p95.
No other trial local timing is a matched 20-edit performance cohort. Scoped per-test build/execution observations
are retained in the issues, but changing test inventories forbid pooling them into one p95.

Routine CI observations: slot 3 root Nextest 11m43s; slot 4 11m45s; slot 5 9m26s;
slot 6 CI/Nextest 12m44s/11m49s; slot 7 10m23s/9m45s; slot 8 11m28s/10m45s; slot 9 12m28s/12m01s.
These have different change cohorts and cache states. Most observed routine runs miss 10 minutes.
Slot 5's 9m57s workflow-to-required result is one sample, not a p95 or a matched speedup.
No 20-head equivalent-cohort CI sample, full queue/runner separation, total job-minute comparison, or
complete inventory-union audit was collected by this trial. Existing iteration-performance.md results remain separate.
The prior 23-head archive's 23.69m median/27.38m p95 slowest-workflow proxy is not a comparable trial baseline.
No stable codec speed ranking or production encoding adoption follows from slot 5's shared-host samples.

## Original cohort disposition and remaining blockers

Age uses original issue creation through this snapshot; it is not active investigation effort.
Original cohort age min/median/max: 1.32/20.23/51.08 days. Closed #1224 age at closure: 21d03h35m33s.

| Issue | State | Age, days | Current obligation / blocker |
| --- | --- | ---: | --- |
| #636 | open | 51.08 | Campaign remains broad; define finite capacity obligations before more optional allocation work. |
| #731 | open | 43.50 | Upstream release and allowlist removal remain pending; recheck availability before scheduling. |
| #756 | open | 41.00 | Upstream version constraints and acceptance removal remain pending; recheck both chains. |
| #775 | open | 39.98 | One-room long cell qualifies; bounded memory growth and the upper workload remain unproved. |
| #795 | open | 39.67 | Functional fixture budget correction is separate from the original macOS generator pause. |
| #811 | open | 39.22 | Passed-but-slow hooks remain; isolate avoidable work from filesystem stalls. |
| #826 | open | 38.22 | A01 inbound/outbound/nested/version evidence accepted; F379 join schema and F380 examples repaired. Full A01 fields/projections/runtime and A01–A22 remain. |
| #853 | open | 37.09 | New readiness repairs exist; the original browser/native establishment cause remains unknown. |
| #870 | open | 36.37 | Admission controls have advanced; finish the issue-specific resource-bound audit and qualification. |
| #889 | open | 35.99 | Two-endpoint diagnostics improved; the original three-player missing receipt is unexplained. |
| #912 | open | 34.46 | Vendored repairs qualify selected paths; upstream replacements still need full compatibility qualification. |
| #953 | open | 32.93 | Iteration work and repeated controls exist; matched historical CI cost attribution remains incomplete. |
| #956 | open | 32.46 | Matched overlap controls pass; they do not attribute the historical relay latency failure. |
| #1009 | open | 30.49 | A fresh long cell passes; neither original Linux stall has a demonstrated cause. |
| #1111 | open | 25.40 | Compatible prost 0.14.6 wire/corpus qualification accepted. Incompatible upstream constraints remain. |
| #1120 | open | 25.06 | Reactor diagnostics exist; the historical reciprocal-reset timeout remains unexplained. |
| #1127 | open | 24.46 | Controls pass; the historical macOS lossy generator delay remains unattributed. |
| #1224 | closed | 21.34 | Completed: all 61 vendored-test lint findings repaired; strict gate and owning controls pass. Closed by #1674. |
| #1226 | open | 21.33 | Analyzer qualification has advanced; the original canceled Miri run remains unexplained. |
| #1240 | open | 21.07 | Negotiated-channel support exists; finish linked native/Web Godot and service qualification. |
| #1244 | open | 20.95 | Four current-source Godot 4.3 controls accepted. Historical startup-gap cause and broader runtime qualification remain. |
| #1264 | open | 19.51 | Transport controls exist; distinguish Engine budget rejection from closure in the original event path. |
| #1290 | open | 18.48 | Executed-sibling controls exclude persistent inheritance; transient ownership and historical timeout remain open. |
| #1307 | open | 17.91 | Sanitizer generator lag has no demonstrated physical cause; retain exact accounting checks. |
| #1319 | open | 17.04 | Windows pacing exposed a separate fixture defect; original saturation setup still needs attribution. |
| #1341 | open | 15.15 | Changed browser setup qualifies; original package-install cancellation remains unexplained. |
| #1352 | open | 14.45 | Stage and poll diagnostics exist; matched coverage controls pass without reproducing the original wait. |
| #1359 | open | 14.21 | Registry warm-up qualifies in the loaded suite; isolated macOS and historical call-site attribution remain. |
| #1363 | open | 14.08 | The scheduled native-suite cancellation still needs actor or timeout attribution. |
| #1365 | open | 13.98 | Internal observer and tooling-only changes still need narrow changelog classification. |
| #1426 | open | 12.12 | A span-across-await defect was found; the historical missing-span panic is not attributed. |
| #1436 | open | 11.93 | First-use 64-byte difference is isolated; allocation-site attribution remains open. |
| #1441 | open | 11.48 | Measured temporary-index overhead reduced. Actual-index sub-second target and historical 14-second cause remain. |
| #1464 | open | 10.99 | Bounded constructor validation and retained config controls accepted. Live socket/process, arbitrary-input constructor and caller/state/concurrency correspondence remain. |
| #1486 | open | 9.47 | Exact-blob refactor checks pass slowly; isolate and remove proven overhead without weakening policy. |
| #1501 | open | 7.95 | Upstream provenance repair and incompatible update bounds assessed. Fresh warning-path reproduction remains. |
| #1504 | open | 7.83 | Checksum portability repaired; broader cold/warm bootstrap and cache assessment remains. |
| #1565 | open | 5.98 | Same-head retry passes; the unnamed original TURN event remains unexplained. |
| #1608 | open | 3.79 | Same-head fuzz retry passes; original build timeout lacks internal logs and artifacts. |
| #1616 | open | 3.34 | Restart qualification passes on retry; original time-sync recommendation remains unexplained. |
| #1639 | open | 2.26 | Bounded two-endpoint SCTP diagnostics exist; original unreliable receipt loss remains unattributed. |
| #1650 | open | 1.32 | Refused-start macOS allocation mismatch still needs source-level attribution. |

The upper #775 retained-record shape requires over 414 GB for only five u64 fields per recipient row;
this 31 GiB host cannot qualify it. Re-entry needs streaming work or a provisioned host.
Issue #1264 lacks the original terminal-event identity; #1426 requires a correlated Windows panic capture.
Issue #1244 and #1501 need changed historical/cast-path experiments, not unchanged passing repeats.
Issue #1111 remains constrained by DashMap/parking_lot core requirements and project Rust 1.94 vs upstream 1.95.
Upstream watches #731/#756 were not refreshed to new release evidence in this slot; their issue evidence is unchanged.
Unselected historical causes and broader campaign obligations remain missing evidence, not repaired incidents.

Current main 3c996b36 docs deployment 43805674927 passes. Recent fuzz 43802936572, nightly 43796940417,
formal 43792837562 and safety 43790855327 pass at their recorded earlier heads. Historical scheduled CI
43712032369 stays failed on 7e2b8725. Main includes #1667 causal fixture repairs; qualification 43742266366
has 18 successful jobs and four policy skips. No historical failure is retried or relabeled by this assessment.

Evidence sources: the [roadmap](roadmap.md), linked original issues and PRs,
[slot 10 experiment/results](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1464#issuecomment-6907627656),
[source-linked boundary notes](../../formal/source-linked/README.md#startup-bounds-and-retained-configuration-evidence),
and [iteration measurements](iteration-performance.md). Raw snapshots remain in
ignored `progress/backlog-trial/slot-10/`; they do not own acceptance criteria.

The trial supports keeping bounded experiments, causal closure rules and explicit
partial acceptance. It does not establish faster overall delivery, universal
local/CI targets or resolution of the remaining 41 original issues. Instrument
active effort and comparable CI cohorts before making such claims. No new board,
hook, stale closure rule or automation was added. This assessment ends the trial;
it does not authorize another session.
