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
    --max-time 60
)
if ! curl "${curl_options[@]}" --output "$output" "$url"; then
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
        --output "$output" "$asset_api"
fi

output_dir="$(dirname "$output")"
output_name="$(basename "$output")"
(
    cd "$output_dir"
    printf '%s  %s\n' "$expected_sha256" "$output_name" | sha256sum -c
)
