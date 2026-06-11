#!/usr/bin/env bash
# run-browser-interop.sh - Build, lint, and run the browser reference
# client's browser<->native interop suite.
#
# The browser reference client (clients/browser/, npm package
# signal-fish-reference-browser) drives a REAL Chromium RTCPeerConnection
# (headless shell via playwright-core). The feature-gated cells in
# clients/native/tests/browser_interop_e2e.rs spawn the real
# signal-fish-server binary plus native client processes plus browser client
# processes (node dist/cli.js), negotiating actual webrtc-rs<->Chromium
# sessions over loopback. The harness locates the binaries via
# SIGNAL_FISH_SERVER_BIN and SIGNAL_FISH_BROWSER_CLI, which this script
# builds and exports. The interop server config disables TURN with zero STUN
# URLs, so the suite itself performs no external network access (the one
# network fetch is the Chromium download at install time, cached thereafter).
#
# Usage:
#   bash scripts/run-browser-interop.sh             # debug profile (default)
#   bash scripts/run-browser-interop.sh --release   # release server/native profile

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
NATIVE_DIR="${REPO_ROOT}/clients/native"
BROWSER_DIR="${REPO_ROOT}/clients/browser"

PROFILE_DIR="debug"
CARGO_PROFILE_ARGS=()
case "${1:-}" in
"")
    ;;
--release)
    PROFILE_DIR="release"
    CARGO_PROFILE_ARGS=(--release)
    ;;
--help | -h)
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
*)
    echo "ERROR: unknown argument '${1}' (supported: --release, --help)" >&2
    exit 1
    ;;
esac

echo "==> Building the server binary (${PROFILE_DIR})"
(cd "${REPO_ROOT}" && cargo build --locked --bin signal-fish-server "${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}")

SERVER_BIN="${REPO_ROOT}/target/${PROFILE_DIR}/signal-fish-server"
if [ ! -f "${SERVER_BIN}" ]; then
    echo "ERROR: expected server binary at ${SERVER_BIN} after the build" >&2
    exit 1
fi

echo "==> Building the native reference client (${PROFILE_DIR})"
(cd "${NATIVE_DIR}" && cargo build --locked "${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}")

echo "==> Checking reference-client formatting (incl. the browser cells)"
(cd "${NATIVE_DIR}" && cargo fmt --check)

echo "==> Linting the browser cells (clippy --features browser-interop, zero warnings)"
# The feature-gated browser_interop_e2e target is OUTSIDE the default native
# lint surface (scripts/run-webrtc-interop.sh); this is the gate that lints it.
(cd "${NATIVE_DIR}" && cargo clippy --locked --all-targets --features browser-interop -- -D warnings)

echo "==> Installing browser-client dependencies (npm ci)"
(cd "${BROWSER_DIR}" && npm ci)

echo "==> Checking browser-client stdout purity (no console.log outside the event writer)"
if grep -rn "console\.log" "${BROWSER_DIR}/src"; then
    echo "ERROR: console.log found in clients/browser/src — stdout is JSONL-only; log to stderr (console.error)" >&2
    exit 1
fi

echo "==> Typechecking the browser client (tsc, strict)"
(cd "${BROWSER_DIR}" && npm run typecheck)

echo "==> Checking browser-client formatting (prettier)"
(cd "${BROWSER_DIR}" && npm run format:check)

echo "==> Building the browser client bundles (esbuild)"
(cd "${BROWSER_DIR}" && npm run build)

BROWSER_CLI="${BROWSER_DIR}/dist/cli.js"
if [ ! -f "${BROWSER_CLI}" ]; then
    echo "ERROR: expected browser CLI bundle at ${BROWSER_CLI} after the build" >&2
    exit 1
fi

echo "==> Installing the Chromium headless shell (playwright-core, lockfile-pinned)"
# Locked binary from node_modules, NOT bare npx: the workflow hygiene policy
# forbids unpinned npx execution. No-op when the browser is already cached.
"${BROWSER_DIR}/node_modules/.bin/playwright-core" install chromium-headless-shell

echo "==> Running the browser interop cells (feature browser-interop)"
(cd "${NATIVE_DIR}" && \
    SIGNAL_FISH_SERVER_BIN="${SERVER_BIN}" \
    SIGNAL_FISH_BROWSER_CLI="${BROWSER_CLI}" \
    cargo test --locked --features browser-interop --test browser_interop_e2e "${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}")

echo "==> Browser interop suite passed"
