//! Shared test utilities for integration tests that exercise shell scripts.
//!
//! Centralizes helpers that were previously duplicated across multiple test
//! files (doc_consistency_script_tests, doc_consistency_policy_tests,
//! workflow_hygiene_script_tests, llm_file_size_script_tests, ci_config_tests).
//!
//! Not every test crate uses every helper, so unused-function warnings are
//! expected and suppressed at the module level.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variables from instrumented parent Cargo processes that should
/// not leak into shell-script tests unless a test explicitly re-adds them.
const NESTED_CARGO_ENV_VARS: &[&str] = &[
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_TARGET_DIR",
    "ASAN_OPTIONS",
    "LSAN_OPTIONS",
    "UBSAN_OPTIONS",
    "TSAN_OPTIONS",
    "MIRIFLAGS",
];

/// Return the repository root (Cargo manifest directory).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read a file to string, panicking with a descriptive message on failure.
pub fn read_file(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()))
}

/// Create a uniquely-named temporary directory with a descriptive prefix.
pub fn unique_temp_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("signal-fish-{prefix}-"))
        .tempdir()
        .unwrap_or_else(|e| panic!("Failed to create temporary directory: {e}"))
}

/// Write `content` to `path`, creating parent directories as needed.
pub fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("Failed to create {}: {e}", parent.display()));
    }
    fs::write(path, content).unwrap_or_else(|e| panic!("Failed to write {}: {e}", path.display()));
}

/// Remove inherited Cargo instrumentation from commands that may run nested
/// Cargo through repository shell scripts.
pub fn scrub_nested_cargo_env(command: &mut Command) {
    for var in NESTED_CARGO_ENV_VARS {
        command.env_remove(var);
    }
}

/// Build a [`Command`] that invokes `bash`.
///
/// On Windows, looks up Git Bash at well-known install paths since the
/// default WSL bash requires a Linux distribution that CI runners lack.
pub fn bash_command() -> Command {
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            Path::new("C:\\Program Files\\Git\\bin\\bash.exe"),
            Path::new("C:\\Program Files (x86)\\Git\\bin\\bash.exe"),
        ];
        for path in &candidates {
            if path.exists() {
                let mut command = Command::new(path);
                scrub_nested_cargo_env(&mut command);
                return command;
            }
        }
        panic!(
            "Git Bash not found at any known location ({candidates:?}). \
             Cannot run bash scripts on Windows without Git Bash."
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut command = Command::new("bash");
        scrub_nested_cargo_env(&mut command);
        command
    }
}
