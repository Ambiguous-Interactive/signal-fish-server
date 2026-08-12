# Session 129 — Measured CI runner consolidation

## Scope and prioritization

The session began from clean `main` at merge commit `9804ab9`. PR #350 had
merged, closing P86 and P87; there were no open or draft pull requests or open
dependency updates. P53/#274 and P56/#290 remain evidence-cohort work whose
production fixes have landed. Issue #351 was the highest-confidence actionable
follow-up because it carried a measured CI-cost baseline and exact coverage,
cache, status, and hosted-validation acceptance criteria.

## Fuzz baseline and design

PR #349 fuzz run `31560173685` supplied a same-SHA warm-cache baseline:

| Target | Job | Raw duration | Rounded minutes | Compile before fuzz |
| --- | ---: | ---: | ---: | ---: |
| `decode_protocol` | `94000691705` | 6m02s | 7 | 3m28s |
| `validate_inputs` | `94000691715` | 4m09s | 5 | 1m42s |
| `fuzz_session_machine` | `94000691728` | 4m52s | 5 | 2m19s |
| `fuzz_reconnect_tokens` | `94000691748` | 4m02s | 5 | 1m28s |

All four jobs restored the identical
`v0-rust-fuzz-Linux-x64-52beb3c9-ca1f8154` cache and ran one 121-second
libFuzzer process. Together they consumed about 19.10 raw and 22 per-job-rounded
Linux minutes while compiling the same root server and fuzz package four times.

The workflow now uses one hosted job. An all-target `cargo fuzz build` prewarms
the shared dependency graph, then four `cargo fuzz run` processes start
concurrently. The prewarm is explicitly best-effort and externally capped at
10 minutes: a compile failure or hang must not mask sibling outcomes. Each live
target run remains gating,
is bounded by an external watchdog below the job timeout, waits to a terminal
result, retains a complete grouped log and separately named artifact bundle,
and contributes its exact exit status to the final aggregate. An exit trap
terminates and reaps every still-active child if the supervisor is interrupted.
The job allows 30 minutes: at most 10 for the target supervisor including its
grace window, plus 20 for cold setup, shared prebuild, and artifact uploads.
The Rust cache remains an accelerator only; it cannot replace a live build or
turn stale/partial state into a green result.

This shape preserves the original one-process-per-target fuzz semantics on the
four-core hosted Linux runner while sharing checkout, toolchain, cache restore,
tool install, and compile work. Hosted validation will record the actual
throughput and runner-minute result rather than treating that expectation as
evidence.

## Documentation consolidation and Lychee audit

PR #350 Documentation Validation run `31569040602` measured the standalone
shellcheck job (`94026904023`) at 14.01 seconds and Markdown Code Validation
(`94026903925`) at 58.78 seconds. The fatal wrapper/helper shellchecks themselves
took about 0.37 seconds, so folding them into Markdown projects 59.15 seconds:
one allocation and one rounded Linux minute instead of two, still below the
102-second documentation workflow critical path. PR #349 run `31560173600`
showed the same one-rounded-minute reduction and no critical-path extension.

The exact shellcheck inputs and fatality when reached are retained before
Markdown Bash-fence validation. One apt installation now serves both consumers.
The workflow keeps exactly its four stable documentation jobs; `Markdown Code
Validation` is not renamed. Folding changes scheduling and failure fanout: an
earlier Markdown-job setup failure can now skip shellcheck, and a shellcheck
failure skips later Markdown checks. Hosted evidence and the preserved fatal
input contract make that explicit tradeoff acceptable for this auxiliary check.

The two offline Lychee jobs remain separate. They overlap on Markdown but are
not interchangeable:

- `Link Check` owns Rust/TOML inputs, `.lychee.toml` changes, and the weekly
  non-gating external-host audit.
- `Documentation Link Check` owns a stable status, the detailed internal-link
  checker, Cargo lock and documentation-helper triggers, and a different
  exclusion surface.
- Broadening Documentation Validation to all TOML changes would allocate three
  expensive Rust/Markdown jobs and could cost more than the lightweight runner
  it replaced; removing only its Lychee step would save no allocation.

## Status-policy and regression coverage

P86's same-day effective classic-protection audit proved an empty required-check
list. The current effective rules endpoint still exposes only deletion,
non-fast-forward, and Copilot-review rules; it contains no required-status rule.
The repository's stable-name contract covers neither fuzz matrix names nor the
removed auxiliary shellcheck name. Collapsing those allocations therefore does
not strand an expected/required status.

The CI configuration regression now:

- equates `FUZZ_TARGETS` exactly to every unique `fuzz/Cargo.toml` bin;
- rejects a matrix or omitted target;
- enforces locked metadata before one best-effort all-target prewarm;
- executes a fake-cargo supervisor regression proving every target starts,
  waits, logs, times out, is reaped, and contributes to aggregate failure;
- requires one artifact/log bundle per target and an unconditional live run;
- requires exactly four documentation jobs, one shellcheck install, both exact
  fatal workflow-script inputs, and execution before Markdown Bash validation.

The Lychee configuration tests remain unchanged because the audit found no safe
runner-consolidation opportunity.

## Changelog classification

This changes internal CI scheduling only. It does not change the server binary,
public API, protocol, configuration, runtime behavior, or release artifacts, so
no new changelog entry is warranted. The existing unreleased nightly-analysis
bullet was corrected from the obsolete “fuzz matrix” term to “fuzz-target
inventory.”

## Verification and publication

Formatting, clippy with warnings denied, the complete all-features test suite,
targeted policy tests, workflow hygiene, CI configuration, documentation
consistency, tooling parity, local shellcheck of the exact extracted inputs,
actionlint, LLM policy, and hook readiness/preflight checks pass. The first
adversarial exact-diff review found and drove the hard-watchdog, corpus-trigger,
executable supervisor-test, cache-language, shellcheck-tradeoff, and diagnostic
fixes. Two follow-up rounds drove the remaining timeout and positive-path gaps
to zero findings. Cold and warm hosted before/after measurement, hosted
check/review closure, and green-PR evidence remain before the phase is complete.

## Hosted cold measurement

Draft PR #353's first consolidated fuzz run `31611351490` was a genuine cold
path: job `94163174579` reported `No cache found`. The best-effort all-target
build ran from 15:16:27Z through 15:21:21Z (4m54s), and all four authoritative
targets then ran concurrently through 15:23:24Z (2m03s). The complete job was
green in 7m29s: 7.48 raw and 8 per-job-rounded Linux minutes.

For a like-for-like cold baseline, run `31482862177` also reported `No cache
found`. Its four jobs consumed 5m54s, 7m13s, 5m32s, and 5m26s: 24.08 raw and 26
per-job-rounded minutes. The consolidated cold path therefore saves 16.60 raw
and 18 rounded Linux minutes plus three runner allocations. Every target's log
ended in `DONE`, and the shared job completed successfully.

Documentation run `31611351537` was also green. `Markdown Code Validation`
finished in 68 seconds after absorbing the deleted 14.01-second shellcheck job.
Against PR #350's 58.78-second Markdown plus 14.01-second shellcheck aggregate,
that removes one allocation and saves about 4.79 raw seconds; both arrangements
round to two Linux minutes on these samples. PR #349's 92.93 + 11.27-second
baseline would instead improve from three rounded minutes to the consolidated
job's two. The result confirms the allocation win but does not overclaim a
rounded-minute saving on every run.

The follow-up commit records the cold baseline in the fuzz workflow itself,
triggering an otherwise build-input-identical warm-cache run. P88 remains
validating until that warm result and the complete hosted check/review rollup
are recorded.
