#![cfg(test)]

mod common;

use common::{bash_command, repo_root, unique_temp_dir, write_file};
use std::fs;

const MATCHING_CLIENT_MANIFEST: &str = "[package]\nrust-version = \"1.88.0\"\n";
const MISMATCHED_CLIENT_MANIFEST: &str = "[package]\nrust-version = \"1.87.0\"\n";
const MATCHING_WASM_MANIFEST: &str = "[package]\nrust-version = \"1.94.0\"\n";
const MISMATCHED_WASM_MANIFEST: &str = "[package]\nrust-version = \"1.93.0\"\n";
const STANDALONE_MANIFESTS: [&str; 4] = [
    "clients/native/Cargo.toml",
    "clients/fortress/Cargo.toml",
    "clients/fortress-wasm/Cargo.toml",
    "fuzz/Cargo.toml",
];

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
        // The native fixtures reuse the server floor. The Godot/WASM fixture
        // independently pins the higher floor imposed by its released adapter.
        let (exit_code, output) = run_msrv_script_with_files(&[
            ("Cargo.toml", cargo_toml),
            ("rust-toolchain.toml", toolchain_toml),
            ("clippy.toml", clippy_toml),
            ("Dockerfile", "FROM rust:1.88-bookworm\n"),
            ("clients/native/Cargo.toml", cargo_toml),
            ("clients/fortress/Cargo.toml", cargo_toml),
            ("clients/fortress-wasm/Cargo.toml", MATCHING_WASM_MANIFEST),
            ("fuzz/Cargo.toml", cargo_toml),
        ]);

        assert_eq!(
            exit_code, 0,
            "case {name} should accept valid TOML whitespace variants.\nOutput:\n{output}"
        );
        assert!(
            output.contains("All server configuration files are consistent with MSRV: 1.88.0")
                && output.contains("Godot/WASM fixture is consistent with adapter MSRV: 1.94.0"),
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
        ("clients/native/Cargo.toml", MATCHING_CLIENT_MANIFEST),
        ("clients/fortress/Cargo.toml", MATCHING_CLIENT_MANIFEST),
        ("clients/fortress-wasm/Cargo.toml", MATCHING_WASM_MANIFEST),
        ("fuzz/Cargo.toml", MATCHING_CLIENT_MANIFEST),
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
fn test_msrv_script_reports_each_standalone_manifest_mismatch() {
    for mismatched_path in STANDALONE_MANIFESTS {
        let mut files = vec![
            ("Cargo.toml", MATCHING_CLIENT_MANIFEST),
            ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.88.0\"\n"),
            ("clippy.toml", "msrv = \"1.88.0\"\n"),
            ("Dockerfile", "FROM rust:1.88-bookworm\n"),
        ];
        files.extend(STANDALONE_MANIFESTS.map(|path| {
            let contents = match (path, path == mismatched_path) {
                ("clients/fortress-wasm/Cargo.toml", true) => MISMATCHED_WASM_MANIFEST,
                ("clients/fortress-wasm/Cargo.toml", false) => MATCHING_WASM_MANIFEST,
                (_, true) => MISMATCHED_CLIENT_MANIFEST,
                (_, false) => MATCHING_CLIENT_MANIFEST,
            };
            (path, contents)
        }));

        let (exit_code, output) = run_msrv_script_with_files(&files);
        assert_eq!(
            exit_code, 1,
            "a rust-version drift in {mismatched_path} must fail the check.\nOutput:\n{output}"
        );
        assert!(
            output.contains(mismatched_path) && output.contains("expected"),
            "mismatch diagnostic should identify {mismatched_path}.\nOutput:\n{output}"
        );
    }
}

#[test]
fn test_msrv_script_fails_when_each_standalone_manifest_is_missing() {
    // Missing standalone manifests are hard failures: a move must update the
    // checker rather than silently dropping MSRV coverage.
    for missing_path in STANDALONE_MANIFESTS {
        let mut files = vec![
            ("Cargo.toml", MATCHING_CLIENT_MANIFEST),
            ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.88.0\"\n"),
            ("clippy.toml", "msrv = \"1.88.0\"\n"),
            ("Dockerfile", "FROM rust:1.88-bookworm\n"),
        ];
        files.extend(
            STANDALONE_MANIFESTS
                .into_iter()
                .filter(|path| *path != missing_path)
                .map(|path| {
                    let manifest = if path == "clients/fortress-wasm/Cargo.toml" {
                        MATCHING_WASM_MANIFEST
                    } else {
                        MATCHING_CLIENT_MANIFEST
                    };
                    (path, manifest)
                }),
        );

        let (exit_code, output) = run_msrv_script_with_files(&files);
        assert_eq!(
            exit_code, 1,
            "a missing {missing_path} must fail the check.\nOutput:\n{output}"
        );
        assert!(
            output.contains(&format!("{missing_path} not found")),
            "diagnostic should name missing {missing_path}.\nOutput:\n{output}"
        );
    }
}

#[test]
fn test_msrv_script_parses_dockerfile_from_line_variants() {
    // The Dockerfile rust base may carry build flags (multi-arch
    // `FROM --platform=$BUILDPLATFORM rust:...`), an `AS <stage>` suffix, a
    // digest, or a 1.88.0 patch form. Version extraction must be tolerant of all
    // of these. A `--platform` prefix silently read as an EMPTY version is
    // exactly what broke the MSRV job after the container-image upgrade, so each
    // shape is pinned here as a regression guard for the whole parser class.
    let dockerfiles = [
        ("plain", "FROM rust:1.88-bookworm\n"),
        ("stage-suffix", "FROM rust:1.88-bookworm AS chef\n"),
        (
            "platform-flag",
            "FROM --platform=$BUILDPLATFORM rust:1.88-bookworm AS chef\n",
        ),
        ("patch-version", "FROM rust:1.88.0-bookworm\n"),
        (
            "multi-stage",
            "FROM --platform=$BUILDPLATFORM rust:1.88-bookworm AS chef\n\
             FROM debian:bookworm-slim AS runtime\n",
        ),
    ];

    for (name, dockerfile) in dockerfiles {
        let (exit_code, output) = run_msrv_script_with_files(&[
            ("Cargo.toml", "[package]\nrust-version = \"1.88.0\"\n"),
            ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.88.0\"\n"),
            ("clippy.toml", "msrv = \"1.88.0\"\n"),
            ("Dockerfile", dockerfile),
            ("clients/native/Cargo.toml", MATCHING_CLIENT_MANIFEST),
            ("clients/fortress/Cargo.toml", MATCHING_CLIENT_MANIFEST),
            ("clients/fortress-wasm/Cargo.toml", MATCHING_WASM_MANIFEST),
            ("fuzz/Cargo.toml", MATCHING_CLIENT_MANIFEST),
        ]);

        assert_eq!(
            exit_code, 0,
            "Dockerfile variant '{name}' should parse cleanly.\nOutput:\n{output}"
        );
        assert!(
            output.contains("Dockerfile (rust=1.88)"),
            "variant '{name}' must extract rust=1.88 from the FROM line.\nOutput:\n{output}"
        );
    }
}

#[test]
fn test_repository_passes_msrv_consistency_script() {
    // Run the checker against the REAL repository tree (not a synthetic
    // fixture), mirroring test_repository_passes_doc_consistency_script. The
    // fixture-based tests above cannot catch drift in the ACTUAL Dockerfile or
    // manifests -- this guard does, and it fails locally via `cargo test` before
    // the dedicated CI job ever runs. It is the test that would have caught the
    // `FROM --platform=... rust:` regression at its source.
    let root = repo_root();
    let output = bash_command()
        .arg("scripts/check-msrv-consistency.sh")
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run MSRV consistency script: {e}"));

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    assert!(
        output.status.success(),
        "Repository must satisfy scripts/check-msrv-consistency.sh (MSRV pinned \
         consistently across rust-toolchain.toml, clippy.toml, Dockerfile, and \
         clients/native/Cargo.toml, clients/fortress/Cargo.toml, and \
         clients/fortress-wasm/Cargo.toml, plus fuzz/Cargo.toml).\nOutput:\n{combined}",
    );
}
