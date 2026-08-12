#!/usr/bin/env bash
# Run every configured cargo-fuzz target concurrently while retaining an
# independent terminal status and complete log for each target.

set -euo pipefail

: "${FUZZ_TARGETS:?FUZZ_TARGETS must list at least one target}"
: "${MAX_TOTAL_TIME:?MAX_TOTAL_TIME must be set}"
: "${RUNNER_TEMP:?RUNNER_TEMP must be set}"

if [[ ! "$MAX_TOTAL_TIME" =~ ^[1-9][0-9]{0,3}$ ]] || ((10#$MAX_TOTAL_TIME > 300)); then
  echo "MAX_TOTAL_TIME must be an integer from 1 through 300 seconds" >&2
  exit 64
fi

watchdog_grace_seconds="${FUZZ_WATCHDOG_GRACE_SECONDS:-300}"
if [[ ! "$watchdog_grace_seconds" =~ ^[0-9]{1,3}$ ]] || ((10#$watchdog_grace_seconds > 300)); then
  echo "FUZZ_WATCHDOG_GRACE_SECONDS must be an integer from 0 through 300 seconds" >&2
  exit 64
fi
watchdog_seconds=$((10#$MAX_TOTAL_TIME + 10#$watchdog_grace_seconds))

read -r -a targets <<< "$FUZZ_TARGETS"
if ((${#targets[@]} == 0)); then
  echo "FUZZ_TARGETS must list at least one target" >&2
  exit 64
fi
logs_dir="$RUNNER_TEMP/fuzz-logs"
mkdir -p "$logs_dir"

pids=()
cleanup_children() {
  local pid
  trap - EXIT INT TERM
  set +e
  for pid in "${pids[@]}"; do
    [ -n "$pid" ] || continue
    kill "$pid" 2>/dev/null
  done
  for pid in "${pids[@]}"; do
    [ -n "$pid" ] || continue
    wait "$pid" 2>/dev/null
  done
}
trap cleanup_children EXIT
trap 'exit 130' INT TERM

for target in "${targets[@]}"; do
  mkdir -p "fuzz/corpus/$target" "fuzz/artifacts/$target"
  : > "$logs_dir/$target.log"
  timeout --signal=TERM --kill-after=10s "${watchdog_seconds}s" \
    cargo +nightly-2026-08-01 fuzz run "$target" \
    --target x86_64-unknown-linux-gnu -- \
    -max_total_time="${MAX_TOTAL_TIME}" -max_len=65536 \
    >"$logs_dir/$target.log" 2>&1 &
  pids+=("$!")
done

failures=()
for index in "${!targets[@]}"; do
  target="${targets[$index]}"
  status=0
  if wait "${pids[$index]}"; then
    :
  else
    status=$?
  fi
  pids[index]=""

  echo "::group::cargo-fuzz $target"
  cat "$logs_dir/$target.log"
  echo "::endgroup::"
  if ((status != 0)); then
    failures+=("$target:$status")
  fi
done

if ((${#failures[@]} != 0)); then
  printf 'Fuzz target failures: %s\n' "${failures[*]}" >&2
  exit 1
fi
