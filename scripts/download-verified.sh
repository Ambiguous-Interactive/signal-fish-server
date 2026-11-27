#!/usr/bin/env bash
# Download one file with bounded retries, verified-cache reuse, and a pinned digest.

set -euo pipefail

if [[ $# -lt 3 || $# -gt 5 ]]; then
    echo "Usage: $0 URL DIGEST OUTPUT [https|http] [sha256|sha512]" >&2
    exit 2
fi

readonly url="$1"
readonly expected_digest="$2"
readonly output="$3"
readonly protocol="${4:-https}"
readonly algorithm="${5:-sha256}"
readonly max_time="${DOWNLOAD_VERIFIED_MAX_TIME:-60}"

case "$protocol" in
    https | http) ;;
    *)
        echo "ERROR: Unsupported download protocol: $protocol" >&2
        exit 2
        ;;
esac

case "$algorithm" in
    sha256)
        digest_length=64
        if command -v sha256sum >/dev/null 2>&1; then
            checksum_command=(sha256sum)
        elif command -v shasum >/dev/null 2>&1; then
            checksum_command=(shasum -a 256)
        else
            echo "ERROR: No SHA-256 checksum command is available" >&2
            exit 2
        fi
        ;;
    sha512)
        digest_length=128
        if command -v sha512sum >/dev/null 2>&1; then
            checksum_command=(sha512sum)
        elif command -v shasum >/dev/null 2>&1; then
            checksum_command=(shasum -a 512)
        else
            echo "ERROR: No SHA-512 checksum command is available" >&2
            exit 2
        fi
        ;;
    *)
        echo "ERROR: Unsupported checksum algorithm: $algorithm" >&2
        exit 2
        ;;
esac

if [[ ${#expected_digest} -ne $digest_length || ! "$expected_digest" =~ ^[a-f0-9]+$ ]]; then
    echo "ERROR: Invalid $algorithm digest: $expected_digest" >&2
    exit 2
fi

if [[ ! "$max_time" =~ ^[0-9]+$ || ! "$max_time" =~ [1-9] ]]; then
    echo "ERROR: DOWNLOAD_VERIFIED_MAX_TIME must be a positive integer" >&2
    exit 2
fi

verify() {
    local candidate="$1"
    local candidate_dir
    local candidate_name
    candidate_dir="$(dirname "$candidate")"
    candidate_name="$(basename "$candidate")"
    (
        cd "$candidate_dir"
        printf '%s  %s\n' "$expected_digest" "$candidate_name" |
            "${checksum_command[@]}" -c - >/dev/null
    )
}

mkdir -p "$(dirname "$output")"

if [[ -f "$output" ]] && verify "$output"; then
    echo "Reusing verified download: $output" >&2
    exit 0
fi

# Never keep bytes that do not match the pin. A partial or corrupt cache entry
# must be replaced by a fresh verified download.
rm -f "$output"
tmp_output="${output}.download.$$"
trap 'rm -f "$tmp_output"' EXIT

curl_options=(
    --fail
    --silent
    --show-error
    --location
    --proto "=${protocol}"
    --tlsv1.2
    --retry 5
    --retry-all-errors
    --retry-delay 2
    --connect-timeout 20
    --max-time "$max_time"
)
if ! curl "${curl_options[@]}" --output "$tmp_output" "$url"; then
    if [[ "$protocol" != https || ! "$url" =~ ^https://github.com/([^/]+)/([^/]+)/releases/download/([^/]+)/([^/?#]+)$ ]] \
        || ! command -v python3 >/dev/null 2>&1; then
        exit 1
    fi
    release_api="https://api.github.com/repos/${BASH_REMATCH[1]}/${BASH_REMATCH[2]}/releases"
    release_tag="${BASH_REMATCH[3]}"
    asset_name="${BASH_REMATCH[4]}"
    echo "Retrying the pinned release through the GitHub API" >&2
    asset_api="$(
        curl "${curl_options[@]}" --header 'Accept: application/vnd.github+json' \
            "${release_api}/tags/${release_tag}" |
            python3 -c '
import json, sys, urllib.parse
name = urllib.parse.unquote(sys.argv[1])
assets = [asset for asset in json.load(sys.stdin)["assets"] if asset["name"] == name]
if len(assets) != 1 or type(assets[0]["id"]) is not int or assets[0]["id"] <= 0:
    raise ValueError("Expected one release asset with a positive integer ID: " + name)
print(sys.argv[2] + "/assets/" + str(assets[0]["id"]))
' "$asset_name" "$release_api"
    )"
    curl "${curl_options[@]}" --header 'Accept: application/octet-stream' \
        --output "$tmp_output" "$asset_api"
fi

if ! verify "$tmp_output"; then
    echo "ERROR: $algorithm verification FAILED for $url" >&2
    exit 1
fi

mv "$tmp_output" "$output"
trap - EXIT
