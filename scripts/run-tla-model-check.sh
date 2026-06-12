#!/usr/bin/env bash
# run-tla-model-check.sh - Model-check the Protocol v3 session-lifecycle TLA+ spec.
#
# Downloads a version-pinned, SHA256-verified tla2tools.jar into a cache
# directory, then runs TLC over the model configurations in formal/tla/
# (desired=mesh, desired=host, desired=host with WebRTC disabled, and the
# relay-floor model with both upgrade transports disabled). The default run
# globs formal/tla/SignalFishSession_*.cfg, so a new configuration file is
# picked up automatically. Any invariant or property violation exits
# nonzero; success is quiet (one summary line per configuration).
#
# TLC deadlock checking stays ENABLED on purpose: the spec's churn-budget
# terminal states self-loop through an explicit `Done` stutter action, so a
# reported deadlock can only be a real modeling bug (a reachable mid-protocol
# state with no enabled action), never the budget running out.
#
# Usage:
#   bash scripts/run-tla-model-check.sh                 # check every formal/tla/*.cfg
#   bash scripts/run-tla-model-check.sh --config Mesh   # check one configuration
#   bash scripts/run-tla-model-check.sh --verbose       # stream full TLC output
#
# --config accepts a full cfg name (SignalFishSession_Mesh), a file name
# (SignalFishSession_Mesh.cfg), or the scenario suffix alone (Mesh).
#
# Environment:
#   SIGNAL_FISH_TLA_CACHE_DIR  Override the jar cache directory
#                              (default: ${XDG_CACHE_HOME:-~/.cache}/signal-fish/tla).
#   TLC_WORKERS                TLC worker count (default: auto).

set -euo pipefail

# Version + checksum pin for the TLA+ tools (TLC 2.19, "The Bender release").
# Update both together; the checksum is verified on every run, including
# cache hits, so a corrupted or tampered jar can never execute.
TLA_TOOLS_VERSION="v1.7.4"
TLA_TOOLS_SHA256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
TLA_TOOLS_URL="https://github.com/tlaplus/tlaplus/releases/download/${TLA_TOOLS_VERSION}/tla2tools.jar"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
TLA_DIR="${REPO_ROOT}/formal/tla"
SPEC_NAME="SignalFishSession"

CACHE_DIR="${SIGNAL_FISH_TLA_CACHE_DIR:-${XDG_CACHE_HOME:-${HOME}/.cache}/signal-fish/tla}"
JAR_PATH="${CACHE_DIR}/tla2tools-${TLA_TOOLS_VERSION}.jar"

SELECTED_CONFIG=""
VERBOSE=false

usage() {
    cat <<'USAGE'
Usage: bash scripts/run-tla-model-check.sh [--config <name>] [--verbose]

Options:
  --config <name>  Check a single configuration. <name> may be the scenario
                   suffix (Mesh, Host, HostDirect, Floor), the cfg base name
                   (SignalFishSession_Mesh), or the file name with .cfg.
  --verbose        Stream full TLC output instead of a quiet summary.
  --help           Show this help.
USAGE
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --config)
            if [ "$#" -lt 2 ]; then
                echo "ERROR: --config requires an argument" >&2
                exit 2
            fi
            SELECTED_CONFIG="$2"
            shift 2
            ;;
        --verbose|-v)
            VERBOSE=true
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: Unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

# --- Locate java -----------------------------------------------------------

if ! command -v java >/dev/null 2>&1; then
    echo "ERROR: 'java' was not found on PATH. TLC (the TLA+ model checker) needs a JRE (11+)." >&2
    echo "Install hints:" >&2
    echo "  Debian/Ubuntu: sudo apt-get install -y default-jre-headless" >&2
    echo "  macOS:         brew install temurin" >&2
    echo "  CI:            actions/setup-java with a Temurin JRE" >&2
    exit 1
fi

# --- Fetch + verify the pinned tla2tools.jar -------------------------------

verify_jar() {
    # Prints nothing; returns nonzero when the checksum does not match.
    local actual
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$JAR_PATH" | cut -d' ' -f1)"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$JAR_PATH" | cut -d' ' -f1)"
    else
        echo "ERROR: Neither sha256sum nor shasum is available to verify the TLA+ tools download." >&2
        exit 1
    fi
    [ "$actual" = "$TLA_TOOLS_SHA256" ]
}

download_jar() {
    mkdir -p "$CACHE_DIR"
    local tmp_path="${JAR_PATH}.download.$$"
    echo "Downloading tla2tools.jar ${TLA_TOOLS_VERSION} ..." >&2
    if command -v curl >/dev/null 2>&1; then
        curl --fail --silent --show-error --location --max-time 300 \
            --output "$tmp_path" "$TLA_TOOLS_URL"
    elif command -v wget >/dev/null 2>&1; then
        wget --quiet --timeout=300 --output-document="$tmp_path" "$TLA_TOOLS_URL"
    else
        echo "ERROR: Neither curl nor wget is available to download tla2tools.jar." >&2
        echo "Fetch it manually from ${TLA_TOOLS_URL} into ${JAR_PATH}" >&2
        exit 1
    fi
    mv "$tmp_path" "$JAR_PATH"
}

if [ ! -f "$JAR_PATH" ]; then
    download_jar
fi

if ! verify_jar; then
    echo "WARNING: cached tla2tools.jar failed checksum verification; re-downloading." >&2
    rm -f "$JAR_PATH"
    download_jar
    if ! verify_jar; then
        echo "ERROR: tla2tools.jar ${TLA_TOOLS_VERSION} does not match the pinned SHA256." >&2
        echo "  expected: ${TLA_TOOLS_SHA256}" >&2
        echo "  url:      ${TLA_TOOLS_URL}" >&2
        echo "Refusing to execute an unverified jar." >&2
        exit 1
    fi
fi

# --- Resolve the configuration list ----------------------------------------

if [ ! -d "$TLA_DIR" ]; then
    echo "ERROR: ${TLA_DIR} does not exist." >&2
    exit 1
fi

CONFIGS=()
if [ -n "$SELECTED_CONFIG" ]; then
    base="${SELECTED_CONFIG%.cfg}"
    case "$base" in
        "$SPEC_NAME"_*) ;; # already fully qualified
        *) base="${SPEC_NAME}_${base}" ;;
    esac
    if [ ! -f "${TLA_DIR}/${base}.cfg" ]; then
        echo "ERROR: configuration '${SELECTED_CONFIG}' not found at ${TLA_DIR}/${base}.cfg" >&2
        echo "Available configurations:" >&2
        for cfg in "${TLA_DIR}/${SPEC_NAME}"_*.cfg; do
            [ -e "$cfg" ] || continue
            echo "  - $(basename "$cfg" .cfg)" >&2
        done
        exit 2
    fi
    CONFIGS+=("${base}.cfg")
else
    for cfg in "${TLA_DIR}/${SPEC_NAME}"_*.cfg; do
        [ -e "$cfg" ] || continue
        CONFIGS+=("$(basename "$cfg")")
    done
    if [ "${#CONFIGS[@]}" -eq 0 ]; then
        echo "ERROR: no ${SPEC_NAME}_*.cfg files found in ${TLA_DIR}" >&2
        exit 1
    fi
fi

# --- Run TLC ----------------------------------------------------------------

WORKERS="${TLC_WORKERS:-auto}"
overall_status=0

for cfg in "${CONFIGS[@]}"; do
    metadir="$(mktemp -d "${TMPDIR:-/tmp}/signal-fish-tlc.XXXXXX")"
    output_file="${metadir}/tlc-output.log"

    # -metadir keeps TLC's state files out of the repository; deadlock
    # checking is deliberately left ON (see the header comment).
    set +e
    (
        cd "$TLA_DIR" && \
        java -XX:+UseParallelGC -cp "$JAR_PATH" tlc2.TLC \
            -workers "$WORKERS" \
            -metadir "$metadir" \
            -config "$cfg" \
            "${SPEC_NAME}.tla"
    ) >"$output_file" 2>&1
    tlc_status=$?
    set -e

    if [ "$VERBOSE" = true ]; then
        cat "$output_file"
    fi

    summary="$(grep -E "states generated|depth of the complete" "$output_file" | tr '\n' ' ' || true)"
    if [ "$tlc_status" -eq 0 ] && grep -q "No error has been found" "$output_file"; then
        echo "OK   ${cfg}: ${summary}"
    else
        overall_status=1
        echo "FAIL ${cfg} (TLC exit ${tlc_status}). Full TLC output:" >&2
        cat "$output_file" >&2
    fi

    rm -rf "$metadir"
done

exit "$overall_status"
