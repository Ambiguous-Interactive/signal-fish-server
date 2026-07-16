#!/usr/bin/env bash
# Compile optional feature combinations once, outside the default Rust test
# suite. Keeping this as a standalone CI step prevents nested Cargo builds from
# running again under nextest, coverage, MSRV, Miri, and sanitizer jobs.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$REPO_ROOT"

FEATURE_CASES=(
    "TLS support|tls"
    "Legacy full-mesh compatibility|legacy-fullmesh"
    "Delivery trace validation|trace-validation"
    "Combined optional feature compatibility|tls,legacy-fullmesh,trace-validation"
)

failures=0

echo "Checking optional feature compile matrix..."
for case in "${FEATURE_CASES[@]}"; do
    label="${case%%|*}"
    feature="${case#*|}"

    echo ""
    echo "==> $label ($feature)"
    if cargo check --locked --no-default-features --features "$feature"; then
        echo "PASS: $label"
    else
        echo "FAIL: $label ($feature)"
        failures=$((failures + 1))
    fi
done

if [ "$failures" -ne 0 ]; then
    echo ""
    echo "Optional feature compile matrix failed: $failures case(s)."
    exit 1
fi

echo ""
echo "Optional feature compile matrix passed."
