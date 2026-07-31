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
ENV SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH=false
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
        (
            "heredoc-cannot-fake-healthcheck",
            "RUN <<EOF\n\
             HEALTHCHECK CMD curl -f http://localhost:3536/v2/health || exit 1\n\
             EOF\n",
            "Dockerfile healthcheck audit cannot safely inspect heredoc instructions.",
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
