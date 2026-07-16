//! Shared test utilities for integration tests that exercise shell scripts.
//!
//! Centralizes helpers that were previously duplicated across multiple test
//! files (doc_consistency_script_tests, doc_consistency_policy_tests,
//! workflow_hygiene_script_tests, agent_skill_file_script_tests, ci_config_tests).
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
///
/// Line endings are normalized to LF (`\r\n` -> `\n`). Every consumer reads
/// committed text to assert on *content*, never on line-ending style, yet Git
/// checks these files out with CRLF on Windows (and WSL2 mounts) under the
/// repo's `* text=auto` attribute. Normalizing here makes the whole
/// content-assertion test suite deterministic across platforms — a test green
/// on Linux/macOS must not flip red on `Nextest (windows-latest)` purely
/// because of checkout EOL. A *lone* mid-line `\r` is deliberately preserved so
/// genuine stray-CR bugs (e.g. markdownlint MD039 link-text breakage) stay
/// detectable.
pub fn read_file(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// Drop full-line `#` and `//` comments from config text, returning only the
/// *live* (uncommented) lines.
///
/// Both markers are stripped because the config formats these tests guard use
/// one or the other for line comments: `#` (YAML, Dockerfile, TOML, POSIX shell,
/// PowerShell) and `//` (JSONC, e.g. `.devcontainer/devcontainer.json`). See the
/// *Scope and contract* section below for the exact rules.
///
/// # Why this exists
///
/// Drift-guard tests assert that a config file still contains some required
/// token, e.g. `assert!(workflow.contains("docker/setup-qemu-action"))`. A raw
/// `String::contains` matches anywhere in the file, so a maintainer who merely
/// *comments out* the real line —
///
/// ```text
/// # uses: docker/setup-qemu-action@v3   # temporarily disabled
/// ```
///
/// — leaves text that still contains the token, and the guard passes while the
/// live config has silently lost the setting. That is exactly the regression
/// these guards exist to catch. Asserting against the comment-stripped view
/// closes the hole.
///
/// # Scope and contract
///
/// - Removes lines whose first non-whitespace character begins a line comment:
///   `#` (YAML, Dockerfile, TOML, POSIX shell, PowerShell) or `//` (JSONC, e.g.
///   `.devcontainer/devcontainer.json`). Those cover every config format these
///   tests guard. A full-line `//` is unambiguous here — a real JSON/YAML value
///   line never *starts* with `//` (a `https://` URL has its key first).
/// - Inline trailing comments (`platforms: linux/amd64  # native`) are
///   deliberately *preserved*: that line is live config, and stripping the tail
///   would risk corrupting a token the assertion looks for.
/// - It does not understand block comments (`<# #>`, `/* */`), heredocs, or
///   strings that merely start with the comment marker. None occur in the
///   guarded config files; keep it that way rather than growing a parser here.
///
/// Use this for **presence** assertions on config files. Do *not* pre-strip
/// content feeding an **absence** assertion (`assert!(!c.contains("| head"))`):
/// there, a commented occurrence is still a real occurrence you want to flag,
/// and stripping would weaken the guard.
pub fn strip_comment_lines(content: &str) -> String {
    content
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with('#') && !trimmed.starts_with("//")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Read a config file and return only its *live* (comment-stripped) lines.
///
/// Equivalent to `strip_comment_lines(&read_file(path))`; the shorthand makes
/// "read this config as the live view a drift guard should assert against" the
/// obvious one-liner at each call site. See [`strip_comment_lines`] for the full
/// rationale and the presence-vs-absence caveat.
pub fn read_live_file(path: &Path) -> String {
    strip_comment_lines(&read_file(path))
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
