#!/usr/bin/env bash
# Download one file with bounded retries and verify its pinned SHA-256 digest.

set -euo pipefail

if [[ $# -lt 3 || $# -gt 4 ]]; then
    echo "Usage: $0 URL SHA256 OUTPUT [https|http]" >&2
    exit 2
fi

readonly url="$1"
readonly expected_sha256="$2"
readonly output="$3"
readonly protocol="${4:-https}"

case "$protocol" in
    https | http) ;;
    *)
        echo "ERROR: Unsupported download protocol: $protocol" >&2
        exit 2
        ;;
esac

mkdir -p "$(dirname "$output")"
curl \
    --fail \
    --silent \
    --show-error \
    --location \
    --proto "=${protocol}" \
    --tlsv1.2 \
    --retry 5 \
    --retry-all-errors \
    --retry-delay 2 \
    --connect-timeout 20 \
    --max-time 60 \
    --output "$output" \
    "$url"

output_dir="$(dirname "$output")"
output_name="$(basename "$output")"
(
    cd "$output_dir"
    printf '%s  %s\n' "$expected_sha256" "$output_name" | sha256sum --check
)
