#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_path(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

#[test]
fn public_upgrade_probe_workflow_requires_repository_configuration_without_external_identity() {
    let workflow = fs::read_to_string(repo_path(".github/workflows/public-upgrade-probe.yml"))
        .expect("read public upgrade probe workflow");
    let plan = fs::read_to_string(repo_path("PLAN.md")).expect("read deployment plan");

    for required in [
        "vars.SIGNAL_FISH_PUBLIC_WS_URL",
        "inputs.endpoint_url || vars.SIGNAL_FISH_PUBLIC_WS_URL",
        "'unconfigured'",
        "Validate probe endpoint configuration",
        "SIGNAL_FISH_PUBLIC_WS_URL is not configured",
    ] {
        assert!(
            workflow.contains(required),
            "the public probe workflow must contain {required:?}"
        );
    }
    let workflow_without_descriptions = workflow
        .lines()
        .filter(|line| !line.trim_start().starts_with("description:"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !workflow_without_descriptions.contains("ws://")
            && !workflow_without_descriptions.contains("wss://"),
        "the probe workflow must not contain a literal public WebSocket endpoint"
    );
    let deployment_section = plan
        .split("### P97 — Public WebSocket upgrade deployment and first eligible probe (#367)")
        .nth(1)
        .expect("P97 deployment section")
        .split("\n### ")
        .next()
        .expect("bounded P97 deployment section");
    assert!(
        !deployment_section.contains("github.com/") && !deployment_section.contains("](http"),
        "the deployment requirement must not identify an external repository:\n{deployment_section}"
    );

    let validation = workflow
        .find("Validate probe endpoint configuration")
        .expect("endpoint validation step");
    let probe = workflow
        .find("Probe simultaneous WebSocket upgrades")
        .expect("probe step");
    assert!(
        validation < probe,
        "missing endpoint configuration must fail clearly before network probing"
    );
    let validation_step = &workflow[validation..probe];
    let empty_check = validation_step
        .find(r#"if [ -z "${PROBE_URL:-}" ]; then"#)
        .expect("validation step must check for an empty probe URL");
    let nonzero_exit = validation_step[empty_check..]
        .find("exit 1")
        .expect("empty endpoint validation must exit nonzero");
    assert!(
        nonzero_exit > 0,
        "empty endpoint validation must fail after detecting the empty URL"
    );
}

fn write_fake_curl(root: &Path) -> PathBuf {
    let path = root.join("fake-curl");
    fs::write(
        &path,
        r#"#!/usr/bin/env bash
set -euo pipefail

if (($# == 0)) || [[ $1 != --disable ]]; then
    echo "probe must pass --disable as curl argument 1 to ignore default curl configuration files" >&2
    exit 2
fi
shift

header_file=
request_url=
request_key=
probe_attempt_id=
connect_timeout=
max_time=
while (($# > 0)); do
    if [[ $1 == --dump-header ]]; then
        header_file=$2
        shift 2
    elif [[ $1 == --connect-timeout ]]; then
        connect_timeout=$2
        shift 2
    elif [[ $1 == --max-time ]]; then
        max_time=$2
        shift 2
    elif [[ $1 == --header ]]; then
        case $2 in
            Sec-WebSocket-Key:*) request_key=${2#*: } ;;
            X-Signal-Fish-Probe-Attempt-Id:*) probe_attempt_id=${2#*: } ;;
        esac
        shift 2
    else
        request_url=$1
        shift
    fi
done
if [[ ${request_url} != https://signal.example/v2/ws ]]; then
    echo "probe did not translate wss:// to curl's https:// transport: ${request_url}" >&2
    exit 3
fi

burst=${SIGNAL_FISH_PROBE_BURST:?}
peer=${SIGNAL_FISH_PROBE_PEER:?}
if [[ -z ${request_key} || -z ${probe_attempt_id} ]]; then
    echo "probe omitted its per-request WebSocket key or attempt ID" >&2
    exit 4
fi
if [[ ${FAKE_CURL_MODE:-accepted} == timeout-contract ]] &&
    { [[ ! ${connect_timeout} =~ ^[1-9][0-9]*$ ]] ||
        [[ ! ${max_time} =~ ^[1-9][0-9]*$ ]] ||
        ((max_time <= connect_timeout)); }; then
    echo "probe total timeout must exceed its connect timeout: connect=${connect_timeout:-none} total=${max_time:-none}" >&2
    exit 5
fi
printf '%s\n' "${request_key}" >"${FAKE_CURL_STATE}/burst-${burst}-peer-${peer}.key"
printf '%s\n' "${probe_attempt_id}" >"${FAKE_CURL_STATE}/burst-${burst}-peer-${peer}.attempt"
touch "${FAKE_CURL_STATE}/burst-${burst}-peer-${peer}"
other=1
if [[ ${peer} == 1 ]]; then
    other=2
fi
overlap_attempt=0
while ((overlap_attempt < 100)); do
    if [[ -f ${FAKE_CURL_STATE}/burst-${burst}-peer-${other} ]]; then
        break
    fi
    sleep 0.01
    overlap_attempt=$((overlap_attempt + 1))
done
if [[ ! -f ${FAKE_CURL_STATE}/burst-${burst}-peer-${other} ]]; then
    echo "fake curl did not overlap both peers in burst ${burst}" >&2
    exit 7
fi
other_key=$(<"${FAKE_CURL_STATE}/burst-${burst}-peer-${other}.key")
other_attempt_id=$(<"${FAKE_CURL_STATE}/burst-${burst}-peer-${other}.attempt")
if [[ ${request_key} == "${other_key}" ]]; then
    echo "probe reused Sec-WebSocket-Key within burst ${burst}" >&2
    exit 8
fi
if [[ ${probe_attempt_id} == "${other_attempt_id}" ]]; then
    echo "probe reused its client attempt ID within burst ${burst}" >&2
    exit 9
fi

if [[ ( ${FAKE_CURL_MODE:-accepted} == rejected || ${FAKE_CURL_MODE:-accepted} == sensitive-rejected ) && ${peer} == 2 ]]; then
    if [[ ${FAKE_CURL_MODE:-accepted} == sensitive-rejected ]]; then
        {
            printf 'HTTP/1.1 503 Service Unavailable\r\n'
            printf 'Connection: close\r\n'
            printf 'x-signal-fish-request-id: 00000000-0000-4000-8000-000000000999\r\n'
            printf 'x-signal-fish-upgrade-outcome: rejected_draining\r\n'
            printf 'Set-Cookie: session=PROBE_PRIVACY_SENTINEL\r\n'
            printf 'Authorization: Bearer PROBE_PRIVACY_SENTINEL\r\n'
            printf 'X-Vendor-Authorization: PROBE_PRIVACY_SENTINEL\r\n'
            printf '\r\n'
        } >"${header_file}"
    else
        printf 'HTTP/1.1 503 Service Unavailable\r\n\r\n' >"${header_file}"
    fi
    printf '503'
    exit 0
fi

request_number=$((burst * 2 + peer))
if [[ ${FAKE_CURL_MODE:-accepted} == duplicate-id ]]; then
    request_number=1
fi
request_id=$(printf '00000000-0000-4000-8000-%012d' "${request_number}")
websocket_accept=$(
    printf '%s%s' "${request_key}" '258EAFA5-E914-47DA-95CA-C5AB0DC85B11' \
        | openssl dgst -sha1 -binary \
        | openssl base64 -A
)
{
    if [[ ${FAKE_CURL_MODE:-accepted} == proxy-response-chain ]]; then
        printf 'HTTP/1.1 200 Connection established\r\n'
        printf 'Upgrade: not-websocket\r\n'
        printf 'x-signal-fish-request-id: not-an-application-id\r\n'
        printf 'x-signal-fish-upgrade-outcome: rejected_by_proxy\r\n'
        printf '\r\n'
    fi
    printf 'HTTP/1.1 101 Switching Protocols\r\n'
    printf 'Upgrade: websocket\r\n'
    if [[ ${FAKE_CURL_MODE:-accepted} == duplicate-upgrade && ${peer} == 2 ]]; then
        printf 'Upgrade: h2c\r\n'
    fi
    if [[ ${FAKE_CURL_MODE:-accepted} == repeated-connection ]]; then
        printf 'Connection: keep-alive\r\n'
        printf 'Connection: Upgrade\r\n'
    else
        printf 'Connection: keep-alive, Upgrade\r\n'
    fi
    if [[ ${FAKE_CURL_MODE:-accepted} != missing-accept ]]; then
        if [[ ${FAKE_CURL_MODE:-accepted} == wrong-accept ]]; then
            printf 'Sec-WebSocket-Accept: invalid\r\n'
        else
            printf 'Sec-WebSocket-Accept: %s\r\n' "${websocket_accept}"
        fi
        if [[ ${FAKE_CURL_MODE:-accepted} == duplicate-accept && ${peer} == 2 ]]; then
            printf 'Sec-WebSocket-Accept: invalid\r\n'
        fi
    fi
    if [[ ${FAKE_CURL_MODE:-accepted} != missing-diagnostics && ${FAKE_CURL_MODE:-accepted} != trailer-diagnostics ]]; then
        printf 'x-signal-fish-request-id: %s\r\n' "${request_id}"
        if [[ ${FAKE_CURL_MODE:-accepted} == duplicate-request-id && ${peer} == 2 ]]; then
            printf 'x-signal-fish-request-id: 00000000-0000-4000-8000-999999999999\r\n'
        fi
        printf 'x-signal-fish-upgrade-outcome: accepted\r\n'
        if [[ ${FAKE_CURL_MODE:-accepted} == duplicate-outcome && ${peer} == 2 ]]; then
            printf 'x-signal-fish-upgrade-outcome: accepted\r\n'
        fi
    fi
    printf '\r\n'
    if [[ ${FAKE_CURL_MODE:-accepted} == trailer-diagnostics ]]; then
        printf 'x-signal-fish-request-id: %s\r\n' "${request_id}"
        printf 'x-signal-fish-upgrade-outcome: accepted\r\n'
        printf '\r\n'
    fi
} >"${header_file}"
printf '101'
exit 28
"#,
    )
    .expect("write fake curl");
    let mut permissions = fs::metadata(&path)
        .expect("fake curl metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make fake curl executable");
    path
}

fn run_probe(mode: &str) -> std::process::Output {
    let temp = tempfile::tempdir().expect("create probe test directory");
    let state = temp.path().join("state");
    fs::create_dir(&state).expect("create fake curl state directory");
    let fake_curl = write_fake_curl(temp.path());

    Command::new("bash")
        .arg(repo_path("scripts/probe-websocket-upgrades.sh"))
        .arg("wss://signal.example/v2/ws")
        .arg("3")
        .env("SIGNAL_FISH_CURL_BIN", fake_curl)
        .env("FAKE_CURL_STATE", state)
        .env("FAKE_CURL_MODE", mode)
        .output()
        .expect("run public upgrade probe")
}

#[test]
fn simultaneous_upgrade_probe_accepts_complete_correlated_bursts() {
    let output = run_probe("accepted");
    assert!(
        output.status.success(),
        "probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("probe stdout is UTF-8");
    assert_eq!(stdout.matches("http_status=101").count(), 6, "{stdout}");
    assert_eq!(stdout.matches("probe_attempt_id=").count(), 6, "{stdout}");
    assert!(
        stdout.contains(
            "PASS: 3 simultaneous two-client WebSocket upgrade bursts completed (3 x 2 accepted requests)"
        ),
        "{stdout}"
    );
}

#[test]
fn simultaneous_upgrade_probe_ignores_default_curl_configuration_files() {
    let output = run_probe("accepted");
    assert!(
        output.status.success(),
        "curl --disable must be argument 1 so default configuration files cannot alter or expose the probe\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn simultaneous_upgrade_probe_validates_only_the_final_response_header_block() {
    let output = run_probe("proxy-response-chain");
    assert!(
        output.status.success(),
        "proxy CONNECT/interim headers must not be conflated with the final WebSocket response\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn simultaneous_upgrade_probe_does_not_treat_trailers_as_response_headers() {
    let output = run_probe("trailer-diagnostics");
    assert!(
        !output.status.success(),
        "trailer fields must not satisfy required final-response headers\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).expect("probe stderr is UTF-8");
    assert!(
        stderr.contains("lacked a valid x-signal-fish-request-id (got none)"),
        "{stderr}"
    );
}

#[test]
fn simultaneous_upgrade_probe_preserves_time_after_the_connect_budget() {
    let output = run_probe("timeout-contract");
    assert!(
        output.status.success(),
        "the total request timeout must leave time to receive HTTP 101 after connecting\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn simultaneous_upgrade_probe_accepts_upgrade_token_in_repeated_connection_field() {
    let output = run_probe("repeated-connection");
    assert!(
        output.status.success(),
        "the upgrade token in a repeated Connection field must be recognized\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn simultaneous_upgrade_probe_reports_the_exact_failed_peer() {
    let output = run_probe("rejected");
    assert!(
        !output.status.success(),
        "rejected peer must fail the probe"
    );
    let stderr = String::from_utf8(output.stderr).expect("probe stderr is UTF-8");
    assert!(
        stderr.contains("burst=1 peer=2 probe_attempt_id=")
            && stderr.contains("expected HTTP 101, got 503"),
        "{stderr}"
    );
    assert!(
        stderr.contains("simultaneous WebSocket upgrade burst 1/3 failed"),
        "{stderr}"
    );
}

#[test]
fn simultaneous_upgrade_probe_redacts_non_allowlisted_response_headers() {
    let output = run_probe("sensitive-rejected");
    assert!(
        !output.status.success(),
        "a rejected peer must fail the probe"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let combined_lower = combined.to_ascii_lowercase();

    for forbidden in [
        "probe_privacy_sentinel",
        "set-cookie",
        "authorization",
        "x-vendor-authorization",
    ] {
        assert!(
            !combined_lower.contains(forbidden),
            "probe output leaked forbidden response evidence {forbidden:?}:\n{combined}"
        );
    }
    for required in [
        "burst=1 peer=2 probe_attempt_id=",
        "expected HTTP 101, got 503",
        "allowlisted response evidence:",
        "HTTP/1.1 503 Service Unavailable",
        "Connection: close",
        "X-Signal-Fish-Request-Id: 00000000-0000-4000-8000-000000000999",
        "X-Signal-Fish-Upgrade-Outcome: rejected_draining",
    ] {
        assert!(
            combined.contains(required),
            "probe output omitted useful allowlisted evidence {required:?}:\n{combined}"
        );
    }
}

#[test]
fn simultaneous_upgrade_probe_fails_closed_without_websocket_accept() {
    let output = run_probe("missing-accept");
    assert!(
        !output.status.success(),
        "HTTP 101 without Sec-WebSocket-Accept must fail the probe"
    );
    let stderr = String::from_utf8(output.stderr).expect("probe stderr is UTF-8");
    assert!(
        stderr.contains("HTTP 101 had invalid Sec-WebSocket-Accept (got none"),
        "{stderr}"
    );
}

#[test]
fn simultaneous_upgrade_probe_fails_closed_on_wrong_websocket_accept() {
    let output = run_probe("wrong-accept");
    assert!(
        !output.status.success(),
        "HTTP 101 with a mismatched Sec-WebSocket-Accept must fail the probe"
    );
    let stderr = String::from_utf8(output.stderr).expect("probe stderr is UTF-8");
    assert!(
        stderr.contains("HTTP 101 had invalid Sec-WebSocket-Accept (got invalid"),
        "{stderr}"
    );
}

#[test]
fn simultaneous_upgrade_probe_fails_closed_without_application_diagnostics() {
    let output = run_probe("missing-diagnostics");
    assert!(
        !output.status.success(),
        "a proxy-only 101 without application evidence must fail the probe"
    );
    let stderr = String::from_utf8(output.stderr).expect("probe stderr is UTF-8");
    assert!(
        stderr.contains("HTTP 101 lacked a valid x-signal-fish-request-id"),
        "{stderr}"
    );
}

#[test]
fn simultaneous_upgrade_probe_rejects_duplicate_correlation_ids() {
    let output = run_probe("duplicate-id");
    assert!(
        !output.status.success(),
        "duplicate correlation IDs must fail the probe"
    );
    let stderr = String::from_utf8(output.stderr).expect("probe stderr is UTF-8");
    assert!(stderr.contains("reused request ID"), "{stderr}");
}

#[test]
fn simultaneous_upgrade_probe_rejects_repeated_singleton_response_headers() {
    for (mode, header) in [
        ("duplicate-upgrade", "Upgrade"),
        ("duplicate-accept", "Sec-WebSocket-Accept"),
        ("duplicate-request-id", "x-signal-fish-request-id"),
        ("duplicate-outcome", "x-signal-fish-upgrade-outcome"),
    ] {
        let output = run_probe(mode);
        assert!(
            !output.status.success(),
            "repeated singleton {header} must fail the probe"
        );
        let stderr = String::from_utf8(output.stderr).expect("probe stderr is UTF-8");
        let expected =
            format!("HTTP 101 had duplicate singleton response header {header} (count=2)");
        assert!(
            stderr.contains("burst=1 peer=2 probe_attempt_id=") && stderr.contains(&expected),
            "mode={mode}, expected duplicate diagnostic {expected:?}:\n{stderr}"
        );
        assert!(
            stderr.contains("simultaneous WebSocket upgrade burst 1/3 failed"),
            "mode={mode}: {stderr}"
        );
    }
}

#[test]
fn simultaneous_upgrade_probe_rejects_invalid_scope_before_network_access() {
    for (url, bursts, expected) in [
        (
            "https://signal.example/v2/ws",
            "1",
            "probe URL must be one absolute ws:// or wss:// URL",
        ),
        (
            "wss://signal.example/v2/ws",
            "0",
            "burst-count must be an integer from 1 through 100",
        ),
        (
            "wss://signal.example/v2/ws",
            "101",
            "burst-count must be an integer from 1 through 100",
        ),
    ] {
        let output = Command::new("bash")
            .arg(repo_path("scripts/probe-websocket-upgrades.sh"))
            .arg(url)
            .arg(bursts)
            .output()
            .expect("run invalid probe case");
        assert_eq!(output.status.code(), Some(2), "url={url}, bursts={bursts}");
        let stderr = String::from_utf8(output.stderr).expect("probe stderr is UTF-8");
        assert!(
            stderr.contains(expected),
            "url={url}, bursts={bursts}: {stderr}"
        );
    }
}

#[test]
fn simultaneous_upgrade_probe_reports_empty_artifacts_when_curl_never_starts() {
    let output = Command::new("bash")
        .arg(repo_path("scripts/probe-websocket-upgrades.sh"))
        .arg("wss://signal.example/v2/ws")
        .arg("1")
        .env("SIGNAL_FISH_CURL_BIN", "false")
        .output()
        .expect("run probe with failing curl executable");

    assert!(!output.status.success(), "a curl startup failure must fail");
    let stderr = String::from_utf8(output.stderr).expect("probe stderr is UTF-8");
    assert!(
        stderr.contains("burst=1 peer=1 probe_attempt_id=")
            && stderr.contains("burst=1 peer=2 probe_attempt_id="),
        "{stderr}"
    );
    assert_eq!(
        stderr
            .matches("expected HTTP 101, got none (curl_exit=1)")
            .count(),
        2,
        "{stderr}"
    );
    assert_eq!(
        stderr
            .matches("allowlisted response evidence: none")
            .count(),
        2,
        "{stderr}"
    );
    assert_eq!(stderr.matches("curl stderr: none").count(), 2, "{stderr}");
}

#[test]
fn public_upgrade_probe_stays_within_macos_bash_3_2_syntax() {
    let script = fs::read_to_string(repo_path("scripts/probe-websocket-upgrades.sh"))
        .expect("read public upgrade probe");

    for bash_4_construct in [
        ",,}",
        "^^}",
        "declare -A",
        "mapfile",
        "readarray",
        "coproc",
        "seq ",
    ] {
        assert!(
            !script.contains(bash_4_construct),
            "probe must support stock macOS Bash 3.2; found {bash_4_construct:?}"
        );
    }
}
