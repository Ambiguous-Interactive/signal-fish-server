# Session 078 — Release, relay, and retry integrity

## Scope

Continue the highest-impact bounded work from open issues #207 and #205, then
recover the incomplete v0.5.2 publication and prevent the same historical
GitHub Release retry failure from recurring.

## Release recovery and root cause

The first manual v0.5.2 publication attempt ran before the release commit's CI
and documentation workflows had finished, so preflight correctly failed. By the
time all required checks were green, `main` contained three later sessions and
its `[Unreleased]` metadata no longer described v0.5.2. Retrying directly from
that head would therefore have been unsafe.

The release resolver already supported this state. An annotated `v0.5.2` tag
was created at the merged release commit `09238c36`, whose required workflows
had passed, and pushed without moving any existing reference. Release run
30763742198 then reused that exact source: crate publication, the versioned
multi-architecture GHCR manifest, and all six platform binary builds succeeded.
crates.io now contains the unyanked 0.5.2 package.

GitHub Release creation alone failed with HTTP 403. The workflow passed the
historical source SHA as `target_commitish` even though `ensure-tag` had already
created and verified the immutable annotated tag. Because workflow files had
changed between that commit and the current default branch, GitHub classified
the redundant target operation as requiring workflow-write permission;
workflow-scoped `GITHUB_TOKEN` cannot receive it. The fix releases the verified
existing tag without passing `target_commitish`. A workflow-policy regression
test and the CI/CD runbook record why that omission is part of source-identity
integrity rather than a weaker check.

The adversarial pass then found a second-order retry hazard: the release action
PATCHes an already-existing Release with its stored target even when no new
target input is supplied, including calls whose apparent purpose is only asset
attachment. The corrected workflow skips creation/mutation after validating an
existing Release and uses `gh release upload --clobber` for SBOM and binary
recovery. Those commands touch assets only and preserve partial-success binary
semantics when one platform class is unavailable.

## Relay allocation and measurement integrity

Failure-first ceilings showed the MessagePack-to-JSON fallback growing its
output three times in two-player rooms and six times in 8-/16-player rooms.
Pre-sizing the fallible JSON writer from the opaque payload length plus fixed
envelope headroom removes those reallocations. Mixed-room allocation operations
move from 18/28/28 to 15/22/22 at 2/8/16 players, while allocated bytes move
from 4,449/9,844/10,164 to 3,572/8,090/8,410. Exact output digests, bytes,
frames, delivery counts, and drained queues are unchanged.

The same audit found a no-op `RelayFrameCache` allocated for repeated frozen-v2
JSON/Rkyv raw passthrough. Those frames already clone a shared `Bytes` handle,
so skipping the cache removes one operation and about 680 bytes at 8/16 players:
3 operations and 664/984 bytes remain, versus 4 and 1,344/1,664 previously.

Criterion's timed relay loop also hashed every emitted byte with SHA-256 solely
to prevent optimizer elision. At 16 players that measured 15–17 MiB of hashing
per sample that production never performs. Exact hashes remain mandatory in a
validation sample, while timed samples black-box frames and retain the cheap
work/accounting ledger without hashing.

## Strict retry cap

Both retry executor paths previously capped exponential backoff and then added
up to 20% jitter. A persistent retry at the documented five-second maximum
could therefore sleep for six seconds. The shared calculation now bounds the
complete jittered delay, caps an overlarge initial delay, preserves fractional
`Duration` precision, and saturates invalid or overflowing factors without
panicking. Deterministic tests cover cap-before-jitter, remaining headroom,
ordinary jitter, sub-millisecond delay, initial-cap, and overflow boundaries.

## Validation and publication

Mandatory local validation completed successfully:

- formatting, strict all-target/all-feature Clippy, and the complete locked
  all-feature test suite;
- all 15 allocation cells across five exact repeats, including checked-in wire
  digests and the tightened operation/reallocation/byte ceilings;
- cargo-deny plus CI configuration, workflow hygiene, tooling parity, MSRV,
  documentation consistency, Markdown, internal-link, LLM-policy, and hook
  preflight suites; and
- two independent adversarial-review rounds.

The first hostile review identified the existing-Release PATCH hazard and the
initially unasserted pre-timing digest; both were fixed and regression-guarded.
It also prompted tighter mixed-room byte ceilings and a fractional-jitter
boundary check during parent review. The second hostile review reported zero
findings after rerunning the focused release, retry, allocation, actionlint,
formatting, and Clippy checks.

Hosted validation completed on PR #245 implementation head `31cb0d5`: all 19
workflow runs reached terminal state, with 18 successes and the expected
Dependabot auto-merge skip. The first head exposed one spelling false positive
in release prose; the corrected head passed Spellcheck and every Rust, safety,
fuzz, interop, formal, documentation, and workflow-policy lane. Cursor Bugbot
reported zero findings on that exact head, no review threads remained, and
Copilot's two requested passes were unavailable only because its account quota
was exhausted.

The final v0.5.2 GitHub Release retry remains gated on merging the corrected
workflow to the default branch. It is intentionally not attempted from the PR
branch because the resolver requires the workflow definition on `main` and the
session target is a fully green PR rather than an unreviewed merge.
