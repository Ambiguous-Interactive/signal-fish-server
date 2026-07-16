#!/usr/bin/env bash
# Replay a JSONL delivery corpus and prove the seeded divergence is detected.

set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "Usage: bash scripts/run-delivery-trace-validation.sh <trace.jsonl> [output-dir]" >&2
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
    OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/signal-fish-delivery-trace.XXXXXX")"
fi

cleanup() {
    if [ "$KEEP_OUTPUT" = false ]; then
        rm -rf "$OUTPUT_DIR"
    fi
}
trap cleanup EXIT

deadlock_reached_at_index() {
    local log_path="$1"
    local expected_index="$2"
    awk -v expected="$expected_index" '
        /^Error: Deadlock reached\.$/ { saw_deadlock = 1 }
        saw_deadlock && /^\/\\ i = [0-9]+$/ { final_index = $4 }
        END { exit !(saw_deadlock && final_index == expected) }
    ' "$log_path"
}

python3 "$SCRIPT_DIR/generate-delivery-contract-trace.py" \
    --input "$TRACE_PATH" \
    --output-dir "$OUTPUT_DIR/positive" \
    --require-production-socket
bash "$SCRIPT_DIR/run-tla-model-check.sh" --tla-dir "$OUTPUT_DIR/positive" \
    | tee "$OUTPUT_DIR/positive.log"

python3 "$SCRIPT_DIR/generate-delivery-contract-trace.py" \
    --input "$TRACE_PATH" \
    --output-dir "$OUTPUT_DIR/seeded-negative" \
    --seeded-bug

set +e
bash "$SCRIPT_DIR/run-tla-model-check.sh" --tla-dir "$OUTPUT_DIR/seeded-negative" \
    >"$OUTPUT_DIR/seeded-negative.log" 2>&1
negative_status=$?
set -e

if [ "$negative_status" -eq 0 ]; then
    echo "ERROR: seeded trace bug unexpectedly passed TLC" >&2
    cat "$OUTPUT_DIR/seeded-negative.log" >&2
    exit 1
fi
if ! deadlock_reached_at_index "$OUTPUT_DIR/seeded-negative.log" 1; then
    echo "ERROR: seeded trace failed for an unexpected reason" >&2
    cat "$OUTPUT_DIR/seeded-negative.log" >&2
    exit 1
fi

echo "OK   seeded trace divergence deadlocked at i = 1 as expected"

python3 "$SCRIPT_DIR/generate-delivery-contract-trace.py" \
    --input "$SCRIPT_DIR/../formal/traces/slow-consumer-close-flush-invalid.jsonl" \
    --output-dir "$OUTPUT_DIR/slow-close-flush-negative"
set +e
bash "$SCRIPT_DIR/run-tla-model-check.sh" --tla-dir "$OUTPUT_DIR/slow-close-flush-negative" \
    >"$OUTPUT_DIR/slow-close-flush-negative.log" 2>&1
close_flush_status=$?
set -e
if [ "$close_flush_status" -eq 0 ] || \
   ! deadlock_reached_at_index "$OUTPUT_DIR/slow-close-flush-negative.log" 5; then
    echo "ERROR: slow-consumer close-flush trace did not deadlock at i = 5" >&2
    cat "$OUTPUT_DIR/slow-close-flush-negative.log" >&2
    exit 1
fi
echo "OK   slow-consumer close-flush trace deadlocked at i = 5 as expected"

python3 "$SCRIPT_DIR/generate-delivery-contract-trace.py" \
    --input "$SCRIPT_DIR/../formal/traces/post-queue-close-live-drain-invalid.jsonl" \
    --output-dir "$OUTPUT_DIR/post-queue-close-drain-negative"
set +e
bash "$SCRIPT_DIR/run-tla-model-check.sh" --tla-dir "$OUTPUT_DIR/post-queue-close-drain-negative" \
    >"$OUTPUT_DIR/post-queue-close-drain-negative.log" 2>&1
post_close_drain_status=$?
set -e
if [ "$post_close_drain_status" -eq 0 ] || \
   ! deadlock_reached_at_index "$OUTPUT_DIR/post-queue-close-drain-negative.log" 5; then
    echo "ERROR: post-QueueClose live-drain trace did not deadlock at i = 5" >&2
    cat "$OUTPUT_DIR/post-queue-close-drain-negative.log" >&2
    exit 1
fi
echo "OK   post-QueueClose live-drain trace deadlocked at i = 5 as expected"

if [ "$KEEP_OUTPUT" = true ]; then
    echo "Trace-validation artifacts: $OUTPUT_DIR"
fi
