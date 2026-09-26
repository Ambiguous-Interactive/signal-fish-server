#!/usr/bin/env bash
# dev-loop.sh - Run tests by name or by changed files through the fastest
# scoped cargo invocation.
#
# A bare `cargo nextest run -E 'test(x)'` rebuilds every test binary (~47 s
# warm here); the same test scoped to its owning target rebuilds one binary
# (~10 s). This script resolves each test-name pattern to its owning target
# with grep (no cargo involvement) and runs the scoped command, so agents and
# humans always take the fast path during red-green iteration (issue #512,
# session 262). It is a loop accelerator, not a gate: run the full local
# sequence from .llm/context.md once before publication.
#
# Usage:
#   scripts/dev-loop.sh [options] <test-name-pattern> [more patterns...]
#   scripts/dev-loop.sh [options] --changed [base-ref]
#
# Options:
#   --all-features   Forward --all-features to cargo (matches the mandatory
#                    gate; slower compile).
#   --clippy         Also run scoped clippy for each owning target.
#   --dry-run        Print the resolved commands instead of running them.
#   -h, --help       Show this help.
#
# Patterns are passed through to nextest's `test(...)` filter. A `module::`
# path or a leading `=` is stripped for ownership resolution only.
#
# `--changed` maps the working tree's Rust deltas (vs `base-ref`, default
# HEAD, untracked files included) onto owning targets and runs each owning
# target's FULL suite once: src/ changes run the unit tests, a tests/<t>.rs
# change runs that target, and a helper-module change runs every top-level
# target that includes it. This is the one-command form of the red-green
# rule: after editing code plus its test file, run exactly the suites that
# can have changed — warm scoped runs cost ~10 s each instead of the ~47 s
# bare rebuild (issue #512, session 262).
#
# Exit codes: 0 all resolved work succeeded; 1 some pattern matched no target
# or a cargo invocation failed; 2 usage error.

set -euo pipefail

usage() {
    # Print the leading comment block (lines 2..first non-comment) so --help
    # never leaks shell code regardless of how long the header grows.
    awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' \
        "${BASH_SOURCE[0]}"
}

DRY_RUN=0
WITH_CLIPPY=0
CHANGED_MODE=0
CHANGED_BASE="HEAD"
PATTERN_ARGS=()
FEATURE_ARGS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run)
            DRY_RUN=1
            ;;
        --clippy)
            WITH_CLIPPY=1
            ;;
        --all-features)
            FEATURE_ARGS+=(--all-features)
            ;;
        --changed)
            CHANGED_MODE=1
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        --*)
            echo "dev-loop: unknown option $1" >&2
            usage >&2
            exit 2
            ;;
        -*)
            echo "dev-loop: unknown option $1" >&2
            usage >&2
            exit 2
            ;;
        *)
            if [ "$CHANGED_MODE" -eq 1 ] && [ "${#PATTERN_ARGS[@]}" -eq 0 ]; then
                CHANGED_BASE="$1"
            else
                PATTERN_ARGS+=("$1")
            fi
            ;;
    esac
    shift
done

if [ "$CHANGED_MODE" -eq 1 ]; then
    if [ "${#PATTERN_ARGS[@]}" -gt 0 ]; then
        echo "dev-loop: --changed takes at most one base ref, not test patterns" >&2
        usage >&2
        exit 2
    fi
else
    if [ "${#PATTERN_ARGS[@]}" -eq 0 ]; then
        usage >&2
        exit 2
    fi
fi

feature_args=("${FEATURE_ARGS[@]+"${FEATURE_ARGS[@]}"}")

run_cargo() {
    if [ "$DRY_RUN" -eq 1 ]; then
        printf 'DRY-RUN:'
        printf ' %q' "$@"
        printf '\n'
    else
        "$@"
    fi
}

overall=0

# Append $1 to the owners list unless it is already present.
add_owner() {
    local candidate="$1"
    local existing
    for existing in ${owners[@]+"${owners[@]}"}; do
        if [ "$existing" = "$candidate" ]; then
            return 0
        fi
    done
    owners+=("$candidate")
}

# `--changed [base-ref]` mode: map the working tree's Rust deltas onto owning
# targets and run each owning target's FULL suite once. The ownership mirror
# of the pattern mode below, minus the name filter — changed files identify
# the suites, not individual test names.
if [ "$CHANGED_MODE" -eq 1 ]; then
    if ! git rev-parse --verify --quiet "$CHANGED_BASE" >/dev/null; then
        echo "dev-loop: '$CHANGED_BASE' is not a valid git ref" >&2
        exit 2
    fi
    changed_files=$(
        {
            git diff --name-only "$CHANGED_BASE" -- '*.rs'
            git ls-files --others --exclude-standard -- '*.rs'
        } | sort -u
    )
    if [ -z "$changed_files" ]; then
        echo "dev-loop: no Rust changes under $CHANGED_BASE; nothing to run"
        exit 0
    fi

    lib_owner=0
    owners=()
    while IFS= read -r file; do
        case "$file" in
            src/*.rs)
                lib_owner=1
                ;;
            tests/*.rs)
                rel="${file#tests/}"
                case "$rel" in
                    */*)
                        # A changed helper module runs under every top-level
                        # target that includes it (same rule as pattern mode).
                        sub="${rel%%/*}"
                        for top in tests/*.rs; do
                            [ -f "$top" ] || continue
                            if grep -qE "(^|[^a-zA-Z0-9_])mod ${sub}([^a-zA-Z0-9_]|$)" "$top"; then
                                add_owner "${top#tests/}"
                            fi
                        done
                        ;;
                    *)
                        # Deleted files identify no runnable target.
                        [ -f "$file" ] && add_owner "$rel"
                        ;;
                esac
                ;;
            clients/*)
                echo "dev-loop: '$file' belongs to a client crate; run its own suite (e.g. cd clients/native)" >&2
                ;;
            benches/*)
                echo "dev-loop: '$file' is a bench target; validate with 'cargo bench --bench <name> --features allocation-tracking'" >&2
                ;;
            *)
                echo "dev-loop: ignoring non-crate Rust change '$file'" >&2
                ;;
        esac
    done <<< "$changed_files"

    overall=0
    if [ "$lib_owner" -eq 1 ]; then
        echo "dev-loop: changed src/ files -> unit tests (src/, --lib)"
        run_cargo cargo nextest run ${feature_args[@]+"${feature_args[@]}"} --no-tests warn --lib || overall=1
        if [ "$WITH_CLIPPY" -eq 1 ]; then
            run_cargo cargo clippy ${feature_args[@]+"${feature_args[@]}"} --lib -- -D warnings || overall=1
        fi
    fi
    for target in ${owners[@]+"${owners[@]}"}; do
        echo "dev-loop: changed tests/$target -> integration target ${target%.rs}"
        run_cargo cargo nextest run ${feature_args[@]+"${feature_args[@]}"} --no-tests warn --test "${target%.rs}" || overall=1
        if [ "$WITH_CLIPPY" -eq 1 ]; then
            run_cargo cargo clippy ${feature_args[@]+"${feature_args[@]}"} --test "${target%.rs}" -- -D warnings || overall=1
        fi
    done
    exit "$overall"
fi

for pattern in "${PATTERN_ARGS[@]}"; do
    # Resolve ownership from a plain function name: strip an exact-match `=`
    # prefix and any `module::path` the caller included for the filter.
    name="${pattern#=}"
    name="${name##*::}"

    owners=()
    while IFS= read -r file; do
        rel="${file#tests/}"
        case "$rel" in
            */*)
                # A helper module's tests (for example
                # tests/websocket_test_helpers/chaos_proxy.rs) compile into
                # and run under every top-level target that includes the
                # module, so those targets own the pattern.
                sub="${rel%%/*}"
                for top in tests/*.rs; do
                    [ -f "$top" ] || continue
                    if grep -qE "(^|[^a-zA-Z0-9_])mod ${sub}([^a-zA-Z0-9_]|$)" "$top"; then
                        stem="${top#tests/}"
                        add_owner "test:${stem%.rs}"
                    fi
                done
                ;;
            *)
                # Top-level ownership mirrors nextest's substring filter, but
                # the file must contain a runnable test form (`#[test]`,
                # `#[tokio::test]` with or without a flavor, or a `proptest!`
                # block); harness-only modules can never own a runnable test.
                if grep -qF "$name" "$file" &&
                    grep -qE '#\[(tokio::)?test(\(|\])|proptest!' "$file"; then
                    add_owner "test:${rel%.rs}"
                fi
                ;;
        esac
    done < <(grep -rlF --include='*.rs' "$name" tests/)

    if grep -rqF --include='*.rs' "$name" src/ 2>/dev/null; then
        add_owner "lib"
    fi

    if [ "${#owners[@]}" -eq 0 ]; then
        echo "dev-loop: no owning target found for '$pattern' (searched '$name' in tests/ and src/)" >&2
        overall=1
        continue
    fi

    for owner in "${owners[@]}"; do
        case "$owner" in
            lib)
                echo "dev-loop: '$pattern' -> unit tests (src/, --lib)"
                run_cargo cargo nextest run ${feature_args[@]+"${feature_args[@]}"} --no-tests warn --lib -E "test($pattern)" || overall=1
                if [ "$WITH_CLIPPY" -eq 1 ]; then
                    run_cargo cargo clippy ${feature_args[@]+"${feature_args[@]}"} --lib -- -D warnings || overall=1
                fi
                ;;
            test:*)
                target="${owner#test:}"
                echo "dev-loop: '$pattern' -> integration target $target"
                run_cargo cargo nextest run ${feature_args[@]+"${feature_args[@]}"} --no-tests warn --test "$target" -E "test($pattern)" || overall=1
                if [ "$WITH_CLIPPY" -eq 1 ]; then
                    run_cargo cargo clippy ${feature_args[@]+"${feature_args[@]}"} --test "$target" -- -D warnings || overall=1
                fi
                ;;
        esac
    done
done

exit "$overall"
