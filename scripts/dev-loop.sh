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
#   --features SET   Select named features. --no-default-features is supported.
#   --clippy         Also run scoped clippy once per owning package.
#   --dry-run        Print the resolved commands instead of running them.
#   -h, --help       Show this help.
#
# Patterns are passed through to nextest's `test(...)` filter. A `module::`
# path or a leading `=` is stripped for ownership resolution only. Patterns
# that resolve to the same owning package run in ONE scoped invocation
# (`-E 'test(a) or test(b)'`), so multi-pattern edits cost one build, not one
# per pattern. Each pattern must be one shell word (nextest test names cannot
# contain spaces; a space would split the merged filterset).
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
CHANGED_BASE_SET=0
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
        --all-features | --no-default-features)
            FEATURE_ARGS+=("$1")
            ;;
        --features)
            if [ $# -lt 2 ]; then
                echo "dev-loop: --features requires a value" >&2
                exit 2
            fi
            FEATURE_ARGS+=(--features "$2")
            shift
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
            if [ "$CHANGED_MODE" -eq 1 ] && [ "$CHANGED_BASE_SET" -eq 0 ]; then
                CHANGED_BASE="$1"
                CHANGED_BASE_SET=1
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

# The independent tooling crate owns explicit test targets whose sources stay
# under tests/. Keep source discovery fast without loading Cargo's build graph.
route_target() {
    local target="$1"
    routed_tool=0
    local owner_manifest="Cargo.toml"
    if [ -f scripts/ci-cohort.py ]; then
        owner_manifest=$(python3 scripts/ci-cohort.py --owner "$target")
    elif [ -f tools/repo-checks/Cargo.toml ]; then
        owner_manifest=$(python3 -c 'import sys,tomllib; data=tomllib.load(open(sys.argv[1], "rb")); print(sys.argv[1] if any(t["name"] == sys.argv[2] for t in data.get("test", [])) else "Cargo.toml")' tools/repo-checks/Cargo.toml "$target")
    fi
    if [ "$owner_manifest" = tools/repo-checks/Cargo.toml ]; then
        tool_args+=(--test "$target")
        routed_tool=1
    else
        root_args+=(--test "$target")
    fi
}

run_batch() {
    local manifest="$1"
    local filter="$2"
    local patterns="$3"
    shift 3
    local manifest_args=()
    local filter_args=()
    local batch_features=("${feature_args[@]+"${feature_args[@]}"}")
    # Server feature names do not belong to the independent tooling package.
    if [ "$manifest" = tools/repo-checks/Cargo.toml ]; then
        batch_features=()
    fi
    [ -z "$manifest" ] || manifest_args+=(--manifest-path "$manifest")
    [ -z "$filter" ] || filter_args+=(-E "$filter")
    # Validate every requested filter against nextest's actual discovered tests.
    # Listing and execution share the same scoped build; a valid pattern must
    # never hide an empty sibling pattern in the merged OR expression.
    if [ "$DRY_RUN" -eq 0 ]; then
        local pattern
        for pattern in $patterns; do
            if ! cargo nextest list ${manifest_args[@]+"${manifest_args[@]}"} ${batch_features[@]+"${batch_features[@]}"} "$@" -E "test($pattern)" --message-format json |
                python3 -c 'import json,sys; data=json.load(sys.stdin); sys.exit(0 if any(case["filter-match"]["status"] == "matches" and not case["ignored"] for suite in data["rust-suites"].values() for case in suite["testcases"].values()) else 1)'; then
                echo "dev-loop: '$pattern' matched no runnable tests or test discovery failed" >&2
                overall=1
                return 0
            fi
        done
    fi
    run_cargo cargo nextest run ${manifest_args[@]+"${manifest_args[@]}"} ${batch_features[@]+"${batch_features[@]}"} --no-tests fail "$@" ${filter_args[@]+"${filter_args[@]}"} || overall=1
    if [ "$WITH_CLIPPY" -eq 1 ]; then
        run_cargo cargo clippy ${manifest_args[@]+"${manifest_args[@]}"} ${batch_features[@]+"${batch_features[@]}"} "$@" -- -D warnings || overall=1
    fi
}

run_batches() {
    [ "${#root_args[@]}" -eq 0 ] || run_batch "" "$1" "$root_patterns" "${root_args[@]}"
    [ "${#tool_args[@]}" -eq 0 ] || run_batch tools/repo-checks/Cargo.toml "$1" "$tool_patterns" "${tool_args[@]}"
    return 0
}

overall=0
root_patterns=""
tool_patterns=""

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
# targets and run each owning target's FULL suite once, plus the repo's
# source-hygiene policy scanners. The ownership mirror of the pattern mode
# below, minus the name filter — changed files identify the suites, not
# individual test names.
if [ "$CHANGED_MODE" -eq 1 ]; then
    if ! git rev-parse --verify --quiet "$CHANGED_BASE" >/dev/null; then
        echo "dev-loop: '$CHANGED_BASE' is not a valid git ref" >&2
        exit 2
    fi
    changed_files=$(
        {
            git diff --name-only "$CHANGED_BASE" -- '*.rs' Cargo.toml Cargo.lock build.rs '**/Cargo.toml' '**/Cargo.lock'
            git ls-files --others --exclude-standard -- '*.rs' Cargo.toml Cargo.lock build.rs '**/Cargo.toml' '**/Cargo.lock'
        } | sort -u
    )
    # The script's own contract suite: a dev-loop.sh edit is not a .rs delta,
    # so the ownership mapping below would never validate the script being
    # changed. Contract: tests/dev_loop_script_tests.rs.
    script_changed=0
    if git diff --name-only "$CHANGED_BASE" -- scripts/dev-loop.sh | grep -q .; then
        script_changed=1
    fi
    if [ -z "$changed_files" ] && [ "$script_changed" -eq 0 ]; then
        echo "dev-loop: no Rust changes under $CHANGED_BASE; nothing to run"
        exit 0
    fi

    lib_owner=0
    main_owner=0
    client_manifests=()
    tooling_manifest_changed=0
    owners=()
    while IFS= read -r file; do
        # A here-string over an empty list still yields one empty line; skip
        # it so a script-only delta (or any non-.rs-only delta) never reports
        # an ignored change with an empty path.
        [ -n "$file" ] || continue
        case "$file" in
            src/main.rs)
                main_owner=1
                lib_owner=1
                ;;
            Cargo.toml | Cargo.lock | build.rs | src/*.rs)
                lib_owner=1
                ;;
            tools/repo-checks/Cargo.toml | tools/repo-checks/Cargo.lock | tools/repo-checks/*.rs)
                tooling_manifest_changed=1
                ;;
            tests/*.rs)
                rel="${file#tests/}"
                case "$rel" in
                    */*)
                        # A changed helper module runs under every top-level
                        # target that includes it (same rule as pattern mode).
                        sub="${rel%%/*}"
                        helper_found=0
                        for top in tests/*.rs; do
                            [ -f "$top" ] || continue
                            if grep -qE "(^|[^a-zA-Z0-9_])mod ${sub}([^a-zA-Z0-9_]|$)" "$top"; then
                                add_owner "${top#tests/}"
                                helper_found=1
                            fi
                        done
                        if [ "$helper_found" -eq 0 ]; then
                            echo "dev-loop: no consumer found for '$file'; verification incomplete" >&2
                            overall=1
                        fi
                        ;;
                    *)
                        # Deleted files identify no runnable target.
                        if [ -f "$file" ]; then
                            add_owner "$rel"
                        else
                            lib_owner=1
                        fi
                        ;;
                esac
                ;;
            clients/*)
                client_dir="${file%/*}"
                while [ "$client_dir" != clients ] && [ ! -f "$client_dir/Cargo.toml" ]; do
                    client_dir="${client_dir%/*}"
                done
                if [ -f "$client_dir/Cargo.toml" ]; then
                    found=0
                    for manifest in ${client_manifests[@]+"${client_manifests[@]}"}; do
                        [ "$manifest" != "$client_dir/Cargo.toml" ] || found=1
                    done
                    [ "$found" -eq 1 ] || client_manifests+=("$client_dir/Cargo.toml")
                else
                    echo "dev-loop: '$file' belongs to a client crate; missing Cargo.toml prevents verification" >&2
                    overall=1
                fi
                ;;
            benches/*)
                echo "dev-loop: '$file' is a bench target; validate with 'cargo bench --bench <name> --features allocation-tracking'" >&2
                overall=1
                ;;
            *)
                echo "dev-loop: unsupported Rust change '$file'; verification incomplete" >&2
                overall=1
                ;;
        esac
    done <<< "$changed_files"

    # Collect root checks before invoking Cargo; one build covers all owners.
    root_args=()
    tool_args=()
    [ "$lib_owner" -eq 0 ] || root_args+=(--lib)
    [ "$main_owner" -eq 0 ] || root_args+=(--bin signal-fish-server)
    for target in ${owners[@]+"${owners[@]}"}; do
        route_target "${target%.rs}"
    done
    if [ "$lib_owner" -eq 1 ] || [ "${#owners[@]}" -gt 0 ]; then
        route_target async_timeout_policy_scan
        route_target loud_test_failures_scan
    fi
    if [ "$script_changed" -eq 1 ]; then
        route_target dev_loop_script_tests
    fi
    if [ "$tooling_manifest_changed" -eq 1 ]; then
        tool_args=(--tests)
    fi
    run_batches ""
    for manifest in ${client_manifests[@]+"${client_manifests[@]}"}; do
        run_batch "$manifest" "" "" --tests
    done
    exit "$overall"
fi

# Resolve every pattern first, grouping patterns by owning target so each
# package costs ONE cargo invocation — one build, one nextest pass — no matter
# how many patterns map to it (issue #512, session 266). The groups live in
# the parallel indexed arrays owner_keys/owner_patterns (Bash 3.2-compatible).
owner_keys=()
owner_patterns=()

for pattern in "${PATTERN_ARGS[@]}"; do
    # Resolve ownership from a plain function name: strip an exact-match `=`
    # prefix and any `module::path` the caller included for the filter.
    name="${pattern#=}"
    name="${name##*::}"

    owners=()
    while IFS= read -r file; do
        case "$file" in
            tools/repo-checks/tests/*.rs)
                tooling_name="${file##*/}"
                add_owner "test:${tooling_name%.rs}"
                continue
                ;;
            tools/repo-checks/src/*.rs)
                add_owner tool-lib
                continue
                ;;
        esac
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
    done < <(grep -rlF --include='*.rs' "$name" tests/ tools/repo-checks/tests/ tools/repo-checks/src/ 2>/dev/null || true)

    if grep -rqF --include='*.rs' "$name" src/ 2>/dev/null; then
        add_owner "lib"
    fi

    if [ "${#owners[@]}" -eq 0 ]; then
        echo "dev-loop: no owning target found for '$pattern' (searched '$name' in tests/ and src/)" >&2
        overall=1
        continue
    fi

    for owner in "${owners[@]}"; do
        # Key targets by a prefixed form: a tests/lib.rs target would
        # otherwise collide with the unit-test owner key.
        case "$owner" in
            lib) key="@lib" ;;
            tool-lib) key="@tool-lib" ;;
            test:*) key="t:${owner#test:}" ;;
        esac
        # Parallel indexed arrays (Bash 3.2-compatible: no associative
        # arrays — the repo guard forbids them). Linear scan matches the
        # small owner count per invocation.
        key_index=""
        idx=0
        for existing in ${owner_keys[@]+"${owner_keys[@]}"}; do
            if [ "$existing" = "$key" ]; then
                key_index=$idx
                break
            fi
            idx=$((idx + 1))
        done
        if [ -z "$key_index" ]; then
            owner_keys+=("$key")
            owner_patterns+=("$pattern")
        else
            owner_patterns[key_index]="${owner_patterns[key_index]} $pattern"
        fi
    done
done

# Join space-separated patterns into one nextest filterset:
# `test(a) or test(b)`.
join_filter() {
    local expr=""
    local pattern
    for pattern in $1; do
        if [ -n "$expr" ]; then
            expr="$expr or "
        fi
        expr="${expr}test($pattern)"
    done
    printf '%s' "$expr"
}

root_args=()
tool_args=()
filters=""
idx=0
while [ "$idx" -lt "${#owner_keys[@]}" ]; do
    key="${owner_keys[$idx]}"
    filterset=$(join_filter "${owner_patterns[$idx]}")
    [ -z "$filters" ] || filters="$filters or "
    filters="$filters$filterset"
    case "$key" in
        @lib) root_args+=(--lib); routed_tool=0 ;;
        @tool-lib) tool_args+=(--lib); routed_tool=1 ;;
        t:*) route_target "${key#t:}" ;;
    esac
    if [ "$routed_tool" -eq 1 ]; then
        tool_patterns="$tool_patterns ${owner_patterns[$idx]}"
    else
        root_patterns="$root_patterns ${owner_patterns[$idx]}"
    fi
    idx=$((idx + 1))
done
run_batches "$filters"
exit "$overall"
