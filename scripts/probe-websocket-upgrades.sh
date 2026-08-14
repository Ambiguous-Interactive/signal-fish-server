#!/usr/bin/env bash

# Verify that repeated pairs of simultaneous clients complete a WebSocket HTTP
# upgrade through the exact URL supplied by an operator. The probe stops after
# HTTP 101; it intentionally does not authenticate or join a room.

set -euo pipefail

usage() {
    echo "Usage: $0 <ws:// or wss:// URL> [burst-count]" >&2
}

probe_url=${1:-}
burst_count=${2:-20}
curl_bin=${SIGNAL_FISH_CURL_BIN:-curl}
connect_timeout_seconds=5
request_timeout_seconds=10

if [[ ! ${probe_url} =~ ^wss?://[^[:space:]]+$ ]]; then
    usage
    echo "ERROR: probe URL must be one absolute ws:// or wss:// URL" >&2
    exit 2
fi
curl_url=${probe_url/#ws:/http:}
curl_url=${curl_url/#wss:/https:}
if [[ ! ${burst_count} =~ ^[1-9][0-9]*$ ]] || ((burst_count > 100)); then
    usage
    echo "ERROR: burst-count must be an integer from 1 through 100" >&2
    exit 2
fi
if ! command -v "${curl_bin}" >/dev/null 2>&1; then
    echo "ERROR: curl executable not found: ${curl_bin}" >&2
    exit 2
fi
if ! command -v openssl >/dev/null 2>&1; then
    echo "ERROR: openssl is required to generate and verify RFC 6455 handshake values" >&2
    exit 2
fi

probe_dir=$(mktemp -d)
cleanup() {
    rm -rf -- "${probe_dir}"
}
trap cleanup EXIT

header_value() {
    local header_file=$1
    local header_name=$2
    awk -F ':[[:space:]]*' -v expected="${header_name}" '
        tolower($1) == expected {
            value = substr($0, index($0, ":") + 1)
            sub(/^[[:space:]]+/, "", value)
            sub(/\r$/, "", value)
            print value
            exit
        }
    ' "${header_file}"
}

header_count() {
    local header_file=$1
    local header_name=$2
    awk -F ':' -v expected="${header_name}" '
        tolower($1) == expected { count++ }
        END { print count + 0 }
    ' "${header_file}"
}

header_values() {
    local header_file=$1
    local header_name=$2
    awk -F ':[[:space:]]*' -v expected="${header_name}" '
        tolower($1) == expected {
            value = substr($0, index($0, ":") + 1)
            sub(/^[[:space:]]+/, "", value)
            sub(/\r$/, "", value)
            values = values separator value
            separator = ","
        }
        END { print values }
    ' "${header_file}"
}

extract_final_response_headers() {
    local source_file=$1
    local destination_file=$2
    awk '
        /^HTTP\/[0-9][0-9.]*[[:space:]]+[0-9][0-9][0-9]([[:space:]]|$)/ {
            final_block = ""
            in_headers = 1
        }
        in_headers {
            final_block = final_block $0 ORS
            if ($0 ~ /^\r?$/) {
                in_headers = 0
            }
        }
        END {
            printf "%s", final_block
        }
    ' "${source_file}" >"${destination_file}"
}

lowercase() {
    printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

print_response_evidence() {
    local prefix=$1
    if [[ -s ${prefix}.final-headers ]]; then
        echo "allowlisted response evidence:" >&2
        awk '
            /^HTTP\/[0-9][0-9.]*[[:space:]]+[0-9][0-9][0-9]([[:space:]]|$)/ {
                sub(/\r$/, "")
                print
                next
            }
            index($0, ":") > 0 {
                name = substr($0, 1, index($0, ":") - 1)
                normalized = tolower(name)
                value = substr($0, index($0, ":") + 1)
                sub(/^[[:space:]]+/, "", value)
                sub(/\r$/, "", value)
                if (normalized == "upgrade") {
                    print "Upgrade: " value
                } else if (normalized == "connection") {
                    print "Connection: " value
                } else if (normalized == "sec-websocket-accept") {
                    print "Sec-WebSocket-Accept: " value
                } else if (normalized == "x-signal-fish-request-id") {
                    print "X-Signal-Fish-Request-Id: " value
                } else if (normalized == "x-signal-fish-upgrade-outcome") {
                    print "X-Signal-Fish-Upgrade-Outcome: " value
                }
            }
        ' "${prefix}.final-headers" >&2
    else
        echo "allowlisted response evidence: none" >&2
    fi
    if [[ -s ${prefix}.stderr ]]; then
        echo "curl stderr: suppressed (see curl_exit for transport status)" >&2
    else
        echo "curl stderr: none" >&2
    fi
}

probe_peer() {
    local burst=$1
    local peer=$2
    local prefix="${probe_dir}/burst-${burst}-peer-${peer}"
    local curl_exit=0
    local websocket_key
    websocket_key=$(openssl rand -base64 16)
    local expected_accept
    expected_accept=$(
        printf '%s%s' "${websocket_key}" '258EAFA5-E914-47DA-95CA-C5AB0DC85B11' \
            | openssl dgst -sha1 -binary \
            | openssl base64 -A
    )
    local probe_attempt_id
    probe_attempt_id=$(openssl rand -hex 16)
    printf '%s\n' "${probe_attempt_id}" >"${prefix}.probe-attempt-id"

    # curl may fail before opening its dump-header target (for example, an
    # executable or TLS setup failure). Keep all diagnostic reads defined so
    # the probe still reports the exact burst, peer, status, and curl exit code.
    : >"${prefix}.headers"
    : >"${prefix}.final-headers"
    : >"${prefix}.status"
    : >"${prefix}.stderr"

    set +e
    # curl reads its default configuration files before ordinary command-line
    # options. --disable must therefore be argument one: proxy, redirect, trace,
    # header, or write-out options from those files must not alter this probe.
    "${curl_bin}" --disable \
        --http1.1 \
        --silent \
        --show-error \
        --no-buffer \
        --connect-timeout "${connect_timeout_seconds}" \
        --max-time "${request_timeout_seconds}" \
        --output /dev/null \
        --dump-header "${prefix}.headers" \
        --write-out '%{http_code}' \
        --header 'Connection: Upgrade' \
        --header 'Upgrade: websocket' \
        --header 'Sec-WebSocket-Version: 13' \
        --header "Sec-WebSocket-Key: ${websocket_key}" \
        --header "X-Signal-Fish-Probe-Attempt-Id: ${probe_attempt_id}" \
        "${curl_url}" >"${prefix}.status" 2>"${prefix}.stderr"
    curl_exit=$?
    set -e

    # A proxy CONNECT or interim response can precede the origin response in
    # curl's dump-header file. Only the final response block is authoritative
    # for the WebSocket handshake and application correlation fields.
    extract_final_response_headers "${prefix}.headers" "${prefix}.final-headers"

    local http_status
    http_status=$(tr -d '[:space:]' <"${prefix}.status")
    local request_id
    request_id=$(header_value "${prefix}.final-headers" 'x-signal-fish-request-id')
    local outcome
    outcome=$(header_value "${prefix}.final-headers" 'x-signal-fish-upgrade-outcome')
    local upgrade_header
    upgrade_header=$(header_value "${prefix}.final-headers" 'upgrade')
    local connection_header
    connection_header=$(header_values "${prefix}.final-headers" 'connection')
    local websocket_accept
    websocket_accept=$(header_value "${prefix}.final-headers" 'sec-websocket-accept')
    local upgrade_header_count
    upgrade_header_count=$(header_count "${prefix}.final-headers" 'upgrade')
    local websocket_accept_count
    websocket_accept_count=$(header_count "${prefix}.final-headers" 'sec-websocket-accept')
    local request_id_count
    request_id_count=$(header_count "${prefix}.final-headers" 'x-signal-fish-request-id')
    local outcome_count
    outcome_count=$(header_count "${prefix}.final-headers" 'x-signal-fish-upgrade-outcome')
    local upgrade_header_lower
    upgrade_header_lower=$(lowercase "${upgrade_header}")
    local connection_header_lower
    connection_header_lower=$(lowercase "${connection_header}")

    if [[ ${http_status} != 101 ]]; then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} expected HTTP 101, got ${http_status:-none} (curl_exit=${curl_exit})" >&2
        print_response_evidence "${prefix}"
        return 1
    fi
    if ((upgrade_header_count > 1)); then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} HTTP 101 had duplicate singleton response header Upgrade (count=${upgrade_header_count})" >&2
        print_response_evidence "${prefix}"
        return 1
    fi
    if ((websocket_accept_count > 1)); then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} HTTP 101 had duplicate singleton response header Sec-WebSocket-Accept (count=${websocket_accept_count})" >&2
        print_response_evidence "${prefix}"
        return 1
    fi
    if ((request_id_count > 1)); then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} HTTP 101 had duplicate singleton response header x-signal-fish-request-id (count=${request_id_count})" >&2
        print_response_evidence "${prefix}"
        return 1
    fi
    if ((outcome_count > 1)); then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} HTTP 101 had duplicate singleton response header x-signal-fish-upgrade-outcome (count=${outcome_count})" >&2
        print_response_evidence "${prefix}"
        return 1
    fi
    if [[ ${upgrade_header_lower} != websocket ]]; then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} HTTP 101 lacked 'Upgrade: websocket' (got ${upgrade_header:-none})" >&2
        print_response_evidence "${prefix}"
        return 1
    fi
    if [[ ! ${connection_header_lower} =~ (^|[[:space:],])upgrade([[:space:],]|$) ]]; then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} HTTP 101 Connection header lacked the upgrade token (got ${connection_header:-none})" >&2
        print_response_evidence "${prefix}"
        return 1
    fi
    if [[ ${websocket_accept} != "${expected_accept}" ]]; then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} HTTP 101 had invalid Sec-WebSocket-Accept (got ${websocket_accept:-none}, expected ${expected_accept})" >&2
        print_response_evidence "${prefix}"
        return 1
    fi
    if [[ ! ${request_id} =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]]; then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} HTTP 101 lacked a valid x-signal-fish-request-id (got ${request_id:-none})" >&2
        print_response_evidence "${prefix}"
        return 1
    fi
    if [[ ${outcome} != accepted ]]; then
        echo "ERROR: burst=${burst} peer=${peer} probe_attempt_id=${probe_attempt_id} HTTP 101 had upgrade outcome ${outcome:-none}, expected accepted" >&2
        print_response_evidence "${prefix}"
        return 1
    fi

    printf '%s\n' "${request_id}" >"${prefix}.request-id"
    printf 'burst=%s peer=%s probe_attempt_id=%s http_status=%s request_id=%s outcome=%s curl_exit=%s\n' \
        "${burst}" "${peer}" "${probe_attempt_id}" "${http_status}" "${request_id}" "${outcome}" "${curl_exit}"
}

: >"${probe_dir}/seen-request-ids"
: >"${probe_dir}/seen-probe-attempt-ids"
burst=1
while ((burst <= burst_count)); do
    SIGNAL_FISH_PROBE_BURST=${burst} SIGNAL_FISH_PROBE_PEER=1 probe_peer "${burst}" 1 &
    left_pid=$!
    SIGNAL_FISH_PROBE_BURST=${burst} SIGNAL_FISH_PROBE_PEER=2 probe_peer "${burst}" 2 &
    right_pid=$!

    left_status=0
    right_status=0
    wait "${left_pid}" || left_status=$?
    wait "${right_pid}" || right_status=$?
    if ((left_status != 0 || right_status != 0)); then
        echo "ERROR: simultaneous WebSocket upgrade burst ${burst}/${burst_count} failed" >&2
        exit 1
    fi

    for peer in 1 2; do
        probe_attempt_id=$(<"${probe_dir}/burst-${burst}-peer-${peer}.probe-attempt-id")
        if grep -Fqx -- "${probe_attempt_id}" "${probe_dir}/seen-probe-attempt-ids"; then
            echo "ERROR: burst=${burst} peer=${peer} reused probe attempt ID ${probe_attempt_id}" >&2
            print_response_evidence "${probe_dir}/burst-${burst}-peer-${peer}"
            exit 1
        fi
        printf '%s\n' "${probe_attempt_id}" >>"${probe_dir}/seen-probe-attempt-ids"

        request_id=$(<"${probe_dir}/burst-${burst}-peer-${peer}.request-id")
        if grep -Fqx -- "${request_id}" "${probe_dir}/seen-request-ids"; then
            echo "ERROR: burst=${burst} peer=${peer} reused request ID ${request_id}" >&2
            print_response_evidence "${probe_dir}/burst-${burst}-peer-${peer}"
            exit 1
        fi
        printf '%s\n' "${request_id}" >>"${probe_dir}/seen-request-ids"
    done
    burst=$((burst + 1))
done

echo "PASS: ${burst_count} simultaneous two-client WebSocket upgrade bursts completed (${burst_count} x 2 accepted requests)"
