#!/usr/bin/env bash
# Replay a sequenced-relay JSONL corpus and prove four seeded divergences.

set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "Usage: bash scripts/run-sequenced-relay-trace-validation.sh <trace.jsonl> [output-dir]" >&2
    exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TRACE_PATH="$1"
KEEP_OUTPUT=false
if [ "$#" -eq 2 ]; then
    OUTPUT_DIR="$2"
    mkdir -p "$OUTPUT_DIR"
    if [ -n "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
        echo "ERROR: output directory must be empty: $OUTPUT_DIR" >&2
        exit 2
    fi
    OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
    KEEP_OUTPUT=true
else
    OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/signal-fish-sequenced-relay-trace.XXXXXX")"
fi

cleanup() {
    if [ "$KEEP_OUTPUT" = false ]; then
        rm -rf "$OUTPUT_DIR"
    fi
}
trap cleanup EXIT

python3 "$SCRIPT_DIR/generate-sequenced-relay-trace.py" \
    --input "$TRACE_PATH" \
    --output-dir "$OUTPUT_DIR/positive"
bash "$SCRIPT_DIR/run-tla-model-check.sh" --tla-dir "$OUTPUT_DIR/positive"

for bug in duplicate-data silent-gap backward-epoch late-lifecycle; do
    bundle="$OUTPUT_DIR/$bug"
    log="$OUTPUT_DIR/$bug.log"
    python3 "$SCRIPT_DIR/generate-sequenced-relay-trace.py" \
        --input "$TRACE_PATH" \
        --output-dir "$bundle" \
        --seeded-bug "$bug"
    set +e
    bash "$SCRIPT_DIR/run-tla-model-check.sh" --tla-dir "$bundle" >"$log" 2>&1
    status=$?
    set -e
    if [ "$status" -eq 0 ] || ! grep -q '^Error: Deadlock reached\.$' "$log"; then
        echo "ERROR: $bug did not produce the expected replay deadlock" >&2
        cat "$log" >&2
        exit 1
    fi
    echo "OK   $bug produced the expected replay deadlock"
done

if [ "$KEEP_OUTPUT" = true ]; then
    echo "Trace-validation artifacts: $OUTPUT_DIR"
fi
