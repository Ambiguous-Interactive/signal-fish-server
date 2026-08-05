#!/usr/bin/env bash
# run-turn-interop.sh - Offline execution phase for the pinned local coturn
# proof. Supported on Linux hosts/containers with Docker and GNU coreutils.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CLIENT_DIR="${REPO_ROOT}/clients/native"
ARTIFACT_DIR="${SF_TURN_INTEROP_ARTIFACT_DIR:-${REPO_ROOT}/target/turn-interop}"
COTURN_IMAGE="coturn/coturn:4.12.0-alpine@sha256:faca4aa57efc436916c31546f3867bd1a3fb1077723291bcfba0bf814bcaf48a"
COTURN_SECRET="signal-fish-local-turn-interop-secret"
COTURN_BAD_SECRET="${COTURN_SECRET}-mismatch"
COTURN_CONTAINER="signal-fish-turn-interop-$$"
COTURN_NETWORK="signal-fish-turn-interop-$$"
LISTEN_PORT="${SF_TURN_INTEROP_LISTEN_PORT:-3478}"
RELAY_MIN_PORT="${SF_TURN_INTEROP_RELAY_MIN_PORT:-49160}"
RELAY_MAX_PORT="${SF_TURN_INTEROP_RELAY_MAX_PORT:-49169}"
LOCK_DIR="${TMPDIR:-/tmp}/signal-fish-turn-${LISTEN_PORT}-${RELAY_MIN_PORT}-${RELAY_MAX_PORT}.lock"

is_ipv4_address() {
    local address=$1
    local octets=()
    local octet
    [[ "${address}" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] || return 1
    IFS=. read -r -a octets <<<"${address}"
    for octet in "${octets[@]}"; do
        ((10#${octet} <= 255)) || return 1
    done
}

for value in "${LISTEN_PORT}" "${RELAY_MIN_PORT}" "${RELAY_MAX_PORT}"; do
    if ! [[ "${value}" =~ ^[0-9]+$ ]]; then
        echo "ERROR: TURN ports must be integers from 1024 through 65535." >&2
        exit 1
    fi
    if ((10#${value} < 1024 || 10#${value} > 65535)); then
        echo "ERROR: TURN ports must be integers from 1024 through 65535." >&2
        exit 1
    fi
done
LISTEN_PORT="$((10#${LISTEN_PORT}))"
RELAY_MIN_PORT="$((10#${RELAY_MIN_PORT}))"
RELAY_MAX_PORT="$((10#${RELAY_MAX_PORT}))"
if ((RELAY_MIN_PORT > RELAY_MAX_PORT)); then
    echo "ERROR: SF_TURN_INTEROP_RELAY_MIN_PORT must not exceed the max port." >&2
    exit 1
fi
RUNNING_IN_CONTAINER=false
SELF_CONTAINER=""
NETWORK_SUBNET=""
USE_PRIVATE_NETWORK_ADDRESS=false
if [ -n "${SF_TURN_INTEROP_HOST:-}" ]; then
    TURN_HOST="${SF_TURN_INTEROP_HOST}"
else
    USE_PRIVATE_NETWORK_ADDRESS=true
    NETWORK_OCTET="$((20 + $$ % 200))"
    NETWORK_SUBNET="10.254.${NETWORK_OCTET}.0/24"
    TURN_HOST="10.254.${NETWORK_OCTET}.2"
    if [ -f /.dockerenv ]; then
        RUNNING_IN_CONTAINER=true
        SELF_CONTAINER="$(hostname)"
    fi
fi
if ! is_ipv4_address "${TURN_HOST}"; then
    echo "ERROR: SF_TURN_INTEROP_HOST must resolve to an IPv4 address." >&2
    exit 1
fi
TURN_URL="turn:${TURN_HOST}:${LISTEN_PORT}?transport=udp"
BIND_HOST="${SF_TURN_INTEROP_BIND_HOST:-127.0.0.1}"
if ! is_ipv4_address "${BIND_HOST}"; then
    echo "ERROR: SF_TURN_INTEROP_BIND_HOST must be an IPv4 address." >&2
    exit 1
fi
RAW_DIR="$(mktemp -d)"
RAW_COTURN_LOG="${RAW_DIR}/coturn.log"
RAW_TEST_LOG="${RAW_DIR}/test.log"
COTURN_RUNNER_PID=""
LOCK_ACQUIRED=false

sanitize_file() {
    local source=$1
    local destination=$2
    if [ ! -f "${source}" ]; then
        : >"${destination}"
        return
    fi
    sed -E \
        -e "s/${COTURN_BAD_SECRET}/[REDACTED]/g" \
        -e "s/${COTURN_SECRET}/[REDACTED]/g" \
        -e 's/(credentials of user )<[^>]+>/\1<[REDACTED]>/g' \
        -e 's/("(username|credential)"[[:space:]]*:[[:space:]]*")[^"]*/\1[REDACTED]/g' \
        -e 's/((username|credential)=)[^ ,}]*/\1[REDACTED]/gI' \
        "${source}" >"${destination}"
}

has_unredacted_credentials() {
    grep -ERoh \
        'credentials of user <[^>]+>|"(username|credential)"[[:space:]]*:[[:space:]]*"[^\"]+"|((username|credential)=)[^ ,}]+' \
        "$@" 2>/dev/null | grep -Fv '[REDACTED]' >/dev/null
}

# Append the host's view of the TURN network to the run's diagnostics.
#
# Issue #276's failures are all "no bound socket reached coturn", and the
# client's own log can only report which addresses it bound. Whether the
# expected source address existed, which device carried it, and whether that
# device was carrier-up are properties of the host that no client log can
# recover after the fact. The bridge's `operstate` is the exact quantity
# `if_addrs::Interface::is_oper_up` reads, so an enumeration that missed the
# route is visible here rather than inferred.
capture_host_routing() {
    local label=$1
    local network_id bridge
    {
        printf '\n===== %s =====\n' "${label}"
        printf 'turn_host=%s listen_port=%s subnet=%s network=%s\n' \
            "${TURN_HOST}" "${LISTEN_PORT}" "${NETWORK_SUBNET:-<published>}" "${COTURN_NETWORK}"
        if command -v ip >/dev/null 2>&1; then
            printf -- '--- ip -4 route get %s ---\n' "${TURN_HOST}"
            timeout 10 ip -4 route get "${TURN_HOST}" 2>&1
            printf -- '--- ip -4 addr ---\n'
            timeout 10 ip -4 addr 2>&1
            printf -- '--- ip -4 route ---\n'
            timeout 10 ip -4 route 2>&1
        else
            printf 'iproute2 is unavailable on this host\n'
        fi
        network_id=$(timeout 10 docker network inspect -f '{{.Id}}' "${COTURN_NETWORK}" 2>&1)
        printf -- '--- docker network id ---\n%s\n' "${network_id}"
        bridge="br-${network_id:0:12}"
        if command -v ip >/dev/null 2>&1 && [ ${#network_id} -ge 12 ]; then
            printf -- '--- ip -d link show dev %s ---\n' "${bridge}"
            timeout 10 ip -d link show dev "${bridge}" 2>&1
            printf -- '--- ip -4 addr show dev %s ---\n' "${bridge}"
            timeout 10 ip -4 addr show dev "${bridge}" 2>&1
        fi
    } >>"${ARTIFACT_DIR}/host-routing.log" 2>&1 || true
}

cleanup() {
    local status=$?
    trap - EXIT INT TERM
    set +e
    # Before the teardown removes the container and the bridge: the state at
    # failure time is the state that matters.
    capture_host_routing "teardown"
    timeout 10 docker rm --force "${COTURN_CONTAINER}" >/dev/null 2>&1 || true
    if [ -n "${COTURN_RUNNER_PID}" ]; then
        wait "${COTURN_RUNNER_PID}" 2>/dev/null || true
    fi
    if [ -n "${SELF_CONTAINER}" ]; then
        timeout 10 docker network disconnect "${COTURN_NETWORK}" "${SELF_CONTAINER}" \
            >/dev/null 2>&1 || true
    fi
    timeout 10 docker network rm "${COTURN_NETWORK}" >/dev/null 2>&1 || true
    if ! sanitize_file "${RAW_COTURN_LOG}" "${ARTIFACT_DIR}/coturn.log"; then
        status=1
    fi
    if ! sanitize_file "${RAW_TEST_LOG}" "${ARTIFACT_DIR}/test.log"; then
        status=1
    fi
    for diagnostic in "${ARTIFACT_DIR}"/server-*.log \
        "${ARTIFACT_DIR}"/client-*.log \
        "${ARTIFACT_DIR}"/client-*.jsonl; do
        [ -f "${diagnostic}" ] || continue
        if sanitize_file "${diagnostic}" "${RAW_DIR}/sanitized"; then
            mv "${RAW_DIR}/sanitized" "${diagnostic}" || status=1
        else
            status=1
        fi
    done
    if [ "${LOCK_ACQUIRED}" = true ]; then
        rmdir "${LOCK_DIR}" 2>/dev/null || true
    fi
    rm -rf "${RAW_DIR}"
    if grep -RF "${COTURN_SECRET}" "${ARTIFACT_DIR}" >/dev/null 2>&1 \
        || grep -RF "${COTURN_BAD_SECRET}" "${ARTIFACT_DIR}" >/dev/null 2>&1 \
        || has_unredacted_credentials "${ARTIFACT_DIR}"; then
        echo "ERROR: TURN diagnostics retained an unredacted shared secret." >&2
        status=1
    fi
    exit "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if ! mkdir "${LOCK_DIR}" 2>/dev/null; then
    trap - EXIT INT TERM
    rm -rf "${RAW_DIR}"
    echo "ERROR: another TURN interop run owns ${LOCK_DIR}; use validated port overrides for a parallel run." >&2
    exit 1
fi
LOCK_ACQUIRED=true
mkdir -p "${ARTIFACT_DIR}"
shopt -s nullglob
prior_diagnostics=(
    "${ARTIFACT_DIR}"/coturn.log
    "${ARTIFACT_DIR}"/test.log
    "${ARTIFACT_DIR}"/host-routing.log
    "${ARTIFACT_DIR}"/diagnostics.manifest
    "${ARTIFACT_DIR}"/server-*.log
    "${ARTIFACT_DIR}"/client-*.log
    "${ARTIFACT_DIR}"/client-*.jsonl
)
if ((${#prior_diagnostics[@]} > 0)); then
    rm -f -- "${prior_diagnostics[@]}"
fi
shopt -u nullglob
: >"${ARTIFACT_DIR}/coturn.log"
: >"${ARTIFACT_DIR}/test.log"

REDACTION_PROBE="${RAW_DIR}/redaction-probe.raw"
REDACTION_RESULT="${RAW_DIR}/redaction-probe.sanitized"
printf '%s\n' \
    "${COTURN_SECRET}" \
    "${COTURN_BAD_SECRET}" \
    'credentials of user <probe-user>' \
    '{"username":"probe-json-user","credential":"probe-json-credential"}' \
    '{"username":"[REDACTED]","credential":"probe-mixed-line"}' \
    'username=probe-query-user credential=probe-query-credential' >"${REDACTION_PROBE}"
if ! has_unredacted_credentials "${REDACTION_PROBE}"; then
    echo "ERROR: TURN diagnostic credential scan self-test failed." >&2
    exit 1
fi
sanitize_file "${REDACTION_PROBE}" "${REDACTION_RESULT}"
if grep -F 'probe-' "${REDACTION_RESULT}" >/dev/null \
    || grep -F "${COTURN_SECRET}" "${REDACTION_RESULT}" >/dev/null \
    || ! grep -F '[REDACTED]' "${REDACTION_RESULT}" >/dev/null \
    || has_unredacted_credentials "${REDACTION_RESULT}"; then
    echo "ERROR: TURN diagnostic redaction self-test failed." >&2
    exit 1
fi

if ! docker info >/dev/null 2>&1; then
    echo "ERROR: Docker is required for the pinned local coturn interoperability lane." >&2
    exit 1
fi
if ! docker image inspect "${COTURN_IMAGE}" >/dev/null 2>&1; then
    echo "ERROR: pinned coturn image is not cached; provision it with: docker pull ${COTURN_IMAGE}" >&2
    exit 1
fi

echo "==> Starting cached, pinned local coturn (${COTURN_IMAGE})"
network_create_args=(--internal)
if [ -n "${NETWORK_SUBNET}" ]; then
    network_create_args+=(--subnet "${NETWORK_SUBNET}")
fi
docker network create "${network_create_args[@]}" "${COTURN_NETWORK}" >/dev/null
docker_run_args=(
    --rm
    --tty
    --pull=never
    --name "${COTURN_CONTAINER}"
    --network "${COTURN_NETWORK}"
)
coturn_network_args=()
if [ "${USE_PRIVATE_NETWORK_ADDRESS}" = true ]; then
    docker_run_args+=(--ip "${TURN_HOST}")
    if [ "${RUNNING_IN_CONTAINER}" = true ]; then
        if ! timeout 10 docker network connect --ip "10.254.${NETWORK_OCTET}.3" \
            "${COTURN_NETWORK}" "${SELF_CONTAINER}"; then
            echo "ERROR: could not attach this container to the isolated TURN network." >&2
            exit 1
        fi
    fi
else
    docker_run_args+=(
        --publish "${BIND_HOST}:${LISTEN_PORT}:${LISTEN_PORT}/tcp"
        --publish "${BIND_HOST}:${LISTEN_PORT}:${LISTEN_PORT}/udp"
        --publish "${BIND_HOST}:${RELAY_MIN_PORT}-${RELAY_MAX_PORT}:${RELAY_MIN_PORT}-${RELAY_MAX_PORT}/udp"
    )
    coturn_network_args+=(--external-ip="${TURN_HOST}")
fi

docker run "${docker_run_args[@]}" "${COTURN_IMAGE}" \
    --fingerprint \
    --log-file=stdout \
    --simple-log \
    --realm=signal-fish-turn-interop \
    "${coturn_network_args[@]}" \
    --listening-port="${LISTEN_PORT}" \
    --min-port="${RELAY_MIN_PORT}" \
    --max-port="${RELAY_MAX_PORT}" \
    --use-auth-secret \
    --static-auth-secret="${COTURN_SECRET}" \
    --allow-loopback-peers \
    --no-multicast-peers \
    --no-tls \
    --no-dtls >"${RAW_COTURN_LOG}" 2>&1 &
COTURN_RUNNER_PID=$!

ready=false
for attempt in $(seq 1 30); do
    if ! kill -0 "${COTURN_RUNNER_PID}" 2>/dev/null; then
        echo "ERROR: coturn exited before readiness" >&2
        exit 1
    fi
    if grep -q "Relay ports initialization done" "${RAW_COTURN_LOG}" 2>/dev/null; then
        ready=true
        echo "==> coturn initialized UDP relay ports on attempt ${attempt}/30"
        break
    fi
    sleep 1
done
if [ "${ready}" != true ]; then
    echo "ERROR: coturn did not initialize UDP relay ports within 30 seconds" >&2
    exit 1
fi

capture_host_routing "coturn ready"

echo "==> Verifying cached native-client dependencies"
(cd "${CLIENT_DIR}" && cargo metadata --locked --offline --format-version 1 >/dev/null)

echo "==> Building the current signaling server offline"
(cd "${REPO_ROOT}" && cargo build --locked --offline --bin signal-fish-server)
SERVER_BIN="${REPO_ROOT}/target/debug/signal-fish-server"

echo "==> Checking TURN reference-client formatting and lints offline"
(cd "${CLIENT_DIR}" && cargo fmt --check)
(cd "${CLIENT_DIR}" && cargo clippy --locked --offline --all-targets -- -D warnings)

run_turn_test() {
    local test_name=$1
    (
        cd "${CLIENT_DIR}"
        SIGNAL_FISH_SERVER_BIN="${SERVER_BIN}" \
            SIGNAL_FISH_TURN_INTEROP_URL="${TURN_URL}" \
            SIGNAL_FISH_TURN_INTEROP_SECRET="${COTURN_SECRET}" \
            SIGNAL_FISH_TURN_INTEROP_BAD_SECRET="${COTURN_BAD_SECRET}" \
            SIGNAL_FISH_INTEROP_ARTIFACT_DIR="${ARTIFACT_DIR}" \
            cargo test --locked --offline --test turn_interop_e2e "${test_name}" -- --ignored --exact --nocapture
    ) 2>&1 | tee -a "${RAW_TEST_LOG}"
}

echo "==> Running TURN-only positive control"
run_turn_test turn_only_pair_selects_relay_candidates_and_keeps_websocket_floor_live

echo "==> Running mismatched-secret control"
run_turn_test mismatched_turn_secret_fails_p2p_and_uses_websocket_fallback

shopt -s nullglob
server_diagnostics=("${ARTIFACT_DIR}"/server-*.log)
client_stderr_diagnostics=("${ARTIFACT_DIR}"/client-*-stderr.log)
client_event_diagnostics=("${ARTIFACT_DIR}"/client-*-events.jsonl)
shopt -u nullglob
scenario_ports=()
while IFS= read -r scenario_port; do
    scenario_ports+=("${scenario_port}")
done < <(
    printf '%s\n' "${client_stderr_diagnostics[@]}" \
        | sed -E 's@.*/client-([0-9]+)-.*@\1@' \
        | sort -u
)
if ((${#server_diagnostics[@]} < 4 \
    || ${#server_diagnostics[@]} % 2 != 0 \
    || ${#client_stderr_diagnostics[@]} != 4 \
    || ${#client_event_diagnostics[@]} != 4 \
    || ${#scenario_ports[@]} != 2)); then
    echo "ERROR: TURN diagnostics do not match the two-scenario/four-client manifest." >&2
    exit 1
fi
for scenario_port in "${scenario_ports[@]}"; do
    for stream in stdout stderr; do
        if [ ! -f "${ARTIFACT_DIR}/server-${scenario_port}-${stream}.log" ]; then
            echo "ERROR: TURN scenario ${scenario_port} is missing its server ${stream} log." >&2
            exit 1
        fi
    done
done
{
    printf 'server_logs=%s\n' "${#server_diagnostics[@]}"
    printf 'successful_server_scenarios=%s\n' "${#scenario_ports[@]}"
    printf 'client_stderr_logs=%s\n' "${#client_stderr_diagnostics[@]}"
    printf 'client_event_logs=%s\n' "${#client_event_diagnostics[@]}"
    for diagnostic in "${server_diagnostics[@]}" \
        "${client_stderr_diagnostics[@]}" "${client_event_diagnostics[@]}"; do
        printf '%s\n' "${diagnostic#"${ARTIFACT_DIR}/"}"
    done
} >"${ARTIFACT_DIR}/diagnostics.manifest"
timeout 10 docker stop --time 5 "${COTURN_CONTAINER}" >/dev/null
wait "${COTURN_RUNNER_PID}" || true
COTURN_RUNNER_PID=""

echo "==> TURN-only interoperability suite passed"
