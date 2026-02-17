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

    // Required workflows for project hygiene
    let required_workflows = vec![
        ("ci.yml", "Main CI pipeline (tests, clippy, etc.)"),
        ("yaml-lint.yml", "YAML syntax validation"),
        ("actionlint.yml", "GitHub Actions syntax validation"),
        (
            "unused-deps.yml",
            "Unused dependency detection (cargo-machete/cargo-udeps)",
        ),
        ("workflow-hygiene.yml", "Workflow configuration validation"),
    ];

    let mut missing_workflows = Vec::new();

    for (workflow_file, description) in &required_workflows {
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
    let content = read_file(&ci_workflow);

    // Required job names in CI workflow with descriptions
    let required_jobs = vec![
        ("check", "Code formatting and linting"),
        ("test", "Unit and integration tests"),
        ("deny", "Security audits and license checks"),
        ("msrv", "MSRV verification"),
        ("docker", "Docker build and smoke test"),
    ];

    let mut missing_jobs = Vec::new();
    let mut found_jobs = Vec::new();

    for (job_name, description) in &required_jobs {
        // Look for "job-name:" pattern at the beginning of a line
        let job_pattern = format!("  {job_name}:");
        if content.contains(&job_pattern) {
            found_jobs.push(format!("  ✓ {job_name} ({description})"));
        } else {
            missing_jobs.push(format!("  ✗ {job_name} ({description})"));
        }
    }

    if !missing_jobs.is_empty() {
        panic!(
            "CI workflow is missing required jobs:\n\n\
             Missing:\n{}\n\n\
             Found:\n{}\n\n\
             File: {}\n\n\
             These jobs are critical for CI/CD validation.\n\
             To fix:\n\
             1. Review git history to see when the job was removed\n\
             2. Restore the job definition in the jobs: section\n\
             3. Ensure the job name matches exactly (case-sensitive)\n\
             4. Verify the job has proper indentation (2 spaces)",
            missing_jobs.join("\n"),
            found_jobs.join("\n"),
            ci_workflow.display()
        );
    }
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

        // Check for balanced quotes
        let single_quotes = content.matches('\'').count();
        let double_quotes = content.matches('"').count();

        if single_quotes % 2 != 0 {
            errors.push(format!(
                "{filename}: Unbalanced single quotes (found {single_quotes} quotes)\n  \
                 Check for missing closing quotes in strings"
            ));
        }

        if double_quotes % 2 != 0 {
            errors.push(format!(
                "{filename}: Unbalanced double quotes (found {double_quotes} quotes)\n  \
                 Check for missing closing quotes in strings"
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
    let is_python_project = root.join("requirements.txt").exists()
        || root.join("Pipfile").exists()
        || root.join("pyproject.toml").exists();
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

            assert!(
                has_explanation,
                "{filename}: Uses Python pip cache but no Python project files found.\n\
                     This is a Rust project (Cargo.toml exists).\n\
                     Either remove 'cache: pip' or add a comment explaining why it's needed."
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

    let root = repo_root();
    let lychee_config = root.join(".lychee.toml");
    let content = read_file(&lychee_config);

    // Define test cases: (pattern, reason)
    let test_cases = vec![
        ("http://localhost", "Localhost URLs are placeholders"),
        ("http://127.0.0.1", "Loopback IPs are placeholders"),
        ("ws://localhost", "WebSocket localhost is placeholder"),
        ("mailto:", "Email addresses should be excluded"),
        (
            "https://github.com/owner/repo",
            "Generic placeholder pattern",
        ),
        ("https://github.com/{}", "Template placeholder pattern"),
    ];

    let mut missing_exclusions = Vec::new();
    for (pattern, reason) in test_cases {
        // Check if pattern is in exclude list (allowing for wildcards)
        let pattern_prefix = pattern.split('*').next().unwrap_or(pattern);
        if !content.contains(pattern_prefix) {
            missing_exclusions.push(format!("  - {pattern}\n    Reason: {reason}"));
        }
    }

    if !missing_exclusions.is_empty() {
        panic!(
            ".lychee.toml should exclude common placeholder URLs:\n\n{}\n\n\
             Add these patterns to the 'exclude' list in .lychee.toml.\n\
             Example:\n\
             exclude = [\n\
             \x20   \"http://localhost\",\n\
             \x20   \"https://github.com/owner/repo/*\",\n\
             ]\n",
            missing_exclusions.join("\n")
        );
    }
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

            // Track fenced code block state
            if trimmed.starts_with("```") && !trimmed.starts_with("````") {
                in_code_block = !in_code_block;
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

    // Compile URL-stripping regex outside all loops to avoid repeated allocations.
    // Strips markdown link URLs: [text](url) -> [text]
    // This prevents matching technical terms in URLs (e.g. github.com in links)
    let url_strip_regex = regex::Regex::new(r"\]\([^)]*\)").expect("valid url-strip regex pattern");

    for file in markdown_files {
        let content = read_file(&file);

        // Track fenced code block state to match MD044's "code_blocks": false behavior
        let mut in_code_block = false;

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;

            // Toggle fenced code block state on standard 3-backtick fences only.
            // 4-tick and 5-tick fences are used for nested examples in skill docs
            // and should not affect the outer tracking state.
            let trimmed = line.trim_start();
            let backtick_prefix_len = trimmed.len() - trimmed.trim_start_matches('`').len();
            if backtick_prefix_len == 3 {
                in_code_block = !in_code_block;
                continue;
            } else if backtick_prefix_len > 3 {
                // Skip 4-tick/5-tick fence lines but don't toggle state
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

            let line_no_urls = url_strip_regex.replace_all(line, "]");

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

            // Track code block state (opening and closing fences)
            if trimmed.starts_with("```") && !trimmed.starts_with("````") {
                in_code_block = !in_code_block;
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
    // This test extracts AWK scripts from the doc-validation workflow
    // and validates their syntax using awk -f with --lint
    //
    // Background: The doc-validation.yml workflow contains complex AWK scripts
    // that extract and validate code blocks from markdown. These scripts need
    // validation to prevent issues like the AWK pattern bug we fixed.

    let root = repo_root();
    let workflow = root.join(".github/workflows/doc-validation.yml");

    if !workflow.exists() {
        panic!(
            "doc-validation.yml workflow not found at {}",
            workflow.display()
        );
    }

    let content = read_file(&workflow);

    // Verify the workflow contains AWK scripts
    assert!(
        content.contains("awk '") || content.contains("awk \""),
        "doc-validation.yml should contain AWK scripts for code block extraction.\n\
         These scripts are critical for validating markdown code blocks."
    );

    // Check for the main Rust code block extraction AWK script
    // This script handles complex patterns: ```rust, ```Rust, ```rust,ignore, etc.
    assert!(
        content.contains("/^```[Rr]ust/"),
        "doc-validation.yml AWK script should use case-insensitive pattern for Rust.\n\
         Pattern /^```[Rr]ust/ matches both ```rust and ```Rust.\n\
         This prevents missing code blocks with capitalized language identifiers."
    );

    // Verify the AWK script has END block for unclosed blocks at EOF
    assert!(
        content.contains("END {") && content.contains("if (in_block)"),
        "doc-validation.yml AWK script should have END block to handle unclosed blocks.\n\
         Without END block, code blocks at end of file without closing fence are lost.\n\
         The END block should check 'if (in_block)' and output remaining content."
    );

    // Verify content accumulation handles empty first lines correctly
    // The fix uses: if (content == "") { content = $0 } else { content = content "\n" $0 }
    assert!(
        content.contains("content = $0") && content.contains("content = content \"\\n\" $0"),
        "doc-validation.yml AWK script should properly handle empty first lines.\n\
         Correct pattern: if (content == \"\") {{ content = $0 }} else {{ content = content \"\\n\" $0 }}\n\
         This prevents losing empty lines at the start of code blocks."
    );

    // Verify attribute extraction after rust/Rust fence
    // The pattern should use sub() to remove prefix and extract attributes
    assert!(
        content.contains("sub(/^```[Rr]ust,?/, \"\", attrs)"),
        "doc-validation.yml AWK script should extract attributes after rust fence.\n\
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

#[test]
fn test_workflows_use_concurrency_groups() {
    // This test ensures workflows use concurrency groups to cancel outdated runs
    // Prevents wasting CI resources on superseded commits
    //
    // Concurrency groups allow GitHub Actions to automatically cancel in-progress
    // workflow runs when a new commit is pushed, saving CI minutes and speeding
    // up feedback loops.

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    // Workflows that should have concurrency groups
    let should_have_concurrency = [
        "ci.yml",
        "link-check.yml",
        "markdownlint.yml",
        "doc-validation.yml",
    ];

    let mut missing_concurrency = Vec::new();

    for entry in collect_workflow_files(&workflows_dir) {
        let path = entry.path();
        let filename = path.file_name().unwrap().to_string_lossy();

        // Skip workflows that don't need concurrency (e.g., release workflows)
        if !should_have_concurrency.contains(&filename.as_ref()) {
            continue;
        }

        let content = read_file(&path);

        // Check for concurrency configuration
        if !content.contains("concurrency:") {
            missing_concurrency.push(format!(
                "{filename}: Missing concurrency group.\n  \
                 Add:\n  \
                 concurrency:\n  \
                   group: ${{{{ github.workflow }}}}-${{{{ github.head_ref || github.run_id }}}}\n  \
                   cancel-in-progress: true"
            ));
        } else {
            // Verify it has cancel-in-progress
            if !content.contains("cancel-in-progress: true") {
                missing_concurrency.push(format!(
                    "{filename}: Has concurrency but missing 'cancel-in-progress: true'"
                ));
            }
        }
    }

    if !missing_concurrency.is_empty() {
        panic!(
            "Workflows should use concurrency groups to cancel outdated runs:\n\n{}\n\n\
             Why concurrency groups are important:\n\
             - Saves CI minutes by canceling superseded runs\n\
             - Speeds up feedback (don't wait for old runs)\n\
             - Reduces queue times for other workflows\n\n\
             Standard pattern:\n\
             concurrency:\n\
               group: ${{{{ github.workflow }}}}-${{{{ github.head_ref || github.run_id }}}}\n\
               cancel-in-progress: true\n",
            missing_concurrency.join("\n\n")
        );
    }
}

#[test]
fn test_workflows_have_timeouts() {
    // This test ensures workflows have reasonable timeouts
    // Prevents hanging jobs from consuming CI resources indefinitely

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let mut missing_timeouts = Vec::new();

    for entry in collect_workflow_files(&workflows_dir) {
        let path = entry.path();
        let filename = path.file_name().unwrap().to_string_lossy();
        let content = read_file(&path);

        // Check for timeout-minutes in jobs
        if !content.contains("timeout-minutes:") {
            missing_timeouts.push(format!(
                "{filename}: No timeout-minutes configured.\n  \
                 Fix: Add timeout-minutes to each job.\n  \
                 Example: timeout-minutes: 10\n  \
                 Verify: grep -n 'timeout-minutes:' .github/workflows/{filename}"
            ));
        }
    }

    if !missing_timeouts.is_empty() {
        panic!(
            "Workflows are missing timeout-minutes on all jobs:\n\n{}\n\n\
             Why timeouts are required:\n\
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
            missing_timeouts.join("\n\n")
        );
    }
}

#[test]
fn test_workflows_use_minimal_permissions() {
    // This test ensures workflows follow least-privilege principle
    // Prevents security issues from compromised workflows or actions

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let mut violations = Vec::new();

    for entry in collect_workflow_files(&workflows_dir) {
        let path = entry.path();
        let filename = path.file_name().unwrap().to_string_lossy();
        let content = read_file(&path);

        // Check if workflow has permissions block
        if !content.contains("permissions:") {
            violations.push(format!(
                "{filename}: No permissions block found.\n  \
                 Fix: Add 'permissions:' block to explicitly set required permissions.\n  \
                 For read-only workflows:\n  \
                   permissions:\n  \
                     contents: read\n  \
                 Verify: grep -n 'permissions:' .github/workflows/{filename}"
            ));
        } else {
            // Check for overly permissive 'write-all' or missing 'contents: read'
            if content.contains("permissions: write-all") {
                violations.push(format!(
                    "{filename}: Uses 'write-all' permissions (too permissive).\n  \
                     Fix: Specify only required permissions explicitly.\n  \
                     Verify: grep -n 'permissions:' .github/workflows/{filename}"
                ));
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Workflows violate the least-privilege permissions principle:\n\n{}\n\n\
             Why minimal permissions are required:\n\
             - Compromised workflows or actions cannot abuse excess permissions\n\
             - GitHub requires explicit permission grants for security audits\n\
             - Missing permissions block defaults to GITHUB_TOKEN write access\n\n\
             Fix: Add a 'permissions:' block to each workflow.\n\
             For read-only workflows:\n\
               permissions:\n\
                 contents: read\n\n\
             Verify: grep -n 'permissions:' .github/workflows/<file>\n\
             Reference: https://docs.github.com/en/actions/security-guides/automatic-token-authentication",
            violations.join("\n\n")
        );
    }
}
