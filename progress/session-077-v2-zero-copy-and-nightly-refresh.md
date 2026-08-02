# Session 077 — V2 zero-copy relay and nightly refresh

## Scope

Advance the highest-gameplay-impacting concrete work under issue #207, then
close issue #243's bounded CI-analysis maintenance in the same session PR.

## V2 raw-binary relay projection

The frozen-v2 JSON and Rkyv binary formats are raw payload passthroughs. Their
payload already enters the relay as shared `Bytes`, but the socket projector
called `payload.to_vec()` and then converted that fresh vector back to `Bytes`.
The universal v2 relay floor therefore paid a full payload copy plus one or two
allocation operations, depending on relay cohort size.

The production-seam allocation harness now includes JSON and Rkyv v2 binary
cells at 2, 8, and 16 players. Before the production change those cells used
4/6/6 allocation operations and 1,419/2,363/2,683 bytes per relay; the checked-
in zero-copy ceilings failed immediately at the two-player cell. Passing the
shared handle directly reduces the cells to 3/4/4 operations and
424/1,344/1,664 bytes. Every pre/post wire-byte ledger and SHA-256 digest is
identical, while codec-work counters remain zero and delivery/queue ledgers
remain exact. No runtime claim is made because the deterministic allocation
result is sufficient and no uncontaminated pre-change timing pair was recorded.

## Analysis nightly baseline

The `nightly-2026-02-01` analysis pin was 182 days old. The replacement
`nightly-2026-08-01` is available with Miri, rust-src, and x86_64 GNU standard
libraries and resolves the locked fuzz graph. Miri, AddressSanitizer,
cargo-fuzz, cargo-udeps, the devcontainer, local fuzz instructions, and current
toolchain documentation now share that explicit pin.

The prior consistency test inspected only the first date in `ci-safety.yml`
and `unused-deps.yml`; it omitted `fuzz.yml`, so both a partial workflow update
and fuzz drift could pass. The replacement scans every live operational
occurrence across all three workflows and the devcontainer. The separate
Fortress WASM pin remains excluded because it is coupled to the released Godot/
Emscripten compatibility matrix. The freshness test now computes real UTC age
instead of comparing against a frozen February 2026 reference.

The audit also found that stable CI compiled all four fuzz targets while the
nightly coverage-guided matrix ran only two. The hosted matrix now smoke-runs
every `[[bin]]` declared by the fuzz manifest, and a manifest-derived policy
test prevents future targets from silently receiving compile-only coverage.

Executing the restored state-machine lane exposed a stale fuzz oracle from
before intentional delivery cancellations became a separate outcome. The
reproducer had ten attempts, eight enqueues, one drop, and one cancellation;
production satisfied the current two-sided conservation law, while the target
still enforced the obsolete three-outcome equality. The oracle now includes
cancellations and uses the same bounded self-stabilizing balance poll as the
stable suite.

The old cargo-udeps 0.1.53 binary also embedded a Cargo parser too old for an
Edition 2024 dependency manifest. The workflow now installs and caches 0.1.61
with the pinned nightly and scans all targets plus all features. Its first
complete run identified two genuinely unused direct dev-dependencies
(`futures` and `tokio-test`); removing them leaves a zero-finding analysis.

## Validation and publication

In progress. Final evidence will include the complete local gauntlet,
adversarial review results, exact commit, hosted analysis step outcomes, all
applicable workflow conclusions, and reviewer state.
