#!/usr/bin/env bash
# run-fortress-interop.sh - Build Signal Fish Server and run the pinned
# Fortress Rollback issue-242 interoperability fixture.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CLIENT_DIR="${REPO_ROOT}/clients/fortress"

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
    sed -n '2,4p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
*)
    echo "ERROR: unknown argument '${1}' (supported: --release, --help)" >&2
    exit 1
    ;;
esac

echo "==> Verifying the Fortress fixture lockfile"
if ! (cd "${CLIENT_DIR}" && cargo metadata --locked --format-version 1 >/dev/null); then
    echo "ERROR: clients/fortress/Cargo.lock is out of date." >&2
    echo "       Run: cargo generate-lockfile --manifest-path clients/fortress/Cargo.toml" >&2
    exit 1
fi

echo "==> Building Signal Fish Server (${PROFILE_DIR})"
(cd "${REPO_ROOT}" && cargo build --locked --bin signal-fish-server "${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}")

SERVER_BIN="${REPO_ROOT}/target/${PROFILE_DIR}/signal-fish-server"
if [ ! -f "${SERVER_BIN}" ]; then
    echo "ERROR: expected server binary at ${SERVER_BIN}" >&2
    exit 1
fi

cd "${CLIENT_DIR}"

echo "==> Checking Fortress fixture formatting"
cargo fmt --check

echo "==> Linting the Fortress fixture"
cargo clippy --locked --all-targets --all-features -- -D warnings

echo "==> Running the Fortress multiprocess interoperability test"
SIGNAL_FISH_SERVER_BIN="${SERVER_BIN}" cargo test --locked --all-features "${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}" -- --nocapture

echo "==> Fortress interoperability fixture passed"
