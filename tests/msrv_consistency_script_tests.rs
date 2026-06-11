#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

fn copy_msrv_script(temp_root: &std::path::Path) {
    for script_name in ["check-msrv-consistency.sh", "read-toml-string.sh"] {
        let script_src = repo_root().join("scripts").join(script_name);
        let script_dst = temp_root.join("scripts").join(script_name);
        let script = fs::read_to_string(&script_src)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", script_src.display()));
        write_file(&script_dst, &script);
    }
}

fn run_msrv_script_with_files(files: &[(&str, &str)]) -> (i32, String) {
    let temp_root = unique_temp_dir("msrv-consistency");
    copy_msrv_script(temp_root.path());

    for (path, content) in files {
        write_file(&temp_root.path().join(path), content);
    }

    let output = bash_command()
        .arg("scripts/check-msrv-consistency.sh")
        .current_dir(temp_root.path())
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "Failed to run check-msrv-consistency.sh in {}: {e}",
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
fn test_msrv_script_accepts_toml_assignment_whitespace_variants() {
    let cases = [
        (
            "no-spaces",
            r#"[package]
rust-version="1.88.0"
"#,
            r#"[toolchain]
channel="1.88.0"
"#,
            r#"msrv="1.88.0"
"#,
        ),
        (
            "tabs",
            "[package]\nrust-version\t=\t\"1.88.0\"\n",
            "[toolchain]\nchannel\t=\t\"1.88.0\"\n",
            "msrv\t=\t\"1.88.0\"\n",
        ),
        (
            "leading-space",
            "[package]\n  rust-version = \"1.88.0\"\n",
            "[toolchain]\n  channel = \"1.88.0\"\n",
            "  msrv = \"1.88.0\"\n",
        ),
        (
            "single-quotes",
            "[package]\nrust-version = '1.88.0'\n",
            "[toolchain]\nchannel = '1.88.0'\n",
            "msrv = '1.88.0'\n",
        ),
    ];

    for (name, cargo_toml, toolchain_toml, clippy_toml) in cases {
        // The reference-client manifest reuses the same `[package]`
        // rust-version shape as the root manifest, so each whitespace
        // variant exercises the clients/native/Cargo.toml check too.
        let (exit_code, output) = run_msrv_script_with_files(&[
            ("Cargo.toml", cargo_toml),
            ("rust-toolchain.toml", toolchain_toml),
            ("clippy.toml", clippy_toml),
            ("Dockerfile", "FROM rust:1.88-bookworm\n"),
            ("clients/native/Cargo.toml", cargo_toml),
        ]);

        assert_eq!(
            exit_code, 0,
            "case {name} should accept valid TOML whitespace variants.\nOutput:\n{output}"
        );
        assert!(
            output.contains("All configuration files are consistent with MSRV: 1.88.0"),
            "case {name} should parse the MSRV consistently.\nOutput:\n{output}"
        );
    }
}

#[test]
fn test_msrv_script_reports_mismatch_after_tolerant_parsing() {
    let (exit_code, output) = run_msrv_script_with_files(&[
        ("Cargo.toml", "[package]\nrust-version= \"1.88.0\"\n"),
        (
            "rust-toolchain.toml",
            "[toolchain]\nchannel\t=\t\"1.87.0\"\n",
        ),
        ("clippy.toml", "msrv = \"1.88.0\"\n"),
        ("Dockerfile", "FROM rust:1.88-bookworm\n"),
        (
            "clients/native/Cargo.toml",
            "[package]\nrust-version = \"1.88.0\"\n",
        ),
    ]);

    assert_eq!(
        exit_code, 1,
        "MSRV mismatch should fail after parsing flexible TOML formatting.\nOutput:\n{output}"
    );
    assert!(
        output.contains("rust-toolchain.toml") && output.contains("expected"),
        "mismatch diagnostic should identify the inconsistent file.\nOutput:\n{output}"
    );
}

#[test]
fn test_msrv_script_reports_client_manifest_mismatch() {
    // ADR-0004: the reference client pins the same rust-version as the
    // server; the script enforces the pin against clients/native/Cargo.toml.
    let (exit_code, output) = run_msrv_script_with_files(&[
        ("Cargo.toml", "[package]\nrust-version = \"1.88.0\"\n"),
        ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.88.0\"\n"),
        ("clippy.toml", "msrv = \"1.88.0\"\n"),
        ("Dockerfile", "FROM rust:1.88-bookworm\n"),
        (
            "clients/native/Cargo.toml",
            "[package]\nrust-version = \"1.87.0\"\n",
        ),
    ]);

    assert_eq!(
        exit_code, 1,
        "a reference-client rust-version drift must fail the check.\nOutput:\n{output}"
    );
    assert!(
        output.contains("clients/native/Cargo.toml") && output.contains("expected"),
        "mismatch diagnostic should identify the client manifest.\nOutput:\n{output}"
    );
}

#[test]
fn test_msrv_script_fails_when_client_manifest_is_missing() {
    // A missing client manifest is a hard failure by design: if
    // clients/native ever moves, the check must be updated instead of
    // silently dropping MSRV-pin coverage.
    let (exit_code, output) = run_msrv_script_with_files(&[
        ("Cargo.toml", "[package]\nrust-version = \"1.88.0\"\n"),
        ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.88.0\"\n"),
        ("clippy.toml", "msrv = \"1.88.0\"\n"),
        ("Dockerfile", "FROM rust:1.88-bookworm\n"),
    ]);

    assert_eq!(
        exit_code, 1,
        "a missing clients/native/Cargo.toml must fail the check.\nOutput:\n{output}"
    );
    assert!(
        output.contains("clients/native/Cargo.toml not found"),
        "the diagnostic should name the missing client manifest.\nOutput:\n{output}"
    );
}
