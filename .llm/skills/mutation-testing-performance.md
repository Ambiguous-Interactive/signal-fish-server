# Skill: Mutation Testing Performance

<!--
  trigger: mutation testing speed, cargo-mutants timeout, mutation shard,
  mold linker, per-mutant relink, mutation.yml performance, --in-place,
  --sharding slice
  | Keeping cargo-mutants CI shards fast and under their timeout | Infrastructure
-->

**Trigger**: When mutation-testing CI is slow or timing out — `cargo-mutants` shards
exceeding their `timeout-minutes`, per-mutant relink cost, shard sizing, or the
`mutation.yml` build profile/linker.

---

## When to Use

- A `cargo-mutants` shard is slow or being cancelled by `timeout-minutes`
- Re-tuning shard count, the per-shard timeout, or the per-mutant budget
- Changing the mutation build profile, linker, oracle scope, or feature set
- Reviewing a change to `scripts/run-mutants.sh`, `.cargo/mutants.toml`, or
  `.github/workflows/mutation.yml`

---

## When NOT to Use

- General application hot-path optimization (see
  [Rust Performance Optimization](./rust-performance-optimization.md))
- Writing the CI configuration tests themselves (see
  [CI Configuration Validation Tests](./github-actions-config-tests.md))
- Generic workflow caching / action pinning (see
  [GitHub Actions Caching & Action Versioning](./github-actions-caching.md))

---

## TL;DR

- `cargo-mutants` rebuilds the crate once per mutant; the slowness is the
  per-mutant **build + relink**, not the test run.
- Build in the warm `./target` (`--in-place`), not a cold `/tmp` scratch dir, and
  warm a **shared** `rust-cache` first (`shared-key: "mutants"`).
- Give each shard a **contiguous** mutant range (`--sharding slice`) for
  incremental-compilation locality.
- Use the `mold` linker, the `mutants` profile (`debug=0`, `incremental=true`),
  and a `--lib`-only oracle; never `--all-features`.
- All flags live in `scripts/run-mutants.sh`; scope lives in
  `.cargo/mutants.toml`. CI and local devs run the identical thing.

---

## 1. Root Cause: Why the Shards Blew the Timeout

The old `mutation.yml` ran 6 shards with a 20-min timeout and **all six were
cancelled**. Three compounding causes:

1. **Per-shard cold dependency compile.** `cargo-mutants` builds in a `/tmp`
   scratch dir and excludes `./target` from its copy, so each shard started with
   an empty `target/` and recompiled ALL ~428 dependencies from scratch — made
   worse by `--all-features` pulling in `rustls` / `matchbox` / `axum-server`.
2. **Round-robin incremental thrashing.** Default round-robin sharding scatters a
   shard's mutants across many files, so every mutant is a large delta from the
   previous one — incremental compilation never gets locality and per-mutant cost
   grew (measured ~55s and climbing).
3. **Per-mutant relink.** Every mutant relinks the crate; a slow linker multiplies
   that across 319 mutants × N shards.

Net effect: each shard paid a full cold build before testing a single mutant, so
the 20-min timeout fired before any shard finished.

---

## 2. The Levers (ranked) — Standing Rules

All levers are centralised in `scripts/run-mutants.sh` so CI and local runs match.

1. **`--in-place` + a warm shared cache (biggest win).** Build in the
   `rust-cache`-warmed `./target` so dependency rlibs are reused — measured **0 cold
   dep recompiles**. A `baseline` job warms a SHARED `rust-cache`
   (`shared-key: "mutants"`) with the **same Cargo profile + nextest profile +
   RUSTFLAGS** the shards use, and is the green-gate for `--baseline=skip`.
   **Rule:** keep `--in-place`; never
   a `/tmp` scratch build (recompiles all deps cold) and never `--copy-target`
   (deep-copies the multi-GB `target/`).
2. **`--sharding slice`.** Each shard gets a CONTIGUOUS mutant range, preserving
   incremental-compilation locality — kept per-mutant ~stable at ~22s vs
   round-robin's ~55s-and-growing. **Rule:** keep `--sharding slice`
   (with `--no-shuffle`); never round-robin.
3. **`mold` linker.** `scripts/run-mutants.sh` sets the linker via the per-target
   `CARGO_TARGET_<triple>_LINKER=clang` env and the mold link-arg via
   `RUSTFLAGS="-C link-arg=-fuse-ld=mold"`, mirroring the `.devcontainer`
   (the release `Dockerfile` deliberately drops mold so multi-arch
   cross-linking stays simple — see `.llm/context-docs-and-ci-pitfalls.md`).
   **Rule:** keep the linker in the per-target var, NOT a bare
   `-C linker=clang` in RUSTFLAGS — cargo-mutants re-encodes RUSTFLAGS
   (`CARGO_ENCODED_RUSTFLAGS`), which would override a devcontainer's per-target
   linker and diverge the shard fingerprint from the `cargo nextest run` warm
   build, forcing a cold rebuild. The warm job and shards must resolve the
   identical linker + RUSTFLAGS so the cached build fingerprint matches.
4. **`[profile.mutants]`.** Inherits `dev` with `debug=0` and `incremental=true`.
   **Rule:** use `profile.mutants`, NOT `profile.ci` — `profile.ci` sets
   `incremental=false`, which defeats lever 2.
5. **`--lib`-only oracle via `.cargo/mutants.toml`
   (`additional_cargo_args = ["--lib"]`).** This applies `--lib` to the BUILD and
   the test, so the build no longer compiles ~20 integration-test binaries per
   mutant. **Rule:** keep `--lib` in `additional_cargo_args` (not
   `additional_cargo_test_args`); never re-add `--all-features` — the scoped
   modules have zero feature gates, so `cargo mutants --list` stays 319 either way.
6. **Process-isolated nextest oracle (`test_tool = "nextest"`).** A mutation
   that removes a progress guard can hang one unit test. Fail-fast alone does
   not terminate a sibling that is already running, so the dedicated
   `[profile.mutants]` also sets a 10-second per-test termination. **Rule:** both
   the baseline and every mutant shard must install `cargo-nextest` and select
   that profile. The baseline must run the exact unmutated oracle:
   `cargo nextest run --lib --cargo-profile mutants --profile mutants --locked`.
   It also installs `cargo-mutants` and runs the inventory guard, ensuring the
   measured mutant count cannot silently skip in CI.
7. **Shard count sized for serial execution.** `--in-place` runs each shard's
   mutants SERIALLY (one source tree, no `-j`). Measured CI shard 22, the worst
   12-mutant shard, took 310.12s (25.843s/mutant). Adding ~10% headroom and
   rounding up gives a 29s/mutant budget. Resharded to **32**: 319 mutants ÷ 32
   rounds up to 10/shard × 29s = 290s/shard; `timeout-minutes: 10`. **Rule:**
   when the mutant count or per-mutant cost changes, re-size N so each serial
   shard still finishes under the 5-minute target with measured headroom.

---

## 3. The Feasibility Contract

Treat these four quantities as one interlocked budget — changing any one without
re-checking the others can silently reintroduce the cancellation:

```text
{ mutant-count (319), shard-count N, per-shard timeout, per-mutant budget }
```

- **Per-shard target: < 5 min.** `ceil(mutant-count / N) × per-mutant-budget`
  must fit inside the timeout with headroom.
- **Per-shard timeout must stay within `[8, 15]` minutes.**
  - The **floor (8 min)** keeps headroom for a cold-cache miss or a loaded
    runner. A too-tight timeout is itself a flake source — see the
    [Zero-Flakiness Policy](../context-testing.md#zero-flakiness-policy-zero-tolerance).
  - The **ceiling (15 min)** enforces the budget so a regression (e.g. a
    re-added cold dep build) fails loudly instead of merely running slowly.
- Enforced by `test_mutation_shard_budget_is_feasible_vs_timeout`.

Measured in CI: shard 22, the worst 12-mutant shard, took 310.12s, or
25.843s/mutant. Adding ~10% headroom and rounding up gives a conservative
29s/mutant budget. Using 32 shards caps the modeled largest shard at
`ceil(319/32) × 29s = 10 × 29s = 290s`, below the 5-minute target. The
10-minute `timeout-minutes` retains headroom for runner variance or an
occasional cache miss.

---

## 4. Guard Map: Invariant → Enforcing Test

| Invariant | Enforcing test |
|-----------|----------------|
| mold linker + `--in-place` | `test_mutation_workflow_uses_fast_linker_and_in_place` |
| `profile.mutants` (`debug=0`, `incremental=true`) | `test_mutation_workflow_uses_optimized_build_profile` |
| shard matrix is the complete contiguous `0..N-1` partition | `test_mutation_shard_matrix_is_complete_contiguous_partition` |
| budget `{count, N, timeout, per-mutant}` is feasible | `test_mutation_shard_budget_is_feasible_vs_timeout` |
| oracle never uses `--all-features` | `test_mutation_oracle_does_not_use_all_features` |
| full-suite caching jobs drop trybuild artifacts | `test_full_suite_caching_jobs_drop_trybuild_artifacts` |
| mutation scope matches the workflow path filter | `test_mutation_scope_matches_workflow_path_filter` |
| script flag/mode invariants (`run-mutants.sh`) | `tests/run_mutants_script_tests.rs` |

Two pre-existing **free guards** also cover this workflow without new code:

- `test_same_action_uses_consistent_ref_across_workflows` — pins every shared
  action (e.g. `Swatinem/rust-cache`, `actions/checkout`) to a consistent ref, so
  the `baseline` and `mutants` jobs cannot drift apart.
- The script-existence guard asserts `scripts/run-mutants.sh` exists, so the
  single source of truth cannot silently disappear.

---

## 5. Single Source of Truth

- **Flags / linker / profile / shard logic:** `scripts/run-mutants.sh` — the one
  script CI (`mutation.yml`) and local devs both invoke
  (`bash scripts/run-mutants.sh --shard <k>/<N>` or `--warm`).
- **Scope / oracle / feature set:** `.cargo/mutants.toml` (`examine_globs`,
  `additional_cargo_args = ["--lib"]`, `test_tool = "nextest"`, mutation
  nextest profile selection).

Do not duplicate flags into the workflow YAML or a developer's shell history;
change them once in the script (or the toml) so every runner stays in lockstep.

---

## Related Skills

- [Rust Performance Optimization](./rust-performance-optimization.md) — Profiles, linker, and build-time levers
- [CI Configuration Validation Tests](./github-actions-config-tests.md) — How the guard-map tests are written
- [GitHub Actions Caching & Action Versioning](./github-actions-caching.md) — Shared `rust-cache` keys, action pinning
- [Testing Core Patterns](./testing-core-patterns.md) — Mutation testing closes gaps these tests leave
