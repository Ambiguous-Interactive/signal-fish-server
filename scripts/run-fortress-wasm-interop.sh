#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
readonly FIXTURE_ROOT="${REPO_ROOT}/clients/fortress-wasm"
readonly MANIFEST_PATH="${FIXTURE_ROOT}/Cargo.toml"
readonly GODOT_PROJECT="${FIXTURE_ROOT}/project"
readonly GODOT_BIN_DIR="${GODOT_PROJECT}/bin"
readonly EXPORT_DIR="${GODOT_PROJECT}/build"
readonly ARTIFACT_DIR="${FIXTURE_ROOT}/artifacts"
readonly NIGHTLY="nightly-2026-03-01"
readonly TARGET="wasm32-unknown-emscripten"
readonly EMSDK_VERSION="3.1.74"
readonly GODOT_VERSION="4.5"
readonly LIBRARY_NAME="signal_fish_fortress_wasm_interop"
readonly TARGET_DIR="${REPO_ROOT}/target/fortress-wasm"
readonly COMMON_RUSTFLAGS="-Z unstable-options -C panic=immediate-abort -C link-arg=-sSIDE_MODULE=2 -C llvm-args=-enable-emscripten-cxx-exceptions=0 -Z default-visibility=hidden -Z link-native-libraries=no"

if ! command -v emcc >/dev/null 2>&1; then
    if [[ -n "${EMSDK:-}" && -f "${EMSDK}/emsdk_env.sh" ]]; then
        # shellcheck disable=SC1091
        source "${EMSDK}/emsdk_env.sh" >/dev/null
    else
        printf 'BUSTED: emcc is unavailable and EMSDK is not configured\n' >&2
        exit 1
    fi
fi
if ! emcc --version | head -n 1 | grep -F "${EMSDK_VERSION}" >/dev/null; then
    printf 'BUSTED: Emscripten %s is required\n' "${EMSDK_VERSION}" >&2
    emcc --version >&2
    exit 1
fi

readonly GODOT_BIN="${GODOT4_BIN:-godot4}"
if [[ ! -x "${GODOT_BIN}" ]] && ! command -v "${GODOT_BIN}" >/dev/null 2>&1; then
    printf 'BUSTED: Godot executable not found: %s\n' "${GODOT_BIN}" >&2
    exit 1
fi
if ! "${GODOT_BIN}" --version | grep -Eq "^${GODOT_VERSION}\.stable"; then
    printf 'BUSTED: Godot %s stable is required\n' "${GODOT_VERSION}" >&2
    "${GODOT_BIN}" --version >&2
    exit 1
fi
if ! rustup toolchain list | grep -q "^${NIGHTLY}"; then
    printf 'BUSTED: Rust %s with rust-src must be installed\n' "${NIGHTLY}" >&2
    exit 1
fi

mkdir -p "${GODOT_BIN_DIR}" "${EXPORT_DIR}" "${ARTIFACT_DIR}"

feature_tree="${ARTIFACT_DIR}/cargo-feature-tree.txt"
cargo +"${NIGHTLY}" tree --manifest-path "${MANIFEST_PATH}" --locked -e features >"${feature_tree}"
grep -F 'signal-fish-client v0.8.0' "${feature_tree}" >/dev/null
grep -F 'fortress-rollback v0.10.0' "${feature_tree}" >/dev/null
grep -F 'godot v0.4.5' "${feature_tree}" >/dev/null
if grep -Eq 'transport-websocket-emscripten|sync-send' "${feature_tree}"; then
    printf 'BUSTED: forbidden raw-WebSocket or Send+Sync feature in released graph\n' >&2
    exit 1
fi

export BINDGEN_EXTRA_CLANG_ARGS_wasm32_unknown_emscripten="--target=wasm32-unknown-emscripten --sysroot=${EMSDK}/upstream/emscripten/cache/sysroot -D__EMSCRIPTEN__"
unset CARGO_ENCODED_RUSTFLAGS
export RUSTFLAGS="${COMMON_RUSTFLAGS}"
cargo +"${NIGHTLY}" build \
    --manifest-path "${MANIFEST_PATH}" \
    --locked \
    --target-dir "${TARGET_DIR}" \
    -Zbuild-std=std \
    --target "${TARGET}" \
    --release
cp "${TARGET_DIR}/${TARGET}/release/${LIBRARY_NAME}.wasm" \
    "${GODOT_BIN_DIR}/${LIBRARY_NAME}.wasm"

"${GODOT_BIN}" --headless --path "${GODOT_PROJECT}" --import
"${GODOT_BIN}" --headless --path "${GODOT_PROJECT}" \
    --export-release Web build/index.html

unset RUSTFLAGS BINDGEN_EXTRA_CLANG_ARGS_wasm32_unknown_emscripten
cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" --locked --bin signal-fish-server
SERVER_BIN="$(cd "${REPO_ROOT}/target/debug" && pwd)/signal-fish-server"
readonly SERVER_BIN
BUILD_SHA="$(git -C "${REPO_ROOT}" rev-parse HEAD)"
readonly BUILD_SHA

npm --prefix "${REPO_ROOT}/clients/browser" ci
if [[ "${INSTALL_PLAYWRIGHT_DEPS:-0}" == "1" ]]; then
    node "${REPO_ROOT}/clients/browser/node_modules/playwright-core/cli.js" install --with-deps chromium
else
    node "${REPO_ROOT}/clients/browser/node_modules/playwright-core/cli.js" install chromium
fi

timeout --foreground 110s node "${FIXTURE_ROOT}/harness.mjs" \
    released "${EXPORT_DIR}" "${SERVER_BIN}" "${ARTIFACT_DIR}" "${BUILD_SHA}"
timeout --foreground 80s node "${FIXTURE_ROOT}/harness.mjs" \
    negative "${EXPORT_DIR}" "${SERVER_BIN}" "${ARTIFACT_DIR}" "${BUILD_SHA}"

printf 'BUSTED (expected): released Signal Fish client 0.8.0 does not satisfy the Godot no-thread WASM healthy gates\n'
