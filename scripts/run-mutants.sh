#!/usr/bin/env bash
# run-mutants.sh - Single source of truth for running cargo-mutants fast.
#
# WHY THIS SCRIPT EXISTS
#   Mutation testing rebuilds the crate once per mutant (343 mutants). Naively,
#   cargo-mutants also recompiles every dependency from scratch in a /tmp scratch
#   dir (it excludes ./target from its copy), so each CI shard paid a full cold
#   dependency build -> every shard blew the 20-min timeout and was cancelled.
#
#   This script centralises the levers that bring each shard under 5 minutes, so
#   CI (.github/workflows/mutation.yml) and local devs run the EXACT same thing:
#     1. --warm builds the crate+deps into target/mutants/ with the SAME profile
#        and RUSTFLAGS the shards use, so Swatinem/rust-cache warms a cache the
#        shards can actually reuse (matching build fingerprint).
#     2. --in-place builds in ./target (warmed by rust-cache) instead of copying
#        the source to a /tmp scratch dir whose target/ starts EMPTY. This is the
#        fix for the per-shard cold dependency rebuild. (--copy-target was the
#        alternative but it deep-copies the whole multi-GB ./target per shard —
#        slow and variable I/O that risks flaky timeouts; --in-place is
#        deterministic and reuses both dep rlibs and the incremental cache.)
#     3. mold linker (mirrors Dockerfile / .devcontainer) cuts per-mutant relink.
#     4. profile "mutants" (debug=0, incremental=true) trims build/relink cost.
#     5. --baseline=skip drops the redundant per-shard baseline test (the --warm
#        job is the green-gate instead); requires an explicit --timeout.
#   --in-place runs mutants serially (one source tree, so no -j parallelism); the
#   shard count in mutation.yml is sized so each serial shard still finishes <5m.
#   The per-mutant oracle is cargo-nextest (`test_tool` in mutants.toml): its
#   dedicated profile terminates an individual mutation-induced hang after 10s.
#
# STANDING RULES (enforced by tests/ci_config_tests.rs + run_mutants_script_tests):
#   - Never re-add --all-features to the oracle (.cargo/mutants.toml stays --lib).
#   - Keep --in-place (a /tmp scratch build recompiles all deps cold; --copy-target
#     deep-copies the multi-GB target). Both regress the per-shard time.
#   - Keep {mutant-count, shard-count N, per-shard timeout, per-mutant budget}
#     feasible together; see .llm/skills/mutation-testing-performance/SKILL.md.
#
# Usage:
#   bash scripts/run-mutants.sh --shard <k>/<N>       # run one shard (CI + local)
#   bash scripts/run-mutants.sh --warm                # warm build + green-gate
#   bash scripts/run-mutants.sh --shard 0/36 --print-cmd   # print, do not run
#
# Env:
#   MUTANTS_DRY_RUN   set to 1 for the same effect as --print-cmd

set -euo pipefail

# --- Levers (keep in sync with tests/ci_config_tests.rs policy tests) --------
MUTANTS_PROFILE="mutants"
MUTANTS_TIMEOUT="120"

# Configure the mold linker the way the Dockerfile / .devcontainer do: pick the
# linker via the per-TARGET `CARGO_TARGET_<triple>_LINKER` env and pass the mold
# link-arg via RUSTFLAGS. This is deliberately NOT a bare `-C linker=clang` in
# RUSTFLAGS: cargo-mutants re-encodes RUSTFLAGS (CARGO_ENCODED_RUSTFLAGS), which
# would override a devcontainer's per-target linker and diverge the shard build
# fingerprint from the `cargo nextest run` --warm build — forcing a cold rebuild and
# defeating the whole warm-shared-cache scheme. Keeping the linker in the
# per-target var means `cargo nextest run` (warm) and `cargo mutants` (shards)
# resolve the IDENTICAL linker + RUSTFLAGS content, so they share one cache
# fingerprint.
# In a devcontainer that already sets the per-target linker we leave it as-is.
HOST_TRIPLE="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' || true)"
if [ -n "$HOST_TRIPLE" ]; then
    LINKER_VAR="CARGO_TARGET_$(printf '%s' "$HOST_TRIPLE" | tr 'a-z-' 'A-Z_')_LINKER"
    if [ -z "${!LINKER_VAR:-}" ]; then
        export "${LINKER_VAR}=clang"
    fi
fi
# Append the mold link-arg so a pre-existing RUSTFLAGS is preserved, never
# clobbered (clobbering would change the fingerprint vs the warm cache).
export RUSTFLAGS="-C link-arg=-fuse-ld=mold ${RUSTFLAGS:-}"

usage() {
    cat >&2 <<'EOF'
Usage:
  run-mutants.sh --shard <k>/<N> [--print-cmd]   Run one mutation shard.
  run-mutants.sh --warm [--print-cmd]            Warm build + green-gate (baseline job).

Exactly one of --shard or --warm is required.
EOF
}

MODE=""
SHARD=""
PRINT_CMD="${MUTANTS_DRY_RUN:-0}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --shard)
            MODE="shard"
            SHARD="${2:-}"
            shift 2 || { echo "ERROR: --shard requires a <k>/<N> value" >&2; exit 2; }
            ;;
        --shard=*)
            MODE="shard"
            SHARD="${1#*=}"
            shift
            ;;
        --warm)
            MODE="warm"
            shift
            ;;
        --print-cmd)
            PRINT_CMD="1"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage
            exit 2
            ;;
    esac
done

if [ -z "$MODE" ]; then
    echo "ERROR: one of --shard <k>/<N> or --warm is required" >&2
    usage
    exit 2
fi

# Build the command for the selected mode.
if [ "$MODE" = "warm" ]; then
    # Warm the cache AND act as the green-gate for --baseline=skip: a full
    # unmutated run of the exact nextest oracle used by cargo-mutants. Both
    # profile flags matter: --cargo-profile selects target/mutants/, while
    # --profile selects nextest's per-test 10s termination policy.
    CMD=(cargo nextest run --lib --features trace-validation --cargo-profile "$MUTANTS_PROFILE" --profile "$MUTANTS_PROFILE" --locked)
else
    # shard mode: validate the k/N form before doing anything expensive.
    if ! printf '%s' "$SHARD" | grep -Eq '^[0-9]+/[0-9]+$'; then
        echo "ERROR: --shard must be <k>/<N> (e.g. 0/36), got: '${SHARD}'" >&2
        exit 2
    fi
    # --in-place: build in ./target (warmed by rust-cache) so deps are reused, not
    #   recompiled cold in a scratch dir. Runs serially (no -j).
    # --sharding slice: give each shard a CONTIGUOUS range of mutants (consecutive
    #   mutants in the same file/function) instead of round-robin scatter. This
    #   preserves Rust INCREMENTAL-compilation locality across a shard: each mutant
    #   is a tiny delta from the previous one, so the per-mutant rebuild stays
    #   ~stable (~20s) instead of thrashing the incremental cache (round-robin
    #   measured ~55s+ and growing, because every mutant lands in a different file).
    # --baseline=skip: the --warm job already proved the suite green; this skips
    #   the redundant per-shard baseline test (requires an explicit --timeout).
    CMD=(
        cargo mutants
        --no-shuffle
        --sharding slice
        --shard "$SHARD"
        --in-place
        --profile "$MUTANTS_PROFILE"
        --baseline=skip
        --timeout "$MUTANTS_TIMEOUT"
        --cargo-arg=--locked
    )
fi

if [ "$PRINT_CMD" = "1" ]; then
    echo "RUSTFLAGS=${RUSTFLAGS}"
    echo "${CMD[*]}"
    exit 0
fi

echo "RUSTFLAGS=${RUSTFLAGS}" >&2
echo "+ ${CMD[*]}" >&2
exec "${CMD[@]}"
