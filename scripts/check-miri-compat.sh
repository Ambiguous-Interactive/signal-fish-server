#!/usr/bin/env bash
# check-miri-compat.sh - Checks for Miri-incompatible patterns in test functions
#
# Scans src/**/*.rs for #[test] and #[tokio::test] functions that call
# Miri-incompatible APIs without a #[cfg_attr(miri, ignore)] attribute.
#
# Miri cannot access the REALTIME system clock (used by wall-clock APIs):
#   - Utc::now()        (chrono — clock_gettime REALTIME)
#   - SystemTime::now() (std — clock_gettime REALTIME)
#
# NOTE: The following are Miri-COMPATIBLE (modern crate versions provide fallbacks):
#   - Instant::now()    (MONOTONIC clock — supported by Miri)
#   - Uuid::new_v4()    (getrandom provides a deterministic Miri fallback)
#   - fill_random       (getrandom provides a deterministic Miri fallback)
#   - rand::rng()       (uses getrandom seed — works under Miri)
#   - tokio::test       (single-threaded runtime works under Miri)
#
# Exit codes:
#   0 - No violations found (or warnings only)

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

log() { echo "[miri-compat] $*"; }
warn() { echo "[miri-compat] WARNING: $*" >&2; }

# Miri-incompatible patterns (pipe-separated for AWK matching)
MIRI_PATTERNS="Utc::now\\(\\)|SystemTime::now\\(\\)"

log "Scanning src/**/*.rs for test functions missing #[cfg_attr(miri, ignore)]..."

# AWK script shared by the per-file warning pass and the summary count pass.
#
# Algorithm:
#   1. When we see #[test] or #[tokio::test], record that we entered a test
#      preamble (the lines between the test attribute and the fn body).
#   2. Also check the 1-2 lines BEFORE the test attribute for miri ignore.
#   3. While in the preamble (before `fn`), check each line for miri ignore.
#   4. Once we hit `fn`, we enter the test body. Scan up to ~50 lines for
#      Miri-incompatible patterns.
#   5. If an incompatible pattern is found without the miri attribute, report it.
#
# The miri ignore attribute can appear before OR after #[test]:
#   #[cfg_attr(miri, ignore)]   <-- before
#   #[test]
#   fn ...
#
#   #[test]
#   #[cfg_attr(miri, ignore)]   <-- after (between #[test] and fn)
#   fn ...
read -r -d '' AWK_SCRIPT << 'AWKEOF' || true
    FILENAME != _prev_file {
        _prev_file = FILENAME
        in_preamble = 0
        in_body = 0
        has_miri_ignore = 0
        test_line = 0
        test_fn_name = ""
        lines_after = 0
        brace_depth = 0
    }

    # Always track where we last saw cfg_attr(miri, ignore)
    /cfg_attr\(miri, ignore\)/ {
        saw_miri_ignore_at = FNR
    }

    # Detect #[test] or #[tokio::test]
    /^[[:space:]]*#\[test\]/ || /^[[:space:]]*#\[tokio::test/ {
        in_preamble = 1
        in_body = 0
        test_line = FNR
        lines_after = 0
        brace_depth = 0
        test_fn_name = ""
        # Check if miri ignore was on this line or within 2 lines before
        has_miri_ignore = (saw_miri_ignore_at >= FNR - 2 && saw_miri_ignore_at <= FNR)
        next
    }

    # In the preamble: lines between #[test] and fn
    in_preamble {
        # Check for miri ignore in the preamble (the "after #[test]" case)
        if (/cfg_attr\(miri, ignore\)/) {
            has_miri_ignore = 1
        }

        # Detect the fn line — transition to body scanning
        if (/^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+/) {
            fn_name = $0
            gsub(/^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+/, "", fn_name)
            gsub(/[^a-zA-Z0-9_].*/, "", fn_name)
            test_fn_name = fn_name
            in_preamble = 0
            in_body = 1
            # Count braces on the fn line itself
            for (i = 1; i <= length($0); i++) {
                c = substr($0, i, 1)
                if (c == "{") brace_depth++
                if (c == "}") brace_depth--
            }
        }
        next
    }

    # In the test body: scan for incompatible patterns
    in_body {
        lines_after++

        # Track brace depth to know when we leave the function
        for (i = 1; i <= length($0); i++) {
            c = substr($0, i, 1)
            if (c == "{") brace_depth++
            if (c == "}") brace_depth--
        }

        # Check for incompatible patterns (skip comment-only lines)
        if (!has_miri_ignore && $0 !~ /^[[:space:]]*\/\//) {
            if (match($0, patterns)) {
                matched = substr($0, RSTART, RLENGTH)
                gsub(/^[[:space:]]+/, "", matched)
                if (mode == "warn") {
                    printf "%s:%d: test fn %s uses %s without #[cfg_attr(miri, ignore)]\n", \
                        FILENAME, test_line, \
                        (test_fn_name != "" ? test_fn_name : "<unknown>"), \
                        matched
                }
                violation_count++
                in_body = 0
                next
            }
        }

        # Stop scanning after ~50 lines or when we leave the function body
        if (lines_after > 50 || (brace_depth <= 0 && lines_after > 2)) {
            in_body = 0
        }
    }
AWKEOF

# Pass 1: emit per-violation warnings
find "$REPO_ROOT/src" -name '*.rs' -type f -print0 \
    | xargs -0 awk -v patterns="$MIRI_PATTERNS" -v mode="warn" \
        "$AWK_SCRIPT" \
    | while IFS= read -r violation; do
        warn "$violation"
    done

# Pass 2: count total violations (the while-pipe above runs in a subshell,
# so we cannot propagate the count; run AWK once more in "count" mode)
TOTAL=$(find "$REPO_ROOT/src" -name '*.rs' -type f -print0 \
    | xargs -0 awk -v patterns="$MIRI_PATTERNS" -v mode="count" \
        "$AWK_SCRIPT
        END { print violation_count + 0 }")

echo ""
if [ "$TOTAL" -gt 0 ]; then
    log "Found $TOTAL test function(s) using Miri-incompatible calls without #[cfg_attr(miri, ignore)]."
    log "Add the attribute before or after #[test] / #[tokio::test]:"
    log ""
    log "    #[test]"
    log "    #[cfg_attr(miri, ignore)]"
    log "    fn my_test() { ... }"
else
    log "All test functions with Miri-incompatible calls have #[cfg_attr(miri, ignore)]."
fi

# Exit 0 — violations are warnings, not errors
exit 0
