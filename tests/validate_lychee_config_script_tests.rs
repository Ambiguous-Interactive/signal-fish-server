#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn lychee_config_with_assignments(
    timeout_assignment: &str,
    concurrency_assignment: &str,
) -> String {
    format!(
        r#"# Test lychee config
{concurrency_assignment}

accept = [
    "100..=103",
    "200..=299",
]

exclude = [
    "^https?://localhost",
    "^https?://127\\.0\\.0\\.1",
    "^wss?://localhost",
    "^mailto:",
]

{timeout_assignment}

user_agent = "Signal Fish test"

exclude_path = [
    "target/",
    ".git/",
    "third_party/",
    "node_modules/",
]
"#
    )
}

fn copy_validator_script(temp_root: &Path) {
    let script_src = repo_root().join("scripts/validate-lychee-config.sh");
    let script_dst = temp_root.join("scripts/validate-lychee-config.sh");
    let script = fs::read_to_string(&script_src)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));
    write_file(&script_dst, &script);
}

fn install_fake_lychee(temp_root: &Path) -> std::path::PathBuf {
    let fake_lychee = temp_root.join("bin/lychee");
    write_file(
        &fake_lychee,
        "#!/usr/bin/env bash\nset -euo pipefail\nif [ \"${1:-}\" = \"--dump\" ]; then exit 0; fi\nexit 1\n",
    );

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&fake_lychee)
            .unwrap_or_else(|e| panic!("Failed to stat {}: {e}", fake_lychee.display()))
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_lychee, permissions)
            .unwrap_or_else(|e| panic!("Failed to chmod {}: {e}", fake_lychee.display()));
    }

    fake_lychee
        .parent()
        .expect("fake lychee has parent")
        .to_path_buf()
}

fn run_validator_with_config(config: &str) -> (i32, String) {
    let temp_root = unique_temp_dir("validate-lychee-config");
    copy_validator_script(temp_root.path());
    write_file(&temp_root.path().join(".lychee.toml"), config);

    let fake_bin = install_fake_lychee(temp_root.path());
    let original_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin).chain(std::env::split_paths(&original_path)),
    )
    .expect("valid PATH");

    let output = bash_command()
        .arg("scripts/validate-lychee-config.sh")
        .env("PATH", path)
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "Failed to run validate-lychee-config.sh in {}: {e}",
                temp_root.path().display()
            )
        });

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    (
        output.status.code().unwrap_or(-1),
        combined.replace("\r\n", "\n"),
    )
}

#[test]
fn test_validate_lychee_config_accepts_toml_assignment_whitespace_variants() {
    let cases = [
        ("no-spaces", "timeout=20", "max_concurrency=16"),
        ("single-spaces", "timeout = 20", "max_concurrency = 16"),
        ("tabs", "timeout\t=\t20", "max_concurrency\t=\t16"),
        (
            "leading-space-and-comments",
            "  timeout = 20 # seconds",
            "  max_concurrency = 16 # workers",
        ),
    ];

    for (name, timeout_assignment, concurrency_assignment) in cases {
        let config = lychee_config_with_assignments(timeout_assignment, concurrency_assignment);
        let (exit_code, output) = run_validator_with_config(&config);

        assert_eq!(
            exit_code, 0,
            "case {name} should pass validation.\nOutput:\n{output}"
        );
        assert!(
            output.contains("Timeout is reasonable (20 seconds)"),
            "case {name} should parse timeout despite TOML whitespace variants.\nOutput:\n{output}"
        );
        assert!(
            output.contains("max_concurrency is reasonable (16)"),
            "case {name} should parse max_concurrency despite TOML whitespace variants.\nOutput:\n{output}"
        );
    }
}

#[test]
fn test_validate_lychee_config_accepts_single_quoted_toml_arrays() {
    let config = r#"
max_concurrency = 16

accept = ['200..=299']

exclude = [
    '^https?://localhost',
    '^https?://127\.0\.0\.1',
    '^wss?://localhost',
    '^mailto:',
]

timeout = 20
user_agent = 'Signal Fish test'
exclude_path = ['target/', '.git/', 'third_party/', 'node_modules/']
"#;

    let (exit_code, output) = run_validator_with_config(config);

    assert_eq!(
        exit_code, 0,
        "single-quoted TOML strings should pass validation.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Excludes: http://127.0.0.1") && output.contains("Excludes: node_modules/"),
        "single-quoted exclude arrays should be parsed.\nOutput:\n{output}"
    );
}

#[test]
fn test_validate_lychee_config_accepts_hash_inside_single_quoted_array_value() {
    let config = r#"
max_concurrency = 16
accept = ['200..=299']
exclude = ['^https?://literal#fragment', '^https?://localhost', '^https?://127\.0\.0\.1', '^wss?://localhost', '^mailto:']
timeout = 20
user_agent = 'Signal Fish test'
exclude_path = ['target/', '.git/', 'third_party/', 'node_modules/']
"#;

    let (exit_code, output) = run_validator_with_config(config);

    assert_eq!(
        exit_code, 0,
        "single-quoted # characters are literal TOML content, not comments.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Excludes: http://localhost") && output.contains("Excludes: mailto:"),
        "inline single-quoted arrays with # must preserve later values.\nOutput:\n{output}"
    );
}

#[test]
fn test_validate_lychee_config_rejects_non_integer_numeric_policy_values() {
    let cases = [
        (
            "quoted-timeout",
            "timeout = \"20\"",
            "max_concurrency = 16",
            "timeout must be an integer",
        ),
        (
            "boolean-timeout",
            "timeout = true",
            "max_concurrency = 16",
            "timeout must be an integer",
        ),
        (
            "negative-timeout",
            "timeout = -1",
            "max_concurrency = 16",
            "timeout must be an integer",
        ),
        (
            "quoted-concurrency",
            "timeout = 20",
            "max_concurrency = \"16\"",
            "max_concurrency must be an integer",
        ),
        (
            "negative-concurrency",
            "timeout = 20",
            "max_concurrency = -1",
            "max_concurrency must be an integer",
        ),
    ];

    for (name, timeout_assignment, concurrency_assignment, expected) in cases {
        let config = lychee_config_with_assignments(timeout_assignment, concurrency_assignment);
        let (exit_code, output) = run_validator_with_config(&config);

        assert_eq!(
            exit_code, 1,
            "case {name} should fail validation.\nOutput:\n{output}"
        );
        assert!(
            output.contains(expected),
            "case {name} should report a useful integer diagnostic.\nExpected: {expected}\nOutput:\n{output}"
        );
    }
}

#[test]
fn test_validate_lychee_config_matches_exact_toml_keys() {
    let config = r#"
max_concurrency_limit = 16

accept = ["200..=299"]
exclude = ["^https?://localhost", "^https?://127\\.0\\.0\\.1", "^wss?://localhost", "^mailto:"]
timeout_seconds = 20
user_agent = "Signal Fish test"
exclude_path = ["target/", ".git/", "third_party/", "node_modules/"]
"#;

    let (exit_code, output) = run_validator_with_config(config);

    assert_eq!(
        exit_code, 1,
        "prefix keys must not satisfy required bare keys.\nOutput:\n{output}"
    );
    assert!(
        output.contains("Missing required field: max_concurrency")
            && output.contains("Missing required field: timeout"),
        "validator should require exact TOML keys, not prefixes.\nOutput:\n{output}"
    );
}

#[test]
fn test_validate_lychee_config_requires_root_keys_not_table_keys() {
    let config = r#"
[policy]
max_concurrency = 16
accept = ['200..=299']
exclude = ['^https?://localhost', '^https?://127\.0\.0\.1', '^wss?://localhost', '^mailto:']
timeout = 20
user_agent = 'Signal Fish test'
exclude_path = ['target/', '.git/', 'third_party/', 'node_modules/']
"#;

    let (exit_code, output) = run_validator_with_config(config);

    assert_eq!(
        exit_code, 1,
        "required lychee settings must be root-level TOML keys.\nOutput:\n{output}"
    );

    for field in [
        "max_concurrency",
        "accept",
        "exclude",
        "timeout",
        "user_agent",
    ] {
        assert!(
            output.contains(&format!("Missing required field: {field}")),
            "table-scoped {field} must not satisfy required root key.\nOutput:\n{output}"
        );
    }
}
