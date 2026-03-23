// CI Configuration Tests
//
// Data-driven tests to validate CI/CD configuration consistency and catch
// common configuration errors before they cause CI failures.
//
// These tests were created to prevent recurrence of actual CI issues:
//   1. MSRV inconsistency across configuration files
//   2. Workflow files with syntax errors or misconfigurations
//   3. Missing required CI validation workflows

#![cfg(test)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{read_file, repo_root};

/// Check whether `cargo-deny` is installed by running `cargo deny --version`.
/// Returns `true` when the subcommand is available, `false` otherwise.
fn cargo_deny_available() -> bool {
    Command::new("cargo")
        .args(["deny", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run `cargo deny <deny_args>` and return `(success, combined_output)`.
/// Returns `None` (and prints a skip message) when cargo-deny is not installed.
fn run_cargo_deny(deny_args: &[&str]) -> Option<(bool, String)> {
    if !cargo_deny_available() {
        eprintln!(
            "Skipping: cargo-deny is not installed.\n\
             Install with: cargo install cargo-deny"
        );
        return None;
    }

    let mut args = vec!["deny"];
    args.extend_from_slice(deny_args);

    let output = Command::new("cargo")
        .args(&args)
        .current_dir(repo_root())
        .output()
        .expect("failed to execute cargo deny");

    let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    Some((output.status.success(), combined))
}

/// Build and run a nested `cargo check` for one optional feature in an isolated
/// target directory with sanitizer-related environment variables removed.
///
/// This avoids false negatives in sanitizer jobs where nested Cargo invocations
/// can conflict with instrumentation flags and cached artifacts.
fn run_isolated_feature_check(feature: &str) -> (bool, String) {
    let root = repo_root();
    let target_dir = root
        .join("target")
        .join("ci-config-feature-check")
        .join(feature.replace([',', ' '], "_"));

    let output = Command::new("cargo")
        .args(["check", "--features", feature, "--locked"])
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTDOCFLAGS")
        .env_remove("ASAN_OPTIONS")
        .env_remove("LSAN_OPTIONS")
        .env_remove("UBSAN_OPTIONS")
        .env_remove("TSAN_OPTIONS")
        .env_remove("MIRIFLAGS")
        .output()
        .unwrap_or_else(|e| panic!("Failed to run cargo check for feature `{feature}`: {e}"));

    let mut combined = String::new();
    combined.push_str(&format!(
        "command: cargo check --features {feature} --locked\n"
    ));
    combined.push_str(&format!("CARGO_TARGET_DIR: {}\n\n", target_dir.display()));
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    combined.push_str(&String::from_utf8_lossy(&output.stderr));

    (output.status.success(), combined)
}

fn parse_github_slug_from_remote_url(remote_url: &str) -> Option<(String, String)> {
    let trimmed = remote_url.trim().trim_end_matches(".git");
    let slug = if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://github.com/") {
        rest
    } else {
        return None;
    };

    let (owner, repo) = slug.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some((owner.to_lowercase(), repo.to_lowercase()))
}

fn expected_first_party_image_refs() -> std::collections::HashSet<String> {
    let mut refs = std::collections::HashSet::from([
        "ghcr.io/ambiguous-interactive/signal-fish-server".to_string(),
        "ambiguous-interactive/signal-fish-server".to_string(),
        // Legacy namespace retained to avoid false positives during migration.
        "ghcr.io/ambiguousinteractive/signal-fish-server".to_string(),
        "ambiguousinteractive/signal-fish-server".to_string(),
    ]);

    let remote_url = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo_root())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).to_string());

    if let Some(remote_url) = remote_url {
        if let Some((owner, repo)) = parse_github_slug_from_remote_url(&remote_url) {
            refs.insert(format!("ghcr.io/{owner}/{repo}"));
            refs.insert(format!("{owner}/{repo}"));
        }
    }

    refs
}

/// Extract Shields.io URLs from text content with their 1-based line numbers.
fn extract_shields_urls(content: &str) -> Vec<(usize, String)> {
    const SHIELDS_PREFIX: &str = "https://img.shields.io/";
    let mut urls = Vec::new();

    for (line_idx, line) in content.lines().enumerate() {
        let mut rest = line;
        while let Some(start) = rest.find(SHIELDS_PREFIX) {
            let candidate = &rest[start..];
            let end = candidate
                .find(|c: char| c == '"' || c == '\'' || c == ')' || c == '>' || c.is_whitespace())
                .unwrap_or(candidate.len());
            urls.push((line_idx + 1, candidate[..end].to_string()));
            rest = &candidate[end..];
        }
    }

    urls
}

/// Return true when a Shields URL has `style=for-the-badge` as a query parameter.
///
/// This mirrors scripts/check-readme-badges.sh semantics:
/// - style must be preceded by '?' or '&'
/// - style must be followed by '&', '#', or end-of-string
fn shields_url_has_for_the_badge_style(url: &str) -> bool {
    const STYLE_PARAM: &str = "style=for-the-badge";

    url.match_indices(STYLE_PARAM).any(|(idx, _)| {
        let has_valid_prefix = idx > 0 && matches!(url.as_bytes()[idx - 1], b'?' | b'&');
        let suffix_idx = idx + STYLE_PARAM.len();
        let has_valid_suffix =
            suffix_idx == url.len() || matches!(url.as_bytes()[suffix_idx], b'&' | b'#');
        has_valid_prefix && has_valid_suffix
    })
}

/// Collect README-style violations for Shields badge URLs that do not include
/// style=for-the-badge.
fn collect_shields_style_violations(file_name: &str, content: &str) -> Vec<String> {
    extract_shields_urls(content)
        .into_iter()
        .filter(|(_, url)| !shields_url_has_for_the_badge_style(url))
        .map(|(line_num, url)| format!("{file_name}:{line_num}: {url}"))
        .collect()
}

/// Write a temporary markdown file inside target/test-temp and return its path.
#[cfg(unix)]
fn write_temp_markdown_file(root: &Path, prefix: &str, content: &str) -> tempfile::NamedTempFile {
    use std::io::Write;
    let temp_dir = root.join("target").join("test-temp");
    fs::create_dir_all(&temp_dir).unwrap_or_else(|e| {
        panic!(
            "Failed to create temporary test directory {}: {e}",
            temp_dir.display()
        )
    });

    let mut temp_file = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(".md")
        .tempfile_in(&temp_dir)
        .unwrap_or_else(|e| {
            panic!(
                "Failed to create temporary markdown file in {}: {e}",
                temp_dir.display()
            )
        });

    temp_file.write_all(content.as_bytes()).unwrap_or_else(|e| {
        panic!(
            "Failed to write temporary markdown file {}: {e}",
            temp_file.path().display()
        )
    });

    temp_file
}

/// Extract the value of a TOML field like `rust-version = "1.88.0"`
fn extract_toml_version(content: &str, field: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(field) {
            let rest = rest.trim();
            if let Some(rest) = rest.strip_prefix('=') {
                return Some(rest.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// Extract the value of a YAML field like `channel = "1.88.0"`
fn extract_yaml_version(content: &str, field: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(field) {
            let rest = rest.trim();
            if let Some(rest) = rest.strip_prefix('=') {
                return Some(rest.trim().trim_matches('"').to_string());
            } else if let Some(rest) = rest.strip_prefix(':') {
                return Some(rest.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// Extract the `if:` condition of a job from workflow YAML content.
///
/// Searches for a job key at 2-space indentation (`  job_key:`) and then
/// looks for the `if:` field at 4-space indentation within that job block.
/// Returns `None` if the job or its `if:` field is not found.
fn extract_job_if_condition(content: &str, job_key: &str) -> Option<String> {
    let job_header = format!("  {job_key}:");
    let mut in_target_job = false;

    for line in content.lines() {
        if line.starts_with(&job_header) {
            in_target_job = true;
            continue;
        }

        if in_target_job {
            let trimmed = line.trim();

            // If we hit another job definition (2-space indent, not a sub-key),
            // we've left the target job block
            if line.starts_with("  ") && !line.starts_with("    ") && !trimmed.is_empty() {
                return None;
            }

            // Look for "    if: <condition>" within the job block
            if let Some(rest) = line.strip_prefix("    if:") {
                return Some(rest.trim().to_string());
            }
        }
    }

    None
}

/// Extract the display name of a job from workflow YAML content.
///
/// Searches for a job key at 2-space indentation (`  job_key:`) and then
/// looks for the `name:` field at 4-space indentation within that job block.
/// Returns `None` if the job or its name field is not found.
fn extract_job_display_name(content: &str, job_key: &str) -> Option<String> {
    let job_header = format!("  {job_key}:");
    let mut in_target_job = false;

    for line in content.lines() {
        if line.starts_with(&job_header) {
            in_target_job = true;
            continue;
        }

        if in_target_job {
            let trimmed = line.trim();

            // If we hit another job definition (2-space indent, not a sub-key),
            // we've left the target job block
            if line.starts_with("  ") && !line.starts_with("    ") && !trimmed.is_empty() {
                return None;
            }

            // Look for "    name: Display Name" within the job block
            if let Some(rest) = line.strip_prefix("    name:") {
                return Some(rest.trim().trim_matches('"').to_string());
            }
        }
    }

    None
}

/// Extract the `audit:` job section from CI workflow YAML content.
///
/// Finds the `  audit:` job header and collects all lines belonging to that job
/// block (4+-space-indented lines and blank lines) into a single string.
fn extract_audit_section(ci_content: &str) -> String {
    ci_content
        .lines()
        .skip_while(|line| !line.starts_with("  audit:"))
        .take_while(|line| {
            line.starts_with("  audit:") || line.starts_with("    ") || line.trim().is_empty()
        })
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Extract the `sbom:` job section from CI workflow YAML content.
///
/// Finds the `  sbom:` job header and collects all lines belonging to that job
/// block (4+-space-indented lines and blank lines) into a single string.
fn extract_sbom_section(ci_content: &str) -> String {
    ci_content
        .lines()
        .skip_while(|line| !line.starts_with("  sbom:"))
        .take_while(|line| {
            line.starts_with("  sbom:") || line.starts_with("    ") || line.trim().is_empty()
        })
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Validate that a workflow file contains all required jobs with the correct
/// display names.
///
/// Uses `extract_job_display_name()` for scoped name matching within each job
/// block, preventing false positives where a display name appears elsewhere
/// in the file (e.g., in comments or unrelated steps).
///
/// Panics with a detailed diagnostic message if any required jobs are missing
/// or have mismatched display names.
fn validate_workflow_has_required_jobs(
    workflow_path: &Path,
    required_jobs: &[(&str, &str, &str)],
    workflow_description: &str,
) {
    let content = read_file(workflow_path);

    let mut missing_jobs = Vec::new();
    let mut found_jobs = Vec::new();

    for (job_key, display_name, description) in required_jobs {
        // Look for "job-key:" pattern at 2-space indentation (top-level job definition)
        let job_pattern = format!("  {job_key}:");
        if content.contains(&job_pattern) {
            // Use scoped extraction to verify the display name belongs to this job block
            let actual_name = extract_job_display_name(&content, job_key);
            match actual_name {
                Some(ref name) if name == display_name => {
                    found_jobs.push(format!(
                        "  + {job_key} (name: \"{display_name}\", {description})"
                    ));
                }
                Some(ref wrong_name) => {
                    missing_jobs.push(format!(
                        "  x {job_key}: job exists but display name \"{wrong_name}\" does not match \
                         expected \"{display_name}\".\n\
                         Expected line: `    name: {display_name}`\n\
                         This will change the GitHub check name, which breaks branch protection.\n\
                         To fix: Update the job's `name:` field to \"{display_name}\""
                    ));
                }
                None => {
                    missing_jobs.push(format!(
                        "  x {job_key}: job exists but has no `name:` field.\n\
                         Expected line: `    name: {display_name}`\n\
                         This will change the GitHub check name, which breaks branch protection.\n\
                         To fix: Add `name: {display_name}` to the job definition"
                    ));
                }
            }
        } else {
            missing_jobs.push(format!("  x {job_key} ({display_name} - {description})"));
        }
    }

    if !missing_jobs.is_empty() {
        panic!(
            "{workflow_description} workflow is missing required jobs or display names:\n\n\
             Missing:\n{}\n\n\
             Found:\n{}\n\n\
             File: {}\n\n\
             These jobs are critical for CI/CD validation.\n\
             To fix:\n\
             1. Review git history to see when the job was removed or renamed\n\
             2. Restore the job definition in the jobs: section\n\
             3. Ensure the job key AND name: field match exactly (case-sensitive)\n\
             4. Update branch protection settings if a rename was intentional",
            missing_jobs.join("\n"),
            found_jobs.join("\n"),
            workflow_path.display()
        );
    }
}

// ============================================================================
// Required Check Naming Contract
// ============================================================================
//
// These constants define the exact GitHub check names that are required for
// branch protection on `main`. Workflow and job names are treated as API
// surface — any rename requires a synchronized update to:
//   1. The workflow/job definition in .github/workflows/
//   2. These constants and tests
//   3. Branch protection settings in GitHub
//   4. CI/CD documentation (docs/ci-cd-testing.md, docs/ci-cd-testing-summary.md)
//
// GitHub constructs check names as: "{workflow name} / {job display name}"
//
// Current required checks (Phase 1-2):
//   - CI / Lint (ubuntu-latest)
//   - CI / Lint (windows-latest)
//   - CI / Lint (macos-latest)
//   - CI / Nextest (ubuntu-latest)
//   - CI / Nextest (windows-latest)
//   - CI / Nextest (macos-latest)
//   - CI / Dependency Audit
//   - CI / MSRV Verification
//   - CI / Docker Build
//   - CI / Coverage (llvm-cov)
//   - CI / Panic Policy
//   - CI / SBOM (CycloneDX)
//   - Documentation Validation / Rustdoc Validation
//   - Documentation Validation / Documentation Tests
//   - Documentation Validation / Markdown Code Validation
//   - Documentation Validation / Documentation Link Check

/// Workflow file -> workflow display name mapping for **branch-protection-relevant**
/// workflows only.
///
/// Unlike `REQUIRED_WORKFLOW_FILES` (which lists all workflows that must exist for
/// CI hygiene), this constant only covers workflows whose jobs produce GitHub check
/// names that are configured as required status checks in branch protection rules.
/// The check name format is `"{workflow display name} / {job display name}"`.
const REQUIRED_WORKFLOW_NAMES: &[(&str, &str)] = &[
    ("ci.yml", "CI"),
    ("doc-validation.yml", "Documentation Validation"),
];

/// Required CI workflow jobs: (job_key, display_name, description)
const REQUIRED_CI_JOBS: &[(&str, &str, &str)] = &[
    (
        "lint",
        "Lint (${{ matrix.os }})",
        "Cross-OS code formatting and linting",
    ),
    (
        "nextest",
        "Nextest (${{ matrix.os }})",
        "Cross-OS test execution via cargo-nextest",
    ),
    (
        "deny",
        "Dependency Audit",
        "Security audits and license checks",
    ),
    (
        "audit",
        "Audit (cargo-audit)",
        "Second-opinion vulnerability scan via cargo-audit",
    ),
    (
        "msrv",
        "MSRV Verification",
        "Minimum Supported Rust Version verification",
    ),
    (
        "docker",
        "Docker Build",
        "Docker image build and smoke test",
    ),
    (
        "coverage",
        "Coverage (llvm-cov)",
        "Linux code coverage gate",
    ),
    (
        "panic-policy",
        "Panic Policy",
        "Zero-panic production code enforcement",
    ),
    (
        "sbom",
        "SBOM (CycloneDX)",
        "Software Bill of Materials generation",
    ),
];

/// Required doc-validation workflow jobs: (job_key, display_name, description)
///
/// Note: `doc-validation.yml` defines 6 jobs total, but only these 4 are listed here.
/// The excluded jobs are:
///   - `shellcheck-workflow` ("Shellcheck Workflow Scripts") — auxiliary static analysis
///     of inline shell scripts; not a documentation quality gate
///   - `inline-code-references` ("Validate Inline Code References") — placeholder job
///     for future inline code reference validation; not required for branch protection
///
/// These auxiliary checks improve workflow quality but are not required for branch
/// protection on `main`.
const REQUIRED_DOC_VALIDATION_JOBS: &[(&str, &str, &str)] = &[
    (
        "rustdoc",
        "Rustdoc Validation",
        "Rustdoc build with strict warnings",
    ),
    ("doc-tests", "Documentation Tests", "Cargo doc-tests"),
    (
        "markdown-code-samples",
        "Markdown Code Validation",
        "Validates code blocks in markdown",
    ),
    (
        "link-check",
        "Documentation Link Check",
        "Internal documentation link checking",
    ),
];

/// Matrix expression placeholder used in GitHub Actions job display names.
/// When a job name contains this placeholder, the job produces one check per
/// matrix value rather than a single check.
const MATRIX_OS_PLACEHOLDER: &str = "${{ matrix.os }}";

/// OS values that `matrix.os` expands to in ci.yml.
/// This must match the `strategy.matrix.os` list in the workflow file.
const MATRIX_OS_VALUES: &[&str] = &["ubuntu-latest", "windows-latest", "macos-latest"];

/// Expand a job display name template that may contain `${{ matrix.os }}` into
/// concrete check names. If the template contains the placeholder, one name is
/// produced per OS value; otherwise the original name is returned as-is.
fn expand_matrix_display_name(workflow_name: &str, display_name: &str) -> Vec<String> {
    if display_name.contains(MATRIX_OS_PLACEHOLDER) {
        MATRIX_OS_VALUES
            .iter()
            .map(|os| {
                let expanded = display_name.replace(MATRIX_OS_PLACEHOLDER, os);
                format!("{workflow_name} / {expanded}")
            })
            .collect()
    } else {
        vec![format!("{workflow_name} / {display_name}")]
    }
}

/// Check whether a concrete job display name (e.g. `Lint (ubuntu-latest)`)
/// matches a template display name that may contain matrix placeholders
/// (e.g. `Lint (${{ matrix.os }})`).
fn display_name_matches_template(concrete: &str, template: &str) -> bool {
    if !template.contains(MATRIX_OS_PLACEHOLDER) {
        return concrete == template;
    }
    MATRIX_OS_VALUES.iter().any(|os| {
        let expanded = template.replace(MATRIX_OS_PLACEHOLDER, os);
        concrete == expanded
    })
}

/// All required GitHub check names for branch protection.
/// Format: "{workflow_name} / {job_display_name}"
const REQUIRED_CHECK_NAMES: &[&str] = &[
    "CI / Lint (ubuntu-latest)",
    "CI / Lint (windows-latest)",
    "CI / Lint (macos-latest)",
    "CI / Nextest (ubuntu-latest)",
    "CI / Nextest (windows-latest)",
    "CI / Nextest (macos-latest)",
    "CI / Dependency Audit",
    "CI / Audit (cargo-audit)",
    "CI / MSRV Verification",
    "CI / Docker Build",
    "CI / Coverage (llvm-cov)",
    "CI / Panic Policy",
    "CI / SBOM (CycloneDX)",
    "Documentation Validation / Rustdoc Validation",
    "Documentation Validation / Documentation Tests",
    "Documentation Validation / Markdown Code Validation",
    "Documentation Validation / Documentation Link Check",
];

/// All workflow files that must exist for CI hygiene.
///
/// Unlike `REQUIRED_WORKFLOW_NAMES` (which only lists workflows whose jobs feed
/// branch protection checks), this constant lists **every** workflow file that
/// the repository depends on for quality assurance.
///
/// Note: `docs-deploy.yml` exists in `.github/workflows/` but is intentionally
/// excluded here because it is a deployment workflow (GitHub Pages publishing),
/// not a quality gate. Its presence is validated indirectly by
/// `test_docs_deploy_requirements_file_exists`.
///
/// (filename, description)
const REQUIRED_WORKFLOW_FILES: &[(&str, &str)] = &[
    (
        "ci.yml",
        "Main CI pipeline (lint, nextest, deny, audit, MSRV, Docker, coverage, panic-policy, SBOM)",
    ),
    (
        "doc-validation.yml",
        "Documentation validation (rustdoc, doc-tests, markdown, links)",
    ),
    ("yaml-lint.yml", "YAML syntax validation"),
    ("actionlint.yml", "GitHub Actions syntax validation"),
    (
        "unused-deps.yml",
        "Unused dependency detection (cargo-machete/cargo-udeps)",
    ),
    ("workflow-hygiene.yml", "Workflow configuration validation"),
    ("markdownlint.yml", "Markdown formatting validation"),
    ("spellcheck.yml", "Spell checking (typos)"),
    ("link-check.yml", "External link validation (lychee)"),
    (
        "release.yml",
        "Release automation (crates.io + GitHub release)",
    ),
    (
        "ci-safety.yml",
        "Advanced safety analysis (Miri, AddressSanitizer — staged)",
    ),
    (
        "llm-file-sizes.yml",
        "LLM skill file size enforcement (max 300 lines per .llm/ file)",
    ),
    (
        "docker-publish.yml",
        "Docker image publish to GHCR (owner/repo-derived image name)",
    ),
];

#[test]
fn test_msrv_consistency_across_config_files() {
    // This test prevents the MSRV inconsistency issue that was fixed in commit d9eac0f
    // All configuration files must use the same Rust version as defined in Cargo.toml

    let root = repo_root();

    // Extract MSRV from Cargo.toml (single source of truth)
    let cargo_toml = root.join("Cargo.toml");
    let cargo_content = read_file(&cargo_toml);
    let msrv = extract_toml_version(&cargo_content, "rust-version")
        .expect("Could not extract rust-version from Cargo.toml");

    assert!(
        !msrv.is_empty(),
        "MSRV must be set in Cargo.toml rust-version field"
    );

    // Validate rust-toolchain.toml
    let rust_toolchain = root.join("rust-toolchain.toml");
    if rust_toolchain.exists() {
        let toolchain_content = read_file(&rust_toolchain);
        let toolchain_version = extract_yaml_version(&toolchain_content, "channel")
            .expect("Could not extract channel from rust-toolchain.toml");

        assert_eq!(
            toolchain_version, msrv,
            "rust-toolchain.toml channel must match Cargo.toml rust-version.\n\
             Expected: {msrv}\n\
             Found: {toolchain_version}\n\
             Fix: Update rust-toolchain.toml to use channel = \"{msrv}\""
        );
    }

    // Validate clippy.toml
    let clippy_toml = root.join("clippy.toml");
    if clippy_toml.exists() {
        let clippy_content = read_file(&clippy_toml);
        if let Some(clippy_msrv) = extract_toml_version(&clippy_content, "msrv") {
            assert_eq!(
                clippy_msrv, msrv,
                "clippy.toml msrv must match Cargo.toml rust-version.\n\
                 Expected: {msrv}\n\
                 Found: {clippy_msrv}\n\
                 Fix: Update clippy.toml to use msrv = \"{msrv}\""
            );
        }
    }

    // Validate Dockerfile
    let dockerfile = root.join("Dockerfile");
    if dockerfile.exists() {
        let dockerfile_content = read_file(&dockerfile);

        // Look for FROM rust:X.Y line
        let rust_version_in_dockerfile = dockerfile_content
            .lines()
            .find(|line| line.trim().starts_with("FROM rust:"))
            .and_then(|line| {
                // Extract version from "FROM rust:1.88-bookworm" or "FROM rust:1.88"
                line.split(':')
                    .nth(1)
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.split('-').next())
                    .map(String::from)
            });

        if let Some(dockerfile_version) = rust_version_in_dockerfile {
            // Docker images may use shortened versions (1.88 instead of 1.88.0)
            // Check if dockerfile version matches MSRV or is a valid prefix
            let msrv_major_minor = to_major_minor(&msrv);
            let version_matches =
                dockerfile_version == msrv || dockerfile_version == msrv_major_minor;

            assert!(
                version_matches,
                "Dockerfile Rust version must match Cargo.toml rust-version.\n\
                 Expected: FROM rust:{msrv} or FROM rust:{msrv_major_minor}\n\
                 Found: FROM rust:{dockerfile_version}\n\
                 Fix: Update Dockerfile to use FROM rust:{msrv}-bookworm or FROM rust:{msrv_major_minor}-bookworm"
            );
        }
    }
}

#[test]
fn test_msrv_version_normalization_logic() {
    // This test validates that our version comparison logic correctly handles
    // both full semver (1.88.0) and Docker's shortened format (1.88).
    //
    // Background: Docker images use "rust:1.88" while Cargo.toml uses "1.88.0".
    // The CI/local scripts must normalize both formats to major.minor for comparison.
    //
    // This test prevents regression of the bug where CI compared "1.88" != "1.88.0"
    // and failed even though the versions were semantically identical.

    // Test case 1: Full semver version (Cargo.toml format)
    let msrv_full = "1.88.0";
    let msrv_major_minor = to_major_minor(msrv_full);
    assert_eq!(msrv_major_minor, "1.88");

    // Test case 2: Docker shortened version should match normalized MSRV
    let dockerfile_version = "1.88";
    assert_eq!(
        dockerfile_version, msrv_major_minor,
        "Normalized MSRV should match Docker version format"
    );

    // Test case 3: Verify that different major.minor versions correctly fail
    let wrong_version = "1.87";
    assert_ne!(
        wrong_version, msrv_major_minor,
        "Different versions should not match"
    );

    // Test case 4: Patch version differences in MSRV shouldn't matter for Docker comparison
    let msrv_different_patch = "1.88.1";
    let normalized_patch = to_major_minor(msrv_different_patch);
    assert_eq!(
        normalized_patch, dockerfile_version,
        "Patch version should be ignored when comparing to Docker format"
    );

    // Test case 5: Verify edge cases with single-digit patch versions
    let msrv_zero_patch = "1.88.0";
    let msrv_nonzero_patch = "1.88.5";
    let norm1 = to_major_minor(msrv_zero_patch);
    let norm2 = to_major_minor(msrv_nonzero_patch);
    assert_eq!(
        norm1, norm2,
        "Both should normalize to same major.minor regardless of patch"
    );
}

#[test]
fn test_ci_workflow_msrv_normalization() {
    // This test validates that the CI workflow's MSRV verification logic
    // correctly normalizes versions before comparison.
    //
    // It simulates the exact bash commands used in .github/workflows/ci.yml
    // to ensure they produce the expected results.

    let root = repo_root();
    let ci_workflow = root.join(".github/workflows/ci.yml");
    let content = read_file(&ci_workflow);

    // Verify that the CI workflow contains the normalization logic
    assert!(
        content.contains("MSRV_SHORT=$(echo \"$MSRV\" | sed -E 's/([0-9]+\\.[0-9]+).*/\\1/')"),
        "CI workflow must normalize MSRV to major.minor format for Dockerfile comparison.\n\
         This prevents false failures when comparing 1.88.0 (Cargo.toml) to 1.88 (Dockerfile)."
    );

    // Verify the comparison uses the normalized version
    assert!(
        content.contains("if [ \"$DOCKERFILE_RUST\" != \"$MSRV_SHORT\" ]"),
        "CI workflow must compare Dockerfile version against normalized MSRV_SHORT, not full MSRV.\n\
         Using full MSRV causes spurious failures (1.88 != 1.88.0)."
    );

    // Verify there's a comment explaining the normalization
    assert!(
        content.contains("Normalize MSRV to major.minor")
            || content.contains("handles both 1.88 and 1.88.0 formats"),
        "CI workflow should document why version normalization is needed"
    );
}

#[test]
fn test_msrv_script_consistency_with_ci() {
    // This test ensures that the local MSRV check script and the CI workflow
    // use the same logic for version comparison.
    //
    // Both must normalize versions to major.minor format to avoid inconsistent
    // behavior between local checks and CI validation.

    let root = repo_root();
    let script = root.join("scripts/check-msrv-consistency.sh");
    let ci_workflow = root.join(".github/workflows/ci.yml");

    if !script.exists() {
        panic!(
            "MSRV consistency check script not found at {}",
            script.display()
        );
    }

    let script_content = read_file(&script);
    let ci_content = read_file(&ci_workflow);

    // Both should normalize MSRV to major.minor for Dockerfile comparison
    let normalization_pattern = "sed -E 's/([0-9]+\\.[0-9]+).*/\\1/'";

    assert!(
        script_content.contains(normalization_pattern),
        "Local script must normalize MSRV version (found in check-msrv-consistency.sh)"
    );

    assert!(
        ci_content.contains(normalization_pattern),
        "CI workflow must normalize MSRV version (found in ci.yml)"
    );

    // Verify both use MSRV_SHORT variable for comparison
    assert!(
        script_content.contains("MSRV_SHORT"),
        "Local script should use MSRV_SHORT variable for normalized version"
    );

    assert!(
        ci_content.contains("MSRV_SHORT"),
        "CI workflow should use MSRV_SHORT variable for normalized version"
    );
}

#[test]
fn test_required_ci_workflows_exist() {
    // This test ensures critical CI validation workflows are present
    // Prevents accidental deletion of important CI checks

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let mut missing_workflows = Vec::new();

    for (workflow_file, description) in REQUIRED_WORKFLOW_FILES {
        let workflow_path = workflows_dir.join(workflow_file);
        if !workflow_path.exists() {
            missing_workflows.push(format!(
                "  - {} ({})\n    Expected at: {}",
                workflow_file,
                description,
                workflow_path.display()
            ));
        }
    }

    if !missing_workflows.is_empty() {
        panic!(
            "Required workflows are missing:\n\n{}\n\n\
             These workflows are required for CI/CD hygiene.\n\
             To fix:\n\
             1. Restore missing workflow files from git history\n\
             2. Or create new workflow files following project patterns\n\
             3. Ensure all workflows are in .github/workflows/",
            missing_workflows.join("\n")
        );
    }
}

#[test]
fn test_ci_workflow_has_required_jobs() {
    // This test validates that the main CI workflow has critical jobs
    // Prevents accidental removal of important checks

    let root = repo_root();
    let ci_workflow = root.join(".github/workflows/ci.yml");
    validate_workflow_has_required_jobs(&ci_workflow, REQUIRED_CI_JOBS, "CI");
}

#[test]
fn test_ci_workflow_matrix_os_values_match_constant() {
    // Validates that the MATRIX_OS_VALUES constant matches the actual
    // strategy.matrix.os lists in ci.yml. If these drift apart, the
    // bidirectional consistency test will silently produce wrong check names.
    //
    // Multiple jobs (lint, nextest) use matrix.os, so we validate ALL
    // `os:` lines at 8-space indent to ensure consistency across jobs.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    // Collect ALL "os: [...]" lines from matrix sections (8-space indent).
    // Multiple jobs (lint, nextest) each have their own matrix.os list.
    let os_lines: Vec<&str> = ci_content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("os:") && line.starts_with("        ")
        })
        .collect();

    assert!(
        !os_lines.is_empty(),
        "Could not find any matrix os: lines in ci.yml.\n\
         Expected lines like '        os: [ubuntu-latest, windows-latest, macos-latest]'"
    );

    for (i, os_line) in os_lines.iter().enumerate() {
        // Parse the OS values from the YAML list: "os: [a, b, c]"
        let list_str = os_line
            .trim()
            .strip_prefix("os:")
            .expect("os: prefix missing")
            .trim();
        let inner = list_str
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or_else(|| {
                panic!(
                    "Could not parse matrix os list #{} from ci.yml.\n\
                     Found: {os_line}\n\
                     Expected format: os: [ubuntu-latest, windows-latest, macos-latest]",
                    i + 1
                )
            });

        let yaml_os_values: Vec<&str> = inner.split(',').map(|s| s.trim()).collect();

        assert_eq!(
            yaml_os_values.len(),
            MATRIX_OS_VALUES.len(),
            "MATRIX_OS_VALUES has {} entries but ci.yml matrix.os line #{} has {} entries.\n\
             MATRIX_OS_VALUES: {:?}\n\
             ci.yml matrix.os: {:?}\n\
             To fix: Update MATRIX_OS_VALUES or the matrix in ci.yml so they match.",
            MATRIX_OS_VALUES.len(),
            i + 1,
            yaml_os_values.len(),
            MATRIX_OS_VALUES,
            yaml_os_values
        );

        for os in &yaml_os_values {
            assert!(
                MATRIX_OS_VALUES.contains(os),
                "ci.yml matrix.os line #{} contains \"{os}\" but MATRIX_OS_VALUES does not.\n\
                 To fix: Add \"{os}\" to MATRIX_OS_VALUES.",
                i + 1
            );
        }

        for os in MATRIX_OS_VALUES {
            assert!(
                yaml_os_values.contains(os),
                "MATRIX_OS_VALUES contains \"{os}\" but ci.yml matrix.os line #{} does not.\n\
                 To fix: Either add \"{os}\" to the matrix in ci.yml or remove it from MATRIX_OS_VALUES.",
                i + 1
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests for expand_matrix_display_name and display_name_matches_template
// ---------------------------------------------------------------------------

#[test]
fn test_expand_matrix_display_name_with_matrix_placeholder() {
    let results = expand_matrix_display_name("CI", "Lint (${{ matrix.os }})");
    assert_eq!(
        results.len(),
        MATRIX_OS_VALUES.len(),
        "expand_matrix_display_name should produce one entry per MATRIX_OS_VALUES element.\n\
         Expected {} entries, got {}.",
        MATRIX_OS_VALUES.len(),
        results.len()
    );
    for os in MATRIX_OS_VALUES {
        let expected = format!("CI / Lint ({os})");
        assert!(
            results.contains(&expected),
            "Expected expanded names to contain \"{expected}\" but got: {results:?}"
        );
    }
}

#[test]
fn test_expand_matrix_display_name_without_placeholder() {
    let results = expand_matrix_display_name("CI", "Test");
    assert_eq!(
        results,
        vec!["CI / Test"],
        "When the display name has no matrix placeholder, expand_matrix_display_name \
         should return a single entry with the format '{{workflow}} / {{display_name}}'."
    );
}

#[test]
fn test_expand_matrix_display_name_uses_matrix_os_values() {
    let results = expand_matrix_display_name("W", "${{ matrix.os }}");
    let expected: Vec<String> = MATRIX_OS_VALUES
        .iter()
        .map(|os| format!("W / {os}"))
        .collect();
    assert_eq!(
        results, expected,
        "expand_matrix_display_name should use exactly the OS values from MATRIX_OS_VALUES.\n\
         Expected: {expected:?}\n\
         Got:      {results:?}"
    );
}

#[test]
fn test_display_name_matches_template_ubuntu() {
    assert!(
        display_name_matches_template("Lint (ubuntu-latest)", "Lint (${{ matrix.os }})"),
        "\"Lint (ubuntu-latest)\" should match template \"Lint (${{{{ matrix.os }}}})\""
    );
}

#[test]
fn test_display_name_matches_template_windows() {
    assert!(
        display_name_matches_template("Lint (windows-latest)", "Lint (${{ matrix.os }})"),
        "\"Lint (windows-latest)\" should match template \"Lint (${{{{ matrix.os }}}})\""
    );
}

#[test]
fn test_display_name_matches_template_macos() {
    assert!(
        display_name_matches_template("Lint (macos-latest)", "Lint (${{ matrix.os }})"),
        "\"Lint (macos-latest)\" should match template \"Lint (${{{{ matrix.os }}}})\""
    );
}

#[test]
fn test_display_name_matches_template_no_match_different_prefix() {
    assert!(
        !display_name_matches_template("Check & Lint", "Lint (${{ matrix.os }})"),
        "\"Check & Lint\" should NOT match template \"Lint (${{{{ matrix.os }}}})\""
    );
}

#[test]
fn test_display_name_matches_template_non_matrix_exact_match() {
    assert!(
        display_name_matches_template("Test", "Test"),
        "A non-matrix template should match itself exactly"
    );
}

#[test]
fn test_display_name_matches_template_non_matrix_no_match() {
    assert!(
        !display_name_matches_template("Test", "Lint (${{ matrix.os }})"),
        "\"Test\" should NOT match template \"Lint (${{{{ matrix.os }}}})\""
    );
}

#[test]
fn test_workflow_files_are_valid_yaml() {
    // This test catches basic YAML syntax errors in workflow files
    // Prevents pushing broken workflows that cause CI to fail

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/\n\
         Expected workflow files (*.yml or *.yaml) to exist in this directory."
    );

    let mut errors = Vec::new();

    for entry in workflow_files {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();

        // Basic YAML validation checks
        // Note: This is not a full YAML parser, but catches common errors

        // Check for balanced quotes, but only on YAML-level lines (not inside
        // multiline scalar blocks). Shell scripts embedded via `run: |` and
        // folded scalars like `args: >-` can legitimately have odd quote
        // counts (AWK programs, glob patterns, etc.), so we skip lines
        // inside any YAML multiline scalar block.
        let mut single_quotes = 0;
        let mut double_quotes = 0;
        let mut in_multiline_block = false;
        let mut block_indent = 0;

        for line in content.lines() {
            let stripped = line.trim();
            let indent = line.len() - line.trim_start().len();

            // Detect start of any YAML multiline scalar block.
            // Matches patterns like: "key: |", "key: >-", "key: |+", etc.
            // The scalar indicator (|, >, |-, >-, |+, >+) after a colon
            // signals that subsequent indented lines are scalar content.
            if stripped.contains(": |") || stripped.contains(": >") {
                // Verify this looks like a YAML key: value with a block scalar indicator
                // (not just any line that happens to contain ": |")
                let after_colon = stripped
                    .split_once(": ")
                    .map(|(_, rest)| rest.trim())
                    .unwrap_or("");
                if after_colon == "|"
                    || after_colon == "|-"
                    || after_colon == "|+"
                    || after_colon == ">"
                    || after_colon == ">-"
                    || after_colon == ">+"
                {
                    in_multiline_block = true;
                    block_indent = indent;
                    continue;
                }
            }

            // Detect end of multiline block (line at same or lesser indent, non-empty)
            if in_multiline_block && !stripped.is_empty() && indent <= block_indent {
                in_multiline_block = false;
            }

            // Only count quotes on YAML-level lines, not multiline scalar content
            if !in_multiline_block {
                single_quotes += line.matches('\'').count();
                double_quotes += line.matches('"').count();
            }
        }

        if single_quotes % 2 != 0 {
            errors.push(format!(
                "{filename}: Unbalanced single quotes in YAML lines (found {single_quotes} quotes)\n  \
                 Check for missing closing quotes in strings (shell script blocks excluded)"
            ));
        }

        if double_quotes % 2 != 0 {
            errors.push(format!(
                "{filename}: Unbalanced double quotes in YAML lines (found {double_quotes} quotes)\n  \
                 Check for missing closing quotes in strings (shell script blocks excluded)"
            ));
        }

        // Check for required GitHub Actions fields
        let mut missing_fields = Vec::new();

        if !content.contains("name:") {
            missing_fields.push("name:");
        }
        if !content.contains("on:") && !content.contains("'on':") {
            missing_fields.push("on:");
        }
        if !content.contains("jobs:") {
            missing_fields.push("jobs:");
        }

        if !missing_fields.is_empty() {
            errors.push(format!(
                "{}: Missing required fields: {}\n  \
                 GitHub Actions workflows must have: name, on, jobs",
                filename,
                missing_fields.join(", ")
            ));
        }
    }

    if !errors.is_empty() {
        panic!(
            "Workflow files have YAML validation errors:\n\n{}\n\n\
             To fix:\n\
             1. Use a YAML validator/linter (yamllint, prettier, or IDE plugin)\n\
             2. Check for missing quotes, colons, or indentation errors\n\
             3. Ensure all required fields (name, on, jobs) are present\n\
             4. Verify quotes are balanced (each opening quote has a closing quote)\n\n\
             Common issues:\n\
             - Missing closing quote: name: \"My Workflow\n\
             - Missing colon: name My Workflow\n\
             - Wrong indentation: jobs should be at root level, not nested",
            errors.join("\n")
        );
    }
}

#[test]
fn test_workflow_files_end_with_newline() {
    // Regression guard for yamllint [new-line-at-end-of-file] failures.
    // Missing trailing newline in workflow files causes CI YAML lint to fail.

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/"
    );

    let mut violations = Vec::new();

    for entry in workflow_files {
        let path = entry.path();
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("Failed to read workflow file {}: {}", path.display(), e));

        if bytes.is_empty() {
            violations.push(format!("{}: file is empty", path.display()));
            continue;
        }

        if !bytes.ends_with(b"\n") {
            violations.push(format!(
                "{}: missing trailing newline at end of file",
                path.display()
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "Workflow files must end with a newline to satisfy YAML lint:\n\n{}\n\n\
         Fix: add a trailing newline to each listed file.",
        violations.join("\n")
    );
}

#[test]
fn test_no_language_specific_cache_mismatch() {
    // This test prevents the Python cache on Rust project issue (yaml-lint.yml)
    // Ensures workflow caching strategies match project type

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    // Determine project type
    let is_rust_project = root.join("Cargo.toml").exists();
    // Also detect requirements-*.txt variants (e.g., requirements-docs.txt for MkDocs)
    let has_any_requirements_txt = root
        .read_dir()
        .map(|entries| {
            entries.filter_map(Result::ok).any(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with("requirements") && name.ends_with(".txt")
            })
        })
        .unwrap_or(false);
    let is_python_project = root.join("requirements.txt").exists()
        || root.join("Pipfile").exists()
        || root.join("pyproject.toml").exists()
        || has_any_requirements_txt;
    let is_node_project = root.join("package.json").exists();

    for entry in collect_workflow_files(&workflows_dir) {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();

        // Check for Python caching on non-Python projects
        if !is_python_project
            && is_rust_project
            && (content.contains("cache: 'pip'") || content.contains("cache: pip"))
        {
            // Allow if there's an explicit comment explaining why
            let has_explanation = content.contains("Pip caching disabled")
                || content.contains("no requirements.txt")
                || content.contains("yamllint install is fast");

            let cache_line = content
                .lines()
                .find(|line| {
                    let trimmed = line.trim();
                    trimmed.starts_with("cache:") && trimmed.contains("pip")
                })
                .unwrap_or("<not found>")
                .trim();

            assert!(
                has_explanation,
                "{filename}: Uses Python pip cache but no Python project files found.\n\
                 This is a Rust project (Cargo.toml exists).\n\
                 Either remove 'cache: pip' or add a comment explaining why it's needed.\n\
                 Cache line: `{cache_line}`\n\
                 Python indicators checked:\n\
                 - requirements.txt: {req_exists}\n\
                 - requirements-*.txt (glob): {glob_exists}\n\
                 - Pipfile: {pipfile_exists}\n\
                 - pyproject.toml: {pyproject_exists}",
                req_exists = root.join("requirements.txt").exists(),
                glob_exists = has_any_requirements_txt,
                pipfile_exists = root.join("Pipfile").exists(),
                pyproject_exists = root.join("pyproject.toml").exists(),
            );
        }

        // Check for Node caching on non-Node projects
        if !is_node_project && is_rust_project {
            assert!(
                !(content.contains("cache: 'npm'")
                    || content.contains("cache: npm")
                    || content.contains("cache: 'yarn'")),
                "{filename}: Uses Node cache but no package.json found.\n\
                 This is a Rust project (Cargo.toml exists).\n\
                 Remove cache configuration or add comment explaining why it's needed."
            );
        }
    }
}

#[test]
fn test_docs_deploy_requirements_file_exists() {
    // This test prevents the case where someone deletes requirements-docs.txt
    // but leaves the docs-deploy workflow referencing it, which would cause
    // the CI build to fail with a missing file error.

    let root = repo_root();
    let docs_deploy = root.join(".github/workflows/docs-deploy.yml");

    if !docs_deploy.exists() {
        // No docs-deploy workflow, nothing to check
        return;
    }

    let content = read_file(&docs_deploy);

    // Collect all references to requirements-docs.txt in the workflow
    let references: Vec<(usize, String)> = content
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("requirements-docs.txt"))
        .map(|(i, line)| (i + 1, line.trim().to_string()))
        .collect();

    if references.is_empty() {
        // Workflow does not reference requirements-docs.txt, nothing to check
        return;
    }

    let requirements_file = root.join("requirements-docs.txt");
    let reference_lines: Vec<String> = references
        .iter()
        .map(|(num, line)| format!("  line {num}: {line}"))
        .collect();

    assert!(
        requirements_file.exists(),
        "docs-deploy.yml references requirements-docs.txt but the file does not exist.\n\
         Workflow: {}\n\
         References found:\n{}\n\
         Either create requirements-docs.txt or update the workflow to remove references to it.",
        docs_deploy.display(),
        reference_lines.join("\n"),
    );
}

#[test]
fn test_scripts_are_executable() {
    // This test ensures shell scripts have executable permissions
    // Prevents "permission denied" errors in CI
    //
    // Platform Limitation:
    // - Unix/Linux/macOS: This test validates executable permissions (mode & 0o111)
    // - Windows: File permissions work differently (no executable bit concept)
    //   Git on Windows stores the executable bit in the index, but file system
    //   permissions are controlled by ACLs, not Unix-style mode bits.
    //   This test only validates on Unix platforms to avoid false failures.
    //
    // Why this matters for CI:
    // - GitHub Actions Linux runners require executable permissions on scripts
    // - Git stores the executable bit and preserves it on clone
    // - Scripts without +x fail with "permission denied" in CI
    // - This test catches the issue before CI runs

    let root = repo_root();
    let directories_to_check = vec![root.join("scripts"), root.join(".githooks")];

    #[cfg(unix)]
    let mut non_executable_scripts = Vec::new();

    for dir in directories_to_check {
        if !dir.exists() {
            continue;
        }

        for entry in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            // Check .sh files and files without extension (common for git hooks)
            let should_check = path.extension().map(|ext| ext == "sh").unwrap_or(false)
                || (path.is_file()
                    && path.extension().is_none()
                    && !path.file_name().unwrap().to_string_lossy().starts_with('.'));

            if should_check {
                let metadata = std::fs::metadata(&path).unwrap_or_else(|e| {
                    panic!("Failed to get metadata for {}: {}", path.display(), e)
                });

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = metadata.permissions().mode();
                    let is_executable = mode & 0o111 != 0;

                    if !is_executable {
                        non_executable_scripts.push(format!(
                            "  - {}\n    Current permissions: {:o}",
                            path.display(),
                            mode & 0o777
                        ));
                    }
                }

                // On non-Unix platforms, just check the file exists
                #[cfg(not(unix))]
                {
                    let _ = metadata; // Suppress unused variable warning
                }
            }
        }
    }

    #[cfg(unix)]
    if !non_executable_scripts.is_empty() {
        panic!(
            "Shell scripts are not executable:\n\n{}\n\n\
             Scripts must have executable permissions to run in CI and locally.\n\n\
             To fix:\n\
             1. Make scripts executable:\n\
                chmod +x <script-path>\n\n\
             2. Update git index to track executable bit:\n\
                git update-index --chmod=+x <script-path>\n\n\
             3. Verify with: git ls-files --stage <script-path>\n\
                Should show: 100755 (executable) not 100644 (non-executable)\n\n\
             Example:\n\
                chmod +x scripts/check-markdown.sh\n\
                git update-index --chmod=+x scripts/check-markdown.sh\n\
                git add scripts/check-markdown.sh\n",
            non_executable_scripts.join("\n")
        );
    }
}

#[test]
fn test_markdown_files_have_language_identifiers() {
    // This test prevents the MD040 markdown linting issue that caused CI failures
    // All code blocks in markdown files must have language identifiers
    // Example: ```bash instead of just ```

    let root = repo_root();

    // Find all markdown files in the repository (excluding dependencies and test fixtures)
    let markdown_files = find_files_with_extension(
        &root,
        "md",
        &[
            "target",
            "third_party",
            "node_modules",
            "test-fixtures",
            ".llm",
        ],
    );

    if markdown_files.is_empty() {
        // No markdown files found, test passes trivially
        return;
    }

    let mut violations = Vec::new();
    let mut total_files_checked = 0;
    let mut total_code_blocks = 0;
    let mut files_with_violations = std::collections::HashSet::new();

    for file in &markdown_files {
        total_files_checked += 1;
        let content = read_file(file);
        let mut in_code_block = false;
        let mut file_has_violation = false;

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1; // 1-indexed for human readability

            let trimmed = line.trim_start();

            // Check for opening code fence (exactly three backticks, not more)
            // This avoids matching ```` which is used for nested code blocks
            if trimmed.starts_with("```") && !trimmed.starts_with("````") {
                if !in_code_block {
                    // Opening fence
                    in_code_block = true;
                    total_code_blocks += 1;

                    // Check if language identifier is present
                    let fence_content = trimmed.trim_start_matches('`').trim();
                    if fence_content.is_empty() {
                        violations.push(format!(
                            "{}:{}: Code block missing language identifier (MD040)",
                            file.display(),
                            line_num
                        ));
                        file_has_violation = true;
                    }
                } else {
                    // Closing fence
                    in_code_block = false;
                }
            }
        }

        if file_has_violation {
            files_with_violations.insert(file.display().to_string());
        }
    }

    if !violations.is_empty() {
        panic!(
            "Markdown files have code blocks without language identifiers (MD040):\n\n{}\n\n\
             Diagnostic Information:\n\
             - Files checked: {}\n\
             - Total code blocks found: {}\n\
             - Files with violations: {}\n\
             - Total violations: {}\n\n\
             Files with violations:\n{}\n\n\
             All code blocks must specify a language identifier after the opening ```.\n\
             Examples:\n\
             - ```bash\n\
             - ```rust\n\
             - ```json\n\
             - ```text\n\n\
             To check markdown files locally:\n\
             ./scripts/check-markdown.sh\n\n\
             To auto-fix markdown issues:\n\
             ./scripts/check-markdown.sh fix",
            violations.join("\n"),
            total_files_checked,
            total_code_blocks,
            files_with_violations.len(),
            violations.len(),
            files_with_violations
                .iter()
                .map(|f| format!("  - {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[test]
fn test_mkdocs_material_tabs_have_lint_suppression() {
    // MkDocs Material tab syntax (`=== "Tab Title"`) creates 4-space indented
    // content blocks that markdownlint MD046 flags as inconsistent indentation.
    // Any markdown file using this syntax must wrap the tabbed section with
    // `<!-- markdownlint-disable MD046 -->` and `<!-- markdownlint-enable MD046 -->`.
    //
    // This test was added after a CI failure in docs/quickstart.md where the
    // MkDocs tab syntax caused MD046 lint errors.

    let root = repo_root();
    let docs_dir = root.join("docs");

    if !docs_dir.exists() {
        // No docs directory, nothing to check
        return;
    }

    let markdown_files = find_files_with_extension(
        &docs_dir,
        "md",
        &["target", "node_modules", "test-fixtures"],
    );

    let mut violations = Vec::new();

    for file in &markdown_files {
        let content = read_file(file);

        // Check if the file uses MkDocs Material tab syntax outside fenced code blocks.
        // We track fences by width (CommonMark spec) to avoid false positives from
        // tab syntax appearing inside fenced code examples.
        let mut fence_width: usize = 0;
        let mut has_tab_syntax_outside_fence = false;

        for line in content.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                let fence_char = trimmed.chars().next().unwrap();
                let width = trimmed.chars().take_while(|&c| c == fence_char).count();
                if fence_width == 0 {
                    // Opening fence
                    fence_width = width;
                } else if width >= fence_width {
                    // Check closing fence: rest after backticks must be blank
                    let rest = &trimmed[width..];
                    if rest.trim().is_empty() {
                        fence_width = 0;
                    }
                }
                continue;
            }
            if fence_width == 0 && trimmed.starts_with("=== \"") {
                has_tab_syntax_outside_fence = true;
                break;
            }
        }

        if !has_tab_syntax_outside_fence {
            continue;
        }

        let has_disable = content.contains("<!-- markdownlint-disable MD046 -->");
        let has_enable = content.contains("<!-- markdownlint-enable MD046 -->");

        if !has_disable || !has_enable {
            let mut missing = Vec::new();
            if !has_disable {
                missing.push("<!-- markdownlint-disable MD046 -->");
            }
            if !has_enable {
                missing.push("<!-- markdownlint-enable MD046 -->");
            }
            violations.push(format!(
                "{}: Uses MkDocs Material tab syntax (=== \"...\") but missing lint suppression.\n\
                 Missing comments: {}",
                file.display(),
                missing.join(", "),
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "Markdown files with MkDocs Material tabs must have MD046 lint suppression:\n\n{}\n\n\
         MkDocs Material tab syntax creates 4-space indented blocks that trigger MD046.\n\
         Wrap tabbed sections with:\n\
         <!-- markdownlint-disable MD046 -->\n\
         === \"Tab 1\"\n\
             content...\n\
         === \"Tab 2\"\n\
             content...\n\
         <!-- markdownlint-enable MD046 -->",
        violations.join("\n\n"),
    );
}

#[test]
fn test_typos_config_exists_and_is_valid() {
    // This test ensures the .typos.toml configuration file exists
    // and contains required technical terms to prevent false positives
    // Prevents the HashiCorp typo false positive issue

    let root = repo_root();
    let typos_config = root.join(".typos.toml");

    assert!(
        typos_config.exists(),
        ".typos.toml configuration file is missing.\n\
         This file is required for the typos spell checker in CI.\n\
         Create it with at least the [default.extend-words] section."
    );

    let content = read_file(&typos_config);

    // Basic validation: check for required sections
    assert!(
        content.contains("[default.extend-words]") || content.contains("[default]"),
        ".typos.toml must contain [default.extend-words] or [default] section"
    );

    // Check for common technical terms that are often flagged as typos
    // These should be explicitly allowed in .typos.toml
    let recommended_terms = vec![
        ("hashicorp", "HashiCorp (company name)"),
        ("github", "GitHub (platform name)"),
        ("websocket", "WebSocket protocol"),
    ];

    let mut missing_terms = Vec::new();
    for (term, description) in recommended_terms {
        // Case-insensitive search since typos.toml entries are lowercase
        if !content.to_lowercase().contains(&format!("{term} =")) {
            missing_terms.push(format!("  - {term} ({description})"));
        }
    }

    if !missing_terms.is_empty() {
        eprintln!(
            "WARNING: .typos.toml is missing some recommended technical terms:\n{}",
            missing_terms.join("\n")
        );
        // This is a warning, not a failure, since these are recommendations
        // Uncomment to make it a hard requirement:
        // panic!("Add recommended terms to .typos.toml");
    }

    // Verify that mixed-case company names are handled in extend-identifiers
    // This prevents false positives when company names use CamelCase (e.g., HashiCorp)
    assert!(
        content.contains("[default.extend-identifiers]"),
        ".typos.toml must contain [default.extend-identifiers] section for mixed-case terms"
    );

    // Check that HashiCorp is properly configured to prevent false positive on first part
    let has_hashicorp_identifier = content.contains("HashiCorp = \"HashiCorp\"");
    assert!(
        has_hashicorp_identifier,
        ".typos.toml must include 'HashiCorp = \"HashiCorp\"' in [default.extend-identifiers]\n\
         This prevents false positive when the spell checker splits the word at case boundaries.\n\
         Mixed-case company names must be in extend-identifiers, not extend-words."
    );
}

#[test]
fn test_typos_config_covers_known_files() {
    // This test verifies that .typos.toml properly covers technical terms appearing
    // in known documentation files, preventing regression of the HashiCorp false positive.
    //
    // Rather than the tautological "file contains HashiCorp" check, this test verifies
    // the typos configuration is sufficient to allow all known technical terms.

    let root = repo_root();
    let typos_config = root.join(".typos.toml");

    assert!(
        typos_config.exists(),
        ".typos.toml must exist to suppress false positives for technical terms.\n\
         Fix: Create .typos.toml with [default.extend-identifiers] section."
    );

    let config_content = read_file(&typos_config);

    // Files known to contain technical terms that require .typos.toml entries
    let known_technical_files: &[(&str, &[&str])] = &[("docs/authentication.md", &["HashiCorp"])];

    let mut violations = Vec::new();

    for (relative_path, required_terms) in known_technical_files {
        let file_path = root.join(relative_path);
        if !file_path.exists() {
            continue;
        }

        for term in *required_terms {
            // The term should be present in extend-identifiers (for CamelCase) or extend-words
            let covered = config_content.contains(&format!("{term} = \"{term}\""))
                || config_content.contains(&format!("{term} ="))
                || config_content
                    .to_lowercase()
                    .contains(&format!("{}  =", term.to_lowercase()));

            if !covered {
                violations.push(format!(
                    "  - '{term}' appears in {relative_path} but is not covered in .typos.toml\n\
                     Fix: Add to [default.extend-identifiers]: {term} = \"{term}\"\n\
                     Verify: grep -i '{term}' .typos.toml"
                ));
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            ".typos.toml does not cover all technical terms from known documentation files:\n\n{}\n\n\
             These terms appear in documentation but are not whitelisted in .typos.toml,\n\
             which will cause the spellcheck workflow to fail.",
            violations.join("\n")
        );
    }
}

#[test]
fn test_markdown_config_exists() {
    // This test ensures the .markdownlint.json configuration exists
    // Prevents missing markdownlint configuration

    let root = repo_root();
    let markdownlint_config = root.join(".markdownlint.json");

    assert!(
        markdownlint_config.exists(),
        ".markdownlint.json configuration file is missing.\n\
         This file is required for markdown linting in CI.\n\
         See .github/workflows/markdownlint.yml"
    );

    let content = read_file(&markdownlint_config);

    // Verify it's valid JSON
    assert!(
        content.trim().starts_with('{') && content.trim().ends_with('}'),
        ".markdownlint.json does not appear to be valid JSON"
    );

    // Check for MD040 rule (code block language identifiers)
    assert!(
        content.contains("MD040"),
        ".markdownlint.json must include MD040 rule (code block language identifiers)"
    );
}

#[test]
fn test_readme_shields_badges_use_for_the_badge_style() {
    let root = repo_root();
    let readme = root.join("README.md");
    let content = read_file(&readme);

    let violations = collect_shields_style_violations("README.md", &content);

    assert!(
        violations.is_empty(),
        "README Shields badge URLs must include style=for-the-badge when present.\n\
         Note: This repository does not require a minimum number of Shields badges.\n\
         Missing style parameter:\n{}",
        violations.join("\n")
    );
}

#[test]
#[cfg(unix)]
fn test_check_readme_badges_script_passes_on_repository() {
    use std::process::Command;

    let root = repo_root();
    let script = root.join("scripts/check-readme-badges.sh");
    assert!(
        script.exists(),
        "scripts/check-readme-badges.sh must exist to validate README badge styles"
    );

    let output = Command::new("bash")
        .arg(&script)
        .arg("README.md")
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run {}: {e}", script.display()));

    assert!(
        output.status.success(),
        "check-readme-badges.sh should pass on the current repository.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_shields_style_validation_allows_files_without_shields_urls() {
    let content = "# README\n\nThis file has no Shields badges.\n";
    let violations = collect_shields_style_violations("README.md", content);
    assert!(
        violations.is_empty(),
        "Shields style validation should pass when no Shields URLs are present."
    );
}

#[test]
fn test_extract_shields_urls_stops_at_whitespace() {
    let content = concat!(
        "<img src=\"https://img.shields.io/badge/docs-ok-blue?style=for-the-badge\">\n",
        "<img src=\"https://img.shields.io/badge/docs-tab-blue\t?style=for-the-badge\">\n",
        "<img src=\"https://img.shields.io/badge/docs-space-blue ?style=for-the-badge\">\n",
    );
    let urls = extract_shields_urls(content);
    let extracted: Vec<&str> = urls.iter().map(|(_, url)| url.as_str()).collect();

    assert_eq!(
        extracted,
        vec![
            "https://img.shields.io/badge/docs-ok-blue?style=for-the-badge",
            "https://img.shields.io/badge/docs-tab-blue",
            "https://img.shields.io/badge/docs-space-blue",
        ],
        "Shields URL extraction should stop at any whitespace boundary."
    );
}

#[test]
fn test_shields_style_matcher_uses_query_parameter_boundaries() {
    let valid_urls = [
        "https://img.shields.io/badge/docs-ok-blue?style=for-the-badge",
        "https://img.shields.io/badge/docs-ok-blue?a=b&style=for-the-badge",
        "https://img.shields.io/badge/docs-ok-blue?style=for-the-badge#readme",
        "https://img.shields.io/badge/docs-ok-blue?a=b&style=for-the-badge&c=d",
    ];
    let invalid_urls = [
        "https://img.shields.io/badge/docs-ok-blue?style=flat-square",
        "https://img.shields.io/badge/docs-ok-blue?nostyle=for-the-badge",
        "https://img.shields.io/badge/docs-ok-blue/path/style=for-the-badge",
        "https://img.shields.io/badge/docs-ok-blue?style=for-the-badges",
    ];

    for url in valid_urls {
        assert!(
            shields_url_has_for_the_badge_style(url),
            "Expected URL to be accepted as style-compliant: {url}"
        );
    }

    for url in invalid_urls {
        assert!(
            !shields_url_has_for_the_badge_style(url),
            "Expected URL to be rejected as style-noncompliant: {url}"
        );
    }
}

#[test]
#[cfg(unix)]
fn test_check_readme_badges_script_passes_when_no_shields_urls() {
    use std::process::Command;

    let root = repo_root();
    let script = root.join("scripts/check-readme-badges.sh");
    assert!(
        script.exists(),
        "scripts/check-readme-badges.sh must exist to validate README badge styles"
    );

    let temp_markdown = write_temp_markdown_file(
        &root,
        "readme-no-shields",
        "# README\n\nNo shields badges in this file.\n",
    );

    let output = Command::new("bash")
        .arg(&script)
        .arg(temp_markdown.path())
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run {}: {e}", script.display()));

    assert!(
        output.status.success(),
        "check-readme-badges.sh should pass when no Shields URLs are present.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("No Shields badge URLs found"),
        "Expected script output to describe no-Shields pass behavior.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn test_check_readme_badges_script_fails_when_style_param_missing() {
    use std::process::Command;

    let root = repo_root();
    let script = root.join("scripts/check-readme-badges.sh");
    assert!(
        script.exists(),
        "scripts/check-readme-badges.sh must exist to validate README badge styles"
    );

    let temp_markdown = write_temp_markdown_file(
        &root,
        "readme-badge-missing-style",
        r#"<img src="https://img.shields.io/badge/docs-GitHub%20Pages-blue">"#,
    );

    let output = Command::new("bash")
        .arg(&script)
        .arg(temp_markdown.path())
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run {}: {e}", script.display()));

    assert!(
        !output.status.success(),
        "check-readme-badges.sh should fail when a Shields URL omits style=for-the-badge.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Missing style=for-the-badge"),
        "Expected script output to identify missing style parameter.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn test_check_readme_badges_script_treats_tab_as_url_terminator() {
    use std::process::Command;

    let root = repo_root();
    let script = root.join("scripts/check-readme-badges.sh");
    assert!(
        script.exists(),
        "scripts/check-readme-badges.sh must exist to validate README badge styles"
    );

    // The style query appears after a tab, so it is not part of the URL token.
    let temp_markdown = write_temp_markdown_file(
        &root,
        "readme-badge-tab-terminated",
        "<img src=\"https://img.shields.io/badge/docs-GitHub%20Pages-blue\t?style=for-the-badge\">",
    );

    let output = Command::new("bash")
        .arg(&script)
        .arg(temp_markdown.path())
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run {}: {e}", script.display()));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "check-readme-badges.sh should fail when style is only present after tab whitespace.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Missing style=for-the-badge"),
        "Expected script output to identify missing style parameter.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("https://img.shields.io/badge/docs-GitHub%20Pages-blue"),
        "Expected script to extract URL only up to tab terminator.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn test_check_readme_badges_script_strict_mode_requires_at_least_one_badge() {
    use std::process::Command;

    let root = repo_root();
    let script = root.join("scripts/check-readme-badges.sh");
    assert!(
        script.exists(),
        "scripts/check-readme-badges.sh must exist to validate README badge styles"
    );

    let temp_markdown = write_temp_markdown_file(
        &root,
        "readme-no-shields-strict",
        "# README\n\nThis file intentionally has no badges.\n",
    );

    let output = Command::new("bash")
        .arg(&script)
        .arg("--require-at-least-one")
        .arg(temp_markdown.path())
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run {}: {e}", script.display()));

    assert!(
        !output.status.success(),
        "check-readme-badges.sh should fail in strict mode when no Shields URLs are present.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("requires at least one Shields"),
        "Expected strict mode output to explain the no-badges failure.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn test_check_readme_badges_script_rejects_multiple_positional_args() {
    use std::process::Command;

    let root = repo_root();
    let script = root.join("scripts/check-readme-badges.sh");
    assert!(
        script.exists(),
        "scripts/check-readme-badges.sh must exist to validate README badge styles"
    );

    let output = Command::new("bash")
        .arg(&script)
        .arg("README.md")
        .arg("Cargo.toml")
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run {}: {e}", script.display()));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "check-readme-badges.sh should fail when too many positional args are provided.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Too many positional arguments."),
        "Expected script output to mention excess positional arguments.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("Unexpected arguments:"),
        "Script should reject extra positional args in-parser and avoid stale post-loop checks.\n\
         stdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn test_markdown_guidance_avoids_stale_md060_references() {
    // The repository disables MD060 in .markdownlint.json, so guidance should not
    // instruct contributors to rely on MD060 behavior.
    let root = repo_root();
    let guidance_files = [
        ".llm/skills/markdown-best-practices-linting.md",
        ".llm/skills/ci-cd-troubleshooting-linting.md",
        "docs/development.md",
        "docs/adr/ci-cd-preventative-measures.md",
        "scripts/check-markdown.sh",
    ];

    let mut violations = Vec::new();
    for relative_path in guidance_files {
        let path = root.join(relative_path);
        let content = read_file(&path);
        if content.contains("MD060") {
            violations.push(relative_path.to_string());
        }
    }

    assert!(
        violations.is_empty(),
        "Guidance references MD060 even though this repo disables that rule.\n\
         Remove or replace MD060 references in:\n  - {}",
        violations.join("\n  - ")
    );
}

#[test]
fn test_check_markdown_script_enforces_pinned_runner_policy() {
    // Supply-chain hardening policy:
    //   - No npx runtime downloads
    //   - No Docker latest fallback for markdownlint
    //   - Enforce pinned markdownlint version via .markdownlint-version
    let root = repo_root();
    let script = root.join("scripts/check-markdown.sh");
    let content = read_file(&script);

    assert!(
        content.contains(".markdownlint-version"),
        "check-markdown.sh must read required version from .markdownlint-version.\n\
         Missing pinned version reference in {}",
        script.display()
    );

    assert!(
        content.contains("command -v markdownlint-cli2"),
        "check-markdown.sh must still detect locally available markdownlint-cli2.\n\
         Missing detection logic in {}",
        script.display()
    );

    assert!(
        !content.contains("npx --yes") && !content.contains("command -v npx"),
        "check-markdown.sh must not execute markdownlint via npx runtime downloads.\n\
         Remove npx fallback logic from {}",
        script.display()
    );

    assert!(
        !content.contains("davidanson/markdownlint-cli2:latest")
            && !content.contains("command -v docker"),
        "check-markdown.sh must not use Docker latest fallback for markdownlint.\n\
         Remove docker fallback logic from {}",
        script.display()
    );

    assert!(
        content.contains("npm install --save-dev --save-exact markdownlint-cli2@${REQUIRED_MARKDOWNLINT_VERSION}")
            && content.contains("npm install -g markdownlint-cli2@${REQUIRED_MARKDOWNLINT_VERSION}"),
        "check-markdown.sh should document both local and global pinned markdownlint installation paths.\n\
         Update install guidance in {}",
        script.display()
    );

    assert!(
        content.contains("Detected runner mode: ${MARKDOWNLINT_MODE}"),
        "check-markdown.sh should print detected runner mode on version mismatch for clearer diagnostics.\n\
         Update mismatch guidance in {}",
        script.display()
    );

    assert!(
        content.contains("./scripts/check-markdown-link-text.sh"),
        "check-markdown.sh must enforce human-readable internal markdown link text via scripts/check-markdown-link-text.sh.\n\
         Missing link-text policy enforcement in {}",
        script.display()
    );
}

#[test]
fn test_pre_commit_hook_fails_closed_when_markdownlint_is_unavailable() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");
    let content = read_file(&hook_path);

    assert!(
        content.contains(
            "check_fail \"Markdown linting\" \"markdownlint-cli2 unavailable or wrong pinned version.\"",
        ),
        ".githooks/pre-commit must fail closed when markdownlint-cli2 is unavailable for staged markdown.\n\
         This prevents CI-only markdownlint failures.\n\
         Missing fail-closed handling in {}",
        hook_path.display()
    );

    assert!(
        !content.contains(
            "check_skip \"Markdown linting\" \"pinned markdownlint-cli2 unavailable (see .markdownlint-version)\"",
        ),
        ".githooks/pre-commit should not skip markdown linting when the pinned tool is unavailable.\n\
         Skipping allows markdown regressions to reach CI."
    );
}

#[test]
fn test_run_local_ci_fails_closed_when_markdownlint_is_unavailable() {
    let root = repo_root();
    let script_path = root.join("scripts/run-local-ci.sh");
    let content = read_file(&script_path);

    assert!(
        content.contains(
            "FAIL${NC}: markdown (pinned markdownlint-cli2 unavailable or version mismatch)",
        ),
        "scripts/run-local-ci.sh must mark markdown as FAIL when markdownlint-cli2 is unavailable.\n\
         Missing fail-closed markdown status in {}",
        script_path.display()
    );

    assert!(
        !content.contains("SKIP${NC}: markdown (pinned markdownlint-cli2 unavailable)"),
        "scripts/run-local-ci.sh should not skip markdown checks when pinned markdownlint-cli2 is unavailable."
    );
}

#[test]
fn test_markdownlint_install_guidance_includes_local_and_global_options() {
    let root = repo_root();
    let guidance_files = [
        "scripts/check-markdown.sh",
        "scripts/enable-hooks.sh",
        "docs/git-hooks-guide.md",
        ".llm/skills/markdown-best-practices-linting.md",
    ];

    let mut missing_local = Vec::new();
    let mut missing_global = Vec::new();

    for relative_path in guidance_files {
        let path = root.join(relative_path);
        let content = read_file(&path);

        if !content.contains("npm install --save-dev --save-exact markdownlint-cli2@") {
            missing_local.push(relative_path.to_string());
        }
        if !content.contains("npm install -g markdownlint-cli2@") {
            missing_global.push(relative_path.to_string());
        }
    }

    assert!(
        missing_local.is_empty(),
        "Markdownlint guidance should include local pinned install instructions.\n\
         Missing in:\n  - {}",
        missing_local.join("\n  - ")
    );
    assert!(
        missing_global.is_empty(),
        "Markdownlint guidance should include global pinned install instructions as an alternative.\n\
         Missing in:\n  - {}",
        missing_global.join("\n  - ")
    );
}

#[test]
fn test_git_hook_skill_guidance_keeps_linter_failure_output_visible() {
    let root = repo_root();
    let guidance_path = root.join(".llm/skills/git-hooks-checks.md");
    let content = read_file(&guidance_path);

    assert!(
        !content.contains("./scripts/check-markdown.sh >/dev/null 2>&1"),
        "git-hooks-checks skill should not suppress markdown lint output in failure paths.\n\
         Update {} to capture and print checker output.",
        guidance_path.display()
    );
    assert!(
        content.contains("MARKDOWN_OUTPUT=$(./scripts/check-markdown.sh 2>&1)")
            && content.contains("echo \"$MARKDOWN_OUTPUT\""),
        "git-hooks-checks skill should demonstrate output capture for markdown lint failures.\n\
         Update {} with actionable failure output handling.",
        guidance_path.display()
    );
}

#[test]
fn test_git_hook_skill_external_code_sample_links_exist() {
    let root = repo_root();
    let skill_path = root.join(".llm/skills/git-hooks-checks.md");
    let content = read_file(&skill_path);

    let sample_files = [
        ".llm/code-samples/git-hooks/pre-commit-fast.sh",
        ".llm/code-samples/git-hooks/performance-patterns.sh",
        ".llm/code-samples/git-hooks/ci-hook-validation-tests.rs",
        ".llm/code-samples/git-hooks/debugging-snippets.sh",
    ];

    let mut issues = Vec::new();

    for sample in sample_files {
        let sample_path = root.join(sample);
        if !sample_path.exists() {
            issues.push(format!(
                "Missing external sample file: {}",
                sample_path.display()
            ));
        }

        let markdown_link = sample.replacen(".llm/", "../", 1);
        if !content.contains(&markdown_link) {
            issues.push(format!(
                "{} does not reference expected link target: {}",
                skill_path.display(),
                markdown_link
            ));
        }
    }

    assert!(
        issues.is_empty(),
        "Git hooks skill external sample references are inconsistent:\n\n{}\n\n\
         Fix by restoring missing sample files and link targets.",
        issues.join("\n")
    );
}

#[test]
fn test_markdownlint_version_file_exists_and_is_semver() {
    let root = repo_root();
    let version_file = root.join(".markdownlint-version");

    assert!(
        version_file.exists(),
        ".markdownlint-version must exist to pin markdownlint-cli2 version for local automation."
    );

    let version = read_file(&version_file).trim().to_string();
    let semver_pattern = regex::Regex::new(r"^\d+\.\d+\.\d+$").expect("valid semver regex");
    assert!(
        semver_pattern.is_match(&version),
        ".markdownlint-version must contain a plain semantic version (X.Y.Z).\n\
         Found: '{version}'"
    );
}

#[test]
fn test_automation_files_avoid_unpinned_tool_execution_patterns() {
    // Prevent recurrence of supply-chain patterns in executable automation:
    // - npx runtime downloads
    // - external Docker image :latest tags
    let root = repo_root();
    let mut automation_files = Vec::new();

    for dir in [root.join("scripts"), root.join(".github/workflows")] {
        if !dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(dir).expect("read_dir should succeed") {
            let entry = entry.expect("directory entry should be readable");
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let is_sh = path.extension().map(|e| e == "sh").unwrap_or(false);
            let is_yaml = path
                .extension()
                .map(|e| e == "yml" || e == "yaml")
                .unwrap_or(false);
            if is_sh || is_yaml {
                automation_files.push(path);
            }
        }
    }
    let pre_commit_hook = root.join(".githooks/pre-commit");
    if pre_commit_hook.exists() {
        automation_files.push(pre_commit_hook);
    }

    let npx_pattern = regex::Regex::new(
        r"(?m)^[[:space:]]*npx([[:space:]]|$)|[;&|][[:space:]]*npx([[:space:]]|$)",
    )
    .expect("valid npx invocation regex");
    let external_latest_pattern =
        regex::Regex::new(r"([A-Za-z0-9._-]+/[A-Za-z0-9._/-]+):[Ll][Aa][Tt][Ee][Ss][Tt]")
            .expect("valid docker image regex");

    let mut violations = Vec::new();
    let first_party_refs = expected_first_party_image_refs();

    for path in automation_files {
        let content = read_file(&path);
        if npx_pattern.is_match(&content) {
            violations.push(format!(
                "{}: contains 'npx' invocation (runtime package execution is disallowed in automation)",
                path.display()
            ));
        }

        for captures in external_latest_pattern.captures_iter(&content) {
            let image = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
            let is_allowed_first_party = first_party_refs.contains(image);
            if !is_allowed_first_party {
                violations.push(format!(
                    "{}: external image uses mutable ':latest' tag: {image}:latest",
                    path.display()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Automation files contain unpinned tooling execution patterns.\n\
         Fix by pinning tool versions and avoiding npx/runtime latest tags.\n\
         Violations:\n  - {}",
        violations.join("\n  - ")
    );
}

#[test]
fn test_docker_publish_workflow_uses_owner_derived_ghcr_image_name() {
    let root = repo_root();
    let workflow_path = root.join(".github/workflows/docker-publish.yml");
    let content = read_file(&workflow_path);

    assert!(
        content.contains("GITHUB_REPOSITORY_OWNER"),
        "docker-publish.yml must derive GHCR image owner from GITHUB_REPOSITORY_OWNER."
    );
    assert!(
        content.contains("GITHUB_REPOSITORY#*/"),
        "docker-publish.yml must derive GHCR repository name from GITHUB_REPOSITORY."
    );
    assert!(
        content.contains("images: ${{ steps.image.outputs.name }}"),
        "docker-publish.yml must pass a derived step output to docker/metadata-action images."
    );
    assert!(
        !content.contains("images: ghcr.io/"),
        "docker-publish.yml must not hard-code GHCR owner/repo in metadata-action images."
    );
}

#[test]
fn test_permissions_guidance_avoids_incorrect_default_claim() {
    let root = repo_root();
    let skill_path = root.join(".llm/skills/github-actions-workflow-config.md");
    let content = read_file(&skill_path);

    assert!(
        !content.contains("defaults to full write access"),
        "github-actions-workflow-config.md should not claim omitted permissions always default to full write access.\n\
         Repo/org defaults vary; guidance should recommend explicit least-privilege permissions."
    );
}

#[test]
fn test_skill_trigger_lines_do_not_form_accidental_setext_headings() {
    // Regression guard: a Trigger line immediately followed by `---` is parsed
    // as a setext heading, causing markdownlint MD003/MD026 failures.
    let root = repo_root();
    let skills_dir = root.join(".llm/skills");
    let files = find_files_with_extension(&skills_dir, "md", &[]);
    assert!(
        !files.is_empty(),
        "Expected at least one markdown skill file in {}",
        skills_dir.display()
    );

    let mut violations = Vec::new();
    for file in files {
        let content = read_file(&file);
        let lines: Vec<&str> = content.lines().collect();

        for idx in 0..lines.len().saturating_sub(1) {
            let current = lines[idx].trim_start();
            let next = lines[idx + 1].trim();
            if current.starts_with("**Trigger**:") && next == "---" {
                violations.push(format!(
                    "{}:{}: `**Trigger**:` is immediately followed by `---`.\n\
                     Add a blank line between them to avoid accidental setext headings.",
                    file.display(),
                    idx + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Skill trigger formatting violations detected:\n\n{}\n\n\
         Fix by adding a blank line between `**Trigger**:` and a subsequent `---` separator.",
        violations.join("\n\n")
    );
}

#[test]
fn test_link_hook_snippet_initializes_failures_and_matches_behavior() {
    let root = repo_root();
    let skill_path = root.join(".llm/skills/markdown-best-practices-links.md");
    let content = read_file(&skill_path);

    assert!(
        content.contains("FAILURES=0"),
        "markdown-best-practices-links.md pre-commit snippet increments FAILURES but does not initialize it."
    );
    assert!(
        content.contains("# Check for links"),
        "markdown-best-practices-links.md pre-commit snippet should describe lychee as link checking."
    );
    assert!(
        !content.contains("# Check for typos\nif command -v lychee"),
        "markdown-best-practices-links.md has a mismatched comment: lychee checks links, not typos."
    );
}

#[test]
fn test_async_network_skills_avoid_unwrap_in_server_startup_examples() {
    // Prevent panic-prone patterns in best-practice guidance snippets.
    let root = repo_root();
    let files = [
        ".llm/skills/async-rust-best-practices.md",
        ".llm/skills/graceful-degradation-deployment.md",
    ];

    let panic_patterns = [
        "TcpListener::bind(\"0.0.0.0:3536\").await.unwrap()",
        "let (stream, _) = accepted.unwrap();",
    ];

    let mut violations = Vec::new();
    for relative_path in files {
        let content = read_file(&root.join(relative_path));
        for pattern in panic_patterns {
            if content.contains(pattern) {
                violations.push(format!("{relative_path}: contains `{pattern}`"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Network best-practices examples should avoid panic paths in bind/accept loops.\n\
         Use Result + ? or explicit error handling.\n\
         Violations:\n  - {}",
        violations.join("\n  - ")
    );
}

#[test]
fn test_dockerfile_uses_docker_version_format() {
    // This test enforces that Dockerfile uses Docker's X.Y format instead of X.Y.Z
    //
    // Rationale:
    // - Docker Hub convention uses major.minor tags (e.g., rust:1.88)
    // - This provides automatic security patches for all 1.88.x releases
    // - Using full semver (1.88.0) would pin to exact patch version
    // - Documentation explicitly recommends X.Y format
    // - CI normalization logic handles the difference between formats

    let root = repo_root();
    let dockerfile = root.join("Dockerfile");

    assert!(
        dockerfile.exists(),
        "Dockerfile not found at {}",
        dockerfile.display()
    );

    let content = read_file(&dockerfile);

    // Extract the Rust version from FROM rust:X.Y or FROM rust:X.Y.Z
    let rust_version = content
        .lines()
        .find(|line| line.trim().starts_with("FROM rust:"))
        .and_then(|line| {
            line.split(':')
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.split('-').next())
                .map(String::from)
        });

    assert!(
        rust_version.is_some(),
        "Could not find 'FROM rust:' line in Dockerfile"
    );

    let version = rust_version.unwrap();

    // Count the number of dots to determine if it's X.Y or X.Y.Z
    let dot_count = version.matches('.').count();

    assert_eq!(
        dot_count, 1,
        "Dockerfile must use Docker format (X.Y) not full semver (X.Y.Z).\n\
         Found: FROM rust:{version}\n\
         Expected: FROM rust:{{major}}.{{minor}} (e.g., FROM rust:1.88)\n\n\
         Why Docker format is preferred:\n\
         - Docker Hub uses major.minor tags (rust:1.88)\n\
         - Provides automatic security patches for all 1.88.x releases\n\
         - Full semver (1.88.0) pins to exact patch version, missing updates\n\
         - CI normalization logic handles format differences\n\n\
         Fix: Change 'FROM rust:{version}' to 'FROM rust:{{major}}.{{minor}}' in Dockerfile"
    );
}

#[test]
fn test_github_actions_use_version_refs_not_commit_hashes() {
    // Enforce explicit version tags for GitHub Actions (e.g., @v4.2.2, @v2)
    // and explicitly reject commit-hash and moving channel refs in workflow files.

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/\n\
         Workflows directory: {}",
        workflows_dir.display()
    );

    let mut violations = Vec::new();
    let mut total_files_checked = 0;
    let mut total_actions_found = 0;
    let mut actions_with_valid_refs = 0;
    let mut malformed_remote_refs = 0;
    let mut files_with_violations = std::collections::HashSet::new();

    for entry in &workflow_files {
        total_files_checked += 1;
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();
        let mut file_has_violation = false;

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1; // 1-indexed for human readability
            let trimmed = line.trim();

            let Some(uses_value) = extract_uses_value(trimmed) else {
                continue;
            };

            let parse_result = classify_action_reference(uses_value);
            let (action_name, action_ref) = match parse_result {
                ActionReferenceParseResult::LocalOrDocker => continue,
                ActionReferenceParseResult::MalformedRemote { reason } => {
                    violations.push(format!(
                        "{filename}:{line_num}: Malformed remote action reference in uses: {uses_value}\n  \
                         Reason: {reason}\n  \
                         Expected format: owner/repo@ref (for example actions/checkout@v6.0.2)."
                    ));
                    malformed_remote_refs += 1;
                    file_has_violation = true;
                    continue;
                }
                ActionReferenceParseResult::Remote {
                    action_name,
                    action_ref,
                } => (action_name, action_ref),
            };

            total_actions_found += 1;

            if is_commit_hash(action_ref) {
                violations.push(format!(
                    "{filename}:{line_num}: Action uses commit hash ref (not allowed): {action_name}@{action_ref}\n  \
                     Expected an explicit version ref like @vX.Y.Z or @vX."
                ));
                file_has_violation = true;
                continue;
            }

            if !is_action_version_ref(action_ref) {
                violations.push(format!(
                    "{filename}:{line_num}: Action ref is not an approved explicit-version format: {action_name}@{action_ref}\n  \
                     Allowed formats: @vX, @vX.Y, @vX.Y.Z, optionally with prerelease/build suffix.\n  \
                     Disallowed moving refs: @stable, @beta, @nightly, @main, @master, @latest."
                ));
                file_has_violation = true;
                continue;
            }

            actions_with_valid_refs += 1;
        }

        if file_has_violation {
            files_with_violations.insert(filename.to_string());
        }
    }

    if !violations.is_empty() {
        panic!(
            "GitHub Actions must use explicit version refs and must not use commit hashes/moving channels:\n\n{}\n\n\
             Diagnostic Information:\n\
             - Workflow files checked: {}\n\
             - Total actions found: {}\n\
             - Actions with valid refs: {}\n\
             - Malformed remote refs: {}\n\
             - Actions with violations: {}\n\
             - Workflows with violations: {}\n\n\
             Workflows with violations:\n{}",
            violations.join("\n"),
            total_files_checked,
            total_actions_found,
            actions_with_valid_refs,
            malformed_remote_refs,
            violations.len(),
            files_with_violations.len(),
            files_with_violations
                .iter()
                .map(|f| format!("  - {f}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[test]
fn test_cargo_deny_action_minimum_version() {
    // This test ensures cargo-deny-action is at least v2.0.15
    // v2.0.15+ includes important security and stability fixes
    //
    // Background: Earlier versions had issues with:
    // - Advisory database sync failures
    // - False positives in license checking
    // - Performance issues with large dependency graphs

    let root = repo_root();
    let ci_workflow = root.join(".github/workflows/ci.yml");
    let content = read_file(&ci_workflow);

    // Find the cargo-deny-action reference
    let mut found_cargo_deny = false;
    let mut violations = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let line_num = line_num + 1; // 1-indexed
        let trimmed = line.trim();

        let Some(uses_value) = extract_uses_value(trimmed) else {
            continue;
        };

        if uses_value.contains("cargo-deny-action") {
            found_cargo_deny = true;

            // Extract and validate action ref
            let Some((_, action_ref)) = parse_remote_action_reference(uses_value) else {
                violations.push(format!(
                    "Line {line_num}: cargo-deny-action reference is malformed: {trimmed}"
                ));
                continue;
            };

            // Parse version (must be vX.Y.Z for this minimum-version check)
            if !action_ref.starts_with('v') {
                violations.push(format!(
                    "Line {line_num}: cargo-deny-action must use an explicit version like @vX.Y.Z, found: @{action_ref}"
                ));
                continue;
            }

            let version_numbers = action_ref.trim_start_matches('v');
            let version_parts: Vec<&str> = version_numbers.split('.').collect();

            if version_parts.len() < 3 {
                violations.push(format!(
                    "Line {line_num}: Invalid version format (expected vX.Y.Z): {action_ref}"
                ));
                continue;
            }

            // Parse major, minor, patch
            let major: u32 = version_parts[0].parse().unwrap_or(0);
            let minor: u32 = version_parts[1].parse().unwrap_or(0);
            let patch: u32 = version_parts[2].parse().unwrap_or(0);

            // Check against minimum version: v2.0.15
            let min_major = 2;
            let min_minor = 0;
            let min_patch = 15;

            let is_sufficient = major > min_major
                || (major == min_major && minor > min_minor)
                || (major == min_major && minor == min_minor && patch >= min_patch);

            if !is_sufficient {
                violations.push(format!(
                    "Line {line_num}: cargo-deny-action version too old: {action_ref}\n  \
                     Minimum required: v{min_major}.{min_minor}.{min_patch}\n  \
                     Found: v{major}.{minor}.{patch}\n  \
                     Please update to v2.0.15 or newer for security and stability fixes."
                ));
            }
        }
    }

    assert!(
        found_cargo_deny,
        "cargo-deny-action not found in CI workflow.\n\
         Expected to find 'uses: EmbarkStudios/cargo-deny-action@...' in {}",
        ci_workflow.display()
    );

    if !violations.is_empty() {
        panic!(
            "cargo-deny-action version check failed:\n\n{}\n",
            violations.join("\n")
        );
    }
}

#[test]
fn test_action_reference_parsing_and_validation_data_driven() {
    let parse_cases = [
        (
            "uses: actions/checkout@v6.0.2",
            Some(("actions/checkout", "v6.0.2")),
        ),
        (
            "- uses: actions/checkout@v6.0.2",
            Some(("actions/checkout", "v6.0.2")),
        ),
        (
            "uses: 'actions/checkout@v6.0.2'",
            Some(("actions/checkout", "v6.0.2")),
        ),
        ("uses: ./.github/actions/custom", None),
        ("- uses: docker://alpine:3.20", None),
        ("uses: actions/checkout", None),
        ("uses: actions/checkout@", None),
        ("uses: checkout@v6.0.2", None),
        ("run: echo hello", None),
    ];

    for (line, expected) in parse_cases {
        let parsed = extract_uses_value(line).and_then(parse_remote_action_reference);
        assert_eq!(parsed, expected, "Unexpected parse result for line: {line}");
    }

    let classification_cases = [
        (
            "uses: actions/checkout@v6.0.2",
            ActionReferenceParseResult::Remote {
                action_name: "actions/checkout",
                action_ref: "v6.0.2",
            },
        ),
        (
            "uses: ./.github/actions/custom",
            ActionReferenceParseResult::LocalOrDocker,
        ),
        (
            "uses: docker://alpine:3.20",
            ActionReferenceParseResult::LocalOrDocker,
        ),
        (
            "uses: actions/checkout",
            ActionReferenceParseResult::MalformedRemote {
                reason: "missing '@' separator",
            },
        ),
        (
            "uses: actions/checkout@",
            ActionReferenceParseResult::MalformedRemote {
                reason: "missing action ref after '@'",
            },
        ),
        (
            "uses: checkout@v6.0.2",
            ActionReferenceParseResult::MalformedRemote {
                reason: "remote action must use owner/repo@ref syntax",
            },
        ),
    ];

    for (line, expected) in classification_cases {
        let actual = extract_uses_value(line)
            .map(classify_action_reference)
            .unwrap_or(ActionReferenceParseResult::MalformedRemote {
                reason: "empty uses value",
            });
        assert_eq!(
            actual, expected,
            "Unexpected action reference classification for line: {line}"
        );
    }

    let ref_cases = [
        ("v2", true),
        ("v2.7", true),
        ("v2.7.5", true),
        ("v2.7.5-beta.1", true),
        ("stable", false),
        ("beta", false),
        ("nightly", false),
        ("main", false),
        ("master", false),
        ("latest", false),
        ("", false),
        ("de0fac2e4500dabe0009e67214ff5f5447ce83dd", false),
    ];

    for (action_ref, expected) in ref_cases {
        assert_eq!(
            is_action_version_ref(action_ref),
            expected,
            "Unexpected action ref policy result for: {action_ref}"
        );
    }
}

// ============================================================================
// Required Check Naming Contract Tests
// ============================================================================
//
// These tests enforce the naming contract defined by the constants above.
// They ensure that workflow files, job keys, display names, and GitHub check
// names remain consistent across all configuration surfaces.

#[test]
fn test_doc_validation_workflow_has_required_jobs() {
    // This test validates that the doc-validation workflow has all required jobs
    // with the correct display names. Prevents accidental removal or renaming of
    // documentation validation jobs, which would silently break branch protection
    // rules that reference the GitHub check name "{workflow_name} / {job_display_name}".

    let root = repo_root();
    let workflow = root.join(".github/workflows/doc-validation.yml");
    validate_workflow_has_required_jobs(
        &workflow,
        REQUIRED_DOC_VALIDATION_JOBS,
        "Documentation Validation",
    );
}

#[test]
fn test_doc_validation_path_filters_cover_critical_paths() {
    // This test validates that doc-validation.yml has path filters that include
    // all critical documentation-related paths. Path filters control when the
    // workflow triggers — if a critical path is missing, the workflow will
    // silently skip important changes (e.g., a Cargo.toml change that breaks
    // doc builds would go unvalidated).

    let root = repo_root();
    let workflow_path = root.join(".github/workflows/doc-validation.yml");
    let content = read_file(&workflow_path);

    // Critical paths that doc-validation must trigger on.
    // These ensure documentation changes are always validated.
    const REQUIRED_DOC_PATHS: &[(&str, &str)] = &[
        ("'**/*.md'", "Markdown documentation files"),
        ("'**/*.rs'", "Rust source files (contain doc-comments)"),
        ("'Cargo.toml'", "Dependency changes affect doc builds"),
        ("'Cargo.lock'", "Lockfile changes affect doc builds"),
        (
            "'.github/workflows/doc-validation.yml'",
            "Self-referential trigger for workflow changes",
        ),
        ("'.github/scripts/**'", "Scripts used by the workflow"),
    ];

    let mut missing_paths = Vec::new();

    for (path_pattern, description) in REQUIRED_DOC_PATHS {
        if !content.contains(path_pattern) {
            missing_paths.push(format!("  - {path_pattern} ({description})"));
        }
    }

    if !missing_paths.is_empty() {
        panic!(
            "doc-validation.yml is missing critical path filters:\n\n{}\n\n\
             The doc-validation workflow uses path filters to trigger only on relevant\n\
             file changes. These paths are required to ensure documentation validation\n\
             runs whenever documentation-related files change.\n\n\
             File: {}\n\n\
             To fix: Add the missing paths to both 'push.paths' and 'pull_request.paths'\n\
             sections in the workflow file.",
            missing_paths.join("\n"),
            workflow_path.display()
        );
    }
}

#[test]
fn test_doc_validation_strict_rustdocflags() {
    // This test ensures the doc-validation workflow enforces strict rustdoc
    // validation via the RUSTDOCFLAGS environment variable. Without these flags,
    // broken documentation links and invalid code block attributes would pass
    // silently, degrading documentation quality over time.

    let root = repo_root();
    let workflow_path = root.join(".github/workflows/doc-validation.yml");
    let content = read_file(&workflow_path);

    // Required RUSTDOCFLAGS for strict documentation validation.
    // Each flag maps to a specific documentation quality gate.
    const REQUIRED_RUSTDOC_FLAGS: &[(&str, &str)] = &[
        ("-D warnings", "Deny all rustdoc warnings"),
        (
            "-D rustdoc::broken_intra_doc_links",
            "Deny broken intra-doc links",
        ),
        (
            "-D rustdoc::private_intra_doc_links",
            "Deny links to private items",
        ),
        (
            "-D rustdoc::invalid_codeblock_attributes",
            "Deny invalid code block attributes",
        ),
    ];

    // Check that RUSTDOCFLAGS is set in the workflow
    assert!(
        content.contains("RUSTDOCFLAGS"),
        "doc-validation.yml must set RUSTDOCFLAGS environment variable for strict validation.\n\
         File: {}\n\
         To fix: Add RUSTDOCFLAGS to the env: section with strict deny flags.",
        workflow_path.display()
    );

    let mut missing_flags = Vec::new();

    for (flag, description) in REQUIRED_RUSTDOC_FLAGS {
        if !content.contains(flag) {
            missing_flags.push(format!("  - {flag} ({description})"));
        }
    }

    if !missing_flags.is_empty() {
        panic!(
            "doc-validation.yml RUSTDOCFLAGS is missing required strict flags:\n\n{}\n\n\
             These flags are required to enforce documentation quality:\n\
             - Broken links in doc-comments are caught at build time\n\
             - Invalid code block attributes are flagged before merge\n\
             - Links to private items are detected (API documentation accuracy)\n\n\
             File: {}\n\n\
             To fix: Add the missing flags to the RUSTDOCFLAGS environment variable.",
            missing_flags.join("\n"),
            workflow_path.display()
        );
    }
}

#[test]
fn test_doc_validation_job_timeout_budgets() {
    // This test validates that all required doc-validation jobs have explicit
    // timeout-minutes settings within a reasonable range. Timeouts prevent
    // hung jobs from consuming CI minutes and blocking the merge queue.
    //
    // Budget: 5-30 minutes per job. Below 5 is too aggressive for documentation
    // builds; above 30 suggests the job needs optimization or splitting.

    let root = repo_root();
    let workflow_path = root.join(".github/workflows/doc-validation.yml");
    let content = read_file(&workflow_path);

    let mut errors = Vec::new();

    for (job_key, display_name, _description) in REQUIRED_DOC_VALIDATION_JOBS {
        // Find the job block
        let job_header = format!("  {job_key}:");
        let mut in_target_job = false;
        let mut found_timeout = false;

        for line in content.lines() {
            if line.starts_with(&job_header) {
                in_target_job = true;
                continue;
            }

            if in_target_job {
                let trimmed = line.trim();

                // If we hit another job definition, we've left the target job block
                if line.starts_with("  ") && !line.starts_with("    ") && !trimmed.is_empty() {
                    break;
                }

                // Look for timeout-minutes at job level (4-space indent)
                if let Some(rest) = line.strip_prefix("    timeout-minutes:") {
                    found_timeout = true;
                    let timeout_str = rest.trim();

                    // Strip inline comments (e.g., "15  # Generous timeout...")
                    let timeout_value = timeout_str.split('#').next().unwrap_or(timeout_str).trim();

                    if let Ok(timeout) = timeout_value.parse::<u32>() {
                        if timeout < 5 {
                            errors.push(format!(
                                "  {job_key} ({display_name}): timeout-minutes={timeout} is too \
                                 aggressive (minimum 5 for documentation builds)"
                            ));
                        } else if timeout > 30 {
                            errors.push(format!(
                                "  {job_key} ({display_name}): timeout-minutes={timeout} exceeds \
                                 budget (maximum 30; consider optimizing or splitting the job)"
                            ));
                        }
                    } else {
                        errors.push(format!(
                            "  {job_key} ({display_name}): timeout-minutes value \
                             \"{timeout_value}\" is not a valid integer"
                        ));
                    }
                    break;
                }
            }
        }

        if in_target_job && !found_timeout {
            errors.push(format!(
                "  {job_key} ({display_name}): missing timeout-minutes setting.\n\
                 Jobs without timeouts can hang indefinitely, wasting CI minutes.\n\
                 To fix: Add 'timeout-minutes: N' to the job definition (5-30 range)."
            ));
        }
    }

    if !errors.is_empty() {
        panic!(
            "doc-validation.yml job timeout budget violations:\n\n{}\n\n\
             All required doc-validation jobs must have explicit timeout-minutes\n\
             settings within the 5-30 minute budget.\n\n\
             File: {}",
            errors.join("\n"),
            workflow_path.display()
        );
    }
}

#[test]
fn test_required_check_names_match_workflow_definitions() {
    // This is the key naming contract test. It validates that every entry in
    // REQUIRED_CHECK_NAMES matches the actual workflow file contents, and that
    // every required job's constructed check name appears in REQUIRED_CHECK_NAMES.
    //
    // GitHub constructs check names as: "{workflow name} / {job display name}"
    // If either the workflow name or job display name changes, the GitHub check
    // name changes too, silently breaking branch protection rules.
    //
    // This test prevents that by:
    //   1. Reading the workflow `name:` field from each required workflow file
    //   2. Reading each required job's `name:` field
    //   3. Constructing the expected GitHub check name
    //   4. Validating bidirectional consistency with REQUIRED_CHECK_NAMES

    let root = repo_root();
    let mut constructed_check_names: Vec<String> = Vec::new();
    let mut errors = Vec::new();

    // Process each required workflow and its jobs
    let workflow_job_sets: &[(&str, &[(&str, &str, &str)])] = &[
        ("ci.yml", REQUIRED_CI_JOBS),
        ("doc-validation.yml", REQUIRED_DOC_VALIDATION_JOBS),
    ];

    for (workflow_file, required_jobs) in workflow_job_sets {
        let workflow_path = root.join(".github/workflows").join(workflow_file);
        let content = read_file(&workflow_path);

        // Extract the workflow name: field (top-level, before any jobs)
        let workflow_name = content
            .lines()
            .find(|line| line.starts_with("name:"))
            .and_then(|line| {
                line.strip_prefix("name:")
                    .map(|s| s.trim().trim_matches('"').to_string())
            });

        let workflow_name = match workflow_name {
            Some(name) => name,
            None => {
                errors.push(format!(
                    "{workflow_file}: Could not extract top-level 'name:' field.\n\
                     Every workflow must have a 'name:' field at the top level."
                ));
                continue;
            }
        };

        for (job_key, expected_display_name, _description) in *required_jobs {
            // Look for the job's name: field
            // We search for "  job_key:" then look for "    name:" on the next non-empty line
            let job_display_name = extract_job_display_name(&content, job_key);

            match job_display_name {
                Some(ref actual_name) => {
                    if actual_name != expected_display_name {
                        errors.push(format!(
                            "{workflow_file}: Job '{job_key}' has name \"{actual_name}\" \
                             but contract expects \"{expected_display_name}\".\n\
                             This changes the GitHub check name from \
                             \"{workflow_name} / {expected_display_name}\" to \
                             \"{workflow_name} / {actual_name}\".\n\
                             To fix: Update the job's name: field or update the contract constants."
                        ));
                    }

                    // Matrix jobs expand to multiple check names (one per OS value).
                    // Non-matrix jobs produce a single check name.
                    let expanded = expand_matrix_display_name(&workflow_name, actual_name);
                    constructed_check_names.extend(expanded);
                }
                None => {
                    errors.push(format!(
                        "{workflow_file}: Could not find 'name:' field for job '{job_key}'.\n\
                         Expected: `    name: {expected_display_name}`"
                    ));
                    // Use the expected name to construct the check name anyway
                    let expanded =
                        expand_matrix_display_name(&workflow_name, expected_display_name);
                    constructed_check_names.extend(expanded);
                }
            }
        }
    }

    // Forward check: every entry in REQUIRED_CHECK_NAMES must match a constructed name
    for required_name in REQUIRED_CHECK_NAMES {
        if !constructed_check_names.iter().any(|c| c == required_name) {
            errors.push(format!(
                "REQUIRED_CHECK_NAMES contains \"{required_name}\" but this check name \
                 was not constructed from any workflow file.\n\
                 To fix: Either update the workflow to produce this check name, \
                 or remove it from REQUIRED_CHECK_NAMES."
            ));
        }
    }

    // Reverse check: every constructed name must appear in REQUIRED_CHECK_NAMES
    for constructed in &constructed_check_names {
        if !REQUIRED_CHECK_NAMES.contains(&constructed.as_str()) {
            errors.push(format!(
                "Workflow files produce check name \"{constructed}\" but it is not in \
                 REQUIRED_CHECK_NAMES.\n\
                 To fix: Either add \"{constructed}\" to REQUIRED_CHECK_NAMES, \
                 or update the workflow job name to match an existing entry."
            ));
        }
    }

    if !errors.is_empty() {
        panic!(
            "Required check naming contract violations:\n\n{}\n\n\
             Constructed check names from workflow files:\n{}\n\n\
             Expected check names from REQUIRED_CHECK_NAMES:\n{}\n\n\
             GitHub constructs check names as: \"{{workflow name}} / {{job display name}}\"\n\
             Any mismatch between these constants and the actual workflow files will cause\n\
             branch protection rules to silently stop matching.",
            errors.join("\n\n"),
            constructed_check_names
                .iter()
                .map(|c| format!("  - {c}"))
                .collect::<Vec<_>>()
                .join("\n"),
            REQUIRED_CHECK_NAMES
                .iter()
                .map(|c| format!("  - {c}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
}

#[test]
fn test_required_workflow_triggers() {
    // This test validates that required workflows have the correct triggers
    // (push to main, pull_request to main). Without these triggers, the
    // workflows would not run on the events that matter for branch protection.
    //
    // Both ci.yml and doc-validation.yml must trigger on:
    //   - pull_request with branches: [main]
    //   - push with branches: [main]
    //
    // Note: doc-validation.yml also has path filters, which are acceptable
    // as long as the branch triggers are present.

    let root = repo_root();
    let mut errors = Vec::new();

    for (workflow_file, _workflow_name) in REQUIRED_WORKFLOW_NAMES {
        let workflow_path = root.join(".github/workflows").join(workflow_file);
        let content = read_file(&workflow_path);

        // Check for pull_request trigger with main branch
        let has_pull_request = content.contains("pull_request:");
        let has_push = content.contains("push:");

        if !has_pull_request {
            errors.push(format!(
                "{workflow_file}: Missing 'pull_request:' trigger.\n\
                 Required workflows must trigger on pull requests to main.\n\
                 To fix: Add pull_request trigger:\n\
                   on:\n\
                     pull_request:\n\
                       branches: [main]"
            ));
        }

        if !has_push {
            errors.push(format!(
                "{workflow_file}: Missing 'push:' trigger.\n\
                 Required workflows must trigger on push to main.\n\
                 To fix: Add push trigger:\n\
                   on:\n\
                     push:\n\
                       branches: [main]"
            ));
        }

        // Validate that both push and pull_request sections have `branches: [main]`.
        // We extract the text between each trigger keyword and the next top-level key
        // to scope the check, avoiding false positives from `branches: [main]` appearing
        // in unrelated parts of the file (e.g., comments or step names).
        let trigger_sections = ["push:", "pull_request:"];
        for trigger in &trigger_sections {
            if let Some(trigger_start) = content.find(trigger) {
                // Find the content from the trigger keyword to the next top-level key.
                // Top-level keys in YAML start at column 0 with a letter (no leading space).
                let after_trigger = &content[trigger_start + trigger.len()..];
                let section_end = after_trigger
                    .find("\n")
                    .and_then(|first_newline| {
                        after_trigger[first_newline..]
                            .lines()
                            .skip(1) // skip the rest of the trigger line
                            .position(|line| {
                                !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#')
                            })
                            .map(|pos| {
                                // Calculate the byte offset within after_trigger
                                let mut offset = first_newline;
                                for (i, line) in
                                    after_trigger[first_newline..].lines().skip(1).enumerate()
                                {
                                    if i == pos {
                                        break;
                                    }
                                    offset += line.len() + 1; // +1 for newline
                                }
                                offset
                            })
                    })
                    .unwrap_or(after_trigger.len());

                let section_content = &after_trigger[..section_end];
                if !section_content.contains("branches: [main]") {
                    errors.push(format!(
                        "{workflow_file}: '{trigger}' section does not contain 'branches: [main]'.\n\
                         Required workflows must filter to the main branch under each trigger.\n\
                         To fix: Add 'branches: [main]' under the {trigger} trigger:\n\
                           {trigger}\n\
                             branches: [main]"
                    ));
                }
            }
        }
    }

    if !errors.is_empty() {
        panic!(
            "Required workflow trigger validation failed:\n\n{}\n\n\
             Required workflows must trigger on both push and pull_request events\n\
             targeting the main branch. Without these triggers, branch protection\n\
             checks will not run and PRs cannot be validated.",
            errors.join("\n\n")
        );
    }
}

#[test]
fn test_workflow_display_names_match_contract() {
    // This test validates that the `name:` field at the top of each required
    // workflow file matches the expected name from REQUIRED_WORKFLOW_NAMES.
    //
    // The workflow display name is the first component of a GitHub check name.
    // If it changes, all check names produced by that workflow change too,
    // silently breaking branch protection rules.

    let root = repo_root();
    let mut errors = Vec::new();

    for (workflow_file, expected_name) in REQUIRED_WORKFLOW_NAMES {
        let workflow_path = root.join(".github/workflows").join(workflow_file);

        if !workflow_path.exists() {
            errors.push(format!(
                "{workflow_file}: Workflow file does not exist.\n\
                 Expected at: {}\n\
                 To fix: Restore the workflow file from git history.",
                workflow_path.display()
            ));
            continue;
        }

        let content = read_file(&workflow_path);

        // Extract the top-level name: field
        let actual_name = content
            .lines()
            .find(|line| line.starts_with("name:"))
            .and_then(|line| {
                line.strip_prefix("name:")
                    .map(|s| s.trim().trim_matches('"').to_string())
            });

        match actual_name {
            Some(actual) => {
                if actual != *expected_name {
                    errors.push(format!(
                        "{workflow_file}: Workflow display name mismatch.\n\
                         Expected: \"{expected_name}\"\n\
                         Found:    \"{actual}\"\n\
                         This changes ALL GitHub check names produced by this workflow.\n\
                         To fix: Either restore the name to \"{expected_name}\" or update\n\
                         REQUIRED_WORKFLOW_NAMES and REQUIRED_CHECK_NAMES constants,\n\
                         then update branch protection settings in GitHub."
                    ));
                }
            }
            None => {
                errors.push(format!(
                    "{workflow_file}: Could not find top-level 'name:' field.\n\
                     Expected: name: {expected_name}\n\
                     To fix: Add 'name: {expected_name}' at the top of the workflow file."
                ));
            }
        }
    }

    if !errors.is_empty() {
        panic!(
            "Workflow display name contract violations:\n\n{}\n\n\
             Workflow display names are the first component of GitHub check names.\n\
             Changing a workflow name from \"CI\" to \"Build\" would change check names\n\
             from \"CI / Test\" to \"Build / Test\", breaking branch protection.\n\n\
             If a rename is intentional, update ALL of:\n\
             1. The workflow file's name: field\n\
             2. REQUIRED_WORKFLOW_NAMES constant\n\
             3. REQUIRED_CHECK_NAMES constant\n\
             4. Branch protection settings in GitHub\n\
             5. Documentation references",
            errors.join("\n\n")
        );
    }
}

#[test]
fn test_required_check_names_are_consistent() {
    // This is a self-consistency test that validates REQUIRED_CHECK_NAMES
    // can be decomposed into valid "{workflow_name} / {job_display_name}" pairs
    // where the workflow name and job display name are found in the other
    // constant arrays (REQUIRED_WORKFLOW_NAMES, REQUIRED_CI_JOBS, REQUIRED_DOC_VALIDATION_JOBS).
    //
    // This catches drift between the constants without requiring file I/O,
    // making it fast and always runnable even if workflow files are temporarily missing.

    let mut errors = Vec::new();

    // Build a set of valid workflow display names from REQUIRED_WORKFLOW_NAMES
    let valid_workflow_names: Vec<&str> = REQUIRED_WORKFLOW_NAMES
        .iter()
        .map(|(_, name)| *name)
        .collect();

    // Build a set of valid job display names from both job arrays
    let valid_job_names: Vec<&str> = REQUIRED_CI_JOBS
        .iter()
        .map(|(_, name, _)| *name)
        .chain(
            REQUIRED_DOC_VALIDATION_JOBS
                .iter()
                .map(|(_, name, _)| *name),
        )
        .collect();

    for check_name in REQUIRED_CHECK_NAMES {
        // Parse the check name into workflow_name and job_name
        let parts: Vec<&str> = check_name.splitn(2, " / ").collect();
        if parts.len() != 2 {
            errors.push(format!(
                "REQUIRED_CHECK_NAMES entry \"{check_name}\" is not in the expected format.\n\
                 Expected: \"{{workflow_name}} / {{job_display_name}}\"\n\
                 The \" / \" separator must be present exactly once."
            ));
            continue;
        }

        let workflow_part = parts[0];
        let job_part = parts[1];

        // Validate the workflow name exists in REQUIRED_WORKFLOW_NAMES
        if !valid_workflow_names.contains(&workflow_part) {
            errors.push(format!(
                "REQUIRED_CHECK_NAMES entry \"{check_name}\" references workflow \
                 \"{workflow_part}\" which is not in REQUIRED_WORKFLOW_NAMES.\n\
                 Known workflow names: {}\n\
                 To fix: Add (\"{{}}.yml\", \"{workflow_part}\") to REQUIRED_WORKFLOW_NAMES \
                 or fix the check name.",
                valid_workflow_names
                    .iter()
                    .map(|n| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        // Validate the job display name exists in the corresponding job array.
        // For matrix jobs, the check name contains an expanded OS value (e.g.
        // "Lint (ubuntu-latest)") while the job array stores the template
        // (e.g. "Lint (${{ matrix.os }})"), so we use template matching.
        let job_matches = valid_job_names
            .iter()
            .any(|template| display_name_matches_template(job_part, template));
        if !job_matches {
            errors.push(format!(
                "REQUIRED_CHECK_NAMES entry \"{check_name}\" references job display name \
                 \"{job_part}\" which is not in REQUIRED_CI_JOBS or REQUIRED_DOC_VALIDATION_JOBS.\n\
                 Known job display names: {}\n\
                 To fix: Add the job to the appropriate REQUIRED_*_JOBS constant \
                 or fix the check name.",
                valid_job_names
                    .iter()
                    .map(|n| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    // Reverse check: every job in REQUIRED_CI_JOBS and REQUIRED_DOC_VALIDATION_JOBS
    // should have a corresponding entry in REQUIRED_CHECK_NAMES
    for (workflow_file, expected_workflow_name) in REQUIRED_WORKFLOW_NAMES {
        let jobs: &[(&str, &str, &str)] = if *workflow_file == "ci.yml" {
            REQUIRED_CI_JOBS
        } else if *workflow_file == "doc-validation.yml" {
            REQUIRED_DOC_VALIDATION_JOBS
        } else {
            continue;
        };

        for (_job_key, display_name, _description) in jobs {
            // Matrix jobs expand to multiple check names; non-matrix jobs
            // produce exactly one.
            let expected_check_names =
                expand_matrix_display_name(expected_workflow_name, display_name);
            for expected_check_name in &expected_check_names {
                if !REQUIRED_CHECK_NAMES.contains(&expected_check_name.as_str()) {
                    errors.push(format!(
                        "Job \"{display_name}\" in {workflow_file} \
                         (workflow \"{expected_workflow_name}\") would produce check name \
                         \"{expected_check_name}\" but it is not in REQUIRED_CHECK_NAMES.\n\
                         To fix: Add \"{expected_check_name}\" to REQUIRED_CHECK_NAMES."
                    ));
                }
            }
        }
    }

    if !errors.is_empty() {
        panic!(
            "Required check naming contract self-consistency check failed:\n\n{}\n\n\
             The REQUIRED_CHECK_NAMES constant must be decomposable into valid\n\
             \"{{workflow_name}} / {{job_display_name}}\" pairs where both components\n\
             exist in the corresponding constant arrays.\n\n\
             This test catches drift between constants without requiring file I/O.",
            errors.join("\n\n")
        );
    }
}

/// Helper function to find all files with a given extension, excluding specified directories
fn find_files_with_extension(root: &Path, extension: &str, exclude_dirs: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();

    fn visit_dirs(dir: &Path, extension: &str, exclude_dirs: &[&str], files: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();

                // Skip excluded directories
                if path.is_dir() {
                    let dir_name = path.file_name().unwrap().to_string_lossy();
                    if exclude_dirs.iter().any(|&excl| dir_name == excl) {
                        continue;
                    }
                    visit_dirs(&path, extension, exclude_dirs, files);
                } else if path
                    .extension()
                    .map(|ext| ext == extension)
                    .unwrap_or(false)
                {
                    files.push(path);
                }
            }
        }
    }

    visit_dirs(root, extension, exclude_dirs, &mut files);
    files
}

fn markdown_is_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return false;
    }

    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return false;
    }

    trimmed
        .chars()
        .nth(hashes)
        .map(|c| c == ' ' || c == '\t')
        .unwrap_or(true)
}

fn collect_markdown_heading_blank_line_violations(file: &Path, content: &str) -> Vec<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut violations = Vec::new();
    let mut in_fenced_code = vec![false; lines.len()];
    let mut in_code_block = false;
    let mut opening_fence_char = '\0';
    let mut opening_fence_count = 0usize;

    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let first_char = trimmed.chars().next().unwrap_or('\0');
        let is_fence_char = first_char == '`' || first_char == '~';

        if is_fence_char {
            let fence_count = trimmed.chars().take_while(|&c| c == first_char).count();
            if fence_count >= 3 {
                in_fenced_code[idx] = true;
                let fence_suffix = trimmed[fence_count..].trim();
                if in_code_block {
                    if first_char == opening_fence_char
                        && fence_count >= opening_fence_count
                        && fence_suffix.is_empty()
                    {
                        in_code_block = false;
                        opening_fence_char = '\0';
                        opening_fence_count = 0;
                    }
                } else {
                    in_code_block = true;
                    opening_fence_char = first_char;
                    opening_fence_count = fence_count;
                }
                continue;
            }
        }

        in_fenced_code[idx] = in_code_block;
    }

    let mut is_html_comment_line = vec![false; lines.len()];
    let mut in_html_comment_block = false;

    for (idx, line) in lines.iter().enumerate() {
        if in_fenced_code[idx] {
            continue;
        }

        let trimmed = line.trim_start();
        if in_html_comment_block {
            is_html_comment_line[idx] = true;
            if trimmed.contains("-->") {
                in_html_comment_block = false;
            }
            continue;
        }

        if trimmed.starts_with("<!--") {
            is_html_comment_line[idx] = true;
            if !trimmed.contains("-->") {
                in_html_comment_block = true;
            }
        }
    }

    for (idx, line) in lines.iter().enumerate() {
        if in_fenced_code[idx] || !markdown_is_heading(line) {
            continue;
        }

        if idx > 0 {
            let previous = lines[idx - 1];
            if !previous.trim().is_empty() && !is_html_comment_line[idx - 1] {
                violations.push(format!(
                    "{}:{}: heading must be preceded by a blank line (MD022).\n  \
                     Heading: {}\n  \
                     Previous: {}",
                    file.display(),
                    idx + 1,
                    line.trim(),
                    previous.trim()
                ));
            }
        }

        if idx + 1 < lines.len() {
            let next = lines[idx + 1];
            if !next.trim().is_empty() && !is_html_comment_line[idx + 1] {
                violations.push(format!(
                    "{}:{}: heading must be followed by a blank line (MD022).\n  \
                     Heading: {}\n  \
                     Next: {}",
                    file.display(),
                    idx + 1,
                    line.trim(),
                    next.trim()
                ));
            }
        }
    }

    violations
}

#[test]
fn test_markdown_headings_have_surrounding_blank_lines() {
    // Regression guard for markdownlint MD022 failures caused by headings that
    // touch list/paragraph content without a separating blank line.
    let root = repo_root();

    // Data-driven input set: markdown files that follow repository lint policy.
    let file_sets = [(
        "repository markdown",
        find_files_with_extension(
            &root,
            "md",
            &["target", "third_party", "node_modules", "test-fixtures"],
        ),
    )];

    let mut violations = Vec::new();
    let mut checked_files = 0usize;

    for (set_name, files) in file_sets {
        assert!(
            !files.is_empty(),
            "{set_name}: expected at least one markdown file to validate"
        );

        for file in files {
            checked_files += 1;
            let content = read_file(&file);
            violations.extend(collect_markdown_heading_blank_line_violations(
                &file, &content,
            ));
        }
    }

    assert!(
        checked_files > 0,
        "Expected to validate at least one markdown file for heading spacing rules"
    );

    assert!(
        violations.is_empty(),
        "Markdown heading spacing violations found (MD022):\n\n{}\n\n\
         Fix by inserting a blank line before and after each heading outside fenced code blocks.",
        violations.join("\n\n")
    );
}

/// Collect all YAML workflow files from the given directory.
///
/// Returns a sorted list of directory entries for `.yml` and `.yaml` files.
/// Panics if the directory exists but cannot be read.
fn collect_workflow_files(workflows_dir: &Path) -> Vec<std::fs::DirEntry> {
    if !workflows_dir.exists() {
        return Vec::new();
    }
    let mut files: Vec<_> = std::fs::read_dir(workflows_dir)
        .expect("Failed to read workflows directory")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "yml" || ext == "yaml")
                .unwrap_or(false)
        })
        .collect();
    // Sort for deterministic ordering across test runs
    files.sort_by_key(|e| e.file_name());
    files
}

/// Return `true` if `reference` is a 40-character hexadecimal commit hash.
fn is_commit_hash(reference: &str) -> bool {
    reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit())
}

/// Extract `uses:` value from either `uses: ...` or `- uses: ...` YAML styles.
fn extract_uses_value(trimmed_line: &str) -> Option<&str> {
    trimmed_line
        .strip_prefix("uses:")
        .or_else(|| trimmed_line.strip_prefix("- uses:"))
        .map(str::trim)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionReferenceParseResult<'a> {
    LocalOrDocker,
    Remote {
        action_name: &'a str,
        action_ref: &'a str,
    },
    MalformedRemote {
        reason: &'static str,
    },
}

/// Parse and classify a `uses:` value.
///
/// Distinguishes local/docker actions from malformed remote references so policy
/// tests can fail on invalid `owner/repo@ref` syntax.
fn classify_action_reference(uses_value: &str) -> ActionReferenceParseResult<'_> {
    // Keep only the first token to ignore trailing inline comments.
    let token = uses_value
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .trim_matches('\'');

    if token.is_empty() {
        return ActionReferenceParseResult::MalformedRemote {
            reason: "empty uses value",
        };
    }

    if token.starts_with("./") || token.starts_with("docker://") {
        return ActionReferenceParseResult::LocalOrDocker;
    }

    let Some((action_name, action_ref)) = token.split_once('@') else {
        return ActionReferenceParseResult::MalformedRemote {
            reason: "missing '@' separator",
        };
    };

    let action_name = action_name.trim();
    let action_ref = action_ref.trim();

    if action_name.is_empty() {
        return ActionReferenceParseResult::MalformedRemote {
            reason: "missing action name before '@'",
        };
    }

    if action_ref.is_empty() {
        return ActionReferenceParseResult::MalformedRemote {
            reason: "missing action ref after '@'",
        };
    }

    // Require `owner/repo` minimum shape, allowing optional subpaths.
    let mut segments = action_name.split('/');
    let owner = segments.next().unwrap_or("");
    let repo = segments.next().unwrap_or("");
    if owner.is_empty() || repo.is_empty() {
        return ActionReferenceParseResult::MalformedRemote {
            reason: "remote action must use owner/repo@ref syntax",
        };
    }

    ActionReferenceParseResult::Remote {
        action_name,
        action_ref,
    }
}

/// Parse remote action reference from a `uses` value.
///
/// Returns `(action_name, action_ref)` for valid remote actions and `None` for
/// local/docker actions or malformed references.
fn parse_remote_action_reference(uses_value: &str) -> Option<(&str, &str)> {
    match classify_action_reference(uses_value) {
        ActionReferenceParseResult::Remote {
            action_name,
            action_ref,
        } => Some((action_name, action_ref)),
        ActionReferenceParseResult::LocalOrDocker
        | ActionReferenceParseResult::MalformedRemote { .. } => None,
    }
}

/// Return `true` if the action reference uses an approved explicit-version format.
///
/// Allowed:
/// - `vX`, `vX.Y`, `vX.Y.Z`, optional prerelease/build suffixes
///
/// Disallowed:
/// - moving channels/branches: `stable`, `beta`, `nightly`, `main`, `master`, `latest`
/// - commit hashes (40-char hex)
fn is_action_version_ref(reference: &str) -> bool {
    if reference.is_empty() || is_commit_hash(reference) {
        return false;
    }

    if matches!(
        reference,
        "stable" | "beta" | "nightly" | "main" | "master" | "latest"
    ) {
        return false;
    }

    let Some(version) = reference.strip_prefix('v') else {
        return false;
    };

    let mut chars = version.chars();
    let Some(first_char) = chars.next() else {
        return false;
    };
    if !first_char.is_ascii_digit() {
        return false;
    }

    version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

/// Truncate a semver string to `major.minor` format.
///
/// Examples:
/// - `"1.88.0"` → `"1.88"`
/// - `"1.88"` → `"1.88"` (already short)
fn to_major_minor(version: &str) -> String {
    version.split('.').take(2).collect::<Vec<_>>().join(".")
}

// ============================================================================
// Link Check Tests
// ============================================================================

#[test]
fn test_lychee_config_exists_and_is_valid() {
    // This test ensures the lychee link checker configuration exists and is valid
    // Prevents link checker failures due to missing or malformed configuration

    let root = repo_root();
    let lychee_config = root.join(".lychee.toml");

    assert!(
        lychee_config.exists(),
        ".lychee.toml configuration file is missing.\n\
         This file is required for link checking in CI.\n\
         See .github/workflows/link-check.yml"
    );

    let content = read_file(&lychee_config);

    // Check for required sections
    let required_fields = vec![
        ("max_concurrency", "Controls parallel link checking"),
        ("accept", "Accepted HTTP status codes"),
        ("exclude", "URLs to exclude from checking"),
        ("timeout", "Request timeout in seconds"),
    ];

    let mut missing_fields = Vec::new();
    for (field, description) in required_fields {
        if !content.contains(field) {
            missing_fields.push(format!("  - {field} ({description})"));
        }
    }

    if !missing_fields.is_empty() {
        panic!(
            ".lychee.toml is missing required fields:\n\n{}\n\n\
             These fields are required for proper link checking.\n\
             Add them to .lychee.toml following the lychee documentation.",
            missing_fields.join("\n")
        );
    }
}

#[test]
fn test_lychee_excludes_placeholder_urls() {
    // This test verifies that placeholder URLs are properly excluded in .lychee.toml
    // Prevents link checker failures on example/placeholder URLs in documentation
    //
    // Background: Documentation often includes placeholder URLs like:
    // - https://github.com/owner/repo
    // - https://github.com/{}
    // - http://localhost:3000
    // These should be excluded to avoid false failures
    //
    // Note: .lychee.toml exclude patterns are **regex** (not globs), so we must
    // compile them and test for matches rather than checking literal substrings.

    let root = repo_root();
    let lychee_config = root.join(".lychee.toml");
    let content = read_file(&lychee_config);

    // Parse the exclude array from .lychee.toml by extracting quoted strings
    // between `exclude = [` and the closing `]`.
    let exclude_patterns = parse_lychee_exclude_patterns(&content);
    assert!(
        !exclude_patterns.is_empty(),
        ".lychee.toml must contain an 'exclude' array with at least one pattern"
    );

    // Compile all exclude patterns as regexes (just like lychee does)
    let compiled: Vec<(&str, regex::Regex)> = exclude_patterns
        .iter()
        .map(|p| {
            let re = regex::Regex::new(p)
                .unwrap_or_else(|e| panic!("Invalid regex in .lychee.toml exclude: {p:?}: {e}"));
            (p.as_str(), re)
        })
        .collect();

    // Define test cases: (url, reason)
    let test_cases: &[(&str, &str)] = &[
        ("http://localhost", "Localhost URLs are placeholders"),
        (
            "http://localhost:3000",
            "Localhost with port is a placeholder",
        ),
        ("https://localhost", "HTTPS localhost is a placeholder"),
        ("http://127.0.0.1", "Loopback IPs are placeholders"),
        ("http://0.0.0.0", "Unspecified IPs are placeholders"),
        ("ws://localhost", "WebSocket localhost is placeholder"),
        (
            "wss://localhost",
            "Secure WebSocket localhost is placeholder",
        ),
        ("mailto:", "Email addresses should be excluded"),
        (
            "https://github.com/owner/repo/",
            "Generic placeholder pattern",
        ),
        ("https://github.com/{}/", "Template placeholder pattern"),
        (
            "https://github.com/{}/releases",
            "Template placeholder with path suffix",
        ),
        ("http://your-server/", "Placeholder server URL"),
        // Truncated URLs extracted by lychee from regex patterns in .lychee.toml
        // itself (defense-in-depth in case exclude_path fails for dotfiles)
        ("https://github/", "Truncated URL from .lychee.toml regex"),
        ("https://github", "Truncated URL without trailing slash"),
        ("https://lib/", "Truncated URL from .lychee.toml regex"),
        ("https://lib", "Truncated URL without trailing slash"),
        // file:// protocol for local file links
        ("file:///tmp/foo", "Local file URLs should be excluded"),
        // Anchor-only links (same-page references)
        ("#section-heading", "Anchor-only links should be excluded"),
        // lib.rs returns 403 for automated checks
        (
            "https://lib.rs/crates/foo",
            "lib.rs returns 403 for automated checks",
        ),
        // URL-encoded brace placeholders
        (
            "https://github.com/%7Buser%7D",
            "URL-encoded brace placeholder should be excluded",
        ),
        (
            "https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.2.0...HEAD",
            "Unreleased compare links may exist before tags are pushed",
        ),
        (
            "https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...v0.2.0",
            "Release compare links may briefly 404 during release cutover",
        ),
    ];

    let mut missing_exclusions = Vec::new();
    for &(url, reason) in test_cases {
        let matched = compiled.iter().any(|(_, re)| re.is_match(url));
        if !matched {
            let tried: Vec<String> = compiled
                .iter()
                .map(|(pat, _)| format!("    {pat:?}"))
                .collect();
            missing_exclusions.push(format!(
                "  - URL: {url}\n    Reason: {reason}\n    Patterns tried:\n{}",
                tried.join("\n")
            ));
        }
    }

    if !missing_exclusions.is_empty() {
        panic!(
            ".lychee.toml exclude patterns do not match these placeholder URLs:\n\n{}\n\n\
             Add or fix regex patterns in the 'exclude' list in .lychee.toml.\n\
             Remember: exclude values are regex, not literal strings.\n",
            missing_exclusions.join("\n\n")
        );
    }
}

/// Parse the `exclude = [...]` array from `.lychee.toml` content, returning
/// the list of unescaped string values (regex patterns).
fn parse_lychee_exclude_patterns(content: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut in_exclude = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Detect the start of the exclude array
        if trimmed.starts_with("exclude") && trimmed.contains('[') {
            // Could also be `exclude_path` or `exclude_link_local` — only match bare `exclude`
            let key = trimmed.split('=').next().unwrap_or("").trim();
            if key != "exclude" {
                continue;
            }
            in_exclude = true;
            // If the opening `[` and closing `]` are on the same line, handle inline
            if trimmed.contains(']') {
                extract_quoted_strings(trimmed, &mut patterns);
                in_exclude = false;
            }
            continue;
        }

        if in_exclude {
            if trimmed.starts_with(']') {
                break;
            }
            extract_quoted_strings(trimmed, &mut patterns);
        }
    }

    patterns
}

/// Extract double-quoted strings from a line, stripping comments.
fn extract_quoted_strings(line: &str, out: &mut Vec<String>) {
    // Strip trailing `# comment`
    let without_comment = strip_trailing_comment(line);
    let mut chars = without_comment.chars();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            let mut s = String::new();
            loop {
                match chars.next() {
                    None | Some('"') => break,
                    Some('\\') => {
                        // TOML basic string escape sequences:
                        // `\\` -> `\`, `\"` -> `"`, `\n` -> newline, etc.
                        // In .lychee.toml, regex backslashes are written as `\\`
                        // (e.g., `\\.` in TOML source becomes `\.` as a regex).
                        if let Some(next) = chars.next() {
                            match next {
                                '\\' => s.push('\\'),
                                '"' => s.push('"'),
                                'n' => s.push('\n'),
                                't' => s.push('\t'),
                                'r' => s.push('\r'),
                                // For any other char, preserve both (lenient)
                                other => {
                                    s.push('\\');
                                    s.push(other);
                                }
                            }
                        }
                    }
                    Some(c) => s.push(c),
                }
            }
            out.push(s);
        }
    }
}

/// Strip a trailing `# comment` from a TOML line, being careful not to
/// strip `#` that appears inside a quoted string.
fn strip_trailing_comment(line: &str) -> &str {
    let mut in_quote = false;
    let mut prev_backslash = false;
    for (i, ch) in line.char_indices() {
        if ch == '"' && !prev_backslash {
            in_quote = !in_quote;
        }
        if ch == '#' && !in_quote {
            return &line[..i];
        }
        // After `\\`, reset so the next char is not treated as escaped.
        prev_backslash = ch == '\\' && !prev_backslash;
    }
    line
}

#[test]
fn test_strip_trailing_comment() {
    // Basic comment stripping
    assert_eq!(strip_trailing_comment("value # comment"), "value ");
    assert_eq!(strip_trailing_comment("no comment"), "no comment");

    // Preserves # inside quoted strings
    assert_eq!(
        strip_trailing_comment(r#""pattern#with#hash""#),
        r#""pattern#with#hash""#
    );

    // Strips comment after quoted string
    assert_eq!(
        strip_trailing_comment(r#""value" # comment"#),
        r#""value" "#
    );

    // Handles escaped quotes inside strings
    assert_eq!(
        strip_trailing_comment(r#""escaped\"quote" # comment"#),
        r#""escaped\"quote" "#
    );

    // Handles double-backslash before closing quote (not an escape)
    assert_eq!(
        strip_trailing_comment(r#""ends_with_backslash\\" # comment"#),
        r#""ends_with_backslash\\" "#
    );
}

#[test]
fn test_extract_quoted_strings() {
    let mut out = Vec::new();

    // Basic string extraction
    extract_quoted_strings(r#""hello""#, &mut out);
    assert_eq!(out, vec!["hello"]);

    // Multiple strings
    out.clear();
    extract_quoted_strings(r#""a", "b""#, &mut out);
    assert_eq!(out, vec!["a", "b"]);

    // TOML escape: \\ becomes single backslash
    out.clear();
    extract_quoted_strings(r#""^https?://127\\.0""#, &mut out);
    assert_eq!(out, vec![r"^https?://127\.0"]);

    // TOML escape: \{ and \} preserved as-is (lenient fallback)
    out.clear();
    extract_quoted_strings(r#""\\{\\}""#, &mut out);
    assert_eq!(out, vec![r"\{\}"]);
}

#[test]
fn test_parse_lychee_exclude_patterns() {
    // Parses only the `exclude` array, not `exclude_path` or `exclude_link_local`
    let content = r#"
exclude = [
    "^https?://localhost",
    "^mailto:",
]

exclude_path = [
    "target/",
    "tests/",
]

exclude_link_local = true
"#;
    let patterns = parse_lychee_exclude_patterns(content);
    assert_eq!(patterns, vec!["^https?://localhost", "^mailto:"]);
}

#[test]
fn test_no_actual_placeholder_urls_in_docs() {
    // This test ensures documentation prose doesn't contain placeholder URLs
    // that should be replaced with real URLs.
    //
    // Scope: Only checks non-code content (code blocks and inline code are excluded
    // because example/tutorial docs legitimately show placeholder patterns).
    // The .llm/ directory is excluded because it documents CI patterns themselves.

    let root = repo_root();
    let markdown_files = find_files_with_extension(&root, "md", &["target", "third_party", ".llm"]);

    // Patterns that indicate a placeholder URL in prose text
    let suspicious_patterns: &[(&str, &str)] = &[
        (
            r"https://github\.com/owner/repo",
            "Generic owner/repo placeholder - replace with actual repo URL",
        ),
        (
            r"https://github\.com/\{\}",
            "Template curly brace placeholder - replace with actual owner/repo",
        ),
        (
            r"https?://example\.com(?!/)",
            "Generic example.com URL - use a real example or inline code",
        ),
        (
            r"http://your-server",
            "Generic your-server placeholder - replace with actual server URL",
        ),
    ];

    // Compile regexes once before the loops for performance.
    // Patterns that fail to compile (e.g., those using unsupported lookahead syntax) are
    // skipped, preserving the original behavior of the per-line `if let Ok(regex)` guard.
    let compiled_suspicious: Vec<(regex::Regex, &str, &str)> = suspicious_patterns
        .iter()
        .filter_map(|(pattern, description)| {
            regex::Regex::new(pattern)
                .ok()
                .map(|re| (re, *pattern, *description))
        })
        .collect();

    let mut violations = Vec::new();

    for file in markdown_files {
        let content = read_file(&file);
        let mut in_code_block = false;
        let mut opening_backtick_count: usize = 0;

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim_start();

            // Track fenced code block state per CommonMark spec:
            // - Opening fence: 3+ backticks, may have info string (e.g., ```rust)
            // - Closing fence: 3+ backticks with NO info string (bare backticks only)
            // - Closing fence must have >= as many backticks as the opening fence
            // When inside a code block, only a bare fence closes it; inner ```rust
            // lines are content, not real fences.
            let backtick_count = trimmed.len() - trimmed.trim_start_matches('`').len();
            if backtick_count >= 3 {
                let after_backticks = trimmed[backtick_count..].trim();
                if in_code_block {
                    if after_backticks.is_empty() && backtick_count >= opening_backtick_count {
                        in_code_block = false;
                    }
                } else {
                    in_code_block = true;
                    opening_backtick_count = backtick_count;
                }
                continue;
            }

            // Skip lines inside code blocks - placeholder URLs in examples are intentional
            if in_code_block {
                continue;
            }

            // Skip lines that are entirely inline code (single-backtick) - these are examples
            if trimmed.starts_with('`') && trimmed.ends_with('`') && trimmed.len() > 2 {
                continue;
            }

            // Strip inline code segments before checking to avoid false positives
            // e.g., "use `https://github.com/owner/repo` as the pattern" should not flag
            let line_without_inline_code = {
                let mut result = String::new();
                let mut in_inline = false;
                for ch in line.chars() {
                    if ch == '`' {
                        in_inline = !in_inline;
                    } else if !in_inline {
                        result.push(ch);
                    }
                }
                result
            };

            for (regex, pattern, description) in &compiled_suspicious {
                if regex.is_match(&line_without_inline_code) {
                    violations.push(format!(
                        "{}:{}: Placeholder URL in documentation prose\n  \
                         Pattern: {}\n  \
                         Description: {}\n  \
                         Fix: Replace with a real URL or move into a code block\n  \
                         Verify: grep -n '{}' {}\n  \
                         Line: {}",
                        file.display(),
                        line_num,
                        pattern,
                        description,
                        pattern,
                        file.display(),
                        line.trim()
                    ));
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Documentation prose contains placeholder URLs that should be replaced:\n\n{}\n\n\
             Placeholder URLs in prose text break link checks and look unprofessional.\n\
             Options:\n\
             1. Replace with the actual URL for this project\n\
             2. Wrap in backticks to mark as a code example: `https://github.com/owner/repo`\n\
             3. Move to a fenced code block if showing a full example\n\
             4. If intentional, add the URL pattern to .lychee.toml exclude list",
            violations.join("\n\n")
        );
    }
}

#[test]
fn test_lychee_config_format_is_valid_toml() {
    // This test validates that .lychee.toml is valid TOML
    // Catches syntax errors before they cause CI failures

    let root = repo_root();
    let lychee_config = root.join(".lychee.toml");
    let content = read_file(&lychee_config);

    // Basic TOML validation (full validation would require a TOML parser)
    // Check for unbalanced quotes
    let double_quotes = content.matches('"').count();
    if double_quotes % 2 != 0 {
        panic!(
            ".lychee.toml has unbalanced quotes.\n\
             Found {double_quotes} double quotes (should be even).\n\
             Check for missing closing quotes."
        );
    }

    // Check for required array syntax
    if content.contains("exclude") {
        assert!(
            content.contains("exclude = ["),
            ".lychee.toml: 'exclude' should be an array (exclude = [...])"
        );
    }

    if content.contains("accept") {
        assert!(
            content.contains("accept = ["),
            ".lychee.toml: 'accept' should be an array (accept = [...])"
        );
    }

    // Check for common TOML mistakes
    if content.contains("= true") || content.contains("= false") {
        // Booleans are valid, but check they're not quoted
        if content.contains("= \"true\"") || content.contains("= \"false\"") {
            panic!(
                ".lychee.toml: Boolean values should not be quoted.\n\
                 Use 'field = true' not 'field = \"true\"'"
            );
        }
    }
}

// ============================================================================
// Markdown Lint Tests
// ============================================================================

#[test]
fn test_markdown_no_capitalized_filenames_in_links() {
    // This test catches improperly capitalized filenames in markdown links
    // Prevents link breakage on case-sensitive filesystems
    //
    // Example violations:
    // - [link](README.MD) when file is README.md
    // - [link](Docs/Config.md) when path is docs/config.md

    let root = repo_root();
    let markdown_files = find_files_with_extension(&root, "md", &["target", "third_party"]);

    let mut violations = Vec::new();

    // Compile regex once outside the loop for better performance
    let link_regex = regex::Regex::new(r"\[([^]]+)\]\(([^)]+)\)").unwrap();

    for file in markdown_files {
        let content = read_file(&file);

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;

            // Extract markdown links: [text](url)
            if let Some(captures) = link_regex.captures(line) {
                let url = captures.get(2).map(|m| m.as_str()).unwrap_or("");

                // Skip external URLs
                if url.starts_with("http://") || url.starts_with("https://") {
                    continue;
                }

                // Check for uppercase file extensions (.MD, .TOML, .RS, etc.)
                if url.ends_with(".MD")
                    || url.ends_with(".TOML")
                    || url.ends_with(".RS")
                    || url.ends_with(".JSON")
                    || url.ends_with(".YAML")
                    || url.ends_with(".YML")
                {
                    violations.push(format!(
                        "{}:{}: Link has uppercase file extension: {}\n  \
                         Use lowercase extensions (.md not .MD) for cross-platform compatibility",
                        file.display(),
                        line_num,
                        url
                    ));
                }

                // Check for capitalized directory names in relative links
                // This is a heuristic check - may need refinement
                if url.contains("/Docs/") || url.contains("/Scripts/") || url.contains("/Tests/") {
                    violations.push(format!(
                        "{}:{}: Link contains capitalized directory: {}\n  \
                         Use lowercase directory names for consistency",
                        file.display(),
                        line_num,
                        url
                    ));
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Markdown files contain links with improper capitalization:\n\n{}\n\n\
             Fix by using lowercase file extensions and directory names.\n\
             This prevents link breakage on case-sensitive filesystems (Linux, macOS).",
            violations.join("\n")
        );
    }
}

// Regex pattern for stripping markdown link URLs: [text](url) -> [text]
const MD_URL_STRIP_PATTERN: &str = r"\]\([^)]*\)";

// Regex pattern for stripping raw URLs (covers HTML attributes like href="...", src="...",
// angle-bracket URLs <https://...>, and bare URLs in text). Uses \S+ to intentionally
// over-strip trailing punctuation/delimiters (e.g., a period after a URL), since the goal
// is removal for capitalization checking, not precise URL extraction.
const RAW_URL_STRIP_PATTERN: &str = r"(?:https?|wss?|ftp)://\S+";

// Regex pattern for stripping HTML elements (opening, closing, and self-closing tags)
// to match .markdownlint.json MD044 "html_elements": false behavior, which skips
// content within HTML elements when checking proper noun capitalization.
const HTML_ELEMENT_PATTERN: &str = r"<[^>]+>";

#[test]
fn test_markdown_technical_terms_consistency() {
    // This test validates that technical terms use consistent capitalization
    // Prevents documentation inconsistency and improves professionalism
    //
    // Based on .markdownlint.json MD044 configuration

    let root = repo_root();

    // Data-driven test cases: (incorrect_pattern, correct_term, context)
    let test_cases = vec![
        (r"\bgithub\b", "GitHub", "Service name"),
        (r"\bwebsocket\b", "WebSocket", "Protocol name"),
        (r"\bjavascript\b", "JavaScript", "Language name"),
        (r"\bdocker\b", "Docker", "Container platform"),
        (r"\bci/cd\b", "CI/CD", "Continuous integration/deployment"),
    ];

    // Compile regexes once before the loops for performance
    let compiled_cases: Vec<(regex::Regex, &str, &str)> = test_cases
        .iter()
        .map(|(pattern, correct, context)| {
            (
                regex::Regex::new(pattern).expect("valid regex pattern"),
                *correct,
                *context,
            )
        })
        .collect();

    let markdown_files = find_files_with_extension(&root, "md", &["target", "third_party"]);
    let mut violations = Vec::new();

    // Compile URL-stripping and HTML-stripping regexes outside all loops to avoid
    // repeated allocations. See constant definitions for detailed documentation.
    let url_strip_regex =
        regex::Regex::new(MD_URL_STRIP_PATTERN).expect("valid url-strip regex pattern");
    let raw_url_regex =
        regex::Regex::new(RAW_URL_STRIP_PATTERN).expect("valid raw-url-strip regex pattern");
    let html_element_regex =
        regex::Regex::new(HTML_ELEMENT_PATTERN).expect("valid html-element regex pattern");

    for file in markdown_files {
        let content = read_file(&file);

        // Track fenced code block state to match MD044's "code_blocks": false behavior
        let mut in_code_block = false;
        let mut opening_backtick_count: usize = 0;

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;

            // Track fenced code block state per CommonMark spec:
            // - Opening fence: 3+ backticks, may have info string (e.g., ```rust)
            // - Closing fence: 3+ backticks with NO info string (just backticks + optional spaces)
            // - Closing fence must have >= as many backticks as the opening fence
            // When already inside a code block, only a bare fence (no info string) closes it.
            // This correctly handles nested code examples in markdown skill docs where
            // inner ```rust fences are content, not real fences.
            let trimmed = line.trim_start();
            let backtick_prefix_len = trimmed.len() - trimmed.trim_start_matches('`').len();
            if backtick_prefix_len >= 3 {
                let after_backticks = trimmed[backtick_prefix_len..].trim();
                if in_code_block {
                    // Inside a code block: only a bare fence line (no info string) closes it,
                    // and it must have at least as many backticks as the opening fence
                    if after_backticks.is_empty() && backtick_prefix_len >= opening_backtick_count {
                        in_code_block = false;
                    }
                    // Lines like ```rust inside a code block are just content
                } else {
                    // Outside a code block: any 3+ backtick line opens one
                    in_code_block = true;
                    opening_backtick_count = backtick_prefix_len;
                }
                continue;
            }

            // Skip lines inside fenced code blocks
            if in_code_block {
                continue;
            }

            // Skip lines containing inline code (backticks) - file paths, commands, etc.
            if line.contains('`') {
                continue;
            }

            // Strip content that should not be checked for capitalization:
            // 1. Markdown link URLs: [text](url) -> [text]
            // 2. HTML elements: <a href="...">text</a> -> text (MD044 html_elements: false)
            // 3. Raw URLs: https://github.io/... -> ""
            let without_md_urls = url_strip_regex.replace_all(line, "]");
            let without_html = html_element_regex.replace_all(&without_md_urls, "");
            let line_no_urls = raw_url_regex.replace_all(&without_html, "");

            for (regex, correct, context) in &compiled_cases {
                if regex.is_match(&line_no_urls) {
                    violations.push(format!(
                        "{}:{}: Incorrect capitalization: should be '{}'\n  \
                         Context: {}\n  \
                         Line: {}",
                        file.display(),
                        line_num,
                        correct,
                        context,
                        line.trim()
                    ));
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Found inconsistent technical term capitalization:\n\n{}\n\n\
             Fix the capitalization in the files above.\n\
             If markdownlint MD044 should catch these, verify .markdownlint.json 'names' array \
             is configured correctly.",
            violations.join("\n\n")
        );
    }
}

#[test]
fn test_technical_terms_url_stripping_skips_urls() {
    // Validates that the URL-stripping and HTML-stripping logic in
    // test_markdown_technical_terms_consistency correctly removes URLs and HTML elements
    // before checking for technical term capitalization.
    // URLs contain domain names (github.io, docker.com) that are correctly lowercase
    // and must not be flagged as capitalization violations.

    let url_strip_regex =
        regex::Regex::new(MD_URL_STRIP_PATTERN).expect("valid url-strip regex pattern");
    let raw_url_regex =
        regex::Regex::new(RAW_URL_STRIP_PATTERN).expect("valid raw-url-strip regex pattern");
    let html_element_regex =
        regex::Regex::new(HTML_ELEMENT_PATTERN).expect("valid html-element regex pattern");
    let github_regex = regex::Regex::new(r"\bgithub\b").expect("valid regex");

    // Lines that contain "github" only inside URLs or HTML -- must NOT match after stripping
    let should_not_match = vec![
        // HTML href attribute
        r#"<a href="https://ambiguous-interactive.github.io/signal-fish-server/">"#,
        // HTML src attribute with URL-encoded term
        r#"<img src="https://img.shields.io/badge/docs-GitHub%20Pages-blue?style=for-the-badge""#,
        // Markdown link URL
        "[Documentation](https://ambiguous-interactive.github.io/signal-fish-server/)",
        // Raw URL in text
        "Visit https://github.com/owner/repo for details",
        // Angle-bracket autolink
        "<https://github.io/some-project>",
        // Multiple URLs on one line
        r#"<a href="https://github.io/a"><img src="https://github.io/b"></a>"#,
        // HTML element with lowercase term in attribute (html_elements: false parity)
        r#"<a title="github project" href="https://example.com">Link</a>"#,
        // wss:// URL with term in domain
        "Connect to wss://github.example.com/ws for live updates",
        // ftp:// URL with term in path
        "Download from ftp://files.github.example.com/archive.tar.gz",
    ];

    for line in &should_not_match {
        let without_md_urls = url_strip_regex.replace_all(line, "]");
        let without_html = html_element_regex.replace_all(&without_md_urls, "");
        let line_no_urls = raw_url_regex.replace_all(&without_html, "");
        assert!(
            !github_regex.is_match(&line_no_urls),
            "URL/HTML stripping should have removed 'github' from line, \
             but '{line_no_urls}' still matches in: {line}",
        );
    }

    // Lines that contain "github" outside URLs -- must still match after stripping
    let should_still_match = vec![
        "Please use github for your source hosting",
        "The github integration is broken",
        // Mixed content: URL followed by text containing the term
        "Visit https://github.com/repo. Use github locally.",
    ];

    for line in &should_still_match {
        let without_md_urls = url_strip_regex.replace_all(line, "]");
        let without_html = html_element_regex.replace_all(&without_md_urls, "");
        let line_no_urls = raw_url_regex.replace_all(&without_html, "");
        assert!(
            github_regex.is_match(&line_no_urls),
            "URL stripping should NOT have removed 'github' from line: {line}",
        );
    }
}

#[test]
fn test_code_block_fence_tracking_commonmark_compliant() {
    // This test validates that the CommonMark-correct code block fence tracking logic
    // handles all markdown files without mismatched fences.
    //
    // Background: The previous code block tracking used a blind toggle
    // (`in_code_block = !in_code_block`) which broke on nested code fences in markdown
    // skill docs. The fix uses proper CommonMark parsing:
    //   - Opening fences can have info strings (e.g., ```rust, ```bash)
    //   - Closing fences must be bare (just backticks + optional whitespace)
    //   - Per CommonMark spec (section 4.5), a closing fence must have at least
    //     as many backtick characters as the opening fence. This means a ````
    //     (4-backtick) block is not closed by a ``` (3-backtick) line.
    //
    // This test ensures every markdown file has balanced fence opens/closes,
    // meaning the parser ends outside any code block after processing the entire file.

    let root = repo_root();
    // Exclude test-fixtures which may contain intentionally malformed markdown
    let markdown_files =
        find_files_with_extension(&root, "md", &["target", "third_party", "test-fixtures"]);

    assert!(
        !markdown_files.is_empty(),
        "Expected to find markdown files in the repository"
    );

    let mut violations = Vec::new();

    for file in &markdown_files {
        let content = read_file(file);

        let mut in_code_block = false;
        let mut opens = 0usize;
        let mut closes = 0usize;
        let mut last_open_line = 0usize;
        let mut opening_backtick_count: usize = 0;

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim_start();

            // Count the leading backtick characters
            let backtick_count = trimmed.len() - trimmed.trim_start_matches('`').len();
            if backtick_count >= 3 {
                let after_backticks = trimmed[backtick_count..].trim();
                if in_code_block {
                    // Inside a code block: only a bare fence closes it, and
                    // per CommonMark spec, the closing fence must have at least
                    // as many backticks as the opening fence.
                    if after_backticks.is_empty() && backtick_count >= opening_backtick_count {
                        in_code_block = false;
                        closes += 1;
                    }
                } else {
                    // Outside a code block: any 3+ backtick line opens one
                    // (may have an info string like ```rust or ```bash)
                    in_code_block = true;
                    opens += 1;
                    last_open_line = line_num;
                    opening_backtick_count = backtick_count;
                }
            }
        }

        // After processing the entire file, we must be outside any code block
        if in_code_block {
            violations.push(format!(
                "{}: Unclosed code block at end of file (last opened at line {}, opens={}, closes={})",
                file.display(),
                last_open_line,
                opens,
                closes,
            ));
        }

        // Opens and closes must balance
        if opens != closes {
            violations.push(format!(
                "{}: Mismatched fences: {} opens vs {} closes",
                file.display(),
                opens,
                closes,
            ));
        }
    }

    if !violations.is_empty() {
        panic!(
            "Code block fence tracking found CommonMark violations:\n\n{}\n\n\
             Fix: Ensure every opening fence (```) has a matching bare closing fence.\n\
             Opening fences may have info strings (e.g., ```rust), \
             but closing fences must be bare (just backticks).",
            violations.join("\n")
        );
    }
}

#[test]
fn test_markdown_common_patterns_are_correct() {
    // This test validates common markdown patterns are correctly formatted.
    // Catches issues that might slip through markdownlint rules.
    //
    // Note: MD040 (code blocks without language identifier) is intentionally excluded
    // here because test_markdown_files_have_language_identifiers provides full coverage
    // of that rule with proper code-block tracking.

    let root = repo_root();
    let markdown_files = find_files_with_extension(&root, "md", &["target", "third_party"]);

    // Test cases: (anti_pattern_regex, description, fix_command)
    // MD040 omitted - covered by test_markdown_files_have_language_identifiers
    let test_cases = [(
        r"\]\([A-Z]:/",
        "Windows absolute path in link",
        "Use forward slashes: sed -i 's/]([A-Z]:\\//)]/g' <file>",
    )];

    // Compile regexes once before the loops for performance
    let compiled_cases: Vec<(regex::Regex, &str, &str)> = test_cases
        .iter()
        .map(|(pattern, description, fix_cmd)| {
            (
                regex::Regex::new(pattern).expect("valid regex pattern"),
                *description,
                *fix_cmd,
            )
        })
        .collect();

    let mut violations = Vec::new();

    for file in &markdown_files {
        let content = read_file(file);
        let mut in_code_block = false;
        let mut opening_backtick_count: usize = 0;

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim_start();

            // Track fenced code block state per CommonMark spec:
            // Opening fences may have info strings; closing fences must be bare.
            // Closing fence must have >= as many backticks as the opening fence.
            let backtick_count = trimmed.len() - trimmed.trim_start_matches('`').len();
            if backtick_count >= 3 {
                let after_backticks = trimmed[backtick_count..].trim();
                if in_code_block {
                    if after_backticks.is_empty() && backtick_count >= opening_backtick_count {
                        in_code_block = false;
                    }
                } else {
                    in_code_block = true;
                    opening_backtick_count = backtick_count;
                }
                continue;
            }

            // Skip checking inside code blocks
            if in_code_block {
                continue;
            }

            for (regex, description, fix_cmd) in &compiled_cases {
                if regex.is_match(line) {
                    violations.push(format!(
                        "{}:{}: {}\n  \
                         Fix: {}\n  \
                         Verify: grep -n '{}' {}\n  \
                         Line: {}",
                        file.display(),
                        line_num,
                        description,
                        fix_cmd,
                        regex.as_str(),
                        file.display(),
                        line.trim()
                    ));
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Markdown files contain formatting violations:\n\n{}\n\n\
             These patterns cause rendering or portability issues.\n\
             Fix each violation using the command shown above.",
            violations.join("\n\n")
        );
    }
}

// ============================================================================
// AWK Script Testing
// ============================================================================

#[test]
fn test_doc_validation_awk_script_extraction() {
    // This test validates the AWK scripts used by doc-validation for Rust
    // code block extraction. The AWK logic may live inline in the workflow
    // or in an external script (.github/scripts/extract-rust-blocks.awk).
    //
    // Background: The doc-validation.yml workflow uses AWK scripts to extract
    // and validate code blocks from markdown. These scripts need validation
    // to prevent issues like the AWK pattern bug we fixed.

    let root = repo_root();
    let workflow = root.join(".github/workflows/doc-validation.yml");
    let external_awk = root.join(".github/scripts/extract-rust-blocks.awk");

    if !workflow.exists() {
        panic!(
            "doc-validation.yml workflow not found at {}",
            workflow.display()
        );
    }

    let workflow_content = read_file(&workflow);

    // The Rust block extraction AWK may be inline or in an external file.
    // Combine both sources for validation.
    let awk_content = if external_awk.exists() {
        // External AWK file is the preferred approach (avoids shell quoting issues)
        read_file(&external_awk)
    } else {
        // Fall back to checking inline AWK in the workflow
        workflow_content.clone()
    };

    // Verify the workflow references AWK (either inline or via awk -f)
    assert!(
        workflow_content.contains("awk '")
            || workflow_content.contains("awk \"")
            || workflow_content.contains("awk -f"),
        "doc-validation.yml should contain AWK scripts or reference external AWK files.\n\
         These scripts are critical for validating markdown code blocks."
    );

    // Check for the main Rust code block extraction AWK script
    // This script handles complex patterns: ```rust, ```Rust, ```rust,ignore, etc.
    assert!(
        awk_content.contains("/^```[Rr]ust/"),
        "Rust block extraction AWK script should use case-insensitive pattern for Rust.\n\
         Pattern /^```[Rr]ust/ matches both ```rust and ```Rust.\n\
         This prevents missing code blocks with capitalized language identifiers.\n\
         Checked in: {}",
        if external_awk.exists() {
            external_awk.display().to_string()
        } else {
            workflow.display().to_string()
        }
    );

    // Verify the AWK script has END block for unclosed blocks at EOF
    assert!(
        awk_content.contains("END {") && awk_content.contains("if (in_block)"),
        "Rust block extraction AWK script should have END block to handle unclosed blocks.\n\
         Without END block, code blocks at end of file without closing fence are lost.\n\
         The END block should check 'if (in_block)' and output remaining content."
    );

    // Verify content accumulation handles empty first lines correctly
    // The fix uses: if (content == "") { content = $0 } else { content = content "\n" $0 }
    assert!(
        awk_content.contains("content = $0")
            && awk_content.contains("content = content \"\\n\" $0"),
        "Rust block extraction AWK script should properly handle empty first lines.\n\
         Correct pattern: if (content == \"\") {{ content = $0 }} else {{ content = content \"\\n\" $0 }}\n\
         This prevents losing empty lines at the start of code blocks."
    );

    // Verify attribute extraction after rust/Rust fence
    // The pattern should use sub() to remove prefix and extract attributes
    assert!(
        awk_content.contains("sub(/^```[Rr]ust,?/, \"\", attrs)"),
        "Rust block extraction AWK script should extract attributes after rust fence.\n\
         Pattern: sub(/^```[Rr]ust,?/, \"\", attrs) removes fence and optional comma,\n\
         leaving attributes like 'ignore', 'no_run', 'should_panic'."
    );
}

#[test]
fn test_awk_pattern_matching_with_fixtures() {
    // This test validates AWK pattern matching using test fixtures
    // Tests all variants: plain rust, capitalized, comma-separated, space-separated,
    // nested blocks, and unclosed blocks at EOF

    let root = repo_root();
    let workflow = root.join(".github/workflows/doc-validation.yml");
    let fixtures_dir = root.join("test-fixtures/markdown");

    if !workflow.exists() {
        panic!(
            "Expected workflow file not found: {}\n\
             This file is required for AWK pattern matching validation.\n\
             Restore the file or update this test.",
            workflow.display()
        );
    }

    if !fixtures_dir.exists() {
        panic!(
            "Test fixtures directory not found at {}\n\
             Create test fixtures for AWK pattern matching validation:\n\
             - test-fixtures/markdown/awk-patterns-plain-rust.md\n\
             - test-fixtures/markdown/awk-patterns-capitalized.md\n\
             - test-fixtures/markdown/awk-patterns-comma-separated.md\n\
             - test-fixtures/markdown/awk-patterns-space-separated.md\n\
             - test-fixtures/markdown/awk-patterns-nested-blocks.md\n\
             - test-fixtures/markdown/awk-patterns-unclosed-eof.md",
            fixtures_dir.display()
        );
    }

    // Data-driven test cases: (fixture_file, expected_blocks, description)
    let test_cases = vec![
        (
            "awk-patterns-plain-rust.md",
            1,
            "Plain rust code blocks (```rust)",
        ),
        (
            "awk-patterns-capitalized.md",
            1,
            "Capitalized Rust code blocks (```Rust)",
        ),
        (
            "awk-patterns-comma-separated.md",
            2,
            "Comma-separated attributes (```rust,ignore)",
        ),
        (
            "awk-patterns-nested-blocks.md",
            2,
            "Nested/multiple code blocks",
        ),
        ("awk-patterns-unclosed-eof.md", 1, "Unclosed block at EOF"),
    ];

    let mut violations = Vec::new();

    for (fixture_file, expected_blocks, description) in &test_cases {
        let fixture_path = fixtures_dir.join(fixture_file);

        if !fixture_path.exists() {
            violations.push(format!(
                "Missing test fixture: {fixture_file}\n  \
                 Description: {description}\n  \
                 Expected: {expected_blocks} code blocks"
            ));
            continue;
        }

        let fixture_content = read_file(&fixture_path);

        // Count actual code blocks by looking for opening fences at start of lines
        // This avoids counting inline code references like "```rust" in descriptions
        let mut rust_blocks = 0;
        for line in fixture_content.lines() {
            let trimmed = line.trim_start();
            // Match opening fences: ```rust or ```Rust (with optional attributes)
            if trimmed.starts_with("```rust") || trimmed.starts_with("```Rust") {
                rust_blocks += 1;
            }
        }

        if rust_blocks != *expected_blocks {
            violations.push(format!(
                "Fixture {fixture_file} block count mismatch\n  \
                 Description: {description}\n  \
                 Expected: {expected_blocks} blocks\n  \
                 Found: {rust_blocks} blocks\n  \
                 This indicates the test fixture needs updating or the pattern is incorrect."
            ));
        }
    }

    // Verify that the space-separated fixture exists (even if pattern doesn't support it yet)
    let space_separated = fixtures_dir.join("awk-patterns-space-separated.md");
    if space_separated.exists() {
        let space_content = read_file(&space_separated);
        // Note: space-separated attributes are less common, but should be documented
        if !space_content.contains("```rust ignore") {
            violations.push(
                "Space-separated fixture should contain ```rust ignore pattern\n  \
                 This tests whether AWK script handles space-separated attributes.\n  \
                 Note: Current implementation may not support this variant."
                    .to_string(),
            );
        }
    }

    if !violations.is_empty() {
        panic!(
            "AWK pattern matching fixture validation failed:\n\n{}\n\n\
             Fix:\n\
             1. Ensure all test fixtures exist in test-fixtures/markdown/\n\
             2. Verify each fixture has the expected number of code blocks\n\
             3. Check that AWK patterns in workflow match fixture patterns\n\
             4. Update fixtures if expected block counts have changed",
            violations.join("\n\n")
        );
    }
}

#[test]
fn test_awk_posix_compatibility() {
    // This test verifies that AWK scripts use POSIX-compatible syntax
    // Prevents issues with different AWK implementations (gawk vs mawk)
    //
    // Background: GitHub Actions runners may use different AWK implementations.
    // - Ubuntu typically uses mawk (faster, POSIX-compliant)
    // - macOS uses awk (BSD variant)
    // - gawk (GNU awk) has extensions not in POSIX
    //
    // POSIX compatibility ensures scripts work across all environments.

    let root = repo_root();
    let workflow = root.join(".github/workflows/doc-validation.yml");

    if !workflow.exists() {
        panic!(
            "Expected workflow file not found: {}\n\
             This file is required for AWK POSIX compatibility validation.\n\
             Restore the file or update this test.",
            workflow.display()
        );
    }

    let content = read_file(&workflow);

    // Extract AWK scripts (simplified check)
    let mut violations = Vec::new();

    // Check for GNU-specific extensions that should be avoided
    if content.contains("gensub(") {
        violations.push(
            "AWK script uses gensub() which is GNU awk specific (not POSIX).\n  \
             Use sub() or gsub() instead for POSIX compatibility.\n  \
             Example: sub(/pattern/, \"replacement\", target) instead of gensub()"
                .to_string(),
        );
    }

    if content.contains("match(") && content.contains(", arr)") {
        violations.push(
            "AWK script uses match() with array capture (GNU awk specific).\n  \
             POSIX match() only accepts two arguments: match(string, regex).\n  \
             Use sub() for replacements instead of match() with captures."
                .to_string(),
        );
    }

    // Verify POSIX-compatible NUL byte output
    // POSIX: printf "%c", 0 (not printf "\\0")
    if content.contains("printf \"%s\\\\0\"") || content.contains("printf \"\\\\0\"") {
        // Check if there's also a POSIX-compatible version
        if !content.contains("printf \"%c\", 0") {
            violations.push(
                "AWK script may use non-POSIX NUL byte output.\n  \
                 POSIX-compatible: printf \"%c\", 0\n  \
                 Non-portable: printf \"\\0\" (may not work in mawk)\n  \
                 The workflow should use printf \"%c\", 0 for NUL delimiters."
                    .to_string(),
            );
        }
    }

    // Check for POSIX-compatible array indexing (should use 'in' operator)
    // This is more of a best practice than a strict requirement
    if content.contains("arr[") && !content.contains("in arr") {
        // This is informational - arrays are used but might not check existence
        eprintln!(
            "INFO: AWK script uses arrays without 'in' operator checks.\n\
             Consider using: if (key in array) before accessing array[key].\n\
             This prevents errors on missing keys."
        );
    }

    if !violations.is_empty() {
        panic!(
            "AWK script POSIX compatibility issues:\n\n{}\n\n\
             Why POSIX compatibility matters:\n\
             - GitHub Actions runners use different AWK implementations\n\
             - Ubuntu uses mawk (POSIX-compliant, no GNU extensions)\n\
             - macOS uses BSD awk (mostly POSIX with some differences)\n\
             - GNU-specific features cause failures on non-gawk systems\n\n\
             Fix:\n\
             1. Replace gensub() with sub() or gsub()\n\
             2. Use printf \"%c\", 0 for NUL bytes (not \\0)\n\
             3. Avoid match() with array captures\n\
             4. Test on multiple AWK implementations (awk, mawk, gawk)",
            violations.join("\n\n")
        );
    }
}

#[test]
fn test_awk_script_syntax_validation() {
    // This test extracts AWK scripts and validates their syntax
    // Uses awk --lint to check for potential issues
    //
    // Note: This is a best-effort test. Full validation requires running
    // the extracted AWK scripts through an AWK interpreter with --lint flag.

    let root = repo_root();
    let workflow = root.join(".github/workflows/doc-validation.yml");

    if !workflow.exists() {
        panic!(
            "Expected workflow file not found: {}\n\
             This file is required for AWK script syntax validation.\n\
             Restore the file or update this test.",
            workflow.display()
        );
    }

    let content = read_file(&workflow);

    // Verify AWK scripts have basic structural correctness
    let mut violations = Vec::new();

    // Count AWK script blocks
    let awk_scripts = content.matches("awk '").count() + content.matches("awk \"").count();

    if awk_scripts == 0 {
        violations.push(
            "No AWK scripts found in doc-validation.yml.\n  \
             Expected AWK scripts for code block extraction.\n  \
             The workflow should use AWK to parse markdown and extract code blocks."
                .to_string(),
        );
    }

    // Check for balanced quotes in AWK scripts (simplified check)
    // This is a heuristic - proper validation requires parsing
    let awk_sections: Vec<&str> = content.split("awk '").collect();
    for (i, section) in awk_sections.iter().enumerate().skip(1) {
        // Skip first split (before any awk)
        // Count single quotes until we find the closing quote
        let mut quote_count = 0;
        let mut in_escape = false;

        for ch in section.chars() {
            if in_escape {
                in_escape = false;
                continue;
            }
            if ch == '\\' {
                in_escape = true;
                continue;
            }
            if ch == '\'' {
                quote_count += 1;
                if quote_count == 1 {
                    // Found closing quote for AWK script
                    break;
                }
            }
        }

        if quote_count == 0 {
            violations.push(format!(
                "AWK script #{i} appears to be missing closing quote.\n  \
                 Check for unbalanced quotes in awk ' ... ' blocks.\n  \
                 This can cause shell syntax errors."
            ));
        }
    }

    // Check for common AWK syntax patterns
    // Basic validation: should have blocks like /pattern/ { action }
    if content.contains("awk '") {
        let has_pattern_action = content.contains("{") && content.contains("}");
        if !has_pattern_action {
            violations.push(
                "AWK scripts should contain pattern-action blocks: /pattern/ { action }.\n  \
                 Basic AWK structure: pattern { action_statements }\n  \
                 Check that AWK scripts have proper syntax."
                    .to_string(),
            );
        }
    }

    if !violations.is_empty() {
        panic!(
            "AWK script syntax validation issues:\n\n{}\n\n\
             These are basic syntax checks. For comprehensive validation:\n\
             1. Extract AWK scripts to separate files\n\
             2. Run: awk --lint -f script.awk /dev/null\n\
             3. Fix any warnings or errors\n\
             4. Test with actual markdown files\n\n\
             The shellcheck-workflow job in CI validates inline bash scripts,\n\
             but AWK syntax requires separate validation.",
            violations.join("\n\n")
        );
    }
}

// ============================================================================
// CI Workflow Validation Tests
// ============================================================================

#[test]
fn test_link_check_workflow_exists_and_is_configured() {
    // This test ensures the link-check workflow exists and is properly configured
    // Prevents link rot from going undetected

    let root = repo_root();
    let workflow = root.join(".github/workflows/link-check.yml");

    assert!(
        workflow.exists(),
        "link-check.yml workflow is missing.\n\
         Link checking is critical for documentation quality.\n\
         Create .github/workflows/link-check.yml with lychee-action"
    );

    let content = read_file(&workflow);

    // Verify workflow uses lychee-action
    assert!(
        content.contains("lycheeverse/lychee-action"),
        "link-check.yml must use lycheeverse/lychee-action"
    );

    // Verify workflow uses .lychee.toml config
    assert!(
        content.contains(".lychee.toml") || content.contains("--config"),
        "link-check.yml must reference .lychee.toml configuration file"
    );

    // Verify workflow has GITHUB_TOKEN for rate limiting
    assert!(
        content.contains("GITHUB_TOKEN"),
        "link-check.yml should use GITHUB_TOKEN to avoid rate limiting"
    );

    // Verify workflow runs on schedule for proactive link rot detection
    assert!(
        content.contains("schedule:") || content.contains("cron:"),
        "link-check.yml should run on a schedule (e.g., weekly) to catch link rot"
    );
}

#[test]
fn test_markdownlint_workflow_exists_and_is_configured() {
    // This test ensures the markdownlint workflow exists and is properly configured
    // Prevents markdown formatting issues from reaching main branch

    let root = repo_root();
    let workflow = root.join(".github/workflows/markdownlint.yml");

    assert!(
        workflow.exists(),
        "markdownlint.yml workflow is missing.\n\
         Markdown linting is required for documentation consistency.\n\
         Create .github/workflows/markdownlint.yml"
    );

    let content = read_file(&workflow);

    // Verify workflow uses markdownlint-cli2-action
    assert!(
        content.contains("DavidAnson/markdownlint-cli2-action")
            || content.contains("markdownlint-cli2"),
        "markdownlint.yml must use markdownlint-cli2"
    );

    // Verify workflow excludes common directories
    let excluded_dirs = vec!["target", "third_party", "node_modules"];
    for dir in excluded_dirs {
        assert!(
            content.contains(dir),
            "markdownlint.yml should exclude {dir} directory"
        );
    }

    // Verify workflow has path filters for efficiency
    assert!(
        content.contains("paths:") && content.contains("**.md"),
        "markdownlint.yml should have path filters to run only on .md changes"
    );

    assert!(
        content.contains("scripts/check-markdown-link-text.sh"),
        "markdownlint.yml must include scripts/check-markdown-link-text.sh in path filters and execution steps."
    );
}

#[test]
fn test_doc_validation_workflow_has_shellcheck() {
    // This test ensures the doc-validation workflow validates its own shell scripts
    // Prevents AWK and bash syntax errors in workflow scripts
    //
    // Background: The doc-validation.yml workflow contains complex AWK and bash scripts
    // that extract and validate code blocks from markdown. These scripts themselves
    // need validation to prevent issues like the AWK pattern bug we fixed.

    let root = repo_root();
    let workflow = root.join(".github/workflows/doc-validation.yml");

    if !workflow.exists() {
        panic!(
            "Expected workflow file not found: {}\n\
             This file is required for shellcheck validation.\n\
             Restore the file or update this test.",
            workflow.display()
        );
    }

    let content = read_file(&workflow);

    // Verify workflow has shellcheck job or step
    assert!(
        content.contains("shellcheck") || content.contains("Shellcheck"),
        "doc-validation.yml should include shellcheck validation of inline scripts.\n\
         This prevents shell/AWK syntax errors in workflow scripts.\n\
         Add a shellcheck job that validates inline bash scripts in the workflow."
    );

    // Verify shellcheck is installed in the workflow
    if content.contains("shellcheck") {
        assert!(
            content.contains("apt-get install") && content.contains("shellcheck")
                || content.contains("brew install shellcheck"),
            "doc-validation.yml should install shellcheck to validate scripts"
        );
    }
}

/// A single hygiene rule applied to workflow files.
///
/// - `name`:    Human-readable label for diagnostic output.
/// - `filter`:  Returns `true` for filenames this rule applies to.
/// - `check`:   Given `(filename, file_content)`, returns per-file violations.
/// - `summary`: Fix instructions shown when violations exist.
struct HygieneRule {
    name: &'static str,
    filter: Box<dyn Fn(&str) -> bool>,
    check: Box<dyn Fn(&str, &str) -> Vec<String>>,
    summary: &'static str,
}

/// Data-driven workflow hygiene test.
///
/// This single test replaces three separate tests that all followed the same
/// pattern: iterate workflow files, read each, check for a specific
/// configuration key, collect violations, and panic with diagnostics. By
/// expressing each hygiene requirement as a declarative rule, we avoid
/// duplicating the iteration/collection/reporting boilerplate and make it
/// trivial to add new checks in the future.
///
/// Each rule specifies:
///   - A human-readable name for diagnostic output.
///   - A file filter that decides which workflows the rule applies to.
///   - A check function that returns a `Vec<String>` of per-file violations.
///   - A summary message (with fix instructions) shown when violations exist.
#[test]
fn test_workflow_hygiene_requirements() {
    // --- Rule definitions ------------------------------------------------
    //
    // `filter`:  &str -> bool — receives the filename, returns true if
    //            the rule applies to that file.
    // `check`:   (&str, &str) -> Vec<String> — receives (filename, content),
    //            returns a list of violation descriptions (empty = pass).

    // Workflows that must have concurrency groups. All workflows except
    // docs-deploy.yml (which uses a special `pages` concurrency group that
    // is intentionally different from the standard pattern).
    let concurrency_allowlist: &[&str] = &[
        "actionlint.yml",
        "ci.yml",
        "ci-safety.yml",
        "doc-validation.yml",
        "link-check.yml",
        "markdownlint.yml",
        "release.yml",
        "spellcheck.yml",
        "unused-deps.yml",
        "workflow-hygiene.yml",
        "yaml-lint.yml",
    ];

    let rules: Vec<HygieneRule> = vec![
        // Rule 1: Concurrency groups -----------------------------------------
        HygieneRule {
            name: "concurrency groups",
            // Applies to the explicit allowlist (docs-deploy.yml is excluded
            // because it uses a special `pages` concurrency group).
            filter: Box::new({
                let list = concurrency_allowlist.to_vec();
                move |filename: &str| list.contains(&filename)
            }),
            check: Box::new(|filename: &str, content: &str| {
                let mut violations = Vec::new();
                if !content.contains("concurrency:") {
                    violations.push(format!(
                        "{filename}: Missing concurrency group.\n  \
                         Add:\n  \
                         concurrency:\n  \
                           group: ${{{{ github.workflow }}}}-${{{{ github.head_ref || github.run_id }}}}\n  \
                           cancel-in-progress: true"
                    ));
                } else if !content.contains("cancel-in-progress:") {
                    violations.push(format!(
                        "{filename}: Has concurrency but missing 'cancel-in-progress' setting"
                    ));
                }
                violations
            }),
            summary: "Why concurrency groups are important:\n\
                      - Saves CI minutes by canceling superseded runs\n\
                      - Speeds up feedback (don't wait for old runs)\n\
                      - Reduces queue times for other workflows\n\n\
                      Standard pattern:\n\
                      concurrency:\n\
                        group: ${{ github.workflow }}-${{ github.head_ref || github.run_id }}\n\
                        cancel-in-progress: true\n\n\
                      Exception: release.yml uses cancel-in-progress: false to prevent\n\
                      aborting in-progress releases (which could leave crates.io half-published).",
        },
        // Rule 2: Job timeouts ------------------------------------------------
        HygieneRule {
            name: "timeout-minutes",
            // Applies to every workflow file — no job should rely on GitHub's
            // 6-hour default timeout.
            filter: Box::new(|_: &str| true),
            check: Box::new(|filename: &str, content: &str| {
                let mut violations = Vec::new();
                if !content.contains("timeout-minutes:") {
                    violations.push(format!(
                        "{filename}: No timeout-minutes configured.\n  \
                         Fix: Add timeout-minutes to each job.\n  \
                         Example: timeout-minutes: 10\n  \
                         Verify: grep -n 'timeout-minutes:' .github/workflows/{filename}"
                    ));
                }
                violations
            }),
            summary: "Why timeouts are required:\n\
                      - Hanging jobs consume CI minutes indefinitely\n\
                      - GitHub's default timeout is 6 hours (way too long)\n\
                      - Explicit timeouts provide fast feedback on stuck jobs\n\n\
                      Fix: Add 'timeout-minutes: N' to each job definition.\n\
                      Example:\n\
                        jobs:\n\
                          build:\n\
                            timeout-minutes: 20\n\
                            runs-on: ubuntu-latest\n\n\
                      Verify: grep -n 'timeout-minutes' .github/workflows/<file>",
        },
        // Rule 3: Minimal permissions -----------------------------------------
        HygieneRule {
            name: "minimal permissions",
            // Applies to every workflow — the least-privilege principle is
            // non-negotiable for supply-chain security.
            filter: Box::new(|_: &str| true),
            check: Box::new(|filename: &str, content: &str| {
                let mut violations = Vec::new();
                if !content.contains("permissions:") {
                    violations.push(format!(
                        "{filename}: No permissions block found.\n  \
                         Fix: Add 'permissions:' block to explicitly set required permissions.\n  \
                         For read-only workflows:\n  \
                           permissions:\n  \
                             contents: read\n  \
                         Verify: grep -n 'permissions:' .github/workflows/{filename}"
                    ));
                } else if content.contains("permissions: write-all") {
                    violations.push(format!(
                        "{filename}: Uses 'write-all' permissions (too permissive).\n  \
                         Fix: Specify only required permissions explicitly.\n  \
                         Verify: grep -n 'permissions:' .github/workflows/{filename}"
                    ));
                }
                violations
            }),
            summary: "Why minimal permissions are required:\n\
                      - Compromised workflows or actions cannot abuse excess permissions\n\
                      - GitHub requires explicit permission grants for security audits\n\
                      - Omitted permissions rely on repo/org defaults, which may be broader than intended\n\n\
                      Fix: Add a 'permissions:' block to each workflow.\n\
                      For read-only workflows:\n\
                        permissions:\n\
                          contents: read\n\n\
                      Verify: grep -n 'permissions:' .github/workflows/<file>\n\
                      Reference: https://docs.github.com/en/actions/security-guides/automatic-token-authentication",
        },
    ];

    // --- Collect all workflow files once -----------------------------------

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let entries = collect_workflow_files(&workflows_dir);

    // Pre-read every file so we only hit the filesystem once.
    let workflows: Vec<(String, String)> = entries
        .iter()
        .map(|entry| {
            let path = entry.path();
            let filename = path.file_name().unwrap().to_string_lossy().to_string();
            let content = read_file(&path);
            (filename, content)
        })
        .collect();

    // --- Evaluate every rule against every applicable workflow -------------

    // Accumulate violations grouped by rule name so the final report is
    // structured and easy to act on.
    let mut all_violations: Vec<(String, Vec<String>, String)> = Vec::new();

    for rule in &rules {
        let mut rule_violations = Vec::new();
        for (filename, content) in &workflows {
            if !(rule.filter)(filename) {
                continue;
            }
            rule_violations.extend((rule.check)(filename, content));
        }
        if !rule_violations.is_empty() {
            all_violations.push((
                rule.name.to_string(),
                rule_violations,
                rule.summary.to_string(),
            ));
        }
    }

    // --- Report all violations at once ------------------------------------

    if !all_violations.is_empty() {
        let mut report = String::from(
            "Workflow hygiene violations detected.\n\
             ======================================\n",
        );

        for (rule_name, violations, summary) in &all_violations {
            report.push_str(&format!(
                "\n--- Rule: {rule_name} ({} violation{}) ---\n\n",
                violations.len(),
                if violations.len() == 1 { "" } else { "s" },
            ));
            report.push_str(&violations.join("\n\n"));
            report.push_str(&format!("\n\n{summary}\n"));
        }

        panic!("{report}");
    }
}

// ============================================================================
// Markdown Relative Link Validation Tests
// ============================================================================
// These tests prevent broken relative links in docs/ that reference .llm/ or
// other directories without the correct ../ prefix. This was a real CI issue:
// docs used `.llm/skills/...` instead of `../.llm/skills/...`, causing broken
// links that passed local editing but failed link validation in CI.

/// Extract all markdown link URLs from content.
///
/// Returns a vector of (line_number, link_text, url) tuples for all markdown
/// links in the format `[text](url)`.
fn extract_markdown_links(content: &str) -> Vec<(usize, String, String)> {
    let mut links = Vec::new();
    let link_pattern = regex::Regex::new(r"\[([^\]]*)\]\(([^)]+)\)").unwrap();

    for (line_idx, line) in content.lines().enumerate() {
        for cap in link_pattern.captures_iter(line) {
            let text = cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let url = cap
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            links.push((line_idx + 1, text, url));
        }
    }

    links
}

#[test]
fn test_docs_relative_links_to_llm_use_parent_prefix() {
    // This test prevents the broken relative link issue where docs/ files
    // linked to .llm/skills/... instead of ../.llm/skills/...
    //
    // Since docs/ is one level deep, any link to .llm/ must go up one
    // directory first with ../ prefix.

    let root = repo_root();
    let docs_dir = root.join("docs");

    if !docs_dir.exists() {
        return;
    }

    let mut violations = Vec::new();

    let docs_files = find_files_with_extension(&docs_dir, "md", &["target", "third_party"]);

    for file in &docs_files {
        let content = read_file(file);
        let relative_path = file.strip_prefix(&root).unwrap_or(file);

        for (line_num, _text, url) in extract_markdown_links(&content) {
            // Skip external URLs and anchors
            if url.starts_with("http://")
                || url.starts_with("https://")
                || url.starts_with("mailto:")
                || url.starts_with('#')
            {
                continue;
            }

            // Check for .llm/ links missing the ../ prefix
            // From docs/, the correct path to .llm/ is ../.llm/
            if url.starts_with(".llm/") {
                violations.push(format!(
                    "{}:{}: Link '{}' should be '../{}'",
                    relative_path.display(),
                    line_num,
                    url,
                    url
                ));
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Docs files contain relative links to .llm/ without required ../ prefix:\n\n{}\n\n\
             Why this matters:\n\
             - Files in docs/ are one directory level deep\n\
             - Links to .llm/ must go up one level first: ../.llm/\n\
             - Using .llm/skills/... instead of ../.llm/skills/... creates broken links\n\n\
             Fix: Change '.llm/' to '../.llm/' in the links listed above.\n\
             Verify: ./scripts/validate-ci.sh --links",
            violations.join("\n")
        );
    }
}

#[test]
fn test_docs_relative_links_resolve_to_existing_files() {
    // This test validates that all relative links in docs/ actually point
    // to files that exist in the repository. Catches broken links early
    // before they reach CI link checking.

    let root = repo_root();
    let docs_dir = root.join("docs");

    if !docs_dir.exists() {
        return;
    }

    let mut broken_links = Vec::new();

    let docs_files = find_files_with_extension(&docs_dir, "md", &["target", "third_party"]);

    for file in &docs_files {
        let content = read_file(file);
        let relative_path = file.strip_prefix(&root).unwrap_or(file);
        let file_dir = file.parent().unwrap_or(&root);

        for (line_num, _text, url) in extract_markdown_links(&content) {
            // Skip external URLs and anchors
            if url.starts_with("http://")
                || url.starts_with("https://")
                || url.starts_with("mailto:")
                || url.starts_with('#')
            {
                continue;
            }

            // Strip anchor portion for file existence check
            let file_part = url.split('#').next().unwrap_or(&url);
            if file_part.is_empty() {
                continue;
            }

            // Resolve the path relative to the markdown file's directory
            let resolved = file_dir.join(file_part);

            // Canonicalize to resolve .. and . components, then check existence
            // Use the resolved path's existence as the check
            if !resolved.exists() {
                // Try canonicalizing parent to handle .. components
                let normalized = normalize_path(&resolved);
                if !normalized.exists() {
                    broken_links.push(format!(
                        "{}:{}: Link '{}' -> file not found (resolved to {})",
                        relative_path.display(),
                        line_num,
                        url,
                        normalized.display()
                    ));
                }
            }
        }
    }

    if !broken_links.is_empty() {
        panic!(
            "Broken relative links found in docs/ markdown files:\n\n{}\n\n\
             Fix: Update the link paths to point to existing files.\n\
             Common issues:\n\
             - Missing ../ prefix for links to parent directories\n\
             - Typo in filename or directory name\n\
             - File was moved or renamed\n\n\
             Verify: ./scripts/validate-ci.sh --links",
            broken_links.join("\n")
        );
    }
}

/// Normalize a path by resolving `.` and `..` components without requiring
/// the path to exist on disk (unlike `canonicalize()`).
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            other => {
                components.push(other);
            }
        }
    }
    components.iter().collect()
}

#[test]
fn test_docs_no_absolute_path_links() {
    // This test flags markdown links in docs/ that use absolute paths starting
    // with `/`. Absolute paths are not portable across machines (e.g.,
    // /workspaces/signal-fish-server/... only works in a specific devcontainer).
    // All links should use relative paths from the file's location.

    let root = repo_root();
    let docs_dir = root.join("docs");

    if !docs_dir.exists() {
        return;
    }

    let mut violations = Vec::new();

    let docs_files = find_files_with_extension(&docs_dir, "md", &["target", "third_party"]);

    for file in &docs_files {
        let content = read_file(file);
        let relative_path = file.strip_prefix(&root).unwrap_or(file);

        for (line_num, _text, url) in extract_markdown_links(&content) {
            // Skip external URLs and anchors
            if url.starts_with("http://")
                || url.starts_with("https://")
                || url.starts_with("mailto:")
                || url.starts_with('#')
            {
                continue;
            }

            // Flag any link that starts with / as a portability issue
            if url.starts_with('/') {
                violations.push(format!(
                    "{}:{}: Absolute path link '{}' is not portable\n  \
                     Fix: Convert to a relative path from the file's directory",
                    relative_path.display(),
                    line_num,
                    url
                ));
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Documentation files contain absolute-path links (not portable):\n\n{}\n\n\
             Absolute paths like /workspaces/... or /home/... only work on one machine.\n\
             Use relative paths instead:\n\
             - To a sibling doc:  `sibling.md`\n\
             - To repo root file: `../README.md`\n\
             - To tests/:        `../tests/ci_config_tests.rs`\n\n\
             Verify: ./scripts/validate-ci.sh --links",
            violations.join("\n")
        );
    }
}

#[test]
fn test_awk_files_have_valid_syntax() {
    // This test validates that all .awk files in the repository parse correctly.
    // Prevents the issue where an AWK script with syntax errors is committed
    // and only discovered when the CI workflow tries to use it.

    let root = repo_root();

    let mut awk_files = Vec::new();

    // Look for .awk files in known locations
    let scripts_dir = root.join(".github/scripts");
    if scripts_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&scripts_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "awk").unwrap_or(false) {
                    awk_files.push(path);
                }
            }
        }
    }

    if awk_files.is_empty() {
        // No AWK files to validate
        return;
    }

    let mut issues = Vec::new();

    for awk_file in &awk_files {
        let content = read_file(awk_file);
        let relative_path = awk_file.strip_prefix(&root).unwrap_or(awk_file);

        // Check for non-POSIX match() function (GNU-specific, breaks on mawk)
        for (line_idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            // Skip comments
            if trimmed.starts_with('#') {
                continue;
            }

            if trimmed.contains("match(") {
                issues.push(format!(
                    "{}:{}: Uses match() function (not POSIX compatible with mawk).\n  \
                     Fix: Use sub() or gsub() instead.",
                    relative_path.display(),
                    line_idx + 1
                ));
            }

            // Check for \0 in printf (not POSIX)
            if trimmed.contains("printf") && trimmed.contains("\\0") {
                issues.push(format!(
                    "{}:{}: Uses \\0 in printf (not POSIX compatible).\n  \
                     Fix: Use printf \"%c\", 0 instead.",
                    relative_path.display(),
                    line_idx + 1
                ));
            }
        }
    }

    if !issues.is_empty() {
        panic!(
            "AWK file validation issues found:\n\n{}\n\n\
             Why this matters:\n\
             - GitHub Actions runners may use mawk (not gawk)\n\
             - Non-POSIX AWK features cause silent failures in CI\n\
             - match() and \\0 are common portability problems\n\n\
             Verify: ./scripts/validate-ci.sh --awk",
            issues.join("\n\n")
        );
    }
}

#[test]
fn test_validate_ci_script_exists() {
    // This test ensures the validate-ci.sh script exists and is the canonical
    // tool for local CI validation. This script was created to prevent the
    // three types of CI/CD regressions that were discovered:
    //   1. AWK syntax errors in .awk files
    //   2. Broken relative links in docs/
    //   3. Shell script issues in .github/scripts/

    let root = repo_root();
    let validate_ci = root.join("scripts/validate-ci.sh");

    assert!(
        validate_ci.exists(),
        "scripts/validate-ci.sh not found.\n\
         This script is required for local CI configuration validation.\n\
         It validates AWK files, shell scripts, and markdown links.\n\
         Create it or restore it from the repository."
    );

    let content = read_file(&validate_ci);

    // Verify it covers the three key validation areas
    assert!(
        content.contains("validate_awk") || content.contains("awk"),
        "scripts/validate-ci.sh should validate AWK files"
    );

    assert!(
        content.contains("shellcheck") || content.contains("validate_shell"),
        "scripts/validate-ci.sh should validate shell scripts with shellcheck"
    );

    assert!(
        content.contains("markdown")
            || content.contains("validate_markdown")
            || content.contains("link"),
        "scripts/validate-ci.sh should validate markdown links"
    );
}

// ============================================================================
// CI/CD Regression Prevention Tests
// ============================================================================
// These tests prevent recurrence of specific CI/CD failures that were fixed:
//   1. cargo-deny Docker container missing pinned Rust toolchain
//   2. Lychee v0.21.0 hidden file matcher bug and exclude_path TOML limitation
// Each test documents the root cause and expected fix.

#[test]
fn test_cargo_deny_uses_explicit_msrv_toolchain_input() {
    // This test prevents regression of cargo-deny toolchain selection in the
    // Dockerized action runtime.
    //
    // Root cause: relying on env overrides like RUSTUP_TOOLCHAIN=stable can fail
    // when that alias is not preinstalled inside the action container.
    //
    // Fix: Extract MSRV from Cargo.toml in a dedicated step and pass it via the
    // action's `rust-version` input, so the action installs the exact toolchain
    // before running cargo-deny.

    let root = repo_root();
    let ci_workflow = root.join(".github/workflows/ci.yml");
    let content = read_file(&ci_workflow);

    assert!(
        content.contains("  deny:"),
        "CI workflow must have a 'deny' job for dependency auditing.\n\
         File: {}",
        ci_workflow.display()
    );

    // Data-driven expectations for required deny job fragments.
    let required_fragments = [
        ("deny-msrv step id", "id: deny-msrv"),
        (
            "MSRV extraction from Cargo.toml",
            "MSRV=$(grep '^rust-version = ' Cargo.toml",
        ),
        (
            "deny-msrv output export",
            "echo \"version=$MSRV\" >> \"$GITHUB_OUTPUT\"",
        ),
        (
            "cargo-deny rust-version input wired to deny-msrv output",
            "rust-version: ${{ steps.deny-msrv.outputs.version }}",
        ),
    ];

    let mut missing = Vec::new();
    for (label, fragment) in required_fragments {
        if !content.contains(fragment) {
            missing.push(format!("Missing {label}: expected fragment `{fragment}`"));
        }
    }

    assert!(
        missing.is_empty(),
        "cargo-deny workflow configuration is incomplete:\n\n{}\n\n\
         The deny job must extract MSRV and pass it to cargo-deny via \
         `with.rust-version` to avoid container-specific toolchain alias failures.\n\
         File: {}",
        missing.join("\n"),
        ci_workflow.display()
    );

    // Guard against regressing back to env-based alias overrides that caused
    // container-specific failures in CI.
    assert!(
        !content.contains("RUSTUP_TOOLCHAIN:"),
        "ci.yml deny job should not set RUSTUP_TOOLCHAIN directly.\n\
         Use cargo-deny `rust-version` input instead to ensure installation\n\
         and deterministic toolchain selection inside the action container.\n\
         File: {}",
        ci_workflow.display()
    );
}

#[test]
fn test_lychee_version_pinned_above_v0_22() {
    // This test prevents regression of the lychee hidden file matcher bug.
    //
    // Root cause: lychee v0.21.0 (bundled with lychee-action v2.7.0) had a bug
    // (#1936) where it scanned hidden/dotfiles like .lychee.toml as input despite
    // --hidden not being set. This caused lychee to extract truncated URLs from
    // regex patterns in its own config file, leading to spurious link check failures.
    //
    // Fix: Pin lycheeVersion to v0.22.0 or newer, which fixes the hidden file
    // matcher bug. The lychee-action's `lycheeVersion` input overrides the bundled
    // binary version.

    let root = repo_root();
    let workflows = vec![
        root.join(".github/workflows/link-check.yml"),
        root.join(".github/workflows/doc-validation.yml"),
    ];

    for workflow in workflows {
        let content = read_file(&workflow);

        // Find the lycheeVersion setting
        let mut found_version = false;
        let mut version_value = String::new();
        let mut version_line = 0;

        for (line_num, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("lycheeVersion:") {
                found_version = true;
                version_value = trimmed
                    .strip_prefix("lycheeVersion:")
                    .unwrap_or("")
                    .trim()
                    .to_string();
                version_line = line_num + 1;
                break;
            }
        }

        assert!(
            found_version,
            "{} must set lycheeVersion to override the bundled lychee binary.\n\
             Without this, the action uses lychee v0.21.0 which has a hidden file matcher bug\n\
             (lycheeverse/lychee#1936) that scans .lychee.toml as input.\n\n\
             Fix: Add 'lycheeVersion: v0.22.0' (or newer) to the lychee-action step's 'with:' block.\n\
             File: {}",
            workflow
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workflow"),
            workflow.display()
        );

        // Parse the version: strip leading 'v' and split into components
        let version_str = version_value.trim_start_matches('v');
        let parts: Vec<u32> = version_str
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect();

        assert!(
            parts.len() >= 2,
            "lycheeVersion must be a valid semver version (e.g., v0.22.0).\n\
             Found: '{}' at line {} in {}\n\
             Expected format: vMAJOR.MINOR.PATCH",
            version_value,
            version_line,
            workflow.display()
        );

        let major = parts[0];
        let minor = parts[1];

        // Version must be >= 0.22.0 (where the hidden file matcher bug was fixed)
        let min_major = 0;
        let min_minor = 22;

        let is_sufficient = major > min_major || (major == min_major && minor >= min_minor);

        assert!(
            is_sufficient,
            "lycheeVersion must be >= v0.22.0 to include the hidden file matcher fix.\n\
             Found: {} (parsed as {}.{}) at line {} in {}\n\
             Minimum required: v0.22.0\n\n\
             Background: lychee v0.21.0 scans dotfiles like .lychee.toml as input,\n\
             extracting truncated URLs from regex patterns and causing false failures.\n\
             This was fixed in v0.22.0 via lycheeverse/lychee#1936.\n\n\
             Fix: Update lycheeVersion to v0.22.0 or newer.",
            version_value,
            major,
            minor,
            version_line,
            workflow.display()
        );
    }
}

#[test]
fn test_lychee_workflows_use_hardened_args_data_driven() {
    // Data-driven guard against config drift between workflows that run lychee.
    // Both workflows should use the shared .lychee.toml policy and critical
    // CLI --exclude-path flags (defense-in-depth for lychee glob behavior).

    let root = repo_root();
    let cases: Vec<(std::path::PathBuf, Vec<&str>)> = vec![
        (
            root.join(".github/workflows/link-check.yml"),
            vec![
                "--config .lychee.toml",
                "--exclude-path tests/",
                "--exclude-path target/",
                "--exclude-path third_party/",
                "--exclude-path '\\.github/test-fixtures/'",
                "--exclude-path 'test-fixtures/'",
                "--exclude-path '\\.lychee\\.toml'",
                "--",
            ],
        ),
        (
            root.join(".github/workflows/doc-validation.yml"),
            vec![
                "--config .lychee.toml",
                "--exclude-path './target/*'",
                "--exclude-path './third_party/*'",
                "--exclude-path './.github/test-fixtures/*'",
                "--exclude-path './test-fixtures/*'",
                "--exclude-path '\\.lychee\\.toml'",
                "--",
            ],
        ),
    ];

    for (workflow, required_fragments) in cases {
        let content = read_file(&workflow);

        assert!(
            content.contains("lycheeverse/lychee-action"),
            "{} must use lycheeverse/lychee-action.\nFile: {}",
            workflow
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workflow"),
            workflow.display()
        );

        let mut missing = Vec::new();
        for fragment in required_fragments {
            if !content.contains(fragment) {
                missing.push(format!("  - Missing fragment: `{fragment}`"));
            }
        }

        assert!(
            missing.is_empty(),
            "{} is missing required lychee hardening fragments:\n\n{}\n\n\
             These settings keep link checking consistent and resilient across workflows.\n\
             File: {}",
            workflow
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workflow"),
            missing.join("\n"),
            workflow.display()
        );
    }
}

#[test]
fn test_lychee_cli_exclude_paths_match_config() {
    // This test ensures defense-in-depth: every exclude_path in .lychee.toml
    // must also appear as a CLI --exclude-path flag in the link-check workflow.
    //
    // Root cause: Lychee's TOML `exclude_path` setting does NOT apply to paths
    // discovered via glob expansion (known bug). When the workflow passes glob
    // patterns like './**/*.md', lychee expands them and the TOML exclude_path
    // entries are silently ignored for those expanded paths.
    //
    // Fix: Duplicate critical exclude_path entries as CLI --exclude-path flags.
    // CLI flags are applied at a different stage and correctly filter glob results.
    // Both TOML and CLI entries are kept as defense-in-depth — if either mechanism
    // is fixed or changed, the other still provides coverage.

    let root = repo_root();
    let lychee_config = root.join(".lychee.toml");
    let link_check = root.join(".github/workflows/link-check.yml");

    let config_content = read_file(&lychee_config);
    let workflow_content = read_file(&link_check);

    // Parse exclude_path entries from .lychee.toml
    let toml_exclude_paths = parse_lychee_exclude_path_patterns(&config_content);

    assert!(
        !toml_exclude_paths.is_empty(),
        ".lychee.toml must have exclude_path entries.\n\
         File: {}",
        lychee_config.display()
    );

    // Extract --exclude-path values from the workflow args
    let cli_exclude_paths: Vec<String> = workflow_content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("--exclude-path") {
                // Parse: "--exclude-path tests/" or "--exclude-path 'value'"
                let value = trimmed
                    .strip_prefix("--exclude-path")
                    .unwrap_or("")
                    .trim()
                    .trim_matches('\'')
                    .trim_matches('"')
                    .to_string();
                if !value.is_empty() {
                    Some(value)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    assert!(
        !cli_exclude_paths.is_empty(),
        "link-check.yml must have --exclude-path CLI flags in the lychee args.\n\
         Without CLI flags, TOML exclude_path entries are silently ignored for\n\
         glob-expanded paths (known lychee bug).\n\
         File: {}",
        link_check.display()
    );

    // Critical paths that MUST be in both TOML exclude_path and CLI --exclude-path.
    // These are paths that lychee's globs ('./**/*.md', './**/*.rs', './**/*.toml')
    // will expand into, so the TOML exclude_path alone is insufficient (known bug).
    //
    // Paths like .git/ are inherently excluded by the shell globs (dotfiles not
    // expanded without --hidden) and don't need CLI coverage. But paths like tests/,
    // target/, and third_party/ contain .md/.rs/.toml files that globs will find.
    let critical_paths = vec![
        ("tests/", "Test files contain placeholder/example URLs"),
        ("target/", "Build artifacts should never be link-checked"),
        ("third_party/", "Vendored dependencies checked separately"),
        (
            ".github/test-fixtures/",
            "Test fixtures with intentional example/placeholder content",
        ),
        (
            "test-fixtures/",
            "Root test fixtures with example/placeholder content",
        ),
    ];

    let mut missing_entries = Vec::new();

    for (critical_path, reason) in &critical_paths {
        let critical_normalized = critical_path.trim_end_matches('/');

        // Check TOML has it
        let in_toml = toml_exclude_paths.iter().any(|p| {
            let normalized = p.trim_end_matches('/');
            normalized == critical_normalized || normalized.ends_with(critical_normalized)
        });

        // Check CLI has it
        let in_cli = cli_exclude_paths.iter().any(|p| {
            let normalized = p.trim_end_matches('/');
            normalized == critical_normalized || normalized.ends_with(critical_normalized)
        });

        if !in_toml {
            missing_entries.push(format!(
                "  Path: {critical_path}\n  \
                 Reason: {reason}\n  \
                 Missing from: .lychee.toml exclude_path"
            ));
        }

        if !in_cli {
            missing_entries.push(format!(
                "  Path: {critical_path}\n  \
                 Reason: {reason}\n  \
                 Missing from: CLI --exclude-path flags"
            ));
        }
    }

    // Additionally verify every CLI --exclude-path has a TOML counterpart
    // (the TOML entry serves as documentation even if the bug makes it ineffective)
    for cli_path in &cli_exclude_paths {
        let cli_normalized = cli_path
            .trim_end_matches('/')
            .replace("\\.", ".")
            .trim_end_matches('$')
            .to_string();

        let in_toml = toml_exclude_paths.iter().any(|p| {
            let normalized = p
                .trim_end_matches('/')
                .trim_end_matches('$')
                .replace("\\.", ".")
                .to_string();
            normalized.contains(&cli_normalized) || cli_normalized.contains(&normalized)
        });

        if !in_toml {
            missing_entries.push(format!(
                "  CLI --exclude-path: {cli_path}\n  \
                 Missing from: .lychee.toml exclude_path (should be documented there too)"
            ));
        }
    }

    if !missing_entries.is_empty() {
        panic!(
            "Defense-in-depth violation: exclude_path mismatch between TOML and CLI:\n\n{}\n\n\
             TOML exclude_path entries:\n{}\n\n\
             CLI --exclude-path flags:\n{}\n\n\
             Why both are needed:\n\
             - TOML exclude_path does NOT apply to glob-expanded paths (known lychee bug)\n\
             - CLI --exclude-path is applied at a different stage and correctly filters globs\n\
             - Both should be kept as defense-in-depth\n\n\
             Fix: Ensure critical paths appear in both .lychee.toml exclude_path\n\
             and as --exclude-path CLI flags in link-check.yml.\n\
             TOML file: {}\n\
             Workflow: {}",
            missing_entries.join("\n\n"),
            toml_exclude_paths
                .iter()
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n"),
            cli_exclude_paths
                .iter()
                .map(|p| format!("  --exclude-path {p}"))
                .collect::<Vec<_>>()
                .join("\n"),
            lychee_config.display(),
            link_check.display()
        );
    }
}

#[test]
fn test_lychee_args_use_double_dash_separator() {
    // This test prevents regression of the argument parsing issue in lychee.
    //
    // Root cause: Without a `--` separator between flags and positional arguments,
    // lychee's argument parser can consume positional glob patterns as values for
    // the preceding --exclude-path flag. For example:
    //   --exclude-path '.lychee.toml' './**/*.md'
    // could be parsed as --exclude-path taking two values instead of one.
    //
    // Fix: Use `--` to explicitly separate option flags from positional arguments:
    //   --exclude-path '.lychee.toml' -- './**/*.md' './**/*.rs' './**/*.toml'

    let root = repo_root();
    let link_check = root.join(".github/workflows/link-check.yml");
    let content = read_file(&link_check);

    // Find the args block for the lychee action
    let mut in_lychee_step = false;
    let mut in_args = false;
    let mut args_lines = Vec::new();
    let mut args_start_line = 0;
    let mut args_indent = 0;

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();

        // Detect the lychee-action step
        if trimmed.contains("lychee-action") {
            in_lychee_step = true;
        }

        // Detect start of args block within the lychee step
        if in_lychee_step && trimmed.starts_with("args:") {
            in_args = true;
            args_start_line = line_num + 1;
            args_indent = indent;
            // The args value might be on the same line (inline) or folded (>-)
            let after_args = trimmed.strip_prefix("args:").unwrap_or("").trim();
            if !after_args.is_empty() && after_args != ">-" && after_args != "|" {
                args_lines.push(after_args.to_string());
            }
            continue;
        }

        // Collect folded args lines (indented continuation lines)
        if in_args {
            // Args continuation lines are more indented than the args: key itself;
            // a line at the same or lesser indent (like `fail:`) ends the block
            if trimmed.is_empty() || indent > args_indent {
                args_lines.push(trimmed.to_string());
            } else {
                break;
            }
        }
    }

    assert!(
        !args_lines.is_empty(),
        "Could not find lychee args block in link-check.yml.\n\
         Expected 'args:' within the lychee-action step.\n\
         File: {}",
        link_check.display()
    );

    // Join all args lines and check for the -- separator
    let full_args = args_lines.join(" ");

    assert!(
        full_args.contains(" -- "),
        "Lychee args must use '--' separator between flags and positional arguments.\n\
         Found args block starting at line {}: {:?}\n\n\
         Without '--', the argument parser may consume glob patterns as values for\n\
         --exclude-path flags instead of treating them as positional file arguments.\n\n\
         Fix: Add '--' before the positional glob patterns:\n\
           args: >-\n\
             --verbose --no-progress --cache ...\n\
             --exclude-path tests/\n\
             --\n\
             './**/*.md' './**/*.rs' './**/*.toml'\n\n\
         File: {}",
        args_start_line,
        full_args,
        link_check.display()
    );
}

// ============================================================================
// Dockerfile Validation Tests
// ============================================================================
// These tests prevent Docker build failures caused by configuration drift
// between the Dockerfile and the actual repository file structure.

#[test]
fn test_dockerfile_copy_targets_exist() {
    // This test validates that every COPY source path in the Dockerfile references
    // a file or directory that actually exists in the repository.
    //
    // Root cause: The Dockerfile referenced a `third_party/` directory that was
    // removed from the repo but the COPY instructions were never cleaned up.
    // This caused Docker builds to fail with:
    //   ERROR: failed to calculate checksum of ref: "/third_party": not found
    //
    // This test catches the issue locally before it reaches CI.

    let root = repo_root();
    let dockerfile = root.join("Dockerfile");

    assert!(
        dockerfile.exists(),
        "Dockerfile not found at {}",
        dockerfile.display()
    );

    let content = read_file(&dockerfile);
    let mut violations = Vec::new();
    let mut total_copy_instructions = 0;

    for (line_num, line) in content.lines().enumerate() {
        let line_num = line_num + 1;
        let trimmed = line.trim();

        // Match COPY and ADD instructions (but not COPY --from=<stage> which copies from build stages)
        // ADD with URLs is skipped since those are remote fetches, not local paths
        let instruction_prefix = if trimmed.starts_with("COPY ") {
            Some("COPY ")
        } else if trimmed.starts_with("ADD ") {
            Some("ADD ")
        } else {
            None
        };

        if let Some(prefix) = instruction_prefix {
            if trimmed.contains("--from=") {
                continue;
            }
            total_copy_instructions += 1;

            // Extract the source path(s) from the instruction
            // COPY/ADD <src> [<src>...] <dest>
            // The last space-separated token is the destination
            let parts: Vec<&str> = trimmed
                .strip_prefix(prefix)
                .unwrap()
                .split_whitespace()
                .collect();

            if parts.len() < 2 {
                continue;
            }

            // All tokens except the last are source paths
            for source in &parts[..parts.len() - 1] {
                // Skip flags (--chown, --chmod, --link, etc.)
                if source.starts_with("--") {
                    continue;
                }
                // Skip ADD with URLs (remote fetches, not local paths)
                if source.starts_with("http://") || source.starts_with("https://") {
                    continue;
                }
                let source_path = root.join(source);
                if !source_path.exists() {
                    violations.push(format!(
                        "  Dockerfile:{line_num}: {prefix}source does not exist: {source}\n    \
                         Full line: {trimmed}\n    \
                         Expected at: {}",
                        source_path.display()
                    ));
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Dockerfile COPY/ADD instructions reference non-existent paths:\n\n{}\n\n\
             Diagnostic Information:\n\
             - Total COPY/ADD instructions checked: {total_copy_instructions}\n\
             - Violations found: {}\n\n\
             This causes Docker builds to fail with:\n\
             ERROR: failed to calculate checksum of ref: \"/<path>\": not found\n\n\
             Fix: Either create the missing file/directory or remove the COPY/ADD instruction\n\
             from the Dockerfile.",
            violations.join("\n"),
            violations.len()
        );
    }
}

#[test]
fn test_workflow_script_references_exist() {
    // This test validates that shell scripts referenced in workflow `run:` steps
    // actually exist in the repository.
    //
    // Root cause: The release.yml workflow referenced `./scripts/verify-sccache.sh`
    // which did not exist, causing a silent failure (masked by continue-on-error).
    //
    // This test catches missing script references locally before CI.

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/"
    );

    let mut violations = Vec::new();
    let mut total_scripts_checked = 0;

    // Regex-like pattern: match ./path/to/script.sh or scripts/something.sh
    // We look for lines that invoke a local script file
    for entry in &workflow_files {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim();

            // Skip YAML comments to avoid false positives on references like:
            // # Removed: ./scripts/old-deploy.sh
            if trimmed.starts_with('#') {
                continue;
            }

            // Look for script invocations in run: blocks
            // Common patterns: ./scripts/foo.sh, bash scripts/foo.sh, sh ./scripts/foo.sh
            for token in trimmed.split_whitespace() {
                // Match tokens that look like local script paths
                let is_script_ref =
                    token.ends_with(".sh") || token.ends_with(".awk") || token.ends_with(".py");
                let is_local_path = token.starts_with("./")
                    || token.starts_with("scripts/")
                    || token.starts_with(".github/scripts/");

                let script_path = if is_script_ref && is_local_path {
                    Some(token.trim_start_matches("./"))
                } else {
                    None
                };

                if let Some(script) = script_path {
                    total_scripts_checked += 1;
                    let full_path = root.join(script);
                    if !full_path.exists() {
                        violations.push(format!(
                            "  {filename}:{line_num}: Script does not exist: {script}\n    \
                             Full line: {trimmed}\n    \
                             Expected at: {}",
                            full_path.display()
                        ));
                    }
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Workflow files reference non-existent scripts:\n\n{}\n\n\
             Diagnostic Information:\n\
             - Scripts checked: {total_scripts_checked}\n\
             - Missing scripts: {}\n\n\
             Fix: Either create the missing script or update the workflow to remove the reference.",
            violations.join("\n"),
            violations.len()
        );
    }
}

#[test]
fn test_release_workflow_conventions() {
    // This test validates that the release workflow follows the same conventions
    // as the other CI workflows (action ref policy is checked separately by
    // test_github_actions_use_version_refs_not_commit_hashes which covers all workflows).
    //
    // Specific checks for release.yml:
    //   1. Has a timeout-minutes to prevent runaway builds
    //   2. Has permissions explicitly set
    //   3. Has a proper name field
    //   4. Does not reference non-existent checkout versions
    //   5. Has a concurrency group with cancel-in-progress: false

    let root = repo_root();
    let release_yml = root.join(".github/workflows/release.yml");

    if !release_yml.exists() {
        // Release workflow is optional
        return;
    }

    let content = read_file(&release_yml);

    // Must have a name
    assert!(
        content.lines().any(|l| l.starts_with("name:")),
        "release.yml must have a top-level 'name:' field.\n\
         File: {}",
        release_yml.display()
    );

    // Must have permissions
    assert!(
        content.contains("permissions:"),
        "release.yml must explicitly set permissions (principle of least privilege).\n\
         File: {}",
        release_yml.display()
    );

    // Must have timeout-minutes on jobs
    let has_timeout = content
        .lines()
        .any(|l| l.trim().starts_with("timeout-minutes:"));
    assert!(
        has_timeout,
        "release.yml jobs must have timeout-minutes to prevent runaway builds.\n\
         File: {}",
        release_yml.display()
    );

    // Must have a concurrency group (releases should never run concurrently)
    assert!(
        content.contains("concurrency:"),
        "release.yml must have a concurrency group to prevent concurrent releases.\n\
         Add:\n\
         concurrency:\n\
           group: ${{{{ github.workflow }}}}-${{{{ github.ref }}}}\n\
           cancel-in-progress: false\n\
         File: {}",
        release_yml.display()
    );

    // Must use cancel-in-progress: false (never abort a release mid-publish)
    assert!(
        content.contains("cancel-in-progress: false"),
        "release.yml must use 'cancel-in-progress: false' to prevent aborting \
         in-progress releases (which could leave crates.io in a half-published state).\n\
         File: {}",
        release_yml.display()
    );
}

#[test]
fn test_release_workflow_requires_preflight() {
    // This test validates that the release workflow gates publishing behind a
    // preflight job that verifies required CI checks have passed. This prevents
    // publishing a broken crate.
    //
    // Checks:
    //   1. release.yml has a `preflight` job
    //   2. The `publish` job depends on `preflight` via `needs:`
    //   3. The preflight job references the required workflow names

    let root = repo_root();
    let release_yml = root.join(".github/workflows/release.yml");

    if !release_yml.exists() {
        // Release workflow is optional
        return;
    }

    let content = read_file(&release_yml);

    // Must have a preflight job
    assert!(
        content.contains("preflight:"),
        "release.yml must have a 'preflight' job that verifies CI checks passed \
         before publishing.\n\
         File: {}",
        release_yml.display()
    );

    // The publish job must depend on preflight
    // Look for `needs:` containing `preflight` in the publish job context
    assert!(
        content.contains("needs: [preflight]") || content.contains("needs: preflight"),
        "release.yml 'publish' job must depend on 'preflight' via needs.\n\
         Add 'needs: [preflight]' to the publish job.\n\
         File: {}",
        release_yml.display()
    );

    // Preflight must reference the required workflow names from REQUIRED_WORKFLOW_NAMES.
    // These are the workflows that must pass before a release can proceed.
    for (_workflow_file, workflow_name) in REQUIRED_WORKFLOW_NAMES {
        assert!(
            content.contains(workflow_name),
            "release.yml preflight job must reference required workflow '{workflow_name}' \
             (from REQUIRED_WORKFLOW_NAMES).\n\
             The preflight job should verify that '{workflow_name}' has passed on the \
             commit being released.\n\
             File: {}",
            release_yml.display()
        );
    }

    // Preflight must validate WORKFLOW_ID uniqueness. If multiple workflows
    // share the same name, the gh API returns multiple IDs and the subsequent
    // run lookup would query the wrong workflow.
    assert!(
        content.contains("Multiple workflows found"),
        "release.yml preflight must check for duplicate WORKFLOW_ID results.\n\
         If multiple workflows share a name, the gh API returns multiple IDs, \
         which would cause the preflight to query the wrong workflow run.\n\
         File: {}",
        release_yml.display()
    );
}

#[test]
fn test_release_workflow_handles_path_filtered_workflows() {
    // This test validates that the release workflow's preflight job handles
    // path-filtered workflows (like Documentation Validation) that may be
    // legitimately skipped when the commit does not touch relevant paths.
    //
    // Without this handling, releases would be blocked whenever the release
    // commit did not touch documentation paths, because the preflight job
    // would find no completed run for "Documentation Validation" and treat
    // that as an error.
    //
    // Checks:
    //   1. The release.yml contains a PATH_FILTERED_WORKFLOWS declaration
    //   2. The path-filtered workflow list references each workflow that has
    //      path filters in REQUIRED_WORKFLOW_NAMES
    //   3. The preflight logic checks changed files when no run is found

    let root = repo_root();
    let release_yml = root.join(".github/workflows/release.yml");

    if !release_yml.exists() {
        // Release workflow is optional
        return;
    }

    let content = read_file(&release_yml);

    // Must declare the PATH_FILTERED_WORKFLOWS associative array
    assert!(
        content.contains("PATH_FILTERED_WORKFLOWS"),
        "release.yml preflight job must declare PATH_FILTERED_WORKFLOWS to handle \
         workflows that use path filters and may be legitimately skipped.\n\
         File: {}",
        release_yml.display()
    );

    // The PATH_FILTERED_WORKFLOWS map must reference "Documentation Validation"
    // since doc-validation.yml uses path filters.
    assert!(
        content.contains("PATH_FILTERED_WORKFLOWS[\"Documentation Validation\"]"),
        "release.yml PATH_FILTERED_WORKFLOWS must include 'Documentation Validation' \
         because doc-validation.yml uses path filters.\n\
         File: {}",
        release_yml.display()
    );

    // Verify the preflight logic checks changed files for path-filtered workflows
    assert!(
        content.contains("commit did not touch relevant paths"),
        "release.yml preflight job must check whether the commit touched relevant \
         paths before treating a missing workflow run as an error.\n\
         File: {}",
        release_yml.display()
    );

    // Verify that path-filtered workflows with matching paths still error
    assert!(
        content.contains("should have triggered this workflow"),
        "release.yml preflight job must still error when a path-filtered workflow \
         has no run but the commit DID touch relevant paths.\n\
         File: {}",
        release_yml.display()
    );

    // Verify fail-closed behavior when CHANGED_FILES cannot be retrieved.
    // If the GitHub API fails to return changed files, the preflight must
    // fail (FAILED=1) rather than warn, to prevent releasing without CI
    // verification.
    assert!(
        content.contains("Failing closed"),
        "release.yml preflight job must fail closed when CHANGED_FILES is empty.\n\
         If the GitHub API fails to return changed files for the commit, the \
         preflight must set FAILED=1 rather than warning, to prevent releasing \
         without CI verification.\n\
         File: {}",
        release_yml.display()
    );

    // Cross-check: every workflow in REQUIRED_WORKFLOW_NAMES that has path
    // filters in its workflow file should appear in PATH_FILTERED_WORKFLOWS.
    for (workflow_file, workflow_name) in REQUIRED_WORKFLOW_NAMES {
        let workflow_path = root.join(".github/workflows").join(workflow_file);
        if !workflow_path.exists() {
            continue;
        }
        let workflow_content = read_file(&workflow_path);

        // Check if this workflow uses path filters (has a `paths:` key)
        let has_path_filters = workflow_content.lines().any(|line| {
            let trimmed = line.trim();
            trimmed == "paths:" || trimmed.starts_with("paths:")
        });

        if has_path_filters {
            let expected = format!("PATH_FILTERED_WORKFLOWS[\"{workflow_name}\"]");
            assert!(
                content.contains(&expected),
                "Workflow '{workflow_name}' ({workflow_file}) uses path filters but is not \
                 listed in PATH_FILTERED_WORKFLOWS in release.yml.\n\
                 Add: {expected}=\"<path patterns>\"\n\
                 File: {}",
                release_yml.display()
            );
        }
    }
}

#[test]
fn test_workflow_files_use_two_space_indentation() {
    // Validates that all workflow YAML files use 2-space indentation as required
    // by .yamllint.yml (indentation.spaces: 2). This catches files accidentally
    // written with 4-space indentation (common when copying from other projects
    // or when editors default to 4 spaces).
    //
    // Two checks are performed:
    //   1. Odd indentation: lines with an odd number of leading spaces (never valid
    //      in 2-space YAML)
    //   2. Minimum indent heuristic: if the smallest non-zero indent across all
    //      YAML-level lines in a file is 4+ spaces, the file is likely using 4-space
    //      (or larger) indentation throughout
    //
    // Only checks YAML structural lines — content inside multiline scalar blocks
    // (run: |, args: >-, etc.) is excluded because those are embedded scripts
    // with their own indentation rules.

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/"
    );

    let mut errors = Vec::new();

    for entry in &workflow_files {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();

        let mut in_multiline_block = false;
        let mut block_indent = 0;
        let mut odd_indent_lines = Vec::new();
        let mut min_yaml_indent = usize::MAX;

        for (line_idx, line) in content.lines().enumerate() {
            let stripped = line.trim();
            let indent = line.len() - line.trim_start().len();

            // Skip empty lines and lines with no indentation
            if stripped.is_empty() || indent == 0 {
                // A non-empty line at indent 0 exits any multiline block
                if !stripped.is_empty() {
                    in_multiline_block = false;
                }
                continue;
            }

            // Detect start of YAML multiline scalar block
            if !in_multiline_block && (stripped.contains(": |") || stripped.contains(": >")) {
                let after_colon = stripped
                    .split_once(": ")
                    .map(|(_, rest)| rest.trim())
                    .unwrap_or("");
                if after_colon == "|"
                    || after_colon == "|-"
                    || after_colon == "|+"
                    || after_colon == ">"
                    || after_colon == ">-"
                    || after_colon == ">+"
                {
                    in_multiline_block = true;
                    block_indent = indent;
                    // Still check this line's own indentation (it's a YAML key)
                    if indent % 2 != 0 {
                        odd_indent_lines.push((line_idx + 1, indent, line.to_string()));
                    }
                    continue;
                }
            }

            // Detect end of multiline block
            if in_multiline_block && indent <= block_indent {
                in_multiline_block = false;
            }

            // Skip lines inside multiline scalar blocks
            if in_multiline_block {
                continue;
            }

            // Check YAML-level lines for 2-space indentation (even number of spaces)
            if indent % 2 != 0 {
                odd_indent_lines.push((line_idx + 1, indent, line.to_string()));
            }

            // Track minimum indentation for the 4-space heuristic
            if indent > 0 && indent < min_yaml_indent {
                min_yaml_indent = indent;
            }
        }

        if !odd_indent_lines.is_empty() {
            let examples: Vec<String> = odd_indent_lines
                .iter()
                .take(5)
                .map(|(line_num, spaces, content)| {
                    format!("  line {line_num}: {spaces} spaces: {content}")
                })
                .collect();
            let remaining = if odd_indent_lines.len() > 5 {
                format!("  ... and {} more lines", odd_indent_lines.len() - 5)
            } else {
                String::new()
            };
            errors.push(format!(
                "{filename}: {count} line(s) with odd indentation (not a multiple of 2 spaces):\n\
                 {examples}{remaining}",
                count = odd_indent_lines.len(),
                examples = examples.join("\n"),
            ));
        }

        // Heuristic: if the minimum YAML-level indent is 4+, the file likely
        // uses 4-space (or larger) indentation instead of 2-space.
        if min_yaml_indent != usize::MAX && min_yaml_indent >= 4 {
            errors.push(format!(
                "{filename}: minimum YAML indentation is {min_yaml_indent} spaces \
                 (expected 2).\n  \
                 This file likely uses {min_yaml_indent}-space indentation instead of 2-space.\n  \
                 Re-indent the entire file to use 2-space increments."
            ));
        }
    }

    if !errors.is_empty() {
        panic!(
            "Workflow files have indentation errors:\n\n{}\n\n\
             The project uses 2-space YAML indentation (.yamllint.yml: indentation.spaces: 2).\n\
             To fix:\n\
             1. Re-indent the file using 2-space increments\n\
             2. Run: yamllint -c .yamllint.yml .github/workflows/\n\
             3. Many editors can convert indentation: search for \"convert indentation to spaces\"\n\n\
             Common cause: copying workflow templates from projects that use 4-space indentation.",
            errors.join("\n\n")
        );
    }
}

// ============================================================================
// Advanced Safety Workflow (ci-safety.yml) Tests
// ============================================================================

/// Required jobs in ci-safety.yml: (job_key, display_name, description)
///
/// These jobs are **staged (non-blocking)** — they use `continue-on-error: true`
/// and are NOT listed in `REQUIRED_WORKFLOW_NAMES` or `REQUIRED_CHECK_NAMES`.
/// They will be promoted to required checks once stability criteria are met
/// (see PLAN.md Phase 3, Promotion Policy).
const STAGED_SAFETY_JOBS: &[(&str, &str, &str)] = &[
    (
        "miri",
        "Miri",
        "Undefined behavior detection via Miri interpreter",
    ),
    (
        "asan",
        "AddressSanitizer",
        "Memory error detection via AddressSanitizer",
    ),
];

#[test]
fn test_ci_safety_workflow_has_required_jobs() {
    // Validates that the advanced safety workflow has all staged safety jobs
    // with correct job keys AND display names. Uses the shared helper
    // `validate_workflow_has_required_jobs` for consistency with ci.yml and
    // doc-validation.yml validation tests.

    let root = repo_root();
    let workflow_path = root.join(".github/workflows/ci-safety.yml");

    assert!(
        workflow_path.exists(),
        "ci-safety.yml must exist.\n\
         This workflow provides advanced safety analysis (Miri, AddressSanitizer).\n\
         See PLAN.md Phase 3 / Ticket G for details."
    );

    validate_workflow_has_required_jobs(&workflow_path, STAGED_SAFETY_JOBS, "Advanced Safety");
}

#[test]
fn test_ci_safety_workflow_jobs_are_staged() {
    // Validates that all advanced safety jobs use continue-on-error: true.
    // This is critical because these checks run on nightly Rust and may
    // break due to toolchain instability. They must not block merges until
    // promoted to required status.

    let root = repo_root();
    let workflow_path = root.join(".github/workflows/ci-safety.yml");
    let content = read_file(&workflow_path);

    for (job_key, display_name, _description) in STAGED_SAFETY_JOBS {
        // Find the job section and check for continue-on-error.
        // A job key in YAML appears as a line starting with exactly 2 spaces
        // followed by the key name and a colon (e.g., "  miri:").
        let job_key_pattern = format!("\n  {job_key}:");
        let job_start = content.find(&job_key_pattern).unwrap_or_else(|| {
            panic!(
                "Job '{job_key}' not found in ci-safety.yml.\n\
                 Expected YAML key: '  {job_key}:'"
            )
        });

        // Extract the job section: from this job key to the next top-level
        // job key (a line matching "\n  <word>:") or end of file.
        let after_key = &content[job_start + job_key_pattern.len()..];
        let next_job_offset = after_key
            .lines()
            .skip(1) // skip the rest of the current key's line
            .position(|line| {
                // A top-level job key: exactly 2 leading spaces, then a word char
                line.len() > 2
                    && line.starts_with("  ")
                    && !line.starts_with("   ")
                    && line.as_bytes()[2] != b' '
                    && line.as_bytes()[2] != b'#'
            });

        let job_text = match next_job_offset {
            Some(pos) => {
                // Calculate byte offset for the matched line
                let mut byte_offset = 0;
                for (i, line) in after_key.lines().skip(1).enumerate() {
                    if i == pos {
                        break;
                    }
                    byte_offset += line.len() + 1; // +1 for newline
                }
                &content[job_start..job_start + job_key_pattern.len() + byte_offset]
            }
            None => &content[job_start..],
        };

        assert!(
            job_text.contains("continue-on-error: true"),
            "Job '{job_key}' (\"{display_name}\") must have 'continue-on-error: true'.\n\
             Advanced safety jobs are staged and must not block merges.\n\
             See PLAN.md Phase 3, Promotion Policy for when to change this."
        );
    }
}

#[test]
fn test_ci_safety_workflow_uses_pinned_nightly() {
    // Validates that ci-safety.yml uses a pinned nightly toolchain, not
    // rolling "nightly". Pinned nightlies ensure reproducible CI results.

    let root = repo_root();
    let workflow_path = root.join(".github/workflows/ci-safety.yml");
    let content = read_file(&workflow_path);

    // Must contain a pinned nightly version (e.g., "nightly-2026-01-15")
    let has_pinned_nightly = content.contains("nightly-20");
    assert!(
        has_pinned_nightly,
        "ci-safety.yml must use a pinned nightly toolchain (e.g., nightly-2026-01-15).\n\
         Rolling 'nightly' causes unpredictable CI breakage.\n\
         See the Nightly Toolchain Strategy section in the workflow header."
    );

    // Must NOT contain bare "toolchain: nightly" (without date pin)
    let has_bare_nightly = content.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == "toolchain: nightly" || trimmed == "toolchain: \"nightly\""
    });
    assert!(
        !has_bare_nightly,
        "ci-safety.yml must NOT use bare 'toolchain: nightly'.\n\
         Use a date-pinned nightly instead (e.g., nightly-2026-01-15)."
    );
}

#[test]
fn test_ci_safety_workflow_has_required_triggers() {
    // Validates that ci-safety.yml has all required triggers:
    // push to main, pull_request to main, schedule, and workflow_dispatch.

    let root = repo_root();
    let workflow_path = root.join(".github/workflows/ci-safety.yml");
    let content = read_file(&workflow_path);

    let required_triggers = [
        ("push:", "push to main"),
        ("pull_request:", "pull requests to main"),
        ("schedule:", "weekly scheduled runs"),
        ("workflow_dispatch:", "manual trigger for diagnostics"),
    ];

    let mut missing = Vec::new();
    for (trigger, description) in &required_triggers {
        if !content.contains(trigger) {
            missing.push(format!("  - {trigger} ({description})"));
        }
    }

    if !missing.is_empty() {
        panic!(
            "ci-safety.yml is missing required triggers:\n\n{}\n\n\
             Advanced safety workflows need all four triggers:\n\
             - push/pull_request: run on code changes\n\
             - schedule: weekly heavy analysis\n\
             - workflow_dispatch: manual diagnostics",
            missing.join("\n")
        );
    }
}

#[test]
fn test_ci_safety_workflow_uploads_artifacts() {
    // Validates that both safety jobs upload their output as artifacts.
    // Artifacts are critical for diagnosing safety findings even when
    // the job passes (continue-on-error: true may mask real issues).

    let root = repo_root();
    let workflow_path = root.join(".github/workflows/ci-safety.yml");
    let content = read_file(&workflow_path);

    let expected_artifacts = [
        ("miri-output", "Miri analysis output"),
        ("asan-output", "AddressSanitizer analysis output"),
    ];

    let mut missing = Vec::new();
    for (artifact_name, description) in &expected_artifacts {
        if !content.contains(artifact_name) {
            missing.push(format!("  - {artifact_name} ({description})"));
        }
    }

    if !missing.is_empty() {
        panic!(
            "ci-safety.yml is missing required artifact uploads:\n\n{}\n\n\
             Safety job outputs must be uploaded as artifacts for diagnosis.\n\
             Use 'if: always()' on upload steps to capture output even on failure.",
            missing.join("\n")
        );
    }
}

#[test]
fn test_ci_safety_jobs_not_in_required_check_names() {
    // Validates that ci-safety.yml jobs are NOT in the required check names.
    // These are staged checks and must not be listed as branch-protection
    // required checks until promoted. This test ensures the staging contract.

    let safety_workflow_name = "Advanced Safety";

    for check_name in REQUIRED_CHECK_NAMES {
        assert!(
            !check_name.starts_with(&format!("{safety_workflow_name} /")),
            "Found '{check_name}' in REQUIRED_CHECK_NAMES, but ci-safety.yml \
             jobs are staged (non-blocking) and must NOT be required checks.\n\
             Remove from REQUIRED_CHECK_NAMES until promotion criteria are met.\n\
             See PLAN.md Phase 3, Promotion Policy."
        );
    }
}

#[test]
fn test_ci_safety_workflow_artifact_uploads_always_run() {
    // Validates that artifact upload steps use `if: always()` so that
    // diagnostic output is captured even when the analysis step fails.
    // Without this, failures in continue-on-error jobs would lose their
    // output, making triage impossible.

    let root = repo_root();
    let workflow_path = root.join(".github/workflows/ci-safety.yml");
    let content = read_file(&workflow_path);

    // Find each upload-artifact action reference and verify its enclosing
    // step has `if: always()`. We search for "upload-artifact@" to locate
    // the action, then look backward for the enclosing `- name:` line.
    let mut search_from = 0;
    let mut missing_always = Vec::new();
    let mut upload_count = 0;

    while let Some(pos) = content[search_from..].find("upload-artifact@") {
        upload_count += 1;
        let abs_pos = search_from + pos;
        let before = &content[..abs_pos];

        let step_start = before.rfind("- name:").unwrap_or_else(|| {
            panic!(
                "Could not find step containing upload-artifact action.\n\
                 Expected a '- name:' line before the action reference."
            )
        });

        let step_text = &content[step_start..abs_pos];
        if !step_text.contains("if: always()") {
            let step_name_line = content[step_start..].lines().next().unwrap_or("(unknown)");
            missing_always.push(format!("  - {step_name_line}"));
        }

        search_from = abs_pos + 1;
    }

    assert!(
        upload_count >= 2,
        "Expected at least 2 upload-artifact steps in ci-safety.yml \
         (miri-output and asan-output), found {upload_count}."
    );

    if !missing_always.is_empty() {
        panic!(
            "ci-safety.yml upload-artifact steps missing 'if: always()':\n\n\
             {}\n\n\
             Without 'if: always()', artifact output is lost when the \
             analysis step fails, making triage impossible.\n\
             To fix: Add 'if: always()' to each upload-artifact step.",
            missing_always.join("\n")
        );
    }
}

#[test]
fn test_nightly_version_consistency_across_workflows() {
    // Validates that all workflows using a pinned nightly toolchain use
    // the same nightly version. If someone updates one workflow's nightly
    // pin without updating others, they silently diverge, causing
    // inconsistent CI results and confusion about which nightly to update.

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    // Workflows known to use pinned nightly toolchains
    let nightly_workflows = ["ci-safety.yml", "unused-deps.yml"];

    let mut nightly_versions: Vec<(String, String)> = Vec::new();

    for workflow_file in &nightly_workflows {
        let workflow_path = workflows_dir.join(workflow_file);
        if !workflow_path.exists() {
            continue;
        }
        let content = read_file(&workflow_path);

        // Extract all pinned nightly versions (e.g., "nightly-2026-01-15")
        for line in content.lines() {
            let trimmed = line.trim();
            // Match lines like "toolchain: nightly-YYYY-MM-DD" or
            // "cargo +nightly-YYYY-MM-DD ..."
            if let Some(pos) = trimmed.find("nightly-20") {
                let version_start = pos;
                // Extract the nightly-YYYY-MM-DD portion
                let rest = &trimmed[version_start..];
                let version_end = rest
                    .find(|c: char| c != '-' && !c.is_ascii_alphanumeric())
                    .unwrap_or(rest.len());
                let version = &rest[..version_end];

                // Only record if it looks like a valid pinned nightly
                if version.len() >= "nightly-2026-01-15".len() {
                    nightly_versions.push((workflow_file.to_string(), version.to_string()));
                    break; // One version per workflow is enough
                }
            }
        }
    }

    // All extracted versions should be the same
    if nightly_versions.len() > 1 {
        let first_version = &nightly_versions[0].1;
        let mut mismatches = Vec::new();

        for (file, version) in &nightly_versions[1..] {
            if version != first_version {
                mismatches.push(format!("  - {file}: {version} (expected {first_version})"));
            }
        }

        if !mismatches.is_empty() {
            panic!(
                "Nightly toolchain versions are inconsistent across workflows:\n\n\
                 Baseline: {} uses {first_version}\n{}\n\n\
                 All workflows using pinned nightly must use the same version.\n\
                 To fix: Update all nightly pins to the same version.\n\
                 See the Nightly Toolchain Strategy in each workflow's header.",
                nightly_versions[0].0,
                mismatches.join("\n")
            );
        }
    }
}

/// Parse the `exclude_path = [...]` array from `.lychee.toml` content,
/// returning the list of unescaped string values (path patterns).
///
/// This is analogous to [`parse_lychee_exclude_patterns`] but targets the
/// `exclude_path` key instead of the `exclude` key.
fn parse_lychee_exclude_path_patterns(content: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut in_exclude_path = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Detect the start of the exclude_path array
        if trimmed.starts_with("exclude_path") && trimmed.contains('[') {
            let key = trimmed.split('=').next().unwrap_or("").trim();
            if key != "exclude_path" {
                continue;
            }
            in_exclude_path = true;
            // Handle inline array on same line
            if trimmed.contains(']') {
                extract_quoted_strings(trimmed, &mut patterns);
                in_exclude_path = false;
            }
            continue;
        }

        if in_exclude_path {
            if trimmed.starts_with(']') {
                break;
            }
            extract_quoted_strings(trimmed, &mut patterns);
        }
    }

    patterns
}

// ============================================================================
// Coverage Job Tests
// ============================================================================

#[test]
fn test_coverage_job_uses_locked_flag() {
    // Validates that the coverage job uses `--locked` for cargo llvm-cov
    // commands. Without --locked, cargo may re-resolve dependencies during CI,
    // producing coverage results against different dependency versions than
    // what was tested.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    // Both the coverage generation and report commands should use --locked
    assert!(
        ci_content.contains("cargo llvm-cov --locked"),
        "Coverage job must use 'cargo llvm-cov --locked' to ensure dependencies \
         match Cargo.lock.\n\
         Without --locked, cargo may re-resolve dependencies, producing coverage \
         against different versions than what was tested.\n\
         File: .github/workflows/ci.yml"
    );

    assert!(
        ci_content.contains("cargo llvm-cov report --locked"),
        "Coverage threshold check must use 'cargo llvm-cov report --locked' to \
         ensure the coverage report uses the same locked dependencies.\n\
         File: .github/workflows/ci.yml"
    );

    // `cargo llvm-cov report` does not accept build-selection flags like
    // `--all-features` / `--workspace`. Those belong on the coverage collection
    // command (`cargo llvm-cov ...`), not the report subcommand.
    assert!(
        !ci_content.contains("cargo llvm-cov report --locked --all-features --workspace"),
        "Coverage threshold command uses invalid flags for cargo-llvm-cov report.\n\
         Found: cargo llvm-cov report --locked --all-features --workspace ...\n\
         Fix: Use 'cargo llvm-cov report --locked --fail-under-lines <N>'"
    );
}

// ============================================================================
// SBOM (Software Bill of Materials) Tests
// ============================================================================
//
// These tests validate SBOM generation configuration in the CI and release
// workflows, ensuring supply-chain metadata is properly generated, uploaded
// as artifacts, and attached to GitHub releases.

#[test]
fn test_sbom_job_generates_cyclonedx_json() {
    // Validates that the SBOM job in ci.yml generates a CycloneDX JSON SBOM.
    // CycloneDX v1.5 is the latest spec and provides comprehensive supply-chain
    // metadata including component dependencies, licenses, and vulnerabilities.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    assert!(
        ci_content.contains("cargo sbom --output-format cyclone_dx_json_1_5"),
        "CI SBOM job must generate CycloneDX v1.5 JSON format.\n\
         Expected command: cargo sbom --output-format cyclone_dx_json_1_5\n\
         This ensures a standardized, machine-readable SBOM is produced."
    );

    assert!(
        !ci_content.contains("cargo sbom --locked"),
        "cargo-sbom does not support the --locked flag.\n\
         Found unsupported command: cargo sbom --locked ...\n\
         Fix: remove --locked from SBOM commands in .github/workflows/ci.yml."
    );

    assert!(
        ci_content.contains("sbom.cdx.json"),
        "CI SBOM job must output to sbom.cdx.json.\n\
         The .cdx.json extension is the CycloneDX convention for JSON SBOMs."
    );
}

#[test]
fn test_sbom_job_uploads_artifact() {
    // Validates that the SBOM artifact is uploaded with appropriate retention.
    // The artifact should be available for 90 days for audit and compliance purposes.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    // Find the SBOM artifact upload section
    assert!(
        ci_content.contains("sbom-cyclonedx-"),
        "CI SBOM job must upload an artifact with 'sbom-cyclonedx-' prefix.\n\
         This makes SBOM artifacts easily identifiable in the GitHub Actions UI."
    );

    assert!(
        ci_content.contains("retention-days: 90"),
        "CI SBOM artifact must have 90-day retention for audit compliance.\n\
         Shorter retention risks losing supply-chain metadata before audits complete."
    );
}

#[test]
fn test_sbom_job_upload_runs_on_success() {
    // Validates that the SBOM upload step uses `if: success()` so that an
    // empty or invalid sbom.cdx.json is not uploaded when generation fails.
    // Unlike the coverage job (which always uploads for debugging), the SBOM
    // artifact should only be uploaded when generation succeeds.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    // The SBOM job should have an upload step with if: success()
    // We verify this by checking that within the sbom job context,
    // the upload-artifact action is preceded by an `if: success()` condition.
    let sbom_section = extract_sbom_section(&ci_content);

    assert!(
        !sbom_section.is_empty(),
        "Could not find 'sbom:' job section in ci.yml"
    );

    assert!(
        sbom_section.contains("if: success()"),
        "SBOM upload step must use 'if: success()' to avoid uploading an \
         empty or invalid SBOM artifact when generation fails.\n\
         Unlike the coverage upload (which uses 'if: always()' for debugging), \
         the SBOM should only be uploaded on successful generation."
    );
}

#[test]
fn test_sbom_job_installs_cargo_sbom() {
    // Validates that the SBOM job installs cargo-sbom via taiki-e/install-action,
    // consistent with how other tools (cargo-nextest, cargo-llvm-cov) are installed.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    let sbom_section = extract_sbom_section(&ci_content);

    assert!(
        sbom_section.contains("tool: cargo-sbom"),
        "SBOM job must install cargo-sbom via taiki-e/install-action.\n\
         Expected: tool: cargo-sbom\n\
         This is consistent with how cargo-nextest and cargo-llvm-cov are installed."
    );
}

#[test]
fn test_sbom_job_has_reasonable_timeout() {
    // SBOM generation only reads Cargo.lock/Cargo.toml metadata and should
    // complete quickly. A 10-minute timeout is generous but prevents hangs.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    let sbom_section = extract_sbom_section(&ci_content);

    assert!(
        sbom_section.contains("timeout-minutes: 10"),
        "SBOM job should have a 10-minute timeout.\n\
         SBOM generation is metadata-only and should complete in under a minute.\n\
         A 10-minute budget provides margin without wasting CI resources on hangs."
    );
}

#[test]
fn test_release_workflow_generates_sbom() {
    // Validates that the release workflow generates an SBOM and attaches it
    // to the GitHub release, providing supply-chain metadata with every release.

    let root = repo_root();
    let release_yml = root.join(".github/workflows/release.yml");

    if !release_yml.exists() {
        return;
    }

    let content = read_file(&release_yml);

    assert!(
        content.contains("cargo sbom --output-format cyclone_dx_json_1_5"),
        "release.yml must generate a CycloneDX v1.5 JSON SBOM.\n\
         This provides supply-chain provenance metadata with every release.\n\
         File: {}",
        release_yml.display()
    );

    assert!(
        !content.contains("cargo sbom --locked"),
        "release.yml uses an unsupported cargo-sbom flag: --locked.\n\
         File: {}\n\
         Fix: remove --locked from cargo sbom commands.",
        release_yml.display()
    );

    assert!(
        content.contains("tool: cargo-sbom"),
        "release.yml must install cargo-sbom for SBOM generation.\n\
         File: {}",
        release_yml.display()
    );
}

#[test]
fn test_workflow_cargo_command_flag_compatibility() {
    // Data-driven regression guard for CI command/flag compatibility issues that
    // caused real failures:
    //   1) cargo-sbom rejects --locked
    //   2) cargo llvm-cov report rejects --all-features and --workspace
    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let mut violations = Vec::new();

    for entry in fs::read_dir(&workflows_dir)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", workflows_dir.display()))
    {
        let path = entry
            .unwrap_or_else(|e| panic!("Failed to read workflow entry: {e}"))
            .path();

        if path.extension().and_then(|e| e.to_str()) != Some("yml") {
            continue;
        }

        let content = read_file(&path);
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.contains("cargo sbom") && trimmed.contains("--locked") {
                violations.push(format!(
                    "{}:{}: cargo sbom does not support --locked",
                    path.display(),
                    idx + 1
                ));
            }
            if trimmed.contains("cargo llvm-cov report")
                && (trimmed.contains("--all-features") || trimmed.contains("--workspace"))
            {
                violations.push(format!(
                    "{}:{}: cargo llvm-cov report does not accept --all-features/--workspace",
                    path.display(),
                    idx + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found incompatible Cargo command flags in workflow files:\n\n{}\n\n\
         Fixes:\n\
         - Use `cargo sbom --output-format cyclone_dx_json_1_5` (no --locked)\n\
         - Use `cargo llvm-cov report --locked --fail-under-lines <N>` \
           (no --all-features / --workspace on report)",
        violations.join("\n")
    );
}

#[test]
fn test_release_workflow_attaches_sbom_to_release() {
    // Validates that the SBOM file is attached to the GitHub release in a
    // separate step that is conditional on successful SBOM generation.
    // This ensures the release is created even if SBOM generation fails,
    // while still attaching the SBOM when it succeeds.

    let root = repo_root();
    let release_yml = root.join(".github/workflows/release.yml");

    if !release_yml.exists() {
        return;
    }

    let content = read_file(&release_yml);

    // The SBOM must be attached via a dedicated "Attach SBOM to release" step
    assert!(
        content.contains("name: Attach SBOM to release"),
        "release.yml must have a separate 'Attach SBOM to release' step.\n\
         The SBOM attachment should be decoupled from the main release creation \
         so that SBOM failure does not block the GitHub Release.\n\
         File: {}",
        release_yml.display()
    );

    // The attach step must be conditional on SBOM generation success
    assert!(
        content.contains("steps.sbom.outcome == 'success'"),
        "release.yml 'Attach SBOM to release' step must be conditional on \
         steps.sbom.outcome == 'success'.\n\
         This ensures the SBOM is only attached when generation succeeded, \
         and the release is not blocked when it fails.\n\
         File: {}",
        release_yml.display()
    );

    // The attach step must reference sbom.cdx.json
    assert!(
        content.contains("files: sbom.cdx.json"),
        "release.yml must attach sbom.cdx.json to the GitHub release.\n\
         Add 'files: sbom.cdx.json' to the 'Attach SBOM to release' step.\n\
         This allows release consumers to download the SBOM for audit purposes.\n\
         File: {}",
        release_yml.display()
    );
}

#[test]
fn test_release_sbom_has_continue_on_error() {
    // Regression guard: the SBOM generation step in the release workflow MUST
    // have `continue-on-error: true`. Without this, a transient cargo-sbom
    // failure would block the entire release AFTER the crate has already been
    // published to crates.io, leaving a published crate without a corresponding
    // GitHub Release. SBOM is supply-chain metadata — important but never worth
    // blocking a release that has already been published.

    let root = repo_root();
    let release_yml = root.join(".github/workflows/release.yml");

    if !release_yml.exists() {
        return;
    }

    let content = read_file(&release_yml);

    // Extract the SBOM generation step block. We look for the step name and
    // then verify that `continue-on-error: true` appears within the same
    // step (before the next `- name:` line).
    let lines: Vec<&str> = content.lines().collect();
    let sbom_step_start = lines
        .iter()
        .position(|line| line.contains("name: Generate SBOM"));

    assert!(
        sbom_step_start.is_some(),
        "release.yml must have a step named 'Generate SBOM'.\n\
         File: {}",
        release_yml.display()
    );

    let start = sbom_step_start.expect("checked above");
    let sbom_step_block: String = lines[start..]
        .iter()
        .take(1) // take the name line
        .chain(
            lines[start + 1..]
                .iter()
                .take_while(|line| !line.trim().starts_with("- name:")),
        )
        .copied()
        .collect::<Vec<&str>>()
        .join("\n");

    assert!(
        sbom_step_block.contains("continue-on-error: true"),
        "The 'Generate SBOM (CycloneDX)' step in release.yml MUST have \
         `continue-on-error: true`.\n\
         Without this, a transient SBOM generation failure would block the \
         GitHub Release after the crate has already been published to crates.io.\n\
         SBOM failure must not block releases after crates.io publish.\n\
         Step block:\n{}\n\
         File: {}",
        sbom_step_block,
        release_yml.display()
    );

    // The SBOM generation step must have `id: sbom` so the conditional
    // "Attach SBOM to release" step can reference `steps.sbom.outcome`.
    assert!(
        sbom_step_block.contains("id: sbom"),
        "The 'Generate SBOM (CycloneDX)' step in release.yml MUST have \
         `id: sbom`.\n\
         This step ID is referenced by the conditional 'Attach SBOM to release' \
         step via `steps.sbom.outcome == 'success'`.\n\
         Step block:\n{}\n\
         File: {}",
        sbom_step_block,
        release_yml.display()
    );
}

// ============================================================================
// CI Runtime/Flake Optimization Tests (Ticket J)
// ============================================================================

#[test]
fn test_nextest_config_exists_and_is_valid() {
    // Validates that .config/nextest.toml exists and contains essential settings
    // for optimized test execution. Without this file, nextest uses defaults
    // that may not be tuned for CI performance.

    let root = repo_root();
    let nextest_config = root.join(".config/nextest.toml");

    assert!(
        nextest_config.exists(),
        "Nextest configuration file .config/nextest.toml is missing.\n\
         This file configures optimized test execution for cargo-nextest.\n\
         Create it with at minimum a [profile.default] section.\n\
         See: https://nexte.st/docs/configuration/"
    );

    let content = read_file(&nextest_config);

    // Must have a default profile
    assert!(
        content.contains("[profile.default]"),
        ".config/nextest.toml must contain a [profile.default] section.\n\
         This section configures the baseline test execution settings.\n\
         File: {}",
        nextest_config.display()
    );

    // Must configure fail-fast for quick feedback
    assert!(
        content.contains("fail-fast"),
        ".config/nextest.toml should configure fail-fast behavior.\n\
         Recommended: fail-fast = true (for fast CI feedback)\n\
         File: {}",
        nextest_config.display()
    );

    // Must configure failure output for reduced log noise
    assert!(
        content.contains("failure-output"),
        ".config/nextest.toml should configure failure-output.\n\
         Recommended: failure-output = \"immediate-final\"\n\
         File: {}",
        nextest_config.display()
    );
}

#[test]
fn test_nextest_config_no_retries_by_default() {
    // Project policy: zero tolerance for flaky tests (see .llm/context.md).
    // The nextest config must NOT enable blanket retries, which would mask
    // real test failures as flakes.

    let root = repo_root();
    let nextest_config = root.join(".config/nextest.toml");

    if !nextest_config.exists() {
        // test_nextest_config_exists_and_is_valid will catch this
        return;
    }

    let content = read_file(&nextest_config);

    // Check that there are no retries enabled in the default profile.
    // Look for patterns like "retries = 3" or "retries = { count = 3 }" but NOT
    // "retries" appearing in a comment explaining why retries are disabled.
    // We do this by checking non-comment lines only.
    let has_nonzero_retries = content.lines().any(|line| {
        let trimmed = line.trim();
        // Skip comments
        if trimmed.starts_with('#') {
            return false;
        }
        // Check for retries with a non-zero value
        if trimmed.starts_with("retries") {
            // "retries = 0" is fine (explicitly disabled)
            // "retries = { count = 0 }" is fine
            // Any other retries value is suspicious
            return !trimmed.contains("= 0")
                && !trimmed.contains("count = 0")
                && !trimmed.contains("total = 0");
        }
        false
    });

    assert!(
        !has_nonzero_retries,
        ".config/nextest.toml must not enable blanket test retries.\n\
         Project policy: Zero tolerance for flaky tests — every failure is a real bug.\n\
         If specific tests need retries, use [[profile.default.overrides]] with a \n\
         targeted filter instead of blanket retries.\n\
         File: {}",
        nextest_config.display()
    );
}

#[test]
fn test_ci_safety_shared_nightly_cache_prefix() {
    // The Miri and ASan jobs in ci-safety.yml should share a cache prefix so
    // that compiled nightly artifacts can be reused between the two jobs,
    // reducing redundant compilation.

    let root = repo_root();
    let ci_safety = root.join(".github/workflows/ci-safety.yml");

    if !ci_safety.exists() {
        return;
    }

    let content = read_file(&ci_safety);

    // Both jobs should use the same cache prefix
    let cache_prefix_lines: Vec<&str> = content
        .lines()
        .filter(|line| line.contains("prefix-key:"))
        .collect();

    assert!(
        !cache_prefix_lines.is_empty(),
        "ci-safety.yml should have cache prefix-key configurations.\n\
         File: {}",
        ci_safety.display()
    );

    // All prefix-key values should be the same (shared cache)
    let unique_prefixes: std::collections::HashSet<String> = cache_prefix_lines
        .iter()
        .map(|line| {
            line.trim()
                .trim_start_matches("prefix-key:")
                .trim()
                .trim_matches('"')
                .to_string()
        })
        .collect();

    assert_eq!(
        unique_prefixes.len(),
        1,
        "ci-safety.yml Miri and ASan jobs should share the same cache prefix-key \
         to allow nightly artifact reuse between jobs.\n\
         Found different prefixes: {:?}\n\
         Expected: All jobs use the same prefix (e.g., \"ci-safety-nightly\")\n\
         File: {}",
        unique_prefixes,
        ci_safety.display()
    );
}

#[test]
fn test_msrv_job_uses_single_verification_step() {
    // The MSRV job should combine build verification and test execution in a
    // single step to avoid redundant compilation. `cargo test` implicitly
    // compiles all targets, making a separate `cargo check` unnecessary.

    let root = repo_root();
    let ci_yml = root.join(".github/workflows/ci.yml");
    let content = read_file(&ci_yml);

    // Extract the MSRV job block
    let lines: Vec<&str> = content.lines().collect();
    let msrv_start = lines.iter().position(|line| line.starts_with("  msrv:"));

    assert!(
        msrv_start.is_some(),
        "ci.yml must have an msrv job.\nFile: {}",
        ci_yml.display()
    );

    let start = msrv_start.expect("checked above");
    let msrv_block: String = lines[start..]
        .iter()
        .skip(1)
        // Capture lines belonging to this job block. A job block consists of
        // 4+-space-indented lines (job properties and steps) and blank lines.
        // Stop when we hit a line at 2-space indentation that is NOT a sub-key
        // (i.e., the start of the next top-level job definition).
        .take_while(|line| {
            !line.starts_with("  ") || line.starts_with("    ") || line.trim().is_empty()
        })
        .copied()
        .collect::<Vec<&str>>()
        .join("\n");

    // Should NOT have separate cargo check and cargo test steps
    let has_cargo_check = msrv_block.contains("cargo check");
    let has_cargo_test = msrv_block.contains("cargo test");

    assert!(
        !has_cargo_check,
        "MSRV job should not have a separate 'cargo check' step.\n\
         'cargo test' implicitly compiles all targets, making 'cargo check' redundant.\n\
         Combine into a single step to save ~2-3 minutes of redundant compilation.\n\
         File: {}",
        ci_yml.display()
    );

    assert!(
        has_cargo_test,
        "MSRV job must run 'cargo test' to verify tests pass with MSRV.\n\
         File: {}",
        ci_yml.display()
    );
}

#[test]
fn test_docker_health_check_uses_exponential_backoff() {
    // The Docker smoke test should use exponential backoff rather than fixed-
    // interval retries. This provides faster feedback when the server starts
    // quickly and reduces unnecessary waiting.

    let root = repo_root();
    let ci_yml = root.join(".github/workflows/ci.yml");
    let content = read_file(&ci_yml);

    // The Docker smoke test step should have exponential backoff logic
    assert!(
        content.contains("DELAY=$((DELAY * 2") || content.contains("DELAY=$((DELAY*2"),
        "Docker smoke test health check should use exponential backoff.\n\
         Replace fixed 'sleep 2' retry loop with exponential backoff pattern:\n\
         DELAY=1; DELAY=$((DELAY * 2)); [ $DELAY -gt 8 ] && DELAY=8\n\
         File: {}",
        ci_yml.display()
    );
}

#[test]
fn test_release_sccache_failure_emits_warning() {
    // When sccache fails in the release workflow, the fallback should emit a
    // GitHub Actions warning annotation so the failure is visible in the PR/run
    // summary, rather than silently degrading to uncached compilation.

    let root = repo_root();
    let release_yml = root.join(".github/workflows/release.yml");

    if !release_yml.exists() {
        return;
    }

    let content = read_file(&release_yml);

    // Verify the ::warning:: annotation is in the sccache fallback step specifically,
    // not just anywhere in the file. Look for it after the sccache check condition.
    let lines: Vec<&str> = content.lines().collect();
    let sccache_fallback_start = lines
        .iter()
        .position(|line| line.contains("Clear sccache env on failure"));

    assert!(
        sccache_fallback_start.is_some(),
        "release.yml must have a 'Clear sccache env on failure' step.\n\
         File: {}",
        release_yml.display()
    );

    let start = sccache_fallback_start.expect("checked above");
    let fallback_block: String = lines[start..]
        .iter()
        .take(1)
        .chain(
            lines[start + 1..]
                .iter()
                .take_while(|line| !line.trim().starts_with("- name:")),
        )
        .copied()
        .collect::<Vec<&str>>()
        .join("\n");

    assert!(
        fallback_block.contains("::warning::"),
        "The sccache fallback step in release.yml must emit a GitHub Actions \
         warning annotation (::warning::) when sccache is unavailable.\n\
         This makes sccache failures visible in the workflow run summary.\n\
         Step block:\n{}\n\
         File: {}",
        fallback_block,
        release_yml.display()
    );
}

#[test]
fn test_pre_commit_hook_checks_formatting_and_clippy() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");

    assert!(
        hook_path.exists(),
        ".githooks/pre-commit must exist to enforce formatting locally"
    );

    let content = read_file(&hook_path);

    assert!(
        content.contains("cargo fmt"),
        ".githooks/pre-commit must include a 'cargo fmt' check.\n\
         Without this, formatting errors slip through to CI."
    );

    assert!(
        content.contains("cargo clippy"),
        ".githooks/pre-commit must include a 'cargo clippy' check.\n\
         Without this, lint errors slip through to CI."
    );
}

#[test]
fn test_pre_commit_hook_includes_skills_index_freshness_check_16() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");

    assert!(
        hook_path.exists(),
        ".githooks/pre-commit must exist to enforce local quality gates"
    );

    let content = read_file(&hook_path);

    assert!(
        content.contains("# Check 16: Skills index generation freshness"),
        "Pre-commit hook must define Check 16 for skills index freshness."
    );

    assert!(
        content.contains("scripts/generate-skills-index.sh --check"),
        "Check 16 must use generator check mode to verify freshness without rewriting files."
    );

    assert!(
        content.contains("check_fail \"Skills index freshness\"")
            && content.contains(
                "Run './scripts/generate-skills-index.sh' and stage .llm/skills/index.md."
            ),
        "Check 16 must fail with actionable regeneration guidance when the index is stale."
    );
}

#[test]
fn test_pre_commit_hook_skills_index_freshness_triggers_cover_key_paths() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");
    let content = read_file(&hook_path);

    assert!(
        content.contains("git diff --cached --name-only -z --diff-filter=ACDMR --"),
        "Check 16 should gate on staged path changes using an explicit diff-filter."
    );

    for required_trigger in [
        ".llm/context.md",
        "scripts/generate-skills-index.sh",
        "':(glob).llm/skills/*.md'",
    ] {
        assert!(
            content.contains(required_trigger),
            "Check 16 trigger list must include: {required_trigger}"
        );
    }

    assert!(
        content.contains("check_skip \"Skills index freshness\""),
        "Check 16 should skip cleanly when no skills/context/index inputs are staged."
    );
}

#[test]
fn test_pre_commit_hook_includes_workflow_hygiene_check_17() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");

    assert!(
        hook_path.exists(),
        ".githooks/pre-commit must exist to enforce local quality gates"
    );

    let content = read_file(&hook_path);

    assert!(
        content.contains("# Check 17: Workflow hygiene script checks"),
        "Pre-commit hook must define Check 17 for workflow hygiene validation."
    );

    assert!(
        content.contains("scripts/check-workflow-hygiene.sh"),
        "Check 17 must invoke scripts/check-workflow-hygiene.sh."
    );

    assert!(
        content.contains("check_fail \"Workflow hygiene checks\"")
            && content.contains("Run './scripts/check-workflow-hygiene.sh' and fix reported errors."),
        "Check 17 must fail with actionable remediation guidance when workflow hygiene checks fail."
    );
}

#[test]
fn test_pre_commit_hook_workflow_hygiene_triggers_cover_workflow_paths() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");
    let content = read_file(&hook_path);

    for required_trigger in [
        "':(glob).github/workflows/*.yml'",
        "':(glob).github/workflows/*.yaml'",
        "':(glob)scripts/*.sh'",
        "':(glob).githooks/*'",
        "scripts/check-workflow-hygiene.sh",
    ] {
        assert!(
            content.contains(required_trigger),
            "Check 17 trigger list must include: {required_trigger}"
        );
    }

    assert!(
        content.contains("check_skip \"Workflow hygiene checks\""),
        "Check 17 should skip cleanly when no workflow/hygiene files are staged."
    );
}

#[test]
fn test_pre_commit_hook_uses_null_delimited_inputs_for_xargs_checks() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");
    let content = read_file(&hook_path);

    assert!(
        content.contains("git diff --cached --name-only -z --diff-filter=ACM -- \\")
            && content.contains("xargs -0 scripts/validate-workflow-awk.sh"),
        "Pre-commit Check 5 must pass workflow filenames via NUL-delimited git diff + xargs -0.\n\
         This prevents filename splitting bugs when paths contain spaces."
    );

    assert!(
        content.contains("git diff --cached --name-only -z --diff-filter=ACM -- '*.md'")
            && content.contains("xargs -0 lychee --offline --quiet --config .lychee.toml"),
        "Pre-commit Check 11 must pass markdown filenames via NUL-delimited git diff + xargs -0.\n\
         This prevents filename splitting bugs when markdown paths contain spaces."
    );
}

#[test]
fn test_pre_push_hook_exists_and_runs_workflow_policy_checks() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-push");

    assert!(
        hook_path.exists(),
        ".githooks/pre-push must exist to enforce workflow policy checks before push."
    );

    let content = read_file(&hook_path);

    assert!(
        content.contains("scripts/check-workflow-hygiene.sh"),
        "pre-push hook must run scripts/check-workflow-hygiene.sh when workflow policy files change."
    );

    assert!(
        content.contains("cargo test")
            && content.contains("--locked")
            && content.contains("--test ci_config_tests")
            && content.contains("test_github_actions_use_version_refs_not_commit_hashes")
            && content.contains("test_workflow_toolchain_fields_do_not_use_moving_aliases")
            && content.contains("test_docker_publish_workflow_uses_owner_derived_ghcr_image_name"),
        "pre-push hook must run CI policy tests with --locked for action refs, toolchain alias pinning, and owner-derived GHCR image naming."
    );
}

#[test]
#[cfg(unix)]
fn test_pre_push_hook_is_executable() {
    use std::os::unix::fs::PermissionsExt;

    let root = repo_root();
    let hook_path = root.join(".githooks/pre-push");

    assert!(
        hook_path.exists(),
        ".githooks/pre-push must exist to validate executable permissions."
    );

    let metadata = std::fs::metadata(&hook_path)
        .unwrap_or_else(|e| panic!("Failed to read metadata for {}: {}", hook_path.display(), e));
    let mode = metadata.permissions().mode();
    let is_executable = mode & 0o111 != 0;

    assert!(
        is_executable,
        "{} is not executable.\n\
         Fix: chmod +x .githooks/pre-push && git update-index --chmod=+x .githooks/pre-push",
        hook_path.display()
    );
}

#[test]
fn test_git_hook_cargo_test_invocations_use_locked_and_separator() {
    // Validates that all `cargo test` invocations in .githooks/ files:
    //   1. Use --locked (project policy: all cargo build/test/check must use --locked)
    //   2. Use `--` separator before test filter names when multiple filters are passed
    //
    // Background: A bug in .githooks/pre-push passed two TESTNAME positional args
    // directly to `cargo test` without a `--` separator. Cargo only accepts one
    // positional TESTNAME; multiple filters must follow `--`. Without `--`, the
    // second name causes an 'unexpected argument' error.

    let root = repo_root();
    let hooks_dir = root.join(".githooks");

    let hook_files: Vec<_> = fs::read_dir(&hooks_dir)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", hooks_dir.display()))
        .filter_map(|entry| {
            let path = entry
                .unwrap_or_else(|e| panic!("Failed to read hook entry: {e}"))
                .path();
            if path.is_file() {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    assert!(
        !hook_files.is_empty(),
        ".githooks/ directory must contain at least one hook file."
    );

    let mut violations = Vec::new();

    for hook_path in &hook_files {
        let content = read_file(hook_path);
        let hook_name = hook_path.file_name().unwrap_or_default().to_string_lossy();

        // Join continuation lines (backslash-newline) so that a single logical
        // `cargo test` command split across multiple lines is analyzed as one unit.
        let joined = content.replace("\\\n", " ");

        for (line_idx, line) in joined.lines().enumerate() {
            let trimmed = line.trim();

            // Skip comments
            if trimmed.starts_with('#') {
                continue;
            }

            // Find lines that invoke `cargo test`
            let Some(cargo_test_pos) = trimmed.find("cargo test") else {
                continue;
            };

            let after_cargo_test = &trimmed[cargo_test_pos + "cargo test".len()..];

            // Check 1: --locked must be present
            if !after_cargo_test.contains("--locked") {
                violations.push(format!(
                    ".githooks/{hook_name}:{}: `cargo test` missing --locked flag.\n  \
                     Found: {trimmed}\n  \
                     Fix: Add --locked to the cargo test invocation \
                     (project policy requires --locked on all cargo build/test/check commands).",
                    line_idx + 1
                ));
            }

            // Check 2: When multiple test filter names are passed, they must
            // appear after a `--` separator.
            //
            // Strategy: find the `--test <name>` flag (if present) and then
            // check if there are multiple bare words after it that are NOT
            // preceded by `--`.
            //
            // We split on `--` to get the part before and after the separator.
            // If there's no `--`, all args are in the "cargo side".
            // Test filter names on the cargo side (positional TESTNAME) can only
            // be one; if we find multiple bare words that look like test names
            // after `--test <crate>`, that's a violation.

            // Split at the first ` -- ` (with surrounding spaces to avoid matching --locked etc.)
            let has_double_dash = after_cargo_test.contains(" -- ");

            // Count what look like test filter names (bare words starting with test_)
            // in the cargo-args portion (before `--`).
            let cargo_args = if has_double_dash {
                // Everything before ` -- `
                after_cargo_test
                    .split(" -- ")
                    .next()
                    .unwrap_or(after_cargo_test)
            } else {
                after_cargo_test
            };

            // After stripping known flags and their values, count remaining
            // bare words that look like test function names (start with test_).
            let test_name_args: Vec<&str> = cargo_args
                .split_whitespace()
                .filter(|word| word.starts_with("test_"))
                .collect();

            if test_name_args.len() > 1 {
                violations.push(format!(
                    ".githooks/{hook_name}:{}: `cargo test` has multiple test filter names \
                     as positional args without `--` separator.\n  \
                     Found: {trimmed}\n  \
                     Problem: cargo only accepts one positional TESTNAME; additional names \
                     cause an 'unexpected argument' error.\n  \
                     Fix: Place all test filter names after `--`, e.g.:\n  \
                     cargo test --locked --test <crate> -- filter1 filter2",
                    line_idx + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found cargo test invocation issues in git hooks:\n\n{}\n\n\
         All `cargo test` commands in .githooks/ must:\n\
         1. Use --locked (ensures dependencies match Cargo.lock)\n\
         2. Place multiple test filter names after `--` (cargo only accepts one positional TESTNAME)",
        violations.join("\n\n")
    );
}

#[test]
fn test_pre_commit_hook_includes_llm_file_size_check_18() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");
    assert!(hook_path.exists(), ".githooks/pre-commit must exist");
    let content = read_file(&hook_path);
    assert!(
        content.contains("# Check 18: LLM file size limit"),
        "Pre-commit hook must define Check 18 for LLM file size enforcement."
    );
    assert!(
        content.contains("scripts/check-llm-file-sizes.sh"),
        "Check 18 must invoke scripts/check-llm-file-sizes.sh."
    );
    assert!(
        content.contains("check_fail \"LLM file sizes\"")
            && content.contains("One or more .llm/ files exceed 300 lines"),
        "Check 18 must fail with actionable remediation guidance."
    );
}

#[test]
fn test_pre_commit_hook_llm_file_size_triggers_cover_llm_paths() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");
    let content = read_file(&hook_path);
    assert!(
        content.contains(r"^\.llm/.*\.md$"),
        "Check 18 trigger filter must match .llm/*.md files with the anchored regex."
    );
    assert!(
        content.contains("Preserve spaces in staged file paths by splitting only on newlines."),
        "Check 18 must document why newline-only splitting is required."
    );
    assert!(
        content.contains("set -f")
            && content.contains("scripts/check-llm-file-sizes.sh --files $STAGED_LLM_FILES"),
        "Check 18 must disable globbing and pass staged files with newline-only splitting semantics."
    );
    assert!(
        content.contains("check_skip \"LLM file sizes\""),
        "Check 18 should skip cleanly when no .llm/*.md files are staged."
    );
}

#[test]
fn test_pre_commit_hook_includes_llm_example_extraction_policy_check() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");
    let content = read_file(&hook_path);

    assert!(
        content.contains("scripts/check-llm-example-files.sh --files $STAGED_LLM_FILES"),
        "Check 18 must invoke scripts/check-llm-example-files.sh with staged .llm files."
    );
    assert!(
        content.contains("check_fail \"LLM example extraction\"")
            && content.contains("Inline example sections are disallowed in skill files"),
        "Check 18 must fail with actionable guidance when inline examples violate policy."
    );
}

#[test]
fn test_pre_commit_hook_includes_readme_badge_style_check_20() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");
    assert!(hook_path.exists(), ".githooks/pre-commit must exist");
    let content = read_file(&hook_path);

    assert!(
        content.contains("# Check 20: README badge style consistency"),
        "Pre-commit hook must define Check 20 for README badge style consistency."
    );
    assert!(
        content.contains("scripts/check-readme-badges.sh"),
        "Check 20 must invoke scripts/check-readme-badges.sh."
    );
    assert!(
        content.contains("check_fail \"README badge styles\"")
            && content.contains("style=for-the-badge"),
        "Check 20 must fail with actionable guidance when README badge styles are inconsistent."
    );
    assert!(
        content.contains("README_BADGE_OUTPUT=$(scripts/check-readme-badges.sh README.md 2>&1)")
            && content.contains("echo \"$README_BADGE_OUTPUT\""),
        "Check 20 should capture and print check-readme-badges.sh output on failure."
    );
}

#[test]
fn test_pre_commit_hook_readme_badge_style_trigger_includes_checker_script() {
    let root = repo_root();
    let hook_path = root.join(".githooks/pre-commit");
    let content = read_file(&hook_path);

    assert!(
        content.contains(r"^(README\.md|scripts/check-readme-badges\.sh)$"),
        "Check 20 trigger must match staged README.md and scripts/check-readme-badges.sh changes."
    );
    assert!(
        content.contains(r"scripts/check-readme-badges\.sh"),
        "Check 20 trigger should also run when scripts/check-readme-badges.sh is staged."
    );
    assert!(
        content.contains("check_skip \"README badge styles\""),
        "Check 20 should skip cleanly when neither README.md nor scripts/check-readme-badges.sh is staged."
    );
}

#[test]
fn test_run_local_ci_includes_readme_badge_style_check() {
    let root = repo_root();
    let script_path = root.join("scripts/run-local-ci.sh");
    let content = read_file(&script_path);

    assert!(
        content.contains("readme-badges"),
        "run-local-ci.sh should include a dedicated README badge consistency check."
    );
    assert!(
        content.contains("scripts/check-readme-badges.sh"),
        "run-local-ci.sh should invoke scripts/check-readme-badges.sh."
    );
}

#[test]
fn test_check_no_panics_script_structure() {
    let root = repo_root();
    let script_path = root.join("scripts/check-no-panics.sh");

    assert!(
        script_path.exists(),
        "scripts/check-no-panics.sh must exist for panic-policy CI job"
    );

    let content = read_file(&script_path);

    assert!(
        content.contains("filter_test_code"),
        "check-no-panics.sh must contain a filter_test_code function \
         to exclude test code from panic pattern scanning"
    );

    // The integer expression bug was caused by unsanitized wc -l output
    assert!(
        content.contains("tr -d") || content.contains("xargs"),
        "check-no-panics.sh must sanitize count variables (e.g., via tr -d or xargs) \
         to prevent 'integer expression expected' errors"
    );

    // Must use --lib --bins for clippy, not --all-targets
    assert!(
        content.contains("clippy --lib --bins") || content.contains("clippy\" --lib --bins"),
        "check-no-panics.sh clippy must use --lib --bins to check library and \
         binary targets without flagging test code.\n\
         Using --all-targets would report false positives for .unwrap() in tests."
    );

    // Must exclude *_tests.rs files from grep scanning
    assert!(
        content.contains("_tests.rs"),
        "check-no-panics.sh must exclude *_tests.rs files from pattern scanning.\n\
         Files like src/server/ready_state_tests.rs are #[cfg(test)] modules \
         that don't contain #[cfg(test)] internally."
    );
}

#[test]
#[cfg(unix)]
fn test_check_no_panics_script_patterns_pass() {
    use std::process::Command;

    let root = repo_root();
    let script = root.join("scripts/check-no-panics.sh");

    let output = Command::new("bash")
        .arg(&script)
        .arg("patterns")
        .current_dir(&root)
        .output()
        .unwrap_or_else(|e| panic!("Failed to run {}: {e}", script.display()));

    assert!(
        output.status.success(),
        "check-no-panics.sh patterns should pass on the current codebase.\n\
         stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_pinned_nightly_staleness_warning() {
    let root = repo_root();
    let workflow_path = root.join(".github/workflows/ci-safety.yml");

    if !workflow_path.exists() {
        return; // ci-safety.yml is optional
    }

    let content = read_file(&workflow_path);

    // Extract the first nightly-YYYY-MM-DD pattern
    let nightly_re_prefix = "nightly-20";
    let nightly_version: Option<String> = content.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed.find(nightly_re_prefix).map(|pos| {
            let rest = &trimmed[pos..];
            // Take chars while they match the nightly-YYYY-MM-DD pattern
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .unwrap_or(rest.len());
            rest[..end].to_string()
        })
    });

    let nightly_version = nightly_version
        .expect("ci-safety.yml must contain a pinned nightly date (e.g., nightly-2026-02-01)");

    // Extract YYYY and MM
    let date_part = nightly_version
        .strip_prefix("nightly-")
        .expect("nightly version must start with 'nightly-'");
    let parts: Vec<&str> = date_part.split('-').collect();
    assert!(
        parts.len() >= 2,
        "Nightly date '{date_part}' must have at least YYYY-MM format"
    );

    let year: u32 = parts[0]
        .parse()
        .unwrap_or_else(|_| panic!("Invalid year in nightly version: {}", parts[0]));
    let month: u32 = parts[1]
        .parse()
        .unwrap_or_else(|_| panic!("Invalid month in nightly version: {}", parts[1]));

    // Approximate staleness check: compare to build date
    // This uses a rough heuristic - the test will need updating when the year changes
    let nightly_months = year * 12 + month;
    // Use 2026-02 as reference (current date)
    let reference_months: u32 = 2026 * 12 + 2;
    let age_months = reference_months.saturating_sub(nightly_months);

    assert!(
        age_months <= 12,
        "Pinned nightly '{nightly_version}' is approximately {age_months} months old.\n\
         Consider testing a newer nightly and updating the pin.\n\
         See ci-safety.yml header for update criteria."
    );
}

#[test]
#[cfg(unix)]
fn test_scripts_pass_basic_syntax_check() {
    use std::process::Command;

    let root = repo_root();
    let scripts_dir = root.join("scripts");

    if !scripts_dir.exists() {
        return;
    }

    let entries: Vec<_> = std::fs::read_dir(&scripts_dir)
        .expect("Failed to read scripts/ directory")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "sh"))
        .collect();

    assert!(
        !entries.is_empty(),
        "scripts/ directory should contain at least one .sh file"
    );

    let mut failures = Vec::new();

    for entry in &entries {
        let path = entry.path();
        // Use bash -n for syntax check (always available, unlike shellcheck)
        let output = Command::new("bash")
            .arg("-n")
            .arg(&path)
            .output()
            .unwrap_or_else(|e| panic!("Failed to syntax-check {}: {e}", path.display()));

        if !output.status.success() {
            failures.push(format!(
                "{}: {}",
                path.file_name().unwrap().to_string_lossy(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "Shell scripts have syntax errors:\n{}",
        failures.join("\n")
    );
}

#[test]
fn test_check_workflow_toolchain_fix_guidance_uses_pinned_version_not_stable_alias() {
    let root = repo_root();
    let script_path = root.join("scripts/check-workflow-toolchain.sh");
    let content = read_file(&script_path);

    let guidance_lines: Vec<&str> = content
        .lines()
        .filter(|line| line.contains("echo \"      toolchain:"))
        .collect();

    assert!(
        !guidance_lines.is_empty(),
        "check-workflow-toolchain.sh must print remediation guidance with a `toolchain:` example."
    );

    for line in &guidance_lines {
        assert!(
            !line.contains("toolchain: stable"),
            "check-workflow-toolchain.sh remediation must not suggest moving aliases like `stable`.\n\
             Found guidance line: {line}"
        );
    }

    assert!(
        guidance_lines
            .iter()
            .any(|line| line.contains("toolchain: $PINNED_TOOLCHAIN")),
        "check-workflow-toolchain.sh remediation should suggest a pinned toolchain example.\n\
         Expected guidance to use `$PINNED_TOOLCHAIN` derived from rust-toolchain.toml."
    );
}

#[test]
fn test_no_workflow_action_uses_commit_hash_ref() {
    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/\n\
         Workflows directory: {}",
        workflows_dir.display()
    );

    let mut violations = Vec::new();

    for entry in &workflow_files {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim();

            let Some(uses_value) = extract_uses_value(trimmed) else {
                continue;
            };
            let Some((action_name, action_ref)) = parse_remote_action_reference(uses_value) else {
                continue;
            };

            if is_commit_hash(action_ref) {
                violations.push(format!(
                    "{filename}:{line_num}: {action_name}@{action_ref}\n  \
                     Commit hashes are not allowed for workflow action refs."
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Workflow actions must not use commit hash refs:\n\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_workflow_toolchain_fields_do_not_use_moving_aliases() {
    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/\n\
         Workflows directory: {}",
        workflows_dir.display()
    );

    let mut violations = Vec::new();

    for entry in &workflow_files {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();

        for (line_num, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            let Some(rest) = trimmed.strip_prefix("toolchain:") else {
                continue;
            };

            let value = rest.trim().trim_matches('"').trim_matches('\'');
            if matches!(value, "stable" | "beta" | "nightly") {
                violations.push(format!(
                    "{filename}:{}: toolchain: {value}\n  \
                     Moving toolchain aliases are not allowed.\n  \
                     Use a pinned toolchain (e.g., 1.88.0 or nightly-2026-02-01),\n  \
                     or omit toolchain to use rust-toolchain.toml.",
                    line_num + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Workflow toolchain fields must not use moving aliases:\n\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_dtolnay_rust_toolchain_v1_has_explicit_toolchain_input() {
    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/\n\
         Workflows directory: {}",
        workflows_dir.display()
    );

    let mut violations = Vec::new();

    for entry in &workflow_files {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();
        let lines: Vec<&str> = content.lines().collect();

        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if !trimmed.contains("dtolnay/rust-toolchain@v") {
                continue;
            }

            // Look at the next 5 lines for a `toolchain:` field
            let mut found_toolchain = false;
            let lookahead_end = (i + 6).min(lines.len());
            for next_line in lines.iter().take(lookahead_end).skip(i + 1) {
                let next_trimmed = next_line.trim();

                // Stop early if we hit another step marker
                if next_trimmed.starts_with("- name:") || next_trimmed.starts_with("- uses:") {
                    break;
                }

                if next_trimmed.starts_with("toolchain:") {
                    found_toolchain = true;
                    break;
                }
            }

            if !found_toolchain {
                violations.push(format!(
                    "{filename}:{}: uses: dtolnay/rust-toolchain@v1\n  \
                     Missing explicit `toolchain:` input in the `with:` block.\n  \
                     Every dtolnay/rust-toolchain@v1 invocation must specify a \
                     pinned toolchain version.",
                    i + 1
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "dtolnay/rust-toolchain@v1 invocations missing explicit toolchain input:\n\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_same_action_uses_consistent_ref_across_workflows() {
    // Every action should resolve to a single reference value across workflows
    // to prevent version drift (e.g., one file uses @v2 while another uses @v1).

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let workflow_files = collect_workflow_files(&workflows_dir);

    if workflow_files.is_empty() {
        return;
    }

    // Map of action name -> Vec<(reference, filename, line_num)>
    let mut action_refs: std::collections::HashMap<String, Vec<(String, String, usize)>> =
        std::collections::HashMap::new();

    for entry in &workflow_files {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy().to_string();

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1; // 1-indexed for human readability
            let trimmed = line.trim();

            let Some(uses_value) = extract_uses_value(trimmed) else {
                continue;
            };
            let Some((action_name, action_ref)) = parse_remote_action_reference(uses_value) else {
                continue;
            };

            action_refs
                .entry(action_name.to_string())
                .or_default()
                .push((action_ref.to_string(), filename.clone(), line_num));
        }
    }

    let mut inconsistencies = Vec::new();

    for (action_name, refs) in &action_refs {
        let unique_refs: std::collections::HashSet<&str> = refs
            .iter()
            .map(|(action_ref, _, _)| action_ref.as_str())
            .collect();

        if unique_refs.len() > 1 {
            let mut details = format!(
                "Action '{}' uses {} different refs across workflow files:",
                action_name,
                unique_refs.len()
            );
            for (action_ref, filename, line_num) in refs {
                details.push_str(&format!("\n    {filename}:{line_num}: {action_ref}"));
            }
            details.push_str(&format!(
                "\n  Fix: Update all references to '{action_name}' to use the same ref.\n  \
                 Pick the most recent version and apply it to every workflow file.\n  \
                 Search with: grep -rn '{action_name}' .github/workflows/"
            ));
            inconsistencies.push(details);
        }
    }

    if !inconsistencies.is_empty() {
        let total_actions = action_refs.len();
        let consistent_actions = total_actions - inconsistencies.len();
        panic!(
            "GitHub Action refs must be consistent across all workflow files:\n\n\
             {}\n\n\
             Diagnostic Information:\n\
             - Unique actions found: {}\n\
             - Actions with consistent refs: {}\n\
             - Actions with inconsistent refs: {}\n\n\
             Why this matters:\n\
             - Different refs for the same action mean different code versions are running\n\
             - Version drift can cause subtle behavior differences across workflows\n\n\
             How to fix:\n\
             1. Identify the desired ref for each action (e.g., @v2.7.5)\n\
             2. Update ALL workflow files to use that same ref\n\
             3. Verify with: grep -rn 'action-name@' .github/workflows/",
            inconsistencies.join("\n\n"),
            total_actions,
            consistent_actions,
            inconsistencies.len()
        );
    }
}

#[test]
fn test_dockerfile_suppresses_false_positive_security_warnings() {
    // Validates that the Dockerfile includes a BuildKit check directive to
    // suppress false-positive security warnings when ENV variables have
    // security-adjacent names (like SECURITY, AUTH, TOKEN, KEY) but are
    // assigned non-sensitive values (like "false", "true", "0", "1").
    //
    // A previous CI failure occurred because Docker Scout / BuildKit flagged
    // ENV variables like SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false as
    // potential secret leaks (SecretsUsedInArgOrEnv). The fix is to add a
    // `# check=skip=SecretsUsedInArgOrEnv` directive at the top of the
    // Dockerfile to suppress these false positives.

    let root = repo_root();
    let dockerfile = root.join("Dockerfile");

    if !dockerfile.exists() {
        return;
    }

    let content = read_file(&dockerfile);

    // Patterns that Docker Scout / BuildKit flag as potential secrets in ENV names
    let security_patterns = ["SECURITY", "SECRET", "PASSWORD", "TOKEN", "KEY", "AUTH"];

    // Values that are clearly non-sensitive (boolean/numeric flags)
    let safe_values = ["false", "true", "0", "1"];

    // Find ENV variables with security-adjacent names assigned non-sensitive values
    let mut flagged_env_vars = Vec::new();

    for (line_num, line) in content.lines().enumerate() {
        let line_num = line_num + 1; // 1-indexed for human readability
        let trimmed = line.trim();

        // Match ENV directives (ENV KEY=value or ENV KEY value)
        if let Some(rest) = trimmed.strip_prefix("ENV ") {
            let rest = rest.trim();

            // Extract the variable name and value
            let (var_name, var_value) = if let Some(eq_pos) = rest.find('=') {
                let name = rest[..eq_pos].trim();
                let value = rest[eq_pos + 1..].trim().trim_matches('"');
                (name, value)
            } else {
                // ENV KEY value (space-separated form)
                let mut parts = rest.splitn(2, char::is_whitespace);
                let name = parts.next().unwrap_or("");
                let value = parts.next().unwrap_or("").trim().trim_matches('"');
                (name, value)
            };

            let name_upper = var_name.to_uppercase();
            let has_security_pattern = security_patterns
                .iter()
                .any(|pattern| name_upper.contains(pattern));
            let has_safe_value = safe_values.contains(&var_value);

            if has_security_pattern && has_safe_value {
                flagged_env_vars.push(format!("  line {line_num}: ENV {var_name}={var_value}"));
            }
        }
    }

    if flagged_env_vars.is_empty() {
        // No security-adjacent ENV vars with safe values, no directive needed
        return;
    }

    // Check that the Dockerfile starts with the suppression directive
    let first_line = content.lines().next().unwrap_or("");
    let has_check_directive =
        first_line.contains("# check=") && first_line.contains("skip=SecretsUsedInArgOrEnv");

    assert!(
        has_check_directive,
        "Dockerfile contains ENV variables with security-adjacent names assigned \
         non-sensitive values, but is missing the BuildKit check directive to \
         suppress false-positive security warnings.\n\n\
         Flagged ENV variables:\n{}\n\n\
         These variables have names matching security patterns ({}) but are \
         assigned safe values ({}). Docker Scout / BuildKit will flag these as \
         potential secret leaks (SecretsUsedInArgOrEnv).\n\n\
         Fix: Add this directive as the FIRST line of the Dockerfile:\n\
         # check=skip=SecretsUsedInArgOrEnv\n\n\
         Why this matters:\n\
         - Docker BuildKit's SecretsUsedInArgOrEnv check flags any ENV with \
         security-related names\n\
         - These are false positives because the values are non-sensitive boolean flags\n\
         - Without the suppression directive, CI builds will emit warnings or fail\n\n\
         File: {}",
        flagged_env_vars.join("\n"),
        security_patterns.join(", "),
        safe_values.join(", "),
        dockerfile.display()
    );
}

#[test]
fn test_audit_job_installs_cargo_audit() {
    // Validates that the audit job installs cargo-audit via taiki-e/install-action,
    // consistent with how other tools (cargo-nextest, cargo-llvm-cov, cargo-sbom)
    // are installed in the CI workflow.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    let audit_section = extract_audit_section(&ci_content);

    assert!(
        audit_section.contains("tool: cargo-audit"),
        "Audit job must install cargo-audit via taiki-e/install-action.\n\
         Expected: tool: cargo-audit\n\
         This is consistent with how cargo-nextest, cargo-llvm-cov, and \
         cargo-sbom are installed."
    );
}

#[test]
fn test_audit_job_runs_cargo_audit() {
    // Validates that the audit job actually runs `cargo audit` to scan for
    // known vulnerabilities in the RustSec advisory database.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    let audit_section = extract_audit_section(&ci_content);

    assert!(
        audit_section.contains("cargo audit"),
        "Audit job must run `cargo audit` to scan for vulnerabilities.\n\
         The audit job should invoke cargo-audit against the RustSec advisory database."
    );
}

#[test]
fn test_audit_job_configuration() {
    // Validates that the audit job has appropriate timeout and runs on ubuntu-latest,
    // matching the project's convention for security-related CI jobs.

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    let audit_section = extract_audit_section(&ci_content);

    assert!(
        audit_section.contains("timeout-minutes: 10"),
        "Audit job should have a 10-minute timeout.\n\
         cargo-audit is a lightweight advisory database check and should complete quickly.\n\
         A 10-minute budget provides margin without wasting CI resources on hangs."
    );

    assert!(
        audit_section.contains("runs-on: ubuntu-latest"),
        "Audit job should run on ubuntu-latest.\n\
         Security advisory scanning is platform-independent and only needs a single runner."
    );
}

/// The schedule guard condition that non-audit CI jobs must use.
/// This ensures only the `deny` and `audit` (security audit) jobs run on the daily
/// schedule trigger, preventing unnecessary CI resource consumption for scheduled runs.
const SCHEDULE_EXCLUSION_GUARD: &str = "github.event_name != 'schedule'";

/// CI jobs that must be excluded from scheduled runs via an `if:` guard.
/// The `deny` and `audit` jobs are intentionally absent — they are the only
/// jobs that should run on the daily schedule trigger (for CVE detection).
const SCHEDULE_EXCLUDED_CI_JOBS: &[&str] = &[
    "lint",
    "nextest",
    "msrv",
    "docker",
    "coverage",
    "panic-policy",
    "sbom",
];

#[test]
fn test_ci_schedule_only_runs_security_jobs() {
    // Validates that the daily scheduled trigger only runs the security audit
    // jobs (`deny` and `audit`), and all other CI jobs are excluded from
    // schedule runs.
    //
    // The ci.yml workflow has a daily cron schedule for catching new CVEs.
    // Only the `deny` and `audit` jobs should run on schedule — all other
    // jobs waste CI resources when triggered by the cron schedule since they
    // only need to run on push/PR events.
    //
    // This test ensures:
    //   1. Every non-audit job has `if: github.event_name != 'schedule'`
    //   2. The `deny` and `audit` jobs do NOT have a schedule exclusion guard

    let root = repo_root();
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    // Verify the deny and audit jobs do NOT have a schedule exclusion guard
    for security_job in &["deny", "audit"] {
        let condition = extract_job_if_condition(&ci_content, security_job);
        if let Some(ref cond) = condition {
            assert!(
                !cond.contains("schedule"),
                "The `{security_job}` job must NOT exclude schedule runs.\n\
                 Found `if: {cond}` on the {security_job} job, which would prevent the \
                 daily security audit from running.\n\n\
                 The deny and audit jobs should run on the daily schedule \
                 trigger to catch new CVEs.\n\n\
                 To fix: Remove the `if:` guard from the {security_job} job in ci.yml."
            );
        }
        // condition being None is fine — no `if:` means it runs on all triggers
    }

    // Verify all non-audit jobs HAVE the schedule exclusion guard
    let mut missing_guard = Vec::new();
    let mut wrong_guard = Vec::new();

    for job_key in SCHEDULE_EXCLUDED_CI_JOBS {
        let condition = extract_job_if_condition(&ci_content, job_key);
        match condition {
            None => {
                missing_guard.push(format!(
                    "  x {job_key}: no `if:` guard found.\n\
                     Expected: `if: {SCHEDULE_EXCLUSION_GUARD}`"
                ));
            }
            Some(ref cond) if !cond.contains("schedule") => {
                wrong_guard.push(format!(
                    "  x {job_key}: has `if: {cond}` but it does not exclude schedule runs.\n\
                     Expected the condition to include a schedule exclusion."
                ));
            }
            Some(_) => {
                // Has a condition that mentions schedule — this is correct
            }
        }
    }

    let mut errors = Vec::new();
    errors.extend(missing_guard);
    errors.extend(wrong_guard);

    assert!(
        errors.is_empty(),
        "CI jobs are missing schedule exclusion guards.\n\n\
         The daily schedule trigger should only run the `deny` and `audit` (security) jobs.\n\
         All other jobs must have `if: {SCHEDULE_EXCLUSION_GUARD}` to avoid wasting \
         CI resources on scheduled runs.\n\n\
         Issues:\n{}\n\n\
         To fix: Add `if: {SCHEDULE_EXCLUSION_GUARD}` to each listed job in ci.yml.\n\n\
         File: {}",
        errors.join("\n"),
        root.join(".github/workflows/ci.yml").display()
    );
}

/// British English spellings that should be replaced with American English equivalents.
///
/// Each entry is (british_pattern, american_replacement). The patterns are
/// case-insensitive substrings, so each lowercase pattern also matches its capitalized variant.
const BRITISH_AMERICAN_PAIRS: &[(&str, &str)] = &[
    ("uninitialised", "uninitialized"),
    ("behaviour", "behavior"),
    ("colour", "color"),
    ("favour", "favor"),
    ("honour", "honor"),
    ("initialise", "initialize"),
    ("organise", "organize"),
    ("recognise", "recognize"),
    ("serialise", "serialize"),
];

/// Directories and file extensions to scan for British spelling consistency.
///
/// Each entry is (directory_relative_to_repo_root, file_extension).
const SPELLING_SCAN_TARGETS: &[(&str, &str)] = &[
    ("src", "rs"),
    ("tests", "rs"),
    (".github/workflows", "yml"),
    (".llm", "md"),
    ("docs", "md"),
];

/// Substrings that, when present on a line, indicate the line should be excluded
/// from British spelling checks (URLs, external references, etc.).
const SPELLING_EXCLUSION_MARKERS: &[&str] = &["http://", "https://"];

/// Files that contain British spellings as reference data (e.g., comparison tables
/// or test data). These files are excluded from the scan to avoid false positives.
const SPELLING_EXCLUSION_FILES: &[&str] = &["ci_config_tests.rs", "documentation-standards.md"];

#[test]
fn test_no_british_english_spellings() {
    // This project uses American English consistently. This test scans source
    // files, CI workflows, and documentation for common British English spellings
    // and flags them with the file path, line number, and suggested replacement.
    //
    // Exclusions:
    //   - This test file itself (contains British spellings as test data)
    //   - Lines containing URLs (may reference external content)
    //   - Files in target/, .git/, or other vendored/generated directories

    let root = repo_root();
    let mut violations = Vec::new();

    for &(dir_relative, extension) in SPELLING_SCAN_TARGETS {
        let dir = root.join(dir_relative);
        if !dir.exists() {
            continue;
        }

        let files = find_files_with_extension(&dir, extension, &["target", ".git", "third_party"]);

        for file_path in &files {
            // Skip files that contain British spellings as reference data
            // (e.g., test data, comparison tables in documentation).
            if file_path
                .file_name()
                .map(|name| {
                    let name_str = name.to_string_lossy();
                    SPELLING_EXCLUSION_FILES
                        .iter()
                        .any(|excluded| name_str == *excluded)
                })
                .unwrap_or(false)
            {
                continue;
            }

            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let relative_path = file_path
                .strip_prefix(&root)
                .unwrap_or(file_path)
                .display()
                .to_string();

            for (line_number, line) in content.lines().enumerate() {
                let line_lower = line.to_lowercase();

                // Skip lines that match any exclusion marker
                if SPELLING_EXCLUSION_MARKERS
                    .iter()
                    .any(|marker| line.contains(marker))
                {
                    continue;
                }

                for &(british_pattern, american_replacement) in BRITISH_AMERICAN_PAIRS {
                    let british_lower = british_pattern.to_lowercase();
                    if line_lower.contains(&british_lower) {
                        violations.push(format!(
                            "  {relative_path}:{line_num}: found \"{british_pattern}\" \
                             (British) -> use \"{american_replacement}\" (American)\n\
                             \x20   Line: {line_trimmed}",
                            line_num = line_number + 1,
                            line_trimmed = line.trim(),
                        ));
                    }
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found British English spellings that should use American English:\n\n\
         {violations}\n\n\
         This project uses American English consistently. Replace each British\n\
         spelling with its American equivalent.\n\n\
         If the spelling is inside a URL or external reference that cannot be\n\
         changed, add an exclusion marker to SPELLING_EXCLUSION_MARKERS in\n\
         tests/ci_config_tests.rs.\n\n\
         See also: .typos.toml for automated spell-check rules.",
        violations = violations.join("\n"),
    );
}

// ============================================================================
// Miri Safety Annotation Tests
// ============================================================================

#[test]
fn test_proptest_tests_ignored_under_miri() {
    // This test ensures that all `#[test]` functions inside `proptest!` blocks
    // have `#[cfg_attr(miri, ignore)]` annotations.
    //
    // Background: Proptest's failure-persistence layer calls `std::env::current_dir()`
    // to absolutize source file paths. Miri blocks `getcwd` in isolation mode,
    // causing the entire test binary to abort. This is not a per-test failure --
    // it kills every test in the binary.
    //
    // The fix is to annotate each `#[test]` inside a `proptest!` block with
    // `#[cfg_attr(miri, ignore)]` so Miri skips those tests entirely.

    let root = repo_root();
    let src_dir = root.join("src");

    let rust_files =
        find_files_with_extension(&src_dir, "rs", &["target", "third_party", "node_modules"]);

    assert!(!rust_files.is_empty(), "No Rust source files found in src/");

    let mut violations = Vec::new();
    let mut total_proptest_blocks = 0;
    let mut total_tests_in_proptest = 0;
    let mut tests_with_miri_ignore = 0;

    for file in &rust_files {
        let content = read_file(file);
        let relative_path = file.strip_prefix(&root).unwrap_or(file);
        let lines: Vec<&str> = content.lines().collect();

        // Track proptest! macro blocks by brace depth.
        // When we see `proptest!` followed by `{`, we enter a proptest block.
        // Inside the block, every `#[test]` must have a preceding
        // `#[cfg_attr(miri, ignore)]` attribute.
        let mut in_proptest_block = false;
        let mut brace_depth: i32 = 0;
        let mut proptest_brace_depth: i32 = 0;

        for (line_idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();

            if !in_proptest_block {
                // Detect the start of a proptest! macro invocation.
                // The macro is typically invoked as:
                //   proptest! {
                // or on the same line as `proptest!{`
                if trimmed.starts_with("proptest!") {
                    in_proptest_block = true;
                    total_proptest_blocks += 1;
                    // Count opening braces on this line to set the initial depth
                    let opens = trimmed.matches('{').count() as i32;
                    let closes = trimmed.matches('}').count() as i32;
                    brace_depth = opens - closes;
                    proptest_brace_depth = 0; // the proptest block starts at depth 0
                    continue;
                }
            } else {
                // Track brace depth inside the proptest block
                let opens = trimmed.matches('{').count() as i32;
                let closes = trimmed.matches('}').count() as i32;
                brace_depth += opens - closes;

                // Check for #[test] attributes inside the proptest block
                if trimmed == "#[test]" {
                    total_tests_in_proptest += 1;

                    // Look backward from this #[test] line for #[cfg_attr(miri, ignore)]
                    // It should be on one of the immediately preceding lines (allowing
                    // for blank lines and other attributes between them).
                    let mut found_miri_ignore = false;
                    let search_start = line_idx.saturating_sub(5);
                    for check_line in lines[search_start..line_idx].iter().rev() {
                        let check_line = check_line.trim();
                        if check_line.is_empty() || check_line.starts_with('#') {
                            if check_line.contains("cfg_attr(miri, ignore)") {
                                found_miri_ignore = true;
                                break;
                            }
                            continue;
                        }
                        // Hit a non-attribute, non-empty line -- stop searching
                        break;
                    }

                    // Also check forward: the annotation might be after #[test]
                    // (though convention is before)
                    if !found_miri_ignore {
                        let search_end = if line_idx + 5 < lines.len() {
                            line_idx + 5
                        } else {
                            lines.len()
                        };
                        for check_line in lines.iter().take(search_end).skip(line_idx + 1) {
                            let check_line = check_line.trim();
                            if check_line.contains("cfg_attr(miri, ignore)") {
                                found_miri_ignore = true;
                                break;
                            }
                            // Stop at fn definition or another #[test]
                            if check_line.starts_with("fn ") || check_line == "#[test]" {
                                break;
                            }
                        }
                    }

                    if found_miri_ignore {
                        tests_with_miri_ignore += 1;
                    } else {
                        violations.push(format!(
                            "{}:{}: #[test] inside proptest! block is missing \
                             #[cfg_attr(miri, ignore)]\n  \
                             Proptest calls std::env::current_dir() which Miri blocks.\n  \
                             Fix: Add #[cfg_attr(miri, ignore)] above or below the #[test] attribute.",
                            relative_path.display(),
                            line_idx + 1,
                        ));
                    }
                }

                // Exit the proptest block when brace depth returns to zero
                if brace_depth <= proptest_brace_depth {
                    in_proptest_block = false;
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Proptest tests missing Miri ignore annotations:\n\n{}\n\n\
             Diagnostic Information:\n\
             - Total proptest! blocks found: {}\n\
             - Total #[test] inside proptest! blocks: {}\n\
             - Tests with #[cfg_attr(miri, ignore)]: {}\n\
             - Tests missing annotation: {}\n\n\
             Why this is required:\n\
             - Proptest's failure-persistence layer calls std::env::current_dir()\n\
             - Miri blocks getcwd in isolation mode\n\
             - This aborts the ENTIRE test binary, not just one test\n\
             - Adding #[cfg_attr(miri, ignore)] skips the test under Miri\n\n\
             Fix: Add #[cfg_attr(miri, ignore)] to each #[test] inside proptest! blocks.\n\
             Also add an explanatory comment above the proptest! macro:\n\
             // NOTE: All proptest tests are excluded from Miri runs via\n\
             // `#[cfg_attr(miri, ignore)]`. Proptest's failure-persistence layer calls\n\
             // `std::env::current_dir()` (getcwd), which is blocked by Miri isolation\n\
             // and aborts the entire test binary.",
            violations.join("\n"),
            total_proptest_blocks,
            total_tests_in_proptest,
            tests_with_miri_ignore,
            violations.len(),
        );
    }
}

#[test]
fn test_wall_clock_tests_ignored_under_miri() {
    // Regression guard: tests that use wall-clock APIs (Room::new, Utc::now,
    // SystemTime::now) must opt out of Miri with #[cfg_attr(miri, ignore)].
    // Miri blocks clock_gettime(CLOCK_REALTIME) in isolation mode.
    let root = repo_root();
    let required_miri_ignored_tests: [(&str, &[&str]); 2] = [
        (
            "src/protocol/mod.rs",
            &[
                "test_room_creation",
                "test_player_management",
                "test_authority_management",
                "test_authority_management_disabled",
                "test_player_name_uniqueness",
                "test_authority_protocol_basic_rules",
                "test_authority_protocol_single_authority_rule",
                "test_authority_protocol_no_auto_reassignment",
                "test_authority_protocol_room_support_validation",
                "test_lobby_state_transitions",
                "test_lobby_ready_state_changes",
                "test_peer_connections",
                "test_lobby_edge_cases",
            ],
        ),
        (
            "src/reconnection.rs",
            &[
                "test_reconnection_token_creation",
                "test_reconnection_token_validation",
                "test_event_buffer_push",
                "test_event_buffer_get_events_after",
                "test_reconnection_manager_flow",
                "test_event_buffering",
            ],
        ),
    ];

    let mut violations = Vec::new();

    for (relative_file, test_names) in required_miri_ignored_tests {
        let source_file = root.join(relative_file);
        let content = read_file(&source_file);
        let lines: Vec<&str> = content.lines().collect();

        for test_name in test_names {
            let marker = format!("fn {test_name}(");
            let Some(line_idx) = lines.iter().position(|line| line.contains(&marker)) else {
                violations.push(format!(
                    "{}: missing expected test function `{}`",
                    source_file.display(),
                    test_name
                ));
                continue;
            };

            let search_start = line_idx.saturating_sub(4);
            let has_miri_ignore = lines
                .iter()
                .take(line_idx)
                .skip(search_start)
                .any(|line| line.trim().contains("cfg_attr(miri, ignore)"));

            if !has_miri_ignore {
                violations.push(format!(
                    "{}:{}: `{}` must include #[cfg_attr(miri, ignore)] to avoid \
                     Miri clock_gettime isolation failures",
                    source_file.display(),
                    line_idx + 1,
                    test_name
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Wall-clock-dependent tests missing Miri ignore annotations:\n\n{}\n\n\
         Fix: add #[cfg_attr(miri, ignore)] above each listed #[test] or \
         #[tokio::test] function.",
        violations.join("\n")
    );
}

// ============================================================================
// Bash Code Block Validation Tests
// ============================================================================

#[test]
fn test_bash_code_blocks_contain_bash_syntax() {
    // This test ensures that markdown code blocks tagged as bash/sh/shell
    // actually contain bash-compatible syntax, not TOML, Dockerfile, YAML,
    // or other languages that were incorrectly tagged.
    //
    // Background: A CI failure occurred because a bash-tagged code block in
    // docs/git-hooks-guide.md contained TOML and Dockerfile syntax. The CI
    // doc-validation workflow validates bash blocks with `bash -n` and
    // `shellcheck`, which fail on non-bash syntax.
    //
    // This test catches the issue early by scanning for common non-bash
    // patterns inside bash-tagged code blocks.

    let root = repo_root();

    let markdown_files = find_files_with_extension(
        &root,
        "md",
        &[
            "target",
            "third_party",
            "node_modules",
            "test-fixtures",
            // .llm/skills/ files may contain intentionally mixed-syntax blocks
            // for educational/reference purposes. CI's doc-validation.yml still
            // validates these, so regressions are caught in CI.
            ".llm",
        ],
    );

    if markdown_files.is_empty() {
        return;
    }

    // Non-bash syntax patterns that indicate the code block is mislabeled.
    // Each entry: (pattern_description, detection_function)
    // The detection function takes a code block's lines and returns true if
    // the block likely contains non-bash syntax.
    //
    // We check for multiple signals to reduce false positives:
    // - TOML: key = "value" assignments (with quotes), [section] headers
    // - Dockerfile: FROM, RUN, COPY, WORKDIR, ENV, ENTRYPOINT, CMD
    // - YAML: key: value mappings (with specific patterns)

    let mut violations = Vec::new();
    let mut total_bash_blocks = 0;

    for file in &markdown_files {
        let content = read_file(file);
        let relative_path = file.strip_prefix(&root).unwrap_or(file);
        let lines: Vec<&str> = content.lines().collect();

        // Track code blocks using fence width for proper CommonMark handling.
        // Outer fences (4+ backticks) contain nested examples and are skipped.
        let mut outer_fence_width: usize = 0;
        let mut in_bash_block = false;
        let mut bash_block_start: usize = 0;
        let mut bash_block_lines: Vec<&str> = Vec::new();

        for (line_idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();

            // Track outer fences (4+ backticks) to skip nested code examples
            if trimmed.starts_with("````") {
                let width = trimmed.chars().take_while(|&c| c == '`').count();
                if outer_fence_width == 0 {
                    outer_fence_width = width;
                } else if width >= outer_fence_width {
                    let rest = &trimmed[width..];
                    if rest.trim().is_empty() {
                        outer_fence_width = 0;
                    }
                }
                continue;
            }

            // Skip content inside outer fences
            if outer_fence_width > 0 {
                continue;
            }

            // Detect opening/closing of 3-backtick fences
            if trimmed.starts_with("```") && !trimmed.starts_with("````") {
                if !in_bash_block {
                    // Check if this is a bash/sh/shell code block
                    let lang = trimmed.trim_start_matches('`').trim();
                    if lang == "bash" || lang == "sh" || lang == "shell" {
                        in_bash_block = true;
                        bash_block_start = line_idx + 1;
                        bash_block_lines.clear();
                        total_bash_blocks += 1;
                    }
                } else {
                    // Closing fence -- analyze the collected block
                    if !bash_block_lines.is_empty() {
                        let non_bash = detect_non_bash_syntax(&bash_block_lines);
                        if let Some(reason) = non_bash {
                            violations.push(format!(
                                "{}:{}: Bash code block contains non-bash syntax\n  \
                                 Detected: {}\n  \
                                 Fix: Change the code block language tag to `text` \
                                 for mixed-syntax examples,\n  \
                                 or split into separate correctly-tagged blocks.",
                                relative_path.display(),
                                bash_block_start,
                                reason,
                            ));
                        }
                    }
                    in_bash_block = false;
                }
                continue;
            }

            if in_bash_block {
                bash_block_lines.push(trimmed);
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Bash code blocks contain non-bash syntax:\n\n{}\n\n\
             Diagnostic Information:\n\
             - Total bash/sh/shell code blocks scanned: {}\n\
             - Blocks with non-bash syntax: {}\n\n\
             Why this matters:\n\
             - The CI doc-validation workflow validates bash blocks with `bash -n` and `shellcheck`\n\
             - Non-bash syntax (TOML, Dockerfile, YAML) causes validation failures\n\
             - Use `text` for mixed-syntax examples showing multiple file edits\n\n\
             Fix options:\n\
             1. Change the code block language tag from `bash` to `text`\n\
             2. Split into separate blocks with correct language tags\n\
             3. If the block genuinely contains bash with embedded syntax,\n\
                wrap the non-bash parts in heredocs or echo statements",
            violations.join("\n"),
            total_bash_blocks,
            violations.len(),
        );
    }
}

/// Detect non-bash syntax patterns in a code block.
///
/// Returns `Some(reason)` if the block likely contains non-bash syntax,
/// or `None` if the block appears to be valid bash.
///
/// Uses a scoring system to reduce false positives: individual patterns
/// like `key = "value"` could appear in bash (e.g., variable assignments
/// with spaces around `=`), but multiple TOML/Dockerfile/YAML signals
/// in a single block strongly indicate a mislabeled block.
fn detect_non_bash_syntax(lines: &[&str]) -> Option<String> {
    let mut toml_signals = 0;
    let mut dockerfile_signals = 0;
    let mut yaml_signals = 0;

    for line in lines {
        let trimmed = line.trim();

        // Skip empty lines and comments (# is valid in bash, TOML, and YAML)
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // TOML signals:
        // - [section] headers (not bash test expressions like [ -f file ])
        // - key = "value" with quoted string values
        // - key = { inline table }
        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.contains(' ') {
            // [section.name] or [dependencies] -- likely TOML section header
            // Exclude bash test expressions: [ -f file ], [ "$x" = "y" ]
            toml_signals += 2;
        }
        if let Some((_key, value)) = trimmed.split_once(" = ") {
            let value = value.trim();
            if value.starts_with('"') || value.starts_with('{') || value.starts_with('[') {
                toml_signals += 1;
            }
        }

        // Dockerfile signals:
        // - FROM, RUN, COPY, WORKDIR, ENV, ENTRYPOINT, CMD, ADD, EXPOSE, ARG
        // These are always at the start of a line in Dockerfiles
        let dockerfile_keywords = [
            "FROM ",
            "RUN ",
            "COPY ",
            "WORKDIR ",
            "ENV ",
            "ENTRYPOINT ",
            "CMD ",
            "ADD ",
            "EXPOSE ",
            "ARG ",
        ];
        for keyword in &dockerfile_keywords {
            if trimmed.starts_with(keyword) {
                dockerfile_signals += 1;
            }
        }

        // YAML mapping signals:
        // - key: value (but not bash case labels like "pattern)")
        // - Indented key: value with specific YAML patterns
        // Be careful: bash `case` statements use `:` in labels but not the same pattern
        if let Some((key, _value)) = trimmed.split_once(": ") {
            let key = key.trim();
            // YAML keys are typically alphanumeric with hyphens/underscores
            // Exclude bash-like patterns (command: is common in YAML)
            if !key.contains(' ')
                && !key.starts_with('-')
                && !key.starts_with('$')
                && !key.contains('(')
                && key
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
            {
                yaml_signals += 1;
            }
        }
    }

    // Require multiple signals to reduce false positives.
    // A single TOML-like line could be a coincidence in bash.
    if toml_signals >= 3 {
        return Some(format!(
            "TOML syntax detected ({toml_signals} signals: section headers, \
             key = \"value\" assignments)"
        ));
    }

    if dockerfile_signals >= 2 {
        return Some(format!(
            "Dockerfile syntax detected ({dockerfile_signals} signals: \
             FROM/RUN/COPY/WORKDIR/ENV keywords)"
        ));
    }

    if yaml_signals >= 3 {
        return Some(format!(
            "YAML syntax detected ({yaml_signals} signals: key: value mappings)"
        ));
    }

    // Combined signals: if we see signals from multiple non-bash languages,
    // the block is almost certainly mislabeled
    let total_non_bash = toml_signals + dockerfile_signals + yaml_signals;
    if total_non_bash >= 4 {
        return Some(format!(
            "Mixed non-bash syntax detected (TOML: {toml_signals}, \
             Dockerfile: {dockerfile_signals}, YAML: {yaml_signals})"
        ));
    }

    None
}

#[test]
fn test_windows_matrix_jobs_specify_shell_bash_for_bash_syntax() {
    // Regression guard: CI workflows that run on Windows via a matrix with
    // `os:` containing a `windows-*` variant must specify `shell: bash` on
    // any `run:` step that uses Bash-specific syntax.  Without this, the
    // step executes under PowerShell (the Windows default), which cannot
    // interpret `$(...)`, `${...}`, `[[`, or other Bash constructs.
    //
    // Background: The "Read Rust toolchain" steps originally used `$(grep …)`
    // without `shell: bash`, causing failures on Windows runners.

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/\n\
         Workflows directory: {}",
        workflows_dir.display()
    );

    // Bash-specific syntax patterns that are incompatible with PowerShell.
    // Each pattern is checked against every line in a `run:` block.
    fn line_has_bash_syntax(line: &str) -> bool {
        let trimmed = line.trim();

        // Skip empty lines and pure comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return false;
        }

        // $(...) command substitution (but not ${{ ... }} which is GitHub Actions expression)
        if trimmed.contains("$(") && !trimmed.contains("${{") {
            return true;
        }

        // ${...} parameter expansion (but not ${{ ... }})
        // Look for ${ not followed by {
        let bytes = trimmed.as_bytes();
        for i in 0..bytes.len().saturating_sub(2) {
            if bytes[i] == b'$' && bytes[i + 1] == b'{' && bytes[i + 2] != b'{' {
                return true;
            }
        }

        // [[ ... ]] Bash conditional
        if trimmed.starts_with("[[") || trimmed.contains(" [[ ") || trimmed.contains("]] ") {
            return true;
        }

        // Bash array syntax: VAR=( or ${VAR[@]} or ${VAR[*]}
        if trimmed.contains("=(") || trimmed.contains("[@]}") || trimmed.contains("[*]}") {
            return true;
        }

        // Bash built-ins that don't exist in PowerShell
        let bash_builtins = [
            "set -e", "set -u", "set -o", "set -x", "set -euo", "set -eux", "shopt ", "export ",
            "source ",
        ];
        for builtin in &bash_builtins {
            if trimmed.starts_with(builtin) {
                return true;
            }
        }

        // Bash-specific redirections: 2>&1, &>, /dev/null
        if trimmed.contains("/dev/null") || trimmed.contains("2>&1") || trimmed.contains("&>") {
            return true;
        }

        // Pipe through grep/sed/awk (common Bash idiom, not PowerShell)
        if trimmed.contains("| grep ")
            || trimmed.contains("| sed ")
            || trimmed.contains("| awk ")
            || trimmed.contains("|grep ")
            || trimmed.contains("|sed ")
            || trimmed.contains("|awk ")
        {
            return true;
        }

        false
    }

    let mut violations = Vec::new();

    for entry in &workflow_files {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();
        let lines: Vec<&str> = content.lines().collect();

        // Phase 1: Identify jobs whose matrix includes a windows OS variant.
        //
        // We look for the `os: [...]` line inside `matrix:` sections.  The
        // typical YAML structure is:
        //
        //     jobs:
        //       job_key:            (indent 2)
        //         strategy:         (indent 4)
        //           matrix:         (indent 6)
        //             os: [...]     (indent 8)
        //         steps:            (indent 4)
        //           - name: ...     (indent 6)
        //             run: ...      (indent 8)
        //             shell: bash   (indent 8)
        //
        // We track the current job key by watching for non-indented keys under
        // `jobs:`.

        // Collect job keys that include windows in their matrix os list.
        let mut windows_jobs: Vec<String> = Vec::new();
        let mut in_jobs = false;
        let mut current_job: Option<String> = None;

        for line in &lines {
            let trimmed = line.trim();

            // Detect the `jobs:` section
            if trimmed == "jobs:" {
                in_jobs = true;
                continue;
            }

            if !in_jobs {
                continue;
            }

            // A non-empty line at indent 0 that isn't `jobs:` ends the jobs section
            if !line.starts_with(' ') && !trimmed.is_empty() {
                break;
            }

            // Job key: exactly 2 spaces of indent, ending with ':'
            let indent = line.len() - line.trim_start().len();
            if indent == 2 && trimmed.ends_with(':') && !trimmed.starts_with('-') {
                current_job = Some(trimmed.trim_end_matches(':').to_string());
                continue;
            }

            // Look for os: [...] lines that contain a windows variant
            if trimmed.starts_with("os:") && trimmed.contains("windows") {
                if let Some(ref job) = current_job {
                    windows_jobs.push(job.clone());
                }
            }
        }

        if windows_jobs.is_empty() {
            continue;
        }

        // Phase 2: For each windows-matrix job, scan its steps for `run:`
        // blocks that contain Bash syntax but lack `shell: bash`.

        in_jobs = false;
        current_job = None;
        let mut in_steps = false;
        let mut current_step_name = String::new();
        let mut current_step_has_shell_bash = false;
        let mut current_step_run_lines: Vec<(usize, String)> = Vec::new();
        let mut in_run_block = false;
        let mut run_block_indent: usize = 0;

        // We need to collect all step info then check at step boundaries.
        // A step boundary is a new `- ` at the step indent level, or end of
        // the steps section.

        let check_step = |step_name: &str,
                          has_shell_bash: bool,
                          run_lines: &[(usize, String)],
                          job_key: &str,
                          filename: &str,
                          violations: &mut Vec<String>| {
            if run_lines.is_empty() || has_shell_bash {
                return;
            }

            let bash_lines: Vec<&(usize, String)> = run_lines
                .iter()
                .filter(|(_, line)| line_has_bash_syntax(line))
                .collect();

            if bash_lines.is_empty() {
                return;
            }

            let examples: Vec<String> = bash_lines
                .iter()
                .take(3)
                .map(|(line_num, content)| format!("    line {}: {}", line_num, content.trim()))
                .collect();

            let step_desc = if step_name.is_empty() {
                "unnamed step".to_string()
            } else {
                format!("step \"{step_name}\"")
            };

            violations.push(format!(
                "{filename}: job \"{job_key}\", {step_desc} uses Bash syntax without \
                     `shell: bash`:\n{}",
                examples.join("\n")
            ));
        };

        for (line_idx, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            let indent = line.len() - line.trim_start().len();

            // Detect the `jobs:` section
            if trimmed == "jobs:" {
                in_jobs = true;
                continue;
            }

            if !in_jobs {
                continue;
            }

            // End of jobs section
            if !line.starts_with(' ') && !trimmed.is_empty() {
                // Flush last step
                if let Some(ref job) = current_job {
                    if windows_jobs.contains(job) {
                        check_step(
                            &current_step_name,
                            current_step_has_shell_bash,
                            &current_step_run_lines,
                            job,
                            &filename,
                            &mut violations,
                        );
                    }
                }
                break;
            }

            // Job key at indent 2
            if indent == 2 && trimmed.ends_with(':') && !trimmed.starts_with('-') {
                // Flush last step from previous job
                if let Some(ref job) = current_job {
                    if windows_jobs.contains(job) {
                        check_step(
                            &current_step_name,
                            current_step_has_shell_bash,
                            &current_step_run_lines,
                            job,
                            &filename,
                            &mut violations,
                        );
                    }
                }
                current_job = Some(trimmed.trim_end_matches(':').to_string());
                in_steps = false;
                in_run_block = false;
                current_step_run_lines.clear();
                current_step_name.clear();
                current_step_has_shell_bash = false;
                continue;
            }

            // Only process steps for windows-matrix jobs
            let is_windows_job = current_job
                .as_ref()
                .map(|j| windows_jobs.contains(j))
                .unwrap_or(false);
            if !is_windows_job {
                continue;
            }

            // Detect `steps:` section (indent 4)
            if indent == 4 && trimmed == "steps:" {
                in_steps = true;
                continue;
            }

            if !in_steps {
                continue;
            }

            // End of steps section: a line at indent <= 4 that is a new job-level key
            if indent <= 4
                && !trimmed.is_empty()
                && trimmed != "steps:"
                && !trimmed.starts_with('-')
            {
                // Flush last step
                if let Some(ref job) = current_job {
                    check_step(
                        &current_step_name,
                        current_step_has_shell_bash,
                        &current_step_run_lines,
                        job,
                        &filename,
                        &mut violations,
                    );
                }
                in_steps = false;
                in_run_block = false;
                current_step_run_lines.clear();
                current_step_name.clear();
                current_step_has_shell_bash = false;
                continue;
            }

            // New step: `- name:` or `- uses:` at indent 6
            if indent == 6 && trimmed.starts_with("- ") {
                // Flush the previous step
                if let Some(ref job) = current_job {
                    check_step(
                        &current_step_name,
                        current_step_has_shell_bash,
                        &current_step_run_lines,
                        job,
                        &filename,
                        &mut violations,
                    );
                }
                current_step_run_lines.clear();
                current_step_has_shell_bash = false;
                in_run_block = false;

                // Extract step name if present
                if let Some(name) = trimmed.strip_prefix("- name:") {
                    current_step_name = name.trim().to_string();
                } else {
                    current_step_name.clear();
                }
                continue;
            }

            // Step-level properties at indent 8
            if indent == 8 && !in_run_block {
                if let Some(name) = trimmed.strip_prefix("name:") {
                    current_step_name = name.trim().to_string();
                }

                if trimmed == "shell: bash" {
                    current_step_has_shell_bash = true;
                }

                // Single-line run: value
                if let Some(rest) = trimmed.strip_prefix("run:") {
                    let value = rest.trim();
                    if value == "|" || value == "|-" || value == "|+" {
                        // Multi-line block follows
                        in_run_block = true;
                        run_block_indent = 8;
                    } else if !value.is_empty() {
                        // Single-line run
                        current_step_run_lines.push((line_idx + 1, value.to_string()));
                    }
                }
                continue;
            }

            // Collect multi-line run block content
            if in_run_block {
                if indent <= run_block_indent && !trimmed.is_empty() {
                    // End of multi-line block
                    in_run_block = false;
                    // Re-process this line as a step property
                    if indent == 8 {
                        if trimmed == "shell: bash" {
                            current_step_has_shell_bash = true;
                        }
                        if let Some(name) = trimmed.strip_prefix("name:") {
                            current_step_name = name.trim().to_string();
                        }
                    }
                } else if !trimmed.is_empty() {
                    current_step_run_lines.push((line_idx + 1, line.to_string()));
                }
            }
        }

        // Flush final step at end of file
        if let Some(ref job) = current_job {
            if windows_jobs.contains(job) {
                check_step(
                    &current_step_name,
                    current_step_has_shell_bash,
                    &current_step_run_lines,
                    job,
                    &filename,
                    &mut violations,
                );
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Windows-matrix jobs have `run:` steps with Bash syntax but no `shell: bash`:\n\n\
         {}\n\n\
         Why this matters:\n\
         - Windows runners default to PowerShell, which cannot interpret Bash syntax\n\
         - Patterns like `$(...)`, `${{...}}`, `[[`, `set -euo pipefail` will fail\n\
         - This caused CI failures in \"Read Rust toolchain\" steps\n\n\
         Fix: add `shell: bash` to each flagged step, e.g.:\n\
         \n\
         \x20   - name: My Step\n\
         \x20     shell: bash\n\
         \x20     run: |\n\
         \x20       CHANNEL=$(grep ... | sed ...)\n",
        violations.join("\n\n")
    );
}

// ============================================================================
// Supply Chain Advisory Prevention Tests
// ============================================================================
// These tests prevent recurrence of RUSTSEC advisory CI failures (e.g.,
// RUSTSEC-2025-0134 for unmaintained rustls-pemfile) by validating that
// deny.toml is correctly configured and that no active advisories exist.

#[test]
fn test_deny_toml_exists_and_has_required_sections() {
    let root = repo_root();
    let deny_path = root.join("deny.toml");

    assert!(
        deny_path.exists(),
        "deny.toml must exist at the repository root for cargo-deny checks.\n\
         This file configures advisory, license, ban, and source policies.\n\
         Create it with: cargo deny init"
    );

    let content = read_file(&deny_path);

    let required_sections = [
        (
            "[advisories]",
            "vulnerability and unmaintained crate detection",
        ),
        ("[licenses]", "license compliance checking"),
        ("[bans]", "banned crate enforcement"),
        ("[sources]", "crate source restrictions"),
    ];

    for (section, purpose) in &required_sections {
        assert!(
            content.contains(section),
            "deny.toml must contain a {section} section for {purpose}.\n\
             File: {}",
            deny_path.display()
        );
    }
}

#[test]
fn test_deny_toml_advisories_section_denies_yanked() {
    let root = repo_root();
    let content = read_file(&root.join("deny.toml"));

    assert!(
        content.contains("yanked = \"deny\""),
        "deny.toml [advisories] must set yanked = \"deny\" to block yanked crates.\n\
         Yanked crates have known issues and must not be used in production."
    );
}

#[test]
fn test_deny_toml_uses_version_2() {
    let root = repo_root();
    let content = read_file(&root.join("deny.toml"));

    let in_advisories = content
        .lines()
        .skip_while(|line| !line.starts_with("[advisories]"))
        .take_while(|line| !line.starts_with('[') || line.starts_with("[advisories]"))
        .any(|line| line.trim() == "version = 2");

    assert!(
        in_advisories,
        "deny.toml [advisories] must use version = 2 (cargo-deny v0.19+).\n\
         Version 2 checks all advisory types (vulnerabilities and unmaintained)\n\
         by default without needing explicit severity configuration."
    );
}

#[test]
fn test_no_rustls_pemfile_dependency() {
    // Regression test for RUSTSEC-2025-0134: rustls-pemfile was flagged as
    // unmaintained. The fix is to use rustls-pki-types built-in PEM parsing.
    let root = repo_root();
    let cargo_toml = read_file(&root.join("Cargo.toml"));

    assert!(
        !cargo_toml.contains("rustls-pemfile"),
        "Cargo.toml must not depend on rustls-pemfile (RUSTSEC-2025-0134: unmaintained).\n\
         Use rustls-pki-types built-in PEM parsing instead.\n\
         Migration: replace rustls_pemfile::certs() with \
         rustls_pki_types::pem::PemObject::pem_file_iter() or similar."
    );
}

#[test]
fn test_tls_feature_uses_rustls_pki_types() {
    let root = repo_root();
    let cargo_toml = read_file(&root.join("Cargo.toml"));

    // The tls feature should include rustls-pki-types as the PEM parsing provider
    let tls_line = cargo_toml
        .lines()
        .find(|line| line.starts_with("tls = ") || line.starts_with("tls="));

    let tls_line = tls_line.expect(
        "Cargo.toml must define a 'tls' feature.\n\
         Expected: tls = [\"axum-server\", \"rustls\", \"rustls-pki-types\"]",
    );

    assert!(
        tls_line.contains("rustls-pki-types"),
        "The tls feature must include rustls-pki-types for PEM parsing.\n\
         Found: {tls_line}\n\
         rustls-pki-types replaces the unmaintained rustls-pemfile crate."
    );
}

#[test]
fn test_check_advisories_script_exists() {
    let root = repo_root();
    let script_path = root.join("scripts/check-advisories.sh");

    assert!(
        script_path.exists(),
        "scripts/check-advisories.sh must exist for local advisory checking.\n\
         This script runs cargo deny check advisories to catch RUSTSEC issues\n\
         before pushing to CI."
    );

    let content = read_file(&script_path);

    assert!(
        content.contains("cargo deny check advisories"),
        "scripts/check-advisories.sh must run 'cargo deny check advisories'."
    );

    assert!(
        content.contains("set -euo pipefail") || content.contains("set -eu"),
        "scripts/check-advisories.sh must use strict error handling (set -euo pipefail)."
    );
}

#[test]
fn test_run_local_ci_includes_advisory_check() {
    let root = repo_root();
    let script = read_file(&root.join("scripts/run-local-ci.sh"));

    assert!(
        script.contains("check-advisories.sh"),
        "scripts/run-local-ci.sh must include scripts/check-advisories.sh\n\
         as a local CI gate to catch RUSTSEC advisories before pushing."
    );
}

#[test]
fn test_cargo_deny_check_advisories_passes() {
    // Run cargo deny check advisories to verify no active RUSTSEC advisories.
    // This test mirrors the CI deny job locally.
    let Some((success, output)) = run_cargo_deny(&["check", "advisories"]) else {
        return;
    };

    assert!(
        success,
        "cargo deny check advisories failed.\n\
         This means there are active RUSTSEC advisories in the dependency tree.\n\n\
         To investigate:\n\
         1. Run: cargo deny check advisories\n\
         2. For each advisory, either:\n\
            a. Update the dependency to a patched version\n\
            b. Replace the dependency with a maintained alternative\n\
            c. Add a documented ignore in deny.toml with justification and expiry\n\n\
         Output:\n{output}",
    );
}

#[test]
fn test_optional_feature_compile_matrix() {
    // Data-driven checks for optional features that are expected to compile in
    // isolation. This prevents regressions from dependency migrations and
    // feature-gating drift.
    const FEATURE_CASES: &[(&str, &str)] = &[
        ("TLS support", "tls"),
        ("Legacy full-mesh compatibility", "legacy-fullmesh"),
        (
            "Combined optional feature compatibility",
            "tls,legacy-fullmesh",
        ),
    ];

    let mut failures = Vec::new();

    for &(label, feature) in FEATURE_CASES {
        let (success, output) = run_isolated_feature_check(feature);
        if !success {
            failures.push(format!(
                "{label} (feature `{feature}`) failed to compile:\n{output}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "One or more optional feature compile checks failed.\n\
         These checks run in an isolated target directory with sanitizer env
         scrubbed to avoid false positives in ASan/Miri contexts.\n\n{}",
        failures.join("\n\n---\n\n"),
    );
}

// ---------------------------------------------------------------------------
// Dependency Health Hardening Tests
//
// Data-driven tests validating that deny.toml maintains a comprehensive
// proactive ban list and that supply-chain policy settings remain strict.
// These tests prevent regression of dependency health invariants.
// ---------------------------------------------------------------------------

/// Crates that must appear in deny.toml [[bans.deny]] entries.
/// Each entry is (crate_name, reason_substring) — the reason substring is
/// checked to ensure the ban has a meaningful justification, not just a name.
const REQUIRED_DENY_BANS: &[(&str, &str)] = &[
    // Original bans (pre-existing)
    ("atty", "std::io::IsTerminal"),
    ("instant", "std::time::Instant"),
    // Security/TLS policy
    ("rustls-pemfile", "RUSTSEC-2025-0134"),
    ("openssl", "rustls"),
    ("openssl-sys", "rustls"),
    ("native-tls", "rustls"),
    // Build system policy
    ("gcc", "cc"),
    // Unmaintained/deprecated
    ("failure", "thiserror"),
    ("failure_derive", "thiserror"),
    ("tempdir", "tempfile"),
    ("term", "crossterm"),
    ("net2", "std::net"),
    ("rustc-serialize", "serde"),
];

#[test]
fn test_deny_toml_bans_known_problematic_crates() {
    let root = repo_root();
    let content = read_file(&root.join("deny.toml"));

    let mut missing = Vec::new();
    let mut bad_reason = Vec::new();

    for &(crate_name, reason_substr) in REQUIRED_DENY_BANS {
        if !content.contains(&format!("name = \"{crate_name}\"")) {
            missing.push(crate_name);
        } else if !content.contains(reason_substr) {
            bad_reason.push((crate_name, reason_substr));
        }
    }

    assert!(
        missing.is_empty(),
        "deny.toml is missing [[bans.deny]] entries for known-problematic crates:\n\
         {}\n\n\
         Add [[bans.deny]] entries with name and reason fields for each.\n\
         See .llm/skills/supply-chain-audit-policy.md for the proactive ban list policy.",
        missing
            .iter()
            .map(|c| format!("  - {c}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    assert!(
        bad_reason.is_empty(),
        "deny.toml ban entries have missing or incorrect reasons:\n\
         {}\n\n\
         Each ban must include a reason that mentions the recommended replacement.",
        bad_reason
            .iter()
            .map(|(c, r)| format!("  - {c}: reason must mention \"{r}\""))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn test_deny_toml_bans_deny_wildcards() {
    let root = repo_root();
    let content = read_file(&root.join("deny.toml"));

    assert!(
        content.contains("wildcards = \"deny\""),
        "deny.toml [bans] must set wildcards = \"deny\" to prevent wildcard version specs.\n\
         Wildcard dependencies (e.g., version = \"*\") bypass reproducible builds."
    );
}

#[test]
fn test_deny_toml_sources_deny_unknown() {
    let root = repo_root();
    let content = read_file(&root.join("deny.toml"));

    let required_settings = [
        ("unknown-registry = \"deny\"", "unknown registries"),
        ("unknown-git = \"deny\"", "unknown git sources"),
        ("allow-git = []", "all git dependencies"),
    ];

    for (setting, description) in required_settings {
        assert!(
            content.contains(setting),
            "deny.toml [sources] must contain `{setting}` to block {description}.\n\
             Only crates.io should be allowed as a dependency source."
        );
    }
}

#[test]
fn test_no_rustls_pemfile_in_cargo_lock() {
    let root = repo_root();
    let lock_content = read_file(&root.join("Cargo.lock"));

    assert!(
        !lock_content.contains("name = \"rustls-pemfile\""),
        "Cargo.lock must not contain rustls-pemfile.\n\
         This crate is unmaintained (RUSTSEC-2025-0134) and has been replaced\n\
         by rustls-pki-types built-in PEM parsing.\n\n\
         If this appeared after a dependency update, check which crate pulls it in:\n\
         cargo tree -i rustls-pemfile"
    );
}

#[test]
fn test_cargo_deny_full_check_passes() {
    let Some((success, output)) = run_cargo_deny(&["--all-features", "check"]) else {
        return;
    };

    assert!(
        success,
        "cargo deny --all-features check failed.\n\
         All four policy areas (advisories, licenses, bans, sources) must pass.\n\n\
         To investigate:\n\
         1. Run: cargo deny --all-features check\n\
         2. Address each failure category separately:\n\
            - advisories: update or replace affected dependency\n\
            - licenses: add exception or replace dependency\n\
            - bans: remove banned crate or add skip entry with justification\n\
            - sources: ensure all deps come from crates.io\n\n\
         Output:\n{output}",
    );
}

#[test]
fn test_check_outdated_script_exists() {
    let root = repo_root();
    let script_path = root.join("scripts/check-outdated.sh");

    assert!(
        script_path.exists(),
        "scripts/check-outdated.sh must exist for local outdated dependency checking.\n\
         This script runs cargo outdated to show which dependencies have newer versions\n\
         available. It is informational only (not a CI gate)."
    );

    let content = read_file(&script_path);

    assert!(
        content.contains("cargo outdated"),
        "scripts/check-outdated.sh must run 'cargo outdated'."
    );

    assert!(
        content.contains("set -euo pipefail") || content.contains("set -eu"),
        "scripts/check-outdated.sh must use strict error handling (set -euo pipefail)."
    );
}

#[test]
fn test_check_outdated_script_has_shebang() {
    let root = repo_root();
    let content = read_file(&root.join("scripts/check-outdated.sh"));

    assert!(
        content.starts_with("#!/usr/bin/env bash") || content.starts_with("#!/bin/bash"),
        "scripts/check-outdated.sh must have a proper bash shebang line."
    );
}

/// Data-driven test for flag and pattern presence in check-outdated.sh.
///
/// Consolidates individual flag-presence assertions into a table-driven structure
/// so that new flags or patterns can be added with a single line.
#[test]
fn test_check_outdated_script_required_flags_and_patterns() {
    let root = repo_root();
    let content = read_file(&root.join("scripts/check-outdated.sh"));

    // (pattern, description) — each must appear in the script
    let required_patterns: &[(&str, &str)] = &[
        ("--help", "must support --help flag"),
        (
            "--root-only",
            "must support --root-only flag to skip transitive deps",
        ),
        ("--json", "must support --json flag for JSON output"),
        (
            "--root-deps-only",
            "must pass --root-deps-only to cargo outdated when --root-only is set",
        ),
        (
            "--format json",
            "must pass --format json to cargo outdated when --json is set",
        ),
    ];

    let mut missing = Vec::new();
    for (pattern, description) in required_patterns {
        if !content.contains(pattern) {
            missing.push(format!("  - '{pattern}': {description}"));
        }
    }

    assert!(
        missing.is_empty(),
        "scripts/check-outdated.sh is missing required flags/patterns:\n{}",
        missing.join("\n")
    );
}

/// Verify the script has a catch-all case for unknown options.
#[test]
fn test_check_outdated_script_handles_unknown_options() {
    let root = repo_root();
    let content = read_file(&root.join("scripts/check-outdated.sh"));

    assert!(
        content.contains("*)") && content.contains("Unknown option"),
        "scripts/check-outdated.sh must have a catch-all `*)` case that reports \
         unknown options to the user."
    );
}

/// Verify the script contains TTY color-detection using `[ -t 1 ]`.
#[test]
fn test_check_outdated_script_has_tty_color_detection() {
    let root = repo_root();
    let content = read_file(&root.join("scripts/check-outdated.sh"));

    assert!(
        content.contains("[ -t 1 ]"),
        "scripts/check-outdated.sh must include TTY detection ([ -t 1 ]) to disable \
         color output when stdout is not a terminal."
    );
}

#[test]
fn test_check_outdated_script_is_informational_only() {
    let root = repo_root();
    let content = read_file(&root.join("scripts/check-outdated.sh"));

    // The script must always exit 0 (informational only).
    // It should contain "exit 0" and NOT use a FAILED variable to gate exit codes.
    assert!(
        content.contains("exit 0"),
        "scripts/check-outdated.sh must exit 0 (informational only — \
         outdated deps are not errors)."
    );

    // Note: `exit 2` is intentionally allowed — it is used only for tool/usage
    // errors (e.g., cargo-outdated not installed, unknown CLI option), NOT for
    // outdated dependencies.  Only `exit 1` and `exit $FAILED` would indicate
    // that the script treats outdated deps as failures.
    assert!(
        !content.contains("exit $FAILED") && !content.contains("exit 1"),
        "scripts/check-outdated.sh must not exit non-zero for outdated dependencies.\n\
         This is an informational tool, not a CI gate.\n\
         (exit 2 is allowed for tool/usage errors, but exit 1 or exit $FAILED is not.)"
    );
}

/// Verify check-outdated.sh has executable permission bits set.
#[test]
#[cfg(unix)]
fn test_check_outdated_script_is_executable() {
    use std::os::unix::fs::PermissionsExt;

    let root = repo_root();
    let script_path = root.join("scripts/check-outdated.sh");

    assert!(
        script_path.exists(),
        "scripts/check-outdated.sh must exist to validate executable permissions."
    );

    let metadata = std::fs::metadata(&script_path).unwrap_or_else(|e| {
        panic!(
            "Failed to read metadata for {}: {}",
            script_path.display(),
            e
        )
    });
    let mode = metadata.permissions().mode();
    let is_executable = mode & 0o111 != 0;

    assert!(
        is_executable,
        "{} is not executable (mode: {:o}).\n\
         Fix: chmod +x scripts/check-outdated.sh && git update-index --chmod=+x scripts/check-outdated.sh",
        script_path.display(),
        mode & 0o777
    );
}

// ---------------------------------------------------------------------------
// Skill Documentation Sync Tests
//
// Data-driven tests ensuring that LLM skill files contain required sections
// and that documentation stays in sync with tooling and policy.
// ---------------------------------------------------------------------------

/// Sections that must exist in dependency-management-cargo.md.
const REQUIRED_DEP_SKILL_SECTIONS: &[(&str, &str)] = &[
    (
        "## Dependency Watch List and Ban Policy",
        "Watch list and ban policy section",
    ),
    ("### Watch List", "Watch list table"),
    ("### Ban List Policy", "Ban list policy criteria"),
    ("### How to Add a Ban", "Ban addition process"),
];

/// Crate names that must appear in the watch list table.
/// These are real crates with known deprecation or risk trends.
const REQUIRED_WATCH_LIST_CRATES: &[(&str, &str)] = &[
    ("once_cell", "LazyLock / OnceLock stabilized in std"),
    ("async-trait", "native async fn in traits (Rust 1.75)"),
    ("chrono", "past RUSTSEC advisories"),
    (
        "futures-util",
        "large dependency tree overlapping with tokio",
    ),
    ("rmp-serde", "maintenance cadence monitoring"),
];

/// Sections that must exist in supply-chain-audit-policy.md.
const REQUIRED_AUDIT_SKILL_SECTIONS: &[(&str, &str)] = &[
    (
        "## Monitoring Obligations",
        "Monitoring obligations section",
    ),
    ("### Scheduled Checks", "Scheduled checks table"),
    ("### Response SLAs", "Response SLA table"),
    (
        "### Escalation Path",
        "Escalation path for RUSTSEC advisories",
    ),
];

#[test]
fn test_dep_skill_contains_watch_list_section() {
    let root = repo_root();
    let content = read_file(&root.join(".llm/skills/dependency-management-cargo.md"));

    let mut missing = Vec::new();
    for &(section, desc) in REQUIRED_DEP_SKILL_SECTIONS {
        if !content.contains(section) {
            missing.push(format!("  - {desc} (expected: \"{section}\")"));
        }
    }

    assert!(
        missing.is_empty(),
        "dependency-management-cargo.md is missing required sections:\n{}\n\n\
         These sections document the dependency watch list and ban policy.\n\
         See ticket K-4 for the required content.",
        missing.join("\n")
    );

    // Verify the watch list is in table format (has a table header row).
    assert!(
        content.contains("| Crate"),
        "dependency-management-cargo.md watch list must be in markdown table format.\n\
         Expected a table header row starting with '| Crate'."
    );
}

#[test]
fn test_dep_skill_contains_ban_policy_referencing_deny_toml() {
    let root = repo_root();
    let content = read_file(&root.join(".llm/skills/dependency-management-cargo.md"));

    assert!(
        content.contains("deny.toml"),
        "dependency-management-cargo.md ban policy section must reference deny.toml.\n\
         The ban policy documents when and how to add [[bans.deny]] entries."
    );

    assert!(
        content.contains("REQUIRED_DENY_BANS"),
        "dependency-management-cargo.md must reference the REQUIRED_DENY_BANS constant\n\
         in ci_config_tests.rs so agents know to update the test when adding bans."
    );
}

#[test]
fn test_dep_skill_watch_list_references_real_crates() {
    let root = repo_root();
    let content = read_file(&root.join(".llm/skills/dependency-management-cargo.md"));

    let mut missing = Vec::new();
    let mut not_in_table = Vec::new();
    for &(crate_name, reason) in REQUIRED_WATCH_LIST_CRATES {
        let backtick_name = format!("`{crate_name}`");
        if !content.contains(&backtick_name) {
            missing.push(format!("  - {crate_name}: {reason}"));
        } else {
            // Verify the crate appears in a table row (line with pipe delimiters),
            // not just mentioned in prose.
            let in_table_row = content
                .lines()
                .any(|line| line.contains(&backtick_name) && line.contains('|'));
            if !in_table_row {
                not_in_table.push(format!(
                    "  - {crate_name}: found in prose but not in a table row"
                ));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "dependency-management-cargo.md watch list is missing real crate entries:\n{}\n\n\
         The watch list must contain actual crates with known risks, not placeholders.",
        missing.join("\n")
    );

    assert!(
        not_in_table.is_empty(),
        "dependency-management-cargo.md watch list crates must appear in a table row (with | delimiters):\n{}\n\n\
         Each watched crate must be in the markdown table, not just mentioned in prose.",
        not_in_table.join("\n")
    );
}

#[test]
fn test_audit_skill_contains_monitoring_obligations() {
    let root = repo_root();
    let content = read_file(&root.join(".llm/skills/supply-chain-audit-policy.md"));

    let mut missing = Vec::new();
    for &(section, desc) in REQUIRED_AUDIT_SKILL_SECTIONS {
        if !content.contains(section) {
            missing.push(format!("  - {desc} (expected: \"{section}\")"));
        }
    }

    assert!(
        missing.is_empty(),
        "supply-chain-audit-policy.md is missing required sections:\n{}\n\n\
         These sections document monitoring obligations, SLAs, and escalation paths.\n\
         See ticket K-4 for the required content.",
        missing.join("\n")
    );
}

#[test]
fn test_audit_skill_monitoring_references_tools() {
    let root = repo_root();
    let content = read_file(&root.join(".llm/skills/supply-chain-audit-policy.md"));

    let required_tool_refs = [
        ("cargo deny check", "primary policy gate"),
        ("cargo audit", "second-opinion vulnerability scanner"),
        ("check-outdated.sh", "outdated dependency reporter"),
        ("check-advisories.sh", "local advisory pre-check"),
    ];

    let mut missing = Vec::new();
    for (tool, desc) in required_tool_refs {
        if !content.contains(tool) {
            missing.push(format!("  - {tool} ({desc})"));
        }
    }

    assert!(
        missing.is_empty(),
        "supply-chain-audit-policy.md monitoring obligations must reference these tools:\n{}\n\n\
         Agents need to know which tools to run and when.",
        missing.join("\n")
    );
}

/// Verify that check-outdated.sh is intentionally excluded from run-local-ci.sh.
///
/// check-outdated.sh is an informational developer tool that reports which
/// dependencies have newer versions available. It always exits 0 and does not
/// gate on any condition. Informational tools do not belong in CI gate scripts
/// because they add noise without enforcing any quality bar. The advisory check
/// (check-advisories.sh) IS included because it enforces a security policy.
#[test]
fn test_run_local_ci_excludes_check_outdated() {
    let root = repo_root();
    let script = read_file(&root.join("scripts/run-local-ci.sh"));

    assert!(
        !script.contains("check-outdated.sh"),
        "scripts/run-local-ci.sh must NOT include check-outdated.sh.\n\
         check-outdated.sh is an informational tool (always exits 0) and does not \
         enforce any policy. Informational tools do not belong in CI gate scripts — \
         they add noise without enforcing a quality bar.\n\
         Developers can run it manually: ./scripts/check-outdated.sh"
    );
}

// ---------------------------------------------------------------------------
// Internal Path Classification Tests
//
// Data-driven tests ensuring that the is_internal_path() function in
// check-doc-consistency.sh and the dep-detect case statement in ci.yml
// stay in sync, and that path classification is correct for known paths.
// These tests prevent regression of the docs/ci-cd-* misclassification bug.
// ---------------------------------------------------------------------------

/// Glob-style patterns that MUST appear in BOTH scripts/check-doc-consistency.sh
/// (is_internal_path function) and .github/workflows/ci.yml (dep-detect case).
/// The CI workflow additionally has Cargo.toml and CHANGELOG.md for dependency
/// bump detection (Cargo.lock is shared but grouped differently in ci.yml).
/// See also .github/test-fixtures/test-doc-consistency.sh which tests these
/// patterns via shell-level integration tests.
const SHARED_INTERNAL_PATH_PATTERNS: &[&str] = &[
    ".github/*",
    ".githooks/*",
    ".devcontainer/*",
    ".config/*",
    ".vscode/*",
    ".claude/*",
    "scripts/*",
    "tests/*",
    "test-fixtures/*",
    ".llm/*",
    "target/*",
    "progress/*",
    "docs/ci-cd-*",
    "docs/test-*",
    "docs/git-hooks-*",
    "docs/hooks-*",
    "docs/pre-commit-*",
    "docs/development.md",
    ".markdownlint*",
    ".lychee.toml",
    ".lycheecache",
    ".typos.toml",
    ".yamllint.yml",
    ".gitignore",
    ".dockerignore",
    "PLAN.md",
    "AGENTS.md",
    "pre-push.txt",
    "logs_*.zip",
    "clippy.toml",
    "deny.toml",
    "tarpaulin.toml",
    "rust-toolchain.toml",
    "mkdocs.yml",
    "requirements-docs.txt",
];

#[test]
fn test_internal_path_patterns_synced_between_script_and_ci() {
    let root = repo_root();
    let script_content = read_file(&root.join("scripts/check-doc-consistency.sh"));
    let ci_content = read_file(&root.join(".github/workflows/ci.yml"));

    let mut missing_from_script = Vec::new();
    let mut missing_from_ci = Vec::new();

    for &pattern in SHARED_INTERNAL_PATH_PATTERNS {
        if !script_content.contains(pattern) {
            missing_from_script.push(pattern);
        }
        if !ci_content.contains(pattern) {
            missing_from_ci.push(pattern);
        }
    }

    assert!(
        missing_from_script.is_empty(),
        "scripts/check-doc-consistency.sh is_internal_path() is missing these patterns:\n\
         {}\n\n\
         The is_internal_path() function and the CI dep-detect case statement must \
         share these patterns. Update is_internal_path() to include the missing patterns.",
        missing_from_script
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    assert!(
        missing_from_ci.is_empty(),
        ".github/workflows/ci.yml dep-detect case is missing these patterns:\n\
         {}\n\n\
         The CI dep-detect case statement and is_internal_path() must share these \
         patterns. Update the dep-detect case in ci.yml to include the missing patterns.",
        missing_from_ci
            .iter()
            .map(|p| format!("  - {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Test cases for internal path classification.
/// Each entry is (path, should_be_internal, reason).
const INTERNAL_PATH_CLASSIFICATION_CASES: &[(&str, bool, &str)] = &[
    // Internal paths
    (
        ".github/workflows/ci.yml",
        true,
        "CI workflow files are internal infrastructure",
    ),
    (
        "scripts/check-doc-consistency.sh",
        true,
        "scripts are internal tooling",
    ),
    ("tests/ci_config_tests.rs", true, "test files are internal"),
    (
        "docs/ci-cd-testing.md",
        true,
        "CI/CD docs are internal infrastructure docs",
    ),
    (
        "docs/test-suite-analysis-ci-config.md",
        true,
        "test-related docs are internal",
    ),
    (
        "docs/pre-commit-hooks-summary.md",
        true,
        "pre-commit docs are internal",
    ),
    ("docs/development.md", true, "development doc is internal"),
    (
        "docs/hooks-quick-reference.md",
        true,
        "hooks docs are internal",
    ),
    (
        "docs/git-hooks-guide.md",
        true,
        "git-hooks docs are internal",
    ),
    ("Cargo.lock", true, "lockfile changes are internal"),
    ("deny.toml", true, "deny.toml is internal tooling config"),
    (".llm/context.md", true, "LLM context files are internal"),
    (
        ".markdownlint.yaml",
        true,
        "linter config files are internal",
    ),
    ("clippy.toml", true, "clippy config is internal"),
    ("PLAN.md", true, "planning docs are internal"),
    (".gitignore", true, "git config files are internal"),
    // Non-internal paths (require CHANGELOG)
    (
        "src/main.rs",
        false,
        "source code changes require CHANGELOG",
    ),
    (
        "src/security/tls.rs",
        false,
        "source code changes require CHANGELOG",
    ),
    ("Cargo.toml", false, "manifest changes require CHANGELOG"),
    (
        "docs/library-usage.md",
        false,
        "user-facing docs require CHANGELOG",
    ),
    (
        "docs/getting-started.md",
        false,
        "user-facing docs require CHANGELOG",
    ),
    (
        "docs/protocol.md",
        false,
        "user-facing docs require CHANGELOG",
    ),
    (
        "docs/configuration.md",
        false,
        "user-facing docs require CHANGELOG",
    ),
    (
        "docs/deployment.md",
        false,
        "user-facing docs require CHANGELOG",
    ),
    (
        "docs/authentication.md",
        false,
        "user-facing docs require CHANGELOG",
    ),
    (
        "docs/features.md",
        false,
        "user-facing docs require CHANGELOG",
    ),
    (
        "docs/architecture.md",
        false,
        "user-facing docs require CHANGELOG",
    ),
    (
        "docs/quickstart.md",
        false,
        "user-facing docs require CHANGELOG",
    ),
    ("docs/index.md", false, "user-facing docs require CHANGELOG"),
    ("README.md", false, "README changes require CHANGELOG"),
    // Edge case: docs/testing-guide.md must NOT match docs/test-* pattern
    (
        "docs/testing-guide.md",
        false,
        "docs/testing-* must NOT match docs/test-* pattern",
    ),
];

/// Simple shell-glob matcher supporting `*` (matches any chars including `/`)
/// and literal characters. This covers all patterns used in is_internal_path().
fn glob_matches(pattern: &str, path: &str) -> bool {
    glob_matches_inner(pattern.as_bytes(), path.as_bytes())
}

fn glob_matches_inner(pattern: &[u8], path: &[u8]) -> bool {
    let mut p = 0;
    let mut s = 0;
    let mut star_p = None;
    let mut star_s = None;

    while s < path.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            star_p = Some(p);
            star_s = Some(s);
            p += 1;
        } else if p < pattern.len() && pattern[p] == path[s] {
            p += 1;
            s += 1;
        } else if let (Some(sp), Some(ss)) = (star_p, star_s) {
            let new_ss = ss + 1;
            p = sp + 1;
            s = new_ss;
            star_s = Some(new_ss);
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

#[test]
fn test_internal_path_classification_data_driven() {
    let root = repo_root();
    let script_content = read_file(&root.join("scripts/check-doc-consistency.sh"));

    // Extract the case patterns from is_internal_path().
    // Find the block between "case \"$path\" in" and the closing "esac".
    let case_start = script_content
        .find("case \"$path\" in")
        .expect("is_internal_path() case statement not found in check-doc-consistency.sh");
    let case_block = &script_content[case_start..];
    let esac_offset = case_block
        .find("\n    esac")
        .or_else(|| case_block.find("\nesac"))
        .expect("esac not found after case \"$path\" in");
    let case_body = &case_block[..esac_offset];

    // Parse individual glob patterns from lines like:
    //   pattern1|pattern2|pattern3)
    // Skip the *) catch-all line.
    let mut patterns: Vec<&str> = Vec::new();
    for line in case_body.lines() {
        let trimmed = line.trim();
        // Skip non-pattern lines (comments, return, case header, empty)
        if !trimmed.ends_with(')') {
            continue;
        }
        // Skip the catch-all wildcard
        if trimmed == "*)" {
            continue;
        }
        // Strip trailing )
        let pat_str = trimmed.trim_end_matches(')');
        for pat in pat_str.split('|') {
            let pat = pat.trim();
            if !pat.is_empty() {
                patterns.push(pat);
            }
        }
    }

    assert!(
        !patterns.is_empty(),
        "Failed to extract any patterns from is_internal_path() in check-doc-consistency.sh"
    );

    let mut failures = Vec::new();

    for &(path, should_be_internal, reason) in INTERNAL_PATH_CLASSIFICATION_CASES {
        let matched = patterns.iter().any(|pat| glob_matches(pat, path));
        if matched != should_be_internal {
            let direction = if should_be_internal {
                "expected INTERNAL but classified as non-internal"
            } else {
                "expected NON-INTERNAL but classified as internal"
            };
            failures.push(format!("  - {path}: {direction} ({reason})"));
        }
    }

    assert!(
        failures.is_empty(),
        "Internal path classification mismatches in is_internal_path():\n\
         {}\n\n\
         Update is_internal_path() in scripts/check-doc-consistency.sh or fix the \
         test cases if the expected classification has changed.",
        failures.join("\n")
    );
}
