# Session 121 — Numeric-conversion safety

## Correctness-first scope

The default branch began clean and green at `a600e4b`, with 52/52 current-SHA
checks successful, no open or draft pull request, and no dependency update left
to incorporate. P56/issue #290 remains the highest gameplay risk but is bound
to its unchanged hosted cohort at 5/20 eligible first attempts; P53 is likewise
bound to 4/20 scheduled allocations per OS. The highest-impact local increment
was issue #213's numeric-conversion audit.

## Failure-first evidence

The normal all-target Clippy policy was green, but enabling
`cast_possible_truncation`, `cast_possible_wrap`, and `cast_sign_loss` exposed
59 reports across production and test targets. Fifteen were in the library and
two in the binary. The material defect was an unrestricted configured `u64`
reconnection window narrowed to `i64`: values above `i64::MAX` wrapped negative,
so token creation and disconnect expiry calculations produced already-expired
deadlines instead of the configured long window.

## Implementation and coverage

Reconnection windows now remain unsigned through the manager, token, and
disconnect-expiry paths. Values beyond Chrono's representable duration saturate
at `DateTime::MAX_UTC`; a focused boundary test proves `u64::MAX` produces a
valid token rather than an immediate expiry. Dashboard percentile selection now
uses exact per-mille integer ranks, avoiding a lossy float-to-index conversion;
data-driven tests pin rounding, empty input, clamping, and `usize::MAX` overflow
safety.

Every other reported root-package production conversion now states its boundary
explicitly: durations and counters saturate, negative timestamps map to zero,
and bounded capacities use checked conversions. Integration and policy fixtures
use checked conversions as well. Prometheus integer parsing no longer
round-trips decimal renderings through `f64`, and production exposition now
renders integer metrics without a floating-point conversion, preserving exact
`u64` values end to end. Cargo's lint policy now enforces the three conversion
lints across every root package target, so the class cannot silently return
there.

The reconnection behavior is user-visible and is recorded under Unreleased /
Fixed. Wire schemas, timeout values, dashboard percentile labels, dependency
graphs, and the P53/P56 selectors and evidence cohorts are unchanged.

## Mandatory hook-latency follow-through

The mandatory final pre-commit profile took 2,637–2,846 ms, including
1,478–1,710 ms in the Rust panic check and 656–683 ms in changed-file
discovery, so session 121 also advanced issue #318 rather than handing off over
the repository's explicit one-second budget. The fast gate had loaded the full
line-aware Rust scanner for any added test-context attribute, even though an
added guard cannot expose an existing production panic.

The gate now inspects one zero-context Git diff and loads the full scanner only
when a panic macro is added or a test-context guard is removed. Untracked Rust,
removed guards, added panic macros, and Git failures remain fail-closed; added
test guards and removed panic macros take the fast path. The PowerShell fixture
pins all four directional classifications in staged and worktree modes.

The first classifier-only profiles exposed two remaining worktree outliers at
1,041 and 1,164 ms. Worktree status refresh and the Rust diff now start together
before PowerShell registers the remaining policy functions, overlapping their
unavoidable Git I/O with hook setup. Twenty-three consecutive enforced
worktree profiles with the hook policy itself changed then completed in
741–989 ms. Five independent staged profiles completed in 535–559 ms. The hook
adds no runtime beyond PowerShell and Git.

The adversarial stress pass also recorded filesystem-contention outliers: 8 of
14 tightly repeated worktree invocations exceeded the budget while changed-file
discovery alone varied from 833 to 3,020 ms. The same reviewer then observed
five quiet consecutive worktree passes at 742–828 ms and seven staged passes at
507–523 ms. The acceptance protocol therefore measures five consecutive warm,
quiescent runs per mode, with no concurrent Cargo or hook process; contention
outliers remain visible diagnostics but are not misclassified as hook-policy
CPU regressions. The staged hook—the developer commit-path guard—remained below
budget throughout both review passes. After the exact publication index was
staged, one warm-up plus five enforced staged profiles completed; the measured
set was 621–804 ms. The matching final worktree preflight completed in 809 ms
and pre-push in 277 ms.

## Review and verification

Five independent adversarial passes found and closed the production Prometheus
precision gap, public reconnection API compatibility regression, missing Chrono
overflow branch, lint-scope wording, premature phase status, directional hook
fixture gap, and worktree profile protocol ambiguity. The final settled review
reported zero findings.

The settled tree passes formatting; warning-denied all-target/all-feature
Clippy; the complete locked all-feature test suite; all 313 CI-config policy
tests; doc, workflow, LLM-file, hook-readiness, pre-commit, and pre-push gates;
and cargo-deny's advisory, ban, license, and source checks. Exact-head hosted
checks, pull-request review state, and publication closure are recorded after
the branch is published.
