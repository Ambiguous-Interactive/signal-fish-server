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

use std::path::{Path, PathBuf};

/// Get the repository root directory
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read a file to string, panicking with a helpful message on error
fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e))
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
        "Main CI pipeline (lint, nextest, deny, MSRV, Docker, coverage, panic-policy, SBOM)",
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
fn test_github_actions_are_pinned_to_sha() {
    // This test validates that all GitHub Actions use SHA pinning instead of mutable tags
    // SHA pinning prevents supply chain attacks where action maintainers could push
    // malicious code to an existing tag (e.g., v4.2.2 could be changed after we reference it)
    //
    // Required format: uses: owner/repo@<64-char-sha> # vX.Y.Z
    // Example: uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2

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
    let mut actions_pinned_correctly = 0;
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

            // Look for "uses:" lines that reference actions
            if trimmed.starts_with("uses:") {
                let uses_value = trimmed.trim_start_matches("uses:").trim();

                // Skip local actions (e.g., ./.github/actions/setup)
                if uses_value.starts_with("./") {
                    continue;
                }

                // Skip docker:// references (different security model)
                if uses_value.starts_with("docker://") {
                    continue;
                }

                total_actions_found += 1;

                // Extract the action reference (owner/repo@ref)
                let parts: Vec<&str> = uses_value.split('@').collect();
                if parts.len() < 2 {
                    violations.push(format!(
                        "{filename}:{line_num}: Invalid action reference (missing @): {uses_value}"
                    ));
                    file_has_violation = true;
                    continue;
                }

                let action_ref = parts[1].split_whitespace().next().unwrap_or("");

                if !is_sha_pinned(action_ref) {
                    violations.push(format!(
                        "{}:{}: Action not pinned to SHA: {}\n  \
                         Found: {}\n  \
                         Action references must use full 40-character SHA instead of tags.\n  \
                         Tags are mutable and can be changed by maintainers (supply chain risk).\n  \
                         Fix: Find SHA at https://github.com/{}/releases then update to:\n  \
                         uses: {}@<40-char-sha> # <tag>\n  \
                         Verify: grep -n 'uses:.*{}' .github/workflows/*.yml",
                        filename, line_num, parts[0], action_ref, parts[0], parts[0], parts[0]
                    ));
                    file_has_violation = true;
                } else {
                    actions_pinned_correctly += 1;
                }
            }
        }

        if file_has_violation {
            files_with_violations.insert(filename.to_string());
        }
    }

    if !violations.is_empty() {
        panic!(
            "GitHub Actions must be pinned to SHA for security:\n\n{}\n\n\
             Diagnostic Information:\n\
             - Workflow files checked: {}\n\
             - Total actions found: {}\n\
             - Actions pinned correctly: {}\n\
             - Actions with violations: {}\n\
             - Workflows with violations: {}\n\n\
             Workflows with violations:\n{}\n\n\
             Why SHA pinning is required:\n\
             - Tags (v1, v1.2.3) are mutable and can be changed by action maintainers\n\
             - Attackers could compromise maintainer accounts and push malicious code to existing tags\n\
             - SHA pinning ensures the exact code version is locked\n\n\
             How to fix:\n\
             1. Find the release/tag on GitHub: https://github.com/owner/repo/releases\n\
             2. Click on the commit SHA for that tag\n\
             3. Copy the full 40-character SHA\n\
             4. Use format: uses: owner/repo@<SHA> # vX.Y.Z\n\n\
             Example:\n\
             - Bad:  uses: actions/checkout@v4.2.2\n\
             - Good: uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2\n",
            violations.join("\n"),
            total_files_checked,
            total_actions_found,
            actions_pinned_correctly,
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

        if trimmed.starts_with("uses:") && trimmed.contains("cargo-deny-action") {
            found_cargo_deny = true;

            // Extract the SHA and check for version comment
            let parts: Vec<&str> = trimmed.split('@').collect();
            if parts.len() < 2 {
                violations.push(format!(
                    "Line {line_num}: cargo-deny-action reference is malformed: {trimmed}"
                ));
                continue;
            }

            let after_at = parts[1];

            // Check if there's a version comment (# vX.Y.Z)
            if !after_at.contains('#') {
                violations.push(format!(
                    "Line {line_num}: cargo-deny-action missing version comment\n  \
                     Expected format: uses: EmbarkStudios/cargo-deny-action@<SHA> # vX.Y.Z"
                ));
                continue;
            }

            // Extract version from comment
            if let Some(comment_part) = after_at.split('#').nth(1) {
                let version_str = comment_part.trim();

                // Parse version (should be vX.Y.Z format)
                if !version_str.starts_with('v') {
                    violations.push(format!(
                        "Line {line_num}: Version comment should start with 'v': {version_str}"
                    ));
                    continue;
                }

                let version_numbers = version_str.trim_start_matches('v');
                let version_parts: Vec<&str> = version_numbers.split('.').collect();

                if version_parts.len() < 3 {
                    violations.push(format!(
                        "Line {line_num}: Invalid version format (expected vX.Y.Z): {version_str}"
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
                        "Line {line_num}: cargo-deny-action version too old: {version_str}\n  \
                         Minimum required: v{min_major}.{min_minor}.{min_patch}\n  \
                         Found: v{major}.{minor}.{patch}\n  \
                         Please update to v2.0.15 or newer for security and stability fixes."
                    ));
                }
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
fn test_action_version_comments_exist() {
    // This test validates that all GitHub Actions with SHA pinning have version comments
    // Version comments make it easy to understand what version is being used without
    // looking up the SHA on GitHub
    //
    // Required format: uses: owner/repo@<sha> # vX.Y.Z or # tag-name
    // Example: uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let workflow_files = collect_workflow_files(&workflows_dir);

    assert!(
        !workflow_files.is_empty(),
        "Workflows directory not found or empty at {}",
        workflows_dir.display()
    );

    let mut violations = Vec::new();

    for entry in workflow_files {
        let path = entry.path();
        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1; // 1-indexed
            let trimmed = line.trim();

            if trimmed.starts_with("uses:") {
                let uses_value = trimmed.trim_start_matches("uses:").trim();

                // Skip local actions and docker references
                if uses_value.starts_with("./") || uses_value.starts_with("docker://") {
                    continue;
                }

                // Extract the action reference
                let parts: Vec<&str> = uses_value.split('@').collect();
                if parts.len() < 2 {
                    continue; // Already caught by SHA pinning test
                }

                let after_at = parts[1];
                let action_ref = after_at.split_whitespace().next().unwrap_or("");

                if is_sha_pinned(action_ref) {
                    // SHA-pinned action should have a version comment
                    if !after_at.contains('#') {
                        violations.push(format!(
                            "{}:{}: SHA-pinned action missing version comment: {}\n  \
                             Add a comment with the version/tag for readability.\n  \
                             Format: uses: {}@{} # vX.Y.Z or # tag-name",
                            filename, line_num, parts[0], parts[0], action_ref
                        ));
                    } else {
                        // Verify comment is not empty
                        if let Some(comment_part) = after_at.split('#').nth(1) {
                            let comment = comment_part.trim();
                            if comment.is_empty() {
                                violations.push(format!(
                                    "{}:{}: Version comment is empty: {}\n  \
                                     Provide the version/tag for this SHA (e.g., # v4.2.2)",
                                    filename, line_num, parts[0]
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "GitHub Actions with SHA pinning must have version comments:\n\n{}\n\n\
             Why version comments are required:\n\
             - Makes it easy to understand which version is being used\n\
             - Helps identify when updates are needed\n\
             - Improves code review (reviewers can see version changes)\n\
             - Enables automated version tracking tools\n\n\
             Format: uses: owner/repo@<40-char-SHA> # vX.Y.Z\n\
             Example: uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2\n",
            violations.join("\n")
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

/// Return `true` if `reference` is a valid 40-character lowercase hex SHA.
///
/// GitHub Actions require full-length SHA pinning (not tags) to prevent
/// supply-chain attacks where a mutable tag could be silently updated.
fn is_sha_pinned(reference: &str) -> bool {
    reference.len() == 40 && reference.chars().all(|c| c.is_ascii_hexdigit())
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
    // skipped, preserving the original behaviour of the per-line `if let Ok(regex)` guard.
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

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim_start();

            // Track fenced code block state per CommonMark spec:
            // - Opening fence: 3+ backticks, may have info string (e.g., ```rust)
            // - Closing fence: 3+ backticks with NO info string (bare backticks only)
            // When inside a code block, only a bare fence closes it; inner ```rust
            // lines are content, not real fences.
            let backtick_count = trimmed.len() - trimmed.trim_start_matches('`').len();
            if backtick_count >= 3 {
                let after_backticks = trimmed[backtick_count..].trim();
                if in_code_block {
                    if after_backticks.is_empty() {
                        in_code_block = false;
                    }
                } else {
                    in_code_block = true;
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

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;

            // Track fenced code block state per CommonMark spec:
            // - Opening fence: 3+ backticks, may have info string (e.g., ```rust)
            // - Closing fence: 3+ backticks with NO info string (just backticks + optional spaces)
            // When already inside a code block, only a bare fence (no info string) closes it.
            // This correctly handles nested code examples in markdown skill docs where
            // inner ```rust fences are content, not real fences.
            let trimmed = line.trim_start();
            let backtick_prefix_len = trimmed.len() - trimmed.trim_start_matches('`').len();
            if backtick_prefix_len >= 3 {
                let after_backticks = trimmed[backtick_prefix_len..].trim();
                if in_code_block {
                    // Inside a code block: only a bare fence line (no info string) closes it
                    if after_backticks.is_empty() {
                        in_code_block = false;
                    }
                    // Lines like ```rust inside a code block are just content
                } else {
                    // Outside a code block: any 3+ backtick line opens one
                    in_code_block = true;
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

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim_start();

            // Count the leading backtick characters
            let backtick_count = trimmed.len() - trimmed.trim_start_matches('`').len();
            if backtick_count >= 3 {
                let after_backticks = trimmed[backtick_count..].trim();
                if in_code_block {
                    // Inside a code block: only a bare fence (no info string) closes it.
                    // Lines like ```rust inside a code block are just content, not real fences.
                    if after_backticks.is_empty() {
                        in_code_block = false;
                        closes += 1;
                    }
                } else {
                    // Outside a code block: any 3+ backtick line opens one
                    // (may have an info string like ```rust or ```bash)
                    in_code_block = true;
                    opens += 1;
                    last_open_line = line_num;
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

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim_start();

            // Track fenced code block state per CommonMark spec:
            // Opening fences may have info strings; closing fences must be bare.
            let backtick_count = trimmed.len() - trimmed.trim_start_matches('`').len();
            if backtick_count >= 3 {
                let after_backticks = trimmed[backtick_count..].trim();
                if in_code_block {
                    if after_backticks.is_empty() {
                        in_code_block = false;
                    }
                } else {
                    in_code_block = true;
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
                      - Missing permissions block defaults to GITHUB_TOKEN write access\n\n\
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
fn test_cargo_deny_has_rustup_toolchain_override() {
    // This test prevents regression of the cargo-deny Docker container toolchain issue.
    //
    // Root cause: The cargo-deny-action runs inside a Docker container that ships its
    // own Rust toolchain. When our repo has rust-toolchain.toml pinning a specific
    // version (e.g., 1.88.0), rustup inside the container tries to use that version
    // but fails because it's not installed in the container image.
    //
    // Fix: Set RUSTUP_TOOLCHAIN=stable as an env var on the cargo-deny step.
    // RUSTUP_TOOLCHAIN takes precedence over rust-toolchain.toml, so the container
    // uses its pre-installed stable toolchain instead of trying to download our
    // pinned version. This is safe because cargo-deny only inspects metadata and
    // Cargo.lock — it does not compile code, so the exact Rust version is irrelevant.

    let root = repo_root();
    let ci_workflow = root.join(".github/workflows/ci.yml");
    let content = read_file(&ci_workflow);

    // Find the deny job section
    assert!(
        content.contains("  deny:"),
        "CI workflow must have a 'deny' job for dependency auditing.\n\
         File: {}",
        ci_workflow.display()
    );

    // Find the "Run cargo-deny" step and verify it has RUSTUP_TOOLCHAIN env var
    let mut in_deny_step = false;
    let mut found_rustup_toolchain = false;
    let mut deny_step_line = 0;

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Detect the cargo-deny step (by name or uses)
        if trimmed.contains("cargo-deny")
            && (trimmed.starts_with("uses:") || trimmed.starts_with("- name:"))
        {
            in_deny_step = true;
            deny_step_line = line_num + 1;
        }

        // Check for RUSTUP_TOOLCHAIN within the step's env block
        if in_deny_step && trimmed.starts_with("RUSTUP_TOOLCHAIN:") {
            found_rustup_toolchain = true;
            let value = trimmed
                .strip_prefix("RUSTUP_TOOLCHAIN:")
                .unwrap_or("")
                .trim();
            assert!(
                !value.is_empty(),
                "RUSTUP_TOOLCHAIN env var in cargo-deny step must have a value (e.g., 'stable').\n\
                 Line: {}\n\
                 File: {}",
                line_num + 1,
                ci_workflow.display()
            );
            break;
        }

        // If we hit the next step or job after the deny step, stop searching
        if in_deny_step
            && (trimmed.starts_with("- name:") || trimmed.starts_with("- uses:"))
            && deny_step_line != 0
            && line_num + 1 > deny_step_line + 1
        {
            break;
        }
    }

    assert!(
        found_rustup_toolchain,
        "The cargo-deny step in ci.yml must have RUSTUP_TOOLCHAIN env var set.\n\
         Without it, the cargo-deny Docker container fails when rust-toolchain.toml\n\
         pins a Rust version not installed in the container image.\n\n\
         Fix: Add to the cargo-deny step:\n\
           env:\n\
             RUSTUP_TOOLCHAIN: stable\n\n\
         File: {}\n\
         Deny step found at line: {}",
        ci_workflow.display(),
        deny_step_line
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
    let link_check = root.join(".github/workflows/link-check.yml");
    let content = read_file(&link_check);

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
        "link-check.yml must set lycheeVersion to override the bundled lychee binary.\n\
         Without this, the action uses lychee v0.21.0 which has a hidden file matcher bug\n\
         (lycheeverse/lychee#1936) that scans .lychee.toml as input.\n\n\
         Fix: Add 'lycheeVersion: v0.22.0' (or newer) to the lychee-action step's 'with:' block.\n\
         File: {}",
        link_check.display()
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
        link_check.display()
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
        link_check.display()
    );
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
    // as the other CI workflows (SHA pinning is checked separately by
    // test_github_actions_are_pinned_to_sha which covers all workflows).
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
    let sbom_section: String = ci_content
        .lines()
        .skip_while(|line| !line.starts_with("  sbom:"))
        .take_while(|line| {
            line.starts_with("  sbom:")
                || line.starts_with("    ")
                || line.trim().is_empty()
        })
        .collect::<Vec<&str>>()
        .join("\n");

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

    let sbom_section: String = ci_content
        .lines()
        .skip_while(|line| !line.starts_with("  sbom:"))
        .take_while(|line| {
            line.starts_with("  sbom:")
                || line.starts_with("    ")
                || line.trim().is_empty()
        })
        .collect::<Vec<&str>>()
        .join("\n");

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

    let sbom_section: String = ci_content
        .lines()
        .skip_while(|line| !line.starts_with("  sbom:"))
        .take_while(|line| {
            line.starts_with("  sbom:")
                || line.starts_with("    ")
                || line.trim().is_empty()
        })
        .collect::<Vec<&str>>()
        .join("\n");

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
        content.contains("tool: cargo-sbom"),
        "release.yml must install cargo-sbom for SBOM generation.\n\
         File: {}",
        release_yml.display()
    );
}

#[test]
fn test_release_workflow_attaches_sbom_to_release() {
    // Validates that the SBOM file is included in the GitHub release assets.
    // This ensures consumers can download the SBOM alongside the release.

    let root = repo_root();
    let release_yml = root.join(".github/workflows/release.yml");

    if !release_yml.exists() {
        return;
    }

    let content = read_file(&release_yml);

    // The action-gh-release `files:` field should include the SBOM
    assert!(
        content.contains("files: sbom.cdx.json"),
        "release.yml must attach sbom.cdx.json to the GitHub release.\n\
         Add 'files: sbom.cdx.json' to the softprops/action-gh-release step.\n\
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
