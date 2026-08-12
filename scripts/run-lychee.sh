#!/usr/bin/env bash
# Run Lychee with fail-if-empty semantics and a durable job summary. Direct
# execution keeps setup under the retrying, checksum-verified repository
# installer while preserving the action's important behavioral guarantees.

set -uo pipefail

report="$(mktemp)"
trap 'rm -f "$report"' EXIT

set +e
lychee --format json --output "$report" "$@"
status=$?

if [[ ! -s "$report" ]]; then
    echo "ERROR: Lychee produced no report" >&2
    if [[ "$status" -ne 0 ]]; then
        exit "$status"
    fi
    exit 1
fi

reporting_status=0
cat "$report" || reporting_status=$?
total="$(sed -n 's/^[[:space:]]*"total":[[:space:]]*\([0-9][0-9]*\),*[[:space:]]*$/\1/p' "$report")" || reporting_status=$?
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    if ! {
        echo "### Lychee link report"
        echo
        echo '```json'
        cat "$report"
        echo '```'
    } >> "$GITHUB_STEP_SUMMARY"; then
        reporting_status=1
    fi
fi

if [[ "$status" -ne 0 ]]; then
    exit "$status"
fi
if [[ "$reporting_status" -ne 0 ]]; then
    echo "ERROR: Could not render the Lychee report" >&2
    exit "$reporting_status"
fi
if [[ -z "$total" || "$total" -eq 0 ]]; then
    echo "ERROR: Lychee found no links; check its inputs and configuration" >&2
    exit 1
fi

exit "$status"
