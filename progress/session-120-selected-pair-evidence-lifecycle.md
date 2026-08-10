# Session 120 — Selected-pair evidence lifecycle

## Scope and prioritization

The default branch began clean and green at `afe6d83`, with nine open issues.
P56/issue #290 remains the highest gameplay risk but can advance only through
its unchanged scheduled cohort (5/20 eligible attempts); P53 is likewise bound
to scheduled three-platform evidence (4/20 per OS). The one open dependency PR,
PR #332 carried grouped GitHub Actions updates but failed its macOS live WebRTC
lane after a completely successful transport exchange omitted one selected-pair
event. Issue #301 was reopened with the exact recurrence evidence.

## Failure-first evidence and root cause

PR #332's macOS client established ICE at `10:15:41.464`, reported peer
connection `connected` at `10:15:41.690`, opened both channels, exchanged exact
reliable and unreliable messages in both directions, reported WebRTC connected,
and exited zero. At `10:15:42.874` its selected-pair lookup exhausted the
one-second statistics budget introduced by P68 and emitted no evidence.

The gameplay transport and the test were both correct. The defect was the
client success contract: it treated eventually consistent diagnostics as a
bounded side effect, so expiration merely logged a warning and allowed exit
zero. A larger timeout would preserve the same failure mode at a new boundary.

## Implementation and deterministic regression

Pair-open handling now starts gameplay exchange and transport-status work
without awaiting statistics at all. A detached, generation-fenced probe returns
its snapshot through the engine event channel; if statistics lag, the client
retains a physical-link-scoped evidence obligation and schedules the next probe
from the completion time. Clean success includes that obligation, so evidence
must complete before the existing whole-run deadline or the client fails with
explicit unmet criteria. A hung probe cannot postpone that deadline. Transport
loss, retry, authoritative replacement, and removed membership clear only their
affected generation; concurrent pairs retain their own obligations.

Paused-time tests cross the former one-second boundary through 101 consecutive
missing observations, prove a hanging probe cannot postpone run expiry, drain
a queued pre-deadline completion before the deadline decision, suspend the soft
cutoff for a harness-held replacement generation, reject a stale completion
after teardown, and suppress duplicate completion for one physical generation.
A controllably hanging production probe proves unrelated work proceeds while
it is pending, and a real in-process WebRTC pair proves the detached probe
reaches concrete selected-path evidence. Five consecutive real two-process mesh
exchanges pass with exact selected host/host paths and bidirectional
reliable/unreliable data.

## Dependency and documentation closure

The session incorporates PR #332's complete grouped refresh of
`Swatinem/rust-cache` 2.9.1 to 2.9.2, `taiki-e/install-action` 2.85.7 to 2.85.10,
and `crate-ci/typos` 1.48.0 to 1.49.0 across every existing workflow consumer.
The native event reference, changelog, and PLAN now describe selected-path
evidence as an asynchronous clean-success postcondition rather than a separate
one-second deadline. P53 and P56 workloads, thresholds, selectors, and evidence
contracts are unchanged.

## Review and verification

The first adversarial review found four lifecycle gaps in the initial inline
poll design: stats could block gameplay and deadline processing, retry cadence
used a stale timestamp, and tests stopped below the production seam. The second
found the held-success cutoff and queued-at-deadline arbitration gaps. The third
found that those fixes needed one directly exercised production arbitration
seam. The detached probe, dedicated result channel, apply-before-deadline seam,
and expanded regression set address every finding. Focused native unit tests,
warnings-denied native Clippy, native dependency policy, and five live
two-process WebRTC exchanges pass. The final zero-finding review, complete local
gauntlet, and hosted PR evidence are recorded before publication closure.

The final adversarial pass reported zero actionable findings. Root formatting,
warnings-denied Clippy, and the complete all-feature suite pass; the native
workflow-equivalent 95-test library/binary suite, dependency audit, and five
fresh live two-process runs pass. Documentation, CI configuration, MSRV,
tooling parity, workflow hygiene, all 212 live action-tag resolutions, LLM
policy, and PowerShell pre-push checks pass. A profiled warm pre-commit run is
915 ms (changed-file discovery 637 ms), within the one-second policy target.
Hosted Linux/Windows/macOS WebRTC evidence and the remaining PR workflow matrix
are the only validation still pending at commit time.
