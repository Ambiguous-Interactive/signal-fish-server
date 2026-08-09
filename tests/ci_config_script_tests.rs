#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const REQUIRED_DOCKERFILE_PREFIX: &str = "\
FROM debian:bookworm-slim AS runtime
EXPOSE 3536
ENV SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false
ENV SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=false
";

fn copy_validator_script(temp_root: &Path) {
    let source = repo_root().join("scripts/check-ci-config.sh");
    let destination = temp_root.join("scripts/check-ci-config.sh");
    let script = fs::read_to_string(&source)
        .unwrap_or_else(|error| panic!("Failed to read {}: {error}", source.display()));
    write_file(&destination, &script);
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    let mut permissions = fs::metadata(path)
        .unwrap_or_else(|error| panic!("Failed to stat {}: {error}", path.display()))
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .unwrap_or_else(|error| panic!("Failed to chmod {}: {error}", path.display()));
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

fn install_fake_cargo_tools(temp_root: &Path) -> PathBuf {
    let fake_bin = temp_root.join("bin");
    for name in ["cargo", "cargo-deny"] {
        let path = fake_bin.join(name);
        write_file(&path, "#!/usr/bin/env bash\nexit 0\n");
        make_executable(&path);
    }
    fake_bin
}

#[test]
fn tla_runner_resolves_longest_module_prefix_fail_closed() {
    let temp_root = unique_temp_dir("tla-module-resolution");
    let tla_dir = temp_root.path().join("tla");
    write_file(
        &tla_dir.join("Simple.tla"),
        "---- MODULE Simple ----\n====\n",
    );
    write_file(
        &tla_dir.join("Module_With_Underscore.tla"),
        "---- MODULE Module_With_Underscore ----\n====\n",
    );

    for (config, expected) in [
        ("Simple_Multi_Word_ExpectedFailure", "Simple"),
        (
            "Module_With_Underscore_Stale_Release_ExpectedFailure.cfg",
            "Module_With_Underscore",
        ),
    ] {
        let output = bash_command()
            .arg(repo_root().join("scripts/run-tla-model-check.sh"))
            .args(["--tla-dir", tla_dir.to_str().unwrap()])
            .args(["--resolve-module", config])
            .output()
            .expect("TLA runner module resolution should execute");
        assert!(
            output.status.success(),
            "{config} should resolve: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    }

    let output = bash_command()
        .arg(repo_root().join("scripts/run-tla-model-check.sh"))
        .args(["--tla-dir", tla_dir.to_str().unwrap()])
        .args(["--resolve-module", "Missing_Scenario_ExpectedFailure"])
        .output()
        .expect("TLA runner missing-module resolution should execute");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("no checked-in TLA+ module prefixes 'Missing_Scenario_ExpectedFailure'"),
        "missing modules must fail with an actionable diagnostic"
    );
}

fn run_validator_with_dockerfile(dockerfile_tail: &str) -> (i32, String) {
    let temp_root = unique_temp_dir("ci-config");
    copy_validator_script(temp_root.path());
    write_file(
        &temp_root.path().join("Cargo.lock"),
        "# synthetic lockfile\nversion = 4\n",
    );
    write_file(&temp_root.path().join("deny.toml"), "");
    write_file(
        &temp_root.path().join(".github/workflows/ci.yml"),
        "\
uses: EmbarkStudios/cargo-deny-action@v2
- name: Smoke test
  run: |
    for i in $(seq 1 3); do retry=true; done
    docker logs signal-fish
",
    );
    write_file(
        &temp_root.path().join("Dockerfile"),
        &format!("{REQUIRED_DOCKERFILE_PREFIX}{dockerfile_tail}"),
    );

    let fake_bin = install_fake_cargo_tools(temp_root.path());
    let original_path = env::var_os("PATH").unwrap_or_default();
    let path = env::join_paths(std::iter::once(fake_bin).chain(env::split_paths(&original_path)))
        .expect("valid PATH");
    let output = bash_command()
        .arg("scripts/check-ci-config.sh")
        .env("PATH", path)
        .current_dir(temp_root.path())
        .output()
        .expect("CI config validator should execute");

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (
        output.status.code().unwrap_or(-1),
        combined.replace("\r\n", "\n"),
    )
}

#[test]
fn test_ci_config_healthcheck_parser_accepts_logical_instruction_shapes() {
    let cases = [
        (
            "single-line",
            "HEALTHCHECK CMD curl -f http://localhost:3536/v2/health || exit 1\n",
        ),
        (
            "multiline",
            "HEALTHCHECK --interval=30s --timeout=5s \\\n\
                 CMD curl -f http://localhost:3536/v2/health || exit 1\n",
        ),
        (
            "continued-comment",
            "HEALTHCHECK --interval=30s \\\n\
                 # ignored full-line comment\n\
                 CMD curl -f http://localhost:3536/v2/health || exit 1\n",
        ),
        (
            "crlf",
            "HEALTHCHECK --interval=30s \\\r\n\
                 CMD curl -f http://localhost:3536/v2/health || exit 1\r\n",
        ),
        (
            "non-heredoc-angle-token",
            "LABEL documentation=<<not-a-heredoc\n\
             HEALTHCHECK CMD curl -f http://localhost:3536/v2/health || exit 1\n",
        ),
    ];

    for (name, dockerfile_tail) in cases {
        let (exit_code, output) = run_validator_with_dockerfile(dockerfile_tail);
        assert_eq!(exit_code, 0, "{name} should pass.\nOutput:\n{output}");
        assert!(
            output.contains("HEALTHCHECK port (3536) matches EXPOSE port (3536).")
                && output.contains("All CI config checks passed."),
            "{name} should report the matching healthcheck.\nOutput:\n{output}"
        );
    }
}

#[test]
fn test_ci_config_healthcheck_parser_rejects_inactive_or_unsafe_shapes() {
    let cases = [
        (
            "commented-out",
            "# HEALTHCHECK CMD curl -f http://localhost:3536/v2/health || exit 1\n",
            "No active HEALTHCHECK directive found in Dockerfile.",
        ),
        (
            "disabled",
            "HEALTHCHECK NONE\n",
            "Dockerfile disables its health probe with 'HEALTHCHECK NONE'.",
        ),
        (
            "absent",
            "CMD [\"./signal-fish-server\"]\n",
            "No active HEALTHCHECK directive found in Dockerfile.",
        ),
        (
            "malformed",
            "HEALTHCHECK http://localhost:3536/v2/health\n",
            "Dockerfile HEALTHCHECK must use 'CMD ...' or 'NONE' grammar.",
        ),
        (
            "malformed-continuation",
            "HEALTHCHECK --interval=30s \\ \n\
             CMD curl -f http://localhost:3536/v2/health || exit 1\n",
            "Dockerfile HEALTHCHECK must use 'CMD ...' or 'NONE' grammar.",
        ),
        (
            "wrong-port",
            "HEALTHCHECK CMD curl -f http://localhost:9000/v2/health || exit 1\n",
            "HEALTHCHECK port (9000) does not match EXPOSE port (3536).",
        ),
        (
            "builder-stage-only",
            "HEALTHCHECK CMD curl -f http://localhost:3536/v2/health || exit 1\n\
             FROM scratch\n\
             CMD [\"./signal-fish-server\"]\n",
            "No active HEALTHCHECK directive found in Dockerfile.",
        ),
        (
            "arbitrary-command-with-url",
            "HEALTHCHECK CMD echo http://localhost:3536/v2/health\n",
            "Dockerfile HEALTHCHECK must run the supported localhost curl probe.",
        ),
        (
            "inert-comment-url",
            "HEALTHCHECK CMD true # curl -f http://localhost:3536/v2/health || exit 1\n",
            "Dockerfile HEALTHCHECK must run the supported localhost curl probe.",
        ),
        (
            "different-host",
            "HEALTHCHECK CMD curl -f http://notlocalhost:3536/v2/health || exit 1\n",
            "Dockerfile HEALTHCHECK must run the supported localhost curl probe.",
        ),
        (
            "wrong-path",
            "HEALTHCHECK CMD curl -f http://localhost:3536/metrics || exit 1\n",
            "Dockerfile HEALTHCHECK must run the supported localhost curl probe.",
        ),
        (
            "eof-after-continuation",
            "HEALTHCHECK CMD curl -f http://localhost:3536/v2/health || exit 1 \\",
            "Dockerfile HEALTHCHECK has an unterminated backslash continuation.",
        ),
    ];

    for (name, dockerfile_tail, expected_diagnostic) in cases {
        let (exit_code, output) = run_validator_with_dockerfile(dockerfile_tail);
        assert_eq!(exit_code, 1, "{name} should fail.\nOutput:\n{output}");
        assert!(
            output.contains(expected_diagnostic),
            "{name} should report `{expected_diagnostic}`.\nOutput:\n{output}"
        );
    }

    for (name, opener) in [
        ("run", "RUN <<EOF"),
        ("copy", "COPY <<EOF /tmp/payload"),
        ("add", "ADD <<EOF /tmp/payload"),
        ("onbuild-add", "ONBUILD ADD <<EOF /tmp/payload"),
    ] {
        let dockerfile_tail = format!(
            "{opener}\n\
             FROM scratch\n\
             HEALTHCHECK CMD curl -f http://localhost:3536/v2/health || exit 1\n\
             EOF\n"
        );
        let (exit_code, output) = run_validator_with_dockerfile(&dockerfile_tail);
        assert_eq!(
            exit_code, 1,
            "{name} heredoc should fail closed.\nOutput:\n{output}"
        );
        assert!(
            output.contains(
                "Dockerfile healthcheck audit cannot safely inspect heredoc instructions."
            ),
            "{name} heredoc body must not impersonate FROM or HEALTHCHECK.\nOutput:\n{output}"
        );
    }
}

#[test]
fn test_repository_ci_config_has_active_matching_healthcheck() {
    let temp_root = unique_temp_dir("ci-config-repository-tools");
    let fake_bin = install_fake_cargo_tools(temp_root.path());
    let original_path = env::var_os("PATH").unwrap_or_default();
    let path = env::join_paths(std::iter::once(fake_bin).chain(env::split_paths(&original_path)))
        .expect("valid PATH");
    let output = bash_command()
        .arg("scripts/check-ci-config.sh")
        .env("PATH", path)
        .current_dir(repo_root())
        .output()
        .expect("repository CI config validator should execute");

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    let combined = combined.replace("\r\n", "\n");

    assert!(
        output.status.success(),
        "checked-in CI configuration should pass.\nOutput:\n{combined}"
    );
    assert!(
        combined.contains("HEALTHCHECK port (3536) matches EXPOSE port (3536).")
            && combined.contains("All CI config checks passed."),
        "checked-in Dockerfile healthcheck should be recognized.\nOutput:\n{combined}"
    );
}

/// Extract one shell function body verbatim from a script, so a test drives
/// the shipped code rather than a copy of it.
fn extract_shell_function(script: &str, name: &str) -> String {
    let header = format!("{name}() {{");
    let start = script
        .find(&header)
        .unwrap_or_else(|| panic!("scripts/run-turn-interop.sh must define {name}()"));
    let rest = &script[start..];
    let end = rest
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{name}() must close with a brace at column 0"));
    rest[..end + 3].to_string()
}

/// The TURN reachability gate must actually retry (issue #276).
///
/// Under `set -e` a bare `probe_turn_udp` call would end the script on the
/// first unanswered probe, collapsing the twenty-attempt wait into one attempt
/// while every symptom — a green run whose probe answered immediately — stayed
/// identical. That is exactly the hollow guard this drives out: the loop is
/// exercised with a stubbed probe whose answer arrives late, never, or not at
/// all.
#[test]
fn test_turn_reachability_gate_retries_until_answered() {
    let script = fs::read_to_string(repo_root().join("scripts/run-turn-interop.sh"))
        .expect("read scripts/run-turn-interop.sh");
    let gate = extract_shell_function(&script, "wait_for_turn_udp_reachable");

    // (probe outcome program, expected exit status, expected marker)
    let cases: [(&str, i32, &str); 4] = [
        // Answered immediately.
        (
            "[ \"${ATTEMPT}\" -ge 1 ] && return 0; return 1",
            0,
            "attempt 1/20",
        ),
        // Answered only on the fifth probe: the wait must absorb the first four.
        (
            "[ \"${ATTEMPT}\" -ge 5 ] && return 0; return 1",
            0,
            "attempt 5/20",
        ),
        // Never answered: the gate fails with its own diagnosis, not silently.
        ("return 1", 1, "cannot reach coturn"),
        // No `/dev/udp` on this host: measurement is impossible, so the gate
        // must stand aside rather than invent a negative result.
        ("return 2", 0, "skipping the reachability gate"),
    ];

    for (probe_program, expected_status, expected_marker) in cases {
        let temp_root = unique_temp_dir("turn-reachability-gate");
        let artifact_dir = temp_root.path().join("artifacts");
        fs::create_dir_all(&artifact_dir).expect("create artifact dir");
        let counter = temp_root.path().join("attempts");
        // Relative names, resolved against the harness's own working
        // directory: a `Path::display()` interpolation would embed Windows
        // backslashes, which bash then eats as escapes.
        let harness = format!(
            "set -euo pipefail\n\
             ARTIFACT_DIR=artifacts\n\
             TURN_HOST=203.0.113.9\n\
             LISTEN_PORT=3478\n\
             COUNTER=attempts\n\
             printf '0' >\"${{COUNTER}}\"\n\
             # Stubbed probe: the gate's contract is what it does with the\n\
             # status, not how the datagram is sent.\n\
             probe_turn_udp() {{\n\
             ATTEMPT=$(( $(cat \"${{COUNTER}}\") + 1 ))\n\
             printf '%s' \"${{ATTEMPT}}\" >\"${{COUNTER}}\"\n\
             {probe_program}\n\
             }}\n\
             # Keep the wall clock out of it; the retry count is the contract.\n\
             sleep() {{ :; }}\n\
             {gate}\n\
             wait_for_turn_udp_reachable\n",
        );
        write_file(&temp_root.path().join("gate.sh"), &harness);

        let output = bash_command()
            .arg("gate.sh")
            .current_dir(temp_root.path())
            .output()
            .expect("reachability gate harness should execute");
        let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        let combined = combined.replace("\r\n", "\n");
        let attempts = fs::read_to_string(&counter).unwrap_or_default();

        assert_eq!(
            output.status.code().unwrap_or(-1),
            expected_status,
            "probe `{probe_program}` must exit {expected_status} \
             (attempts: {attempts})\nOutput:\n{combined}"
        );
        assert!(
            combined.contains(expected_marker),
            "probe `{probe_program}` must report `{expected_marker}` \
             (attempts: {attempts})\nOutput:\n{combined}"
        );
    }
}
