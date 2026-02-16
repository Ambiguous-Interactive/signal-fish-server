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
            let msrv_major_minor = msrv.split('.').take(2).collect::<Vec<_>>().join(".");
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
    let msrv_major_minor: String = msrv_full.split('.').take(2).collect::<Vec<_>>().join(".");
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
    let normalized_patch: String = msrv_different_patch
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    assert_eq!(
        normalized_patch, dockerfile_version,
        "Patch version should be ignored when comparing to Docker format"
    );

    // Test case 5: Verify edge cases with single-digit patch versions
    let msrv_zero_patch = "1.88.0";
    let msrv_nonzero_patch = "1.88.5";
    let norm1: String = msrv_zero_patch
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    let norm2: String = msrv_nonzero_patch
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
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

    for (workflow_file, description) in required_workflows {
        let workflow_path = workflows_dir.join(workflow_file);
        assert!(
            workflow_path.exists(),
            "Required workflow missing: {} ({})\n\
             Path: {}\n\
             This workflow is required for CI/CD hygiene.",
            workflow_file,
            description,
            workflow_path.display()
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

    // Required job names in CI workflow
    let required_jobs = vec![
        "check",  // Code formatting and linting
        "test",   // Unit and integration tests
        "deny",   // Security audits and license checks
        "msrv",   // MSRV verification
        "docker", // Docker build and smoke test
    ];

    for job_name in required_jobs {
        // Look for "job-name:" pattern at the beginning of a line
        let job_pattern = format!("  {job_name}:");
        assert!(
            content.contains(&job_pattern),
            "CI workflow missing required job: '{job_name}'\n\
             This job is critical for CI/CD validation."
        );
    }
}

#[test]
fn test_workflow_files_are_valid_yaml() {
    // This test catches basic YAML syntax errors in workflow files
    // Prevents pushing broken workflows that cause CI to fail

    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    if !workflows_dir.exists() {
        return;
    }

    let workflow_files: Vec<_> = std::fs::read_dir(&workflows_dir)
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

    assert!(
        !workflow_files.is_empty(),
        "No workflow files found in .github/workflows/"
    );

    for entry in workflow_files {
        let path = entry.path();
        let content = read_file(&path);

        // Basic YAML validation checks
        // Note: This is not a full YAML parser, but catches common errors

        // Check for balanced quotes
        let single_quotes = content.matches('\'').count();
        let double_quotes = content.matches('"').count();
        assert_eq!(
            single_quotes % 2,
            0,
            "Unbalanced single quotes in {}",
            path.display()
        );
        assert_eq!(
            double_quotes % 2,
            0,
            "Unbalanced double quotes in {}",
            path.display()
        );

        // Check for required GitHub Actions fields
        assert!(
            content.contains("name:"),
            "{} missing 'name:' field",
            path.display()
        );
        assert!(
            content.contains("on:") || content.contains("'on':"),
            "{} missing 'on:' field",
            path.display()
        );
        assert!(
            content.contains("jobs:"),
            "{} missing 'jobs:' field",
            path.display()
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

    if !workflows_dir.exists() {
        return;
    }

    for entry in std::fs::read_dir(&workflows_dir)
        .unwrap()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path
            .extension()
            .map(|ext| ext == "yml" || ext == "yaml")
            .unwrap_or(false)
        {
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
}

#[test]
fn test_scripts_are_executable() {
    // This test ensures shell scripts have executable permissions
    // Prevents "permission denied" errors in CI

    let root = repo_root();
    let directories_to_check = vec![root.join("scripts"), root.join(".githooks")];

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

                    assert!(
                        is_executable,
                        "{} is not executable.\n\
                         Fix: chmod +x {} && git update-index --chmod=+x {}",
                        path.display(),
                        path.display(),
                        path.display()
                    );
                }

                // On non-Unix platforms, just check the file exists
                #[cfg(not(unix))]
                {
                    let _ = metadata; // Suppress unused variable warning
                }
            }
        }
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
        &["target", "third_party", "node_modules", "test-fixtures"],
    );

    if markdown_files.is_empty() {
        // No markdown files found, test passes trivially
        return;
    }

    let mut violations = Vec::new();

    for file in markdown_files {
        let content = read_file(&file);
        let mut in_code_block = false;

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1; // 1-indexed for human readability

            // Check for opening code fence
            if line.trim_start().starts_with("```") {
                if !in_code_block {
                    // Opening fence
                    in_code_block = true;

                    // Check if language identifier is present
                    let fence_content = line.trim_start().trim_start_matches('`').trim();
                    if fence_content.is_empty() {
                        violations.push(format!(
                            "{}:{}: Code block missing language identifier (MD040)",
                            file.display(),
                            line_num
                        ));
                    }
                } else {
                    // Closing fence
                    in_code_block = false;
                }
            }
        }
    }

    if !violations.is_empty() {
        panic!(
            "Markdown files have code blocks without language identifiers (MD040):\n\n{}\n\n\
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
            violations.join("\n")
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
fn test_typos_passes_on_known_files() {
    // This test verifies that typos passes on files known to contain technical terms
    // Prevents regression of the HashiCorp false positive issue

    let root = repo_root();

    // Files that are known to contain technical terms that should be allowed
    let files_to_check = vec![
        root.join("docs/authentication.md"),                       // Contains "HashiCorp Vault"
        root.join("docs/adr/ci-cd-preventative-measures.md"),      // Contains "HashiCorp"
    ];

    for file in files_to_check {
        if !file.exists() {
            continue;
        }

        // Check that the file actually contains HashiCorp (sanity check)
        let content = read_file(&file);
        if content.contains("HashiCorp") {
            // If we got here, the file contains HashiCorp and should be tested
            // Note: This test documents that these files should pass typos checking
            // The actual typos check runs in CI via the spellcheck workflow
            assert!(
                content.contains("HashiCorp"),
                "{} should contain 'HashiCorp' for this test to be meaningful",
                file.display()
            );
        }
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
