#!/usr/bin/env bash
# Install the pinned Lychee release with transport retries and checksum
# verification. lychee-action downloads its override with one `curl -sfLO`, so
# a transient GitHub Releases reset can fail CI before link validation starts.

set -euo pipefail

readonly LYCHEE_VERSION="0.24.2"
readonly LYCHEE_X86_64_MUSL_SHA256="73657a111819a30c47c08352896796f23d64e4eb2b3ed39b6d32149241566fc5"
readonly LYCHEE_AARCH64_MUSL_SHA256="5d0b0e3aeab240f41920c633a6eaf97599be6eedda034b36e858ede7dba5e535"

case "$(uname -m)" in
    x86_64)
        target="x86_64-unknown-linux-musl"
        expected_sha256="$LYCHEE_X86_64_MUSL_SHA256"
        ;;
    aarch64 | arm64)
        target="aarch64-unknown-linux-musl"
        expected_sha256="$LYCHEE_AARCH64_MUSL_SHA256"
        ;;
    *)
        echo "ERROR: Unsupported Lychee installer architecture: $(uname -m)" >&2
        exit 1
        ;;
esac

install_dir="${SIGNAL_FISH_LYCHEE_INSTALL_DIR:-${RUNNER_TEMP:?RUNNER_TEMP must be set}/lychee/bin}"
scratch_dir="$(mktemp -d)"
trap 'rm -rf "$scratch_dir"' EXIT

archive="lychee-${target}.tar.gz"
release_dir="lychee-${target}"
url="https://github.com/lycheeverse/lychee/releases/download/lychee-v${LYCHEE_VERSION}/${archive}"
bash "$(dirname "$0")/download-verified.sh" \
    "$url" \
    "$expected_sha256" \
    "${scratch_dir}/${archive}"
(
    cd "$scratch_dir"
    tar -xzf "$archive" "${release_dir}/lychee"
)

mkdir -p "$install_dir"
install -m 0755 "${scratch_dir}/${release_dir}/lychee" "${install_dir}/lychee"
if [[ -n "${GITHUB_PATH:-}" ]]; then
    printf '%s\n' "$install_dir" >> "$GITHUB_PATH"
else
    printf 'Installed lychee in %s\n' "$install_dir"
fi
