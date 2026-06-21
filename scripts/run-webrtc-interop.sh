#!/usr/bin/env bash
# run-webrtc-interop.sh - Build, lint, and run the native reference client's
# WebRTC interop suite.
#
# The reference client (clients/native/, package
# signal-fish-reference-native) is a standalone crate OUTSIDE the root
# package: its interop tests spawn the REAL signal-fish-server binary plus
# several real client processes, negotiating actual WebRTC sessions (DTLS +
# SCTP data channels over loopback UDP) brokered by the server's v3
# signaling. The harness locates the server binary via the
# SIGNAL_FISH_SERVER_BIN environment variable, which this script builds and
# exports. The interop server config disables TURN and configures zero STUN
# URLs, so the suite performs no external network access.
#
# Usage:
#   bash scripts/run-webrtc-interop.sh             # debug profile (default)
#   bash scripts/run-webrtc-interop.sh --release   # release profile

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CLIENT_DIR="${REPO_ROOT}/clients/native"

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
    sed -n '2,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
*)
    echo "ERROR: unknown argument '${1}' (supported: --release, --help)" >&2
    exit 1
    ;;
esac

echo "==> Verifying the native client lockfile is in sync (fast --locked precheck)"
# A stale clients/native/Cargo.lock makes every --locked build below fail late
# with a cryptic "lock file needs to be updated" error -- after a multi-minute
# cold compile. `cargo metadata --locked` reproduces that exact lock check up
# front via dependency resolution only (no compilation; it may fetch the index
# on a cold cache, but the build below would anyway), so drift fails fast with
# an actionable fix instead of burning a full build first.
if ! (cd "${CLIENT_DIR}" && cargo metadata --locked --format-version 1 >/dev/null); then
    echo "ERROR: clients/native/Cargo.lock is out of date with clients/native/Cargo.toml." >&2
    echo "       Regenerate and commit it before re-running:" >&2
    echo "         cargo update -p signal-fish-server --manifest-path clients/native/Cargo.toml  # after a root version bump" >&2
    echo "         cargo generate-lockfile --manifest-path clients/native/Cargo.toml             # for other dependency changes" >&2
    exit 1
fi

echo "==> Building the server binary (${PROFILE_DIR})"
(cd "${REPO_ROOT}" && cargo build --locked --bin signal-fish-server "${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}")

SERVER_BIN="${REPO_ROOT}/target/${PROFILE_DIR}/signal-fish-server"
if [ ! -f "${SERVER_BIN}" ]; then
    echo "ERROR: expected server binary at ${SERVER_BIN} after the build" >&2
    exit 1
fi

cd "${CLIENT_DIR}"

echo "==> Checking reference-client formatting"
cargo fmt --check

echo "==> Linting the reference client (clippy, zero warnings)"
cargo clippy --locked --all-targets -- -D warnings

echo "==> Running the reference-client test suite (unit + multi-process interop)"
SIGNAL_FISH_SERVER_BIN="${SERVER_BIN}" cargo test --locked "${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}"

echo "==> WebRTC interop suite passed"
