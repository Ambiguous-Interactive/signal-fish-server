# CI Configuration Test Suite Analysis

**Date:** 2026-02-17
**Test File:** `tests/ci_config_tests.rs`
**Total Tests:** 35
**Lines of Code:** 2,492

## Executive Summary

The CI configuration test suite is comprehensive and well-structured, with 35 tests covering MSRV
consistency, workflow validation, markdown linting, and AWK script validation. While the tests are
thorough, there are significant opportunities for consolidation through data-driven patterns,
improved diagnostics, and helper function extraction.

### Key Findings

- **Test Coverage**: Excellent coverage of CI/CD configuration concerns
- **Duplication**: ~40% of tests share common patterns that could be consolidated
- **Data-Driven Potential**: High - many tests iterate over similar structures
- **Diagnostic Quality**: Good error messages, but could be more actionable
- **Complexity**: Some tests are doing too much; helper functions needed

---

## Analysis by Category

### 1. MSRV Consistency Tests (4 tests)

**Current Tests:**

1. `test_msrv_consistency_across_config_files` - Validates MSRV across `Cargo.toml`,
   `rust-toolchain.toml`, `clippy.toml`, Dockerfile
2. `test_msrv_version_normalization_logic` - Unit test for version comparison logic
3. `test_ci_workflow_msrv_normalization` - Validates CI workflow normalization
4. `test_msrv_script_consistency_with_ci` - Validates local script matches CI

**Overlapping Concerns:**

- All tests validate MSRV-related logic
- Tests 1, 3, and 4 all read and parse similar files
- Tests 2 is a pure unit test (no I/O)

**Consolidation Opportunity:** ✅ HIGH

**Recommendation:**

- **Keep:** `test_msrv_version_normalization_logic` (pure unit test)
- **Keep:** `test_msrv_consistency_across_config_files` (comprehensive file validation)
- **Consolidate:** Merge tests 3 and 4 into the main test as additional validation steps
- **Result:** 4 tests → 2 tests

**Data-Driven Improvement:**

```rust
// Instead of hardcoded checks, use a data structure:
struct MsrvConfigFile {
    path: &'static str,
    field_name: &'static str,
    parser: fn(&str, &str) -> Option<String>,
    allow_shorthand: bool,  // For Docker (1.88 vs 1.88.0)
}

const MSRV_FILES: &[MsrvConfigFile] = &[
    MsrvConfigFile {
        path: "rust-toolchain.toml",
        field_name: "channel",
        parser: extract_yaml_version,
        allow_shorthand: false,
    },
    MsrvConfigFile {
        path: "clippy.toml",
        field_name: "msrv",
        parser: extract_toml_version,
        allow_shorthand: false,
    },
    MsrvConfigFile {
        path: "Dockerfile",
        field_name: "FROM rust:",
        parser: extract_dockerfile_version,
        allow_shorthand: true,
    },
];

#[test]
fn test_msrv_consistency_across_config_files() {
    let root = repo_root();
    let cargo_toml = root.join("Cargo.toml");
    let msrv = extract_toml_version(&read_file(&cargo_toml), "rust-version")
        .expect("MSRV not found in Cargo.toml");

    let mut violations = Vec::new();

    for config in MSRV_FILES {
        let path = root.join(config.path);
        if !path.exists() {
            continue;
        }

        let content = read_file(&path);
        let found = (config.parser)(&content, config.field_name);

        if let Some(version) = found {
            let matches = if config.allow_shorthand {
                version_matches_allowing_shorthand(&msrv, &version)
            } else {
                version == msrv
            };

            if !matches {
                violations.push(format!(
                    "{}: MSRV mismatch\n  Expected: {}\n  Found: {}\n  Fix: Update {} to \"{}\"",
                    config.path, msrv, version, config.field_name, msrv
                ));
            }
        }
    }

    assert!(violations.is_empty(), "MSRV consistency violations:\n\n{}", violations.join("\n\n"));
}
```

**Enhanced Diagnostics:**

```rust
// Instead of:
assert_eq!(toolchain_version, msrv, "rust-toolchain.toml channel must match Cargo.toml rust-version");

// Provide:
assert_eq!(
    toolchain_version, msrv,
    "MSRV mismatch in rust-toolchain.toml\n\
     \n\
     Expected: {msrv}\n\
     Found:    {toolchain_version}\n\
     \n\
     Fix this with:\n\
     sed -i 's/channel = \"{toolchain_version}\"/channel = \"{msrv}\"/' rust-toolchain.toml\n\
     \n\
     Or manually edit rust-toolchain.toml and set:\n\
     channel = \"{msrv}\"\n\
     \n\
     Why this matters:\n\
     - CI uses rust-toolchain.toml to select Rust version\n\
     - Cargo.toml rust-version is the source of truth\n\
     - Mismatches cause CI failures when features require specific Rust versions"
);
```

---

### 2. Workflow Validation Tests (7 tests)

**Current Tests:**

1. `test_required_ci_workflows_exist` - Checks for required workflow files
2. `test_ci_workflow_has_required_jobs` - Validates ci.yml has required jobs
3. `test_workflow_files_are_valid_yaml` - Basic YAML syntax validation
4. `test_no_language_specific_cache_mismatch` - Prevents Python cache on Rust project
5. `test_link_check_workflow_exists_and_is_configured` - Validates link-check.yml
6. `test_markdownlint_workflow_exists_and_is_configured` - Validates markdownlint.yml
7. `test_doc_validation_workflow_has_shellcheck` - Validates doc-validation.yml

**Overlapping Concerns:**

- Tests 1, 5, 6, 7 all validate workflow file existence and configuration
- Tests 2 and 1 both validate ci.yml structure
- All tests read and parse workflow files

**Consolidation Opportunity:** ✅ HIGH

**Recommendation:**

- Create a data-driven `WorkflowSpec` structure
- Single test validates all workflows against their specs
- **Result:** 7 tests → 2-3 tests (generic validation + specific edge cases)

**Data-Driven Improvement:**

```rust
struct WorkflowSpec {
    filename: &'static str,
    description: &'static str,
    required: bool,
    required_actions: Vec<&'static str>,
    required_fields: Vec<&'static str>,
    required_config_refs: Vec<&'static str>,
    should_have_concurrency: bool,
    should_have_schedule: bool,
}

const WORKFLOW_SPECS: &[WorkflowSpec] = &[
    WorkflowSpec {
        filename: "ci.yml",
        description: "Main CI pipeline",
        required: true,
        required_actions: vec![], // Validated separately by required_jobs
        required_fields: vec!["name:", "on:", "jobs:"],
        required_config_refs: vec![],
        should_have_concurrency: true,
        should_have_schedule: false,
    },
    WorkflowSpec {
        filename: "link-check.yml",
        description: "Link checking with lychee",
        required: true,
        required_actions: vec!["lycheeverse/lychee-action"],
        required_fields: vec!["GITHUB_TOKEN"],
        required_config_refs: vec![".lychee.toml"],
        should_have_concurrency: true,
        should_have_schedule: true,
    },
    WorkflowSpec {
        filename: "markdownlint.yml",
        description: "Markdown linting",
        required: true,
        required_actions: vec!["DavidAnson/markdownlint-cli2-action"],
        required_fields: vec!["paths:", "**.md"],
        required_config_refs: vec![],
        should_have_concurrency: true,
        should_have_schedule: false,
    },
    // ... more specs
];

#[test]
fn test_workflow_configurations() {
    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let mut violations = Vec::new();

    for spec in WORKFLOW_SPECS {
        let path = workflows_dir.join(spec.filename);

        // Check existence
        if spec.required && !path.exists() {
            violations.push(format!(
                "Missing required workflow: {}\n  Description: {}\n  Create: {}",
                spec.filename, spec.description, path.display()
            ));
            continue;
        }

        if !path.exists() {
            continue;
        }

        let content = read_file(&path);

        // Validate required actions
        for action in &spec.required_actions {
            if !content.contains(action) {
                violations.push(format!(
                    "{}: Missing required action: {}\n  Add 'uses: {}@...' to the workflow",
                    spec.filename, action, action
                ));
            }
        }

        // Validate required fields
        for field in &spec.required_fields {
            if !content.contains(field) {
                violations.push(format!(
                    "{}: Missing required field: {}",
                    spec.filename, field
                ));
            }
        }

        // Validate config references
        for config in &spec.required_config_refs {
            if !content.contains(config) {
                violations.push(format!(
                    "{}: Should reference config file: {}",
                    spec.filename, config
                ));
            }
        }

        // Validate concurrency
        if spec.should_have_concurrency && !content.contains("concurrency:") {
            violations.push(format!(
                "{}: Missing concurrency group (wastes CI resources)",
                spec.filename
            ));
        }

        // Validate schedule
        if spec.should_have_schedule && !content.contains("schedule:") {
            violations.push(format!(
                "{}: Should run on schedule for proactive checks",
                spec.filename
            ));
        }
    }

    assert!(violations.is_empty(), "Workflow validation failures:\n\n{}", violations.join("\n\n"));
}
```

---

### 3. GitHub Actions Security Tests (3 tests)

**Current Tests:**

1. `test_github_actions_are_pinned_to_sha` - Validates SHA pinning
2. `test_cargo_deny_action_minimum_version` - Validates cargo-deny version
3. `test_action_version_comments_exist` - Validates version comments

**Overlapping Concerns:**

- All three tests parse the same workflow files
- All three examine `uses:` lines
- Tests 1 and 3 both validate SHA-pinned actions

**Consolidation Opportunity:** ✅ MEDIUM

**Recommendation:**

- Merge into a single `test_github_actions_security` test
- Parse `uses:` lines once, validate multiple aspects
- **Result:** 3 tests → 1 test

**Data-Driven Improvement:**

```rust
struct ActionSecurityRule {
    action_pattern: &'static str,
    min_version: Option<(u32, u32, u32)>,
    require_sha_pinning: bool,
    require_version_comment: bool,
}

const ACTION_SECURITY_RULES: &[ActionSecurityRule] = &[
    ActionSecurityRule {
        action_pattern: "EmbarkStudios/cargo-deny-action",
        min_version: Some((2, 0, 15)),
        require_sha_pinning: true,
        require_version_comment: true,
    },
    ActionSecurityRule {
        action_pattern: "actions/checkout",
        min_version: None,
        require_sha_pinning: true,
        require_version_comment: true,
    },
    // ... more rules
];

#[test]
fn test_github_actions_security() {
    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");
    let mut violations = Vec::new();

    for entry in std::fs::read_dir(&workflows_dir)? {
        let path = entry.path();
        if !is_yaml(&path) { continue; }

        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();

        for (line_num, line) in content.lines().enumerate() {
            if !line.trim().starts_with("uses:") { continue; }

            let action_ref = parse_action_reference(line);
            if action_ref.is_local() { continue; }

            // Find applicable rules
            for rule in ACTION_SECURITY_RULES {
                if !action_ref.matches(rule.action_pattern) { continue; }

                // Validate SHA pinning
                if rule.require_sha_pinning && !action_ref.is_sha_pinned() {
                    violations.push(/* ... */);
                }

                // Validate version comment
                if rule.require_version_comment && action_ref.version_comment.is_none() {
                    violations.push(/* ... */);
                }

                // Validate minimum version
                if let Some(min_ver) = rule.min_version {
                    if let Some(ver) = action_ref.version() {
                        if ver < min_ver {
                            violations.push(/* ... */);
                        }
                    }
                }
            }
        }
    }

    assert!(violations.is_empty(), /* ... */);
}
```

---

### 4. Markdown Validation Tests (7 tests)

**Current Tests:**

1. `test_markdown_files_have_language_identifiers` - MD040 validation
2. `test_markdown_config_exists` - Validates .markdownlint.json exists
3. `test_markdown_no_capitalized_filenames_in_links` - Link case validation
4. `test_markdown_technical_terms_consistency` - MD044 validation (strips URLs/HTML before checking)
5. `test_markdown_common_patterns_are_correct` - Pattern validation
6. `test_lychee_excludes_placeholder_urls` - Link checker config
7. `test_no_actual_placeholder_urls_in_docs` - Placeholder detection

**Overlapping Concerns:**

- Tests 1, 3, 4, 5, 7 all parse markdown files
- Tests 2 and 6 validate config files
- All tests read similar file structures

**Consolidation Opportunity:** ✅ MEDIUM

**Recommendation:**

- Create a single markdown validation test with data-driven rules
- Separate config validation from content validation
- **Result:** 7 tests → 3 tests (config, content, links)

**Data-Driven Improvement:**

```rust
struct MarkdownRule {
    name: &'static str,
    pattern: &'static str,
    severity: Severity,
    message: &'static str,
    suggestion: &'static str,
}

enum Severity { Error, Warning, Info }

const MARKDOWN_RULES: &[MarkdownRule] = &[
    MarkdownRule {
        name: "MD040",
        pattern: r"^```\s*$",
        severity: Severity::Error,
        message: "Code block without language identifier",
        suggestion: "Add language after opening fence: ```rust, ```bash, etc.",
    },
    MarkdownRule {
        name: "uppercase-extension",
        pattern: r"\]\([^)]+\.(MD|TOML|RS|JSON|YAML|YML)\)",
        severity: Severity::Error,
        message: "Link has uppercase file extension",
        suggestion: "Use lowercase extensions for cross-platform compatibility",
    },
    MarkdownRule {
        name: "capitalized-directory",
        pattern: r"\]\([^)]*/(Docs|Scripts|Tests)/",
        severity: Severity::Warning,
        message: "Link contains capitalized directory name",
        suggestion: "Use lowercase directory names for consistency",
    },
    // ... more rules
];

#[test]
fn test_markdown_content_validation() {
    let root = repo_root();
    let markdown_files = find_files_with_extension(&root, "md", EXCLUDE_DIRS);

    let mut violations = Vec::new();
    let mut stats = ValidationStats::new();

    for file in &markdown_files {
        let content = read_file(file);
        let mut in_code_block = false;

        for (line_num, line) in content.lines().enumerate() {
            // Track code blocks
            if line.starts_with("```") {
                in_code_block = !in_code_block;
            }

            // Apply rules
            for rule in MARKDOWN_RULES {
                if should_skip_rule(rule, in_code_block) { continue; }

                if let Ok(regex) = Regex::new(rule.pattern) {
                    if regex.is_match(line) {
                        let violation = Violation {
                            file: file.display().to_string(),
                            line: line_num + 1,
                            rule: rule.name,
                            severity: rule.severity,
                            message: rule.message,
                            suggestion: rule.suggestion,
                            context: line.trim(),
                        };

                        match violation.severity {
                            Severity::Error => violations.push(violation),
                            Severity::Warning => stats.warnings.push(violation),
                            Severity::Info => stats.info.push(violation),
                        }
                    }
                }
            }
        }
    }

    // Report warnings and info
    if !stats.warnings.is_empty() {
        eprintln!("Warnings:\n{}", format_violations(&stats.warnings));
    }

    if !stats.info.is_empty() {
        eprintln!("Info:\n{}", format_violations(&stats.info));
    }

    // Fail only on errors
    assert!(violations.is_empty(), "Markdown validation errors:\n\n{}", format_violations(&violations));
}
```

---

### 5. AWK Script Validation Tests (4 tests)

**Current Tests:**

1. `test_doc_validation_awk_script_extraction` - Validates AWK patterns in workflow
2. `test_awk_pattern_matching_with_fixtures` - Fixture-based validation
3. `test_awk_posix_compatibility` - POSIX compliance checks
4. `test_awk_script_syntax_validation` - Syntax validation

**Overlapping Concerns:**

- All tests parse the same workflow file
- All tests examine AWK script content
- Tests 1, 3, 4 all validate similar patterns

**Consolidation Opportunity:** ✅ MEDIUM-HIGH

**Recommendation:**

- Merge tests 1, 3, 4 into a single comprehensive AWK validation test
- Keep test 2 separate (fixture-based testing has different goals)
- **Result:** 4 tests → 2 tests

**Data-Driven Improvement:**

```rust
struct AwkValidationRule {
    name: &'static str,
    check: fn(&str) -> Option<String>,
    description: &'static str,
}

const AWK_RULES: &[AwkValidationRule] = &[
    AwkValidationRule {
        name: "case-insensitive-rust-pattern",
        check: |content| {
            if !content.contains("/^```[Rr]ust/") {
                Some("AWK pattern should match both ```rust and ```Rust".to_string())
            } else {
                None
            }
        },
        description: "Ensures case-insensitive matching for Rust code blocks",
    },
    AwkValidationRule {
        name: "end-block-for-unclosed",
        check: |content| {
            if !(content.contains("END {") && content.contains("if (in_block)")) {
                Some("AWK script should handle unclosed blocks at EOF".to_string())
            } else {
                None
            }
        },
        description: "Handles code blocks without closing fence at end of file",
    },
    AwkValidationRule {
        name: "posix-no-gensub",
        check: |content| {
            if content.contains("gensub(") {
                Some("Use sub() or gsub() instead of gensub() for POSIX compatibility".to_string())
            } else {
                None
            }
        },
        description: "Prevents GNU awk-specific extensions",
    },
    // ... more rules
];

#[test]
fn test_awk_script_validation() {
    let root = repo_root();
    let workflow = root.join(".github/workflows/doc-validation.yml");

    if !workflow.exists() {
        return;
    }

    let content = read_file(&workflow);
    let mut violations = Vec::new();

    // Apply all validation rules
    for rule in AWK_RULES {
        if let Some(error) = (rule.check)(&content) {
            violations.push(format!(
                "AWK validation failed: {}\n  Rule: {}\n  Description: {}\n  Error: {}",
                rule.name, rule.name, rule.description, error
            ));
        }
    }

    // Additional structural validation
    if !content.contains("awk '") && !content.contains("awk \"") {
        violations.push("No AWK scripts found in doc-validation.yml".to_string());
    }

    assert!(violations.is_empty(), "AWK script issues:\n\n{}", violations.join("\n\n"));
}
```

---

### 6. Configuration File Tests (6 tests)

**Current Tests:**

1. `test_typos_config_exists_and_is_valid` - .typos.toml validation
2. `test_typos_passes_on_known_files` - Typos integration test
3. `test_lychee_config_exists_and_is_valid` - .lychee.toml validation
4. `test_lychee_config_format_is_valid_toml` - TOML format validation
5. `test_link_check_workflow_uses_lychee_config` - Integration validation
6. `test_scripts_are_executable` - Script permissions

**Overlapping Concerns:**

- Tests 3 and 4 both validate .lychee.toml
- Tests 1 and 2 both validate .typos.toml
- Tests 3, 4, 5 all relate to link checking configuration

**Consolidation Opportunity:** ✅ MEDIUM

**Recommendation:**

- Merge lychee tests (3, 4, 5) into one comprehensive test
- Merge typos tests (1, 2) into one test
- Keep script permissions separate (different concern)
- **Result:** 6 tests → 3 tests

**Data-Driven Improvement:**

```rust
struct ConfigFileSpec {
    path: &'static str,
    description: &'static str,
    required: bool,
    format: ConfigFormat,
    required_sections: Vec<&'static str>,
    required_fields: Vec<&'static str>,
    validation_fn: Option<fn(&str) -> Vec<String>>,
}

enum ConfigFormat { Toml, Json, Yaml }

const CONFIG_SPECS: &[ConfigFileSpec] = &[
    ConfigFileSpec {
        path: ".lychee.toml",
        description: "Link checker configuration",
        required: true,
        format: ConfigFormat::Toml,
        required_sections: vec![],
        required_fields: vec!["max_concurrency", "accept", "exclude", "timeout"],
        validation_fn: Some(validate_lychee_config),
    },
    ConfigFileSpec {
        path: ".typos.toml",
        description: "Spell checker configuration",
        required: true,
        format: ConfigFormat::Toml,
        required_sections: vec!["default.extend-words", "default.extend-identifiers"],
        required_fields: vec![],
        validation_fn: Some(validate_typos_config),
    },
    ConfigFileSpec {
        path: ".markdownlint.json",
        description: "Markdown linter configuration",
        required: true,
        format: ConfigFormat::Json,
        required_sections: vec![],
        required_fields: vec!["MD040"],
        validation_fn: None,
    },
];

#[test]
fn test_config_files() {
    let root = repo_root();
    let mut violations = Vec::new();

    for spec in CONFIG_SPECS {
        let path = root.join(spec.path);

        // Check existence
        if spec.required && !path.exists() {
            violations.push(format!(
                "Missing required config: {}\n  Description: {}\n  Create: {}",
                spec.path, spec.description, path.display()
            ));
            continue;
        }

        if !path.exists() {
            continue;
        }

        let content = read_file(&path);

        // Validate format
        match spec.format {
            ConfigFormat::Toml => validate_toml_format(&content, spec.path, &mut violations),
            ConfigFormat::Json => validate_json_format(&content, spec.path, &mut violations),
            ConfigFormat::Yaml => validate_yaml_format(&content, spec.path, &mut violations),
        }

        // Validate required sections
        for section in &spec.required_sections {
            if !content.contains(section) {
                violations.push(format!(
                    "{}: Missing required section: [{}]",
                    spec.path, section
                ));
            }
        }

        // Validate required fields
        for field in &spec.required_fields {
            if !content.contains(field) {
                violations.push(format!(
                    "{}: Missing required field: {}",
                    spec.path, field
                ));
            }
        }

        // Custom validation
        if let Some(validator) = spec.validation_fn {
            let custom_violations = validator(&content);
            for v in custom_violations {
                violations.push(format!("{}: {}", spec.path, v));
            }
        }
    }

    assert!(violations.is_empty(), "Config file validation failures:\n\n{}", violations.join("\n\n"));
}
```

---

### 7. Workflow Best Practices Tests (1 consolidated test)

**Current Test:**

1. `test_workflow_hygiene_requirements` - Data-driven validation of concurrency groups,
   timeouts, and minimal permissions across all workflow files

**Status:** ✅ CONSOLIDATED

This was previously 3 separate tests that iterated over workflow files checking different
structural patterns. They have been merged into a single data-driven test using a
`HygieneRule` struct with closures for filtering and checking.

**Data-Driven Improvement:**

```rust
struct WorkflowBestPractice {
    name: &'static str,
    check: fn(&str, &str) -> Option<String>,
    severity: Severity,
    applies_to: Vec<&'static str>,
}

const WORKFLOW_BEST_PRACTICES: &[WorkflowBestPractice] = &[
    WorkflowBestPractice {
        name: "concurrency-groups",
        check: |content, filename| {
            if !content.contains("concurrency:") {
                Some(format!("{}: Missing concurrency group (wastes CI resources)", filename))
            } else if !content.contains("cancel-in-progress: true") {
                Some(format!("{}: Concurrency group missing 'cancel-in-progress: true'", filename))
            } else {
                None
            }
        },
        severity: Severity::Error,
        applies_to: vec!["ci.yml", "link-check.yml", "markdownlint.yml"],
    },
    WorkflowBestPractice {
        name: "timeout-minutes",
        check: |content, filename| {
            if !content.contains("timeout-minutes:") {
                Some(format!("{}: No timeout configured (may hang indefinitely)", filename))
            } else {
                None
            }
        },
        severity: Severity::Warning,
        applies_to: vec![], // Applies to all
    },
    WorkflowBestPractice {
        name: "minimal-permissions",
        check: |content, filename| {
            if !content.contains("permissions:") {
                Some(format!("{}: No permissions block (should use minimal permissions)", filename))
            } else if content.contains("permissions: write-all") {
                Some(format!("{}: Uses overly permissive 'write-all'", filename))
            } else {
                None
            }
        },
        severity: Severity::Info,
        applies_to: vec![], // Applies to all
    },
];

#[test]
fn test_workflow_hygiene_requirements() {
    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut info = Vec::new();

    for entry in std::fs::read_dir(&workflows_dir)? {
        let path = entry.path();
        if !is_yaml(&path) { continue; }

        let filename = path.file_name().unwrap().to_string_lossy();
        let content = read_file(&path);

        for practice in WORKFLOW_BEST_PRACTICES {
            // Check if this practice applies to this file
            if !practice.applies_to.is_empty()
                && !practice.applies_to.contains(&filename.as_ref()) {
                continue;
            }

            if let Some(violation) = (practice.check)(&content, &filename) {
                match practice.severity {
                    Severity::Error => errors.push(violation),
                    Severity::Warning => warnings.push(violation),
                    Severity::Info => info.push(violation),
                }
            }
        }
    }

    // Report non-errors
    if !warnings.is_empty() {
        eprintln!("Warnings:\n{}", warnings.join("\n"));
    }
    if !info.is_empty() {
        eprintln!("Info:\n{}", info.join("\n"));
    }

    // Fail only on errors
    assert!(errors.is_empty(), "Workflow best practice violations:\n\n{}", errors.join("\n\n"));
}
```

---

## Consolidated Test Structure Recommendation

### Proposed Test Organization (19 tests)

#### Category: MSRV Validation (2 tests)

1. `test_msrv_version_normalization_logic` - Pure unit test for version comparison
2. `test_msrv_consistency` - Comprehensive MSRV validation across all config files (merges 3 tests)

#### Category: Workflow Validation (3 tests)

1. `test_workflow_configurations` - Data-driven workflow spec validation (merges 5 tests)
2. `test_workflow_yaml_syntax` - YAML syntax validation
3. `test_workflow_hygiene_requirements` - Concurrency, timeouts, permissions (✅ DONE)

#### Category: GitHub Actions Security (1 test)

1. `test_github_actions_security` - SHA pinning, versions, comments (merges 3 tests)

#### Category: Markdown Validation (3 tests)

1. `test_markdown_config_files` - Config existence and format (merges 2 tests)
2. `test_markdown_content_validation` - Data-driven content rules (merges 4 tests)
3. `test_markdown_link_validation` - Link checker config and placeholder detection (merges 2 tests)

#### Category: AWK Script Validation (2 tests)

1. `test_awk_script_validation` - POSIX compliance, syntax, patterns (merges 3 tests)
2. `test_awk_pattern_matching_with_fixtures` - Fixture-based testing (kept separate)

#### Category: Configuration Files (3 tests)

1. `test_config_files` - Data-driven config validation (merges 4 tests)
2. `test_scripts_are_executable` - Script permissions (kept separate)
3. `test_dockerfile_uses_docker_version_format` - Docker-specific validation (kept separate)

#### Category: Language/Platform Validation (1 test)

1. `test_no_language_specific_cache_mismatch` - Prevents incorrect cache configurations

#### Category: Integration Tests (4 tests)

1. `test_ci_workflow_integration` - CI workflow structure and job validation
2. `test_link_check_integration` - Link checking workflow integration
3. `test_markdownlint_integration` - Markdown linting workflow integration
4. `test_typos_integration` - Spell checking integration

### Summary

- **Current:** 35 tests
- **Proposed:** 19 tests
- **Reduction:** 45% fewer tests
- **Benefits:**
  - Easier to add new validations (just add to data structures)
  - Consistent error message formatting
  - Better separation of concerns
  - More maintainable test suite

---

## Enhanced Test Diagnostics

### Current State

Error messages are generally good but could be more actionable.

### Recommendations

#### 1. Include Fix Commands

```rust
// Instead of:
panic!("MSRV mismatch");

// Provide:
panic!(
    "MSRV mismatch in rust-toolchain.toml\n\
     \n\
     Expected: {expected}\n\
     Found:    {found}\n\
     \n\
     Fix with one command:\n\
     sed -i 's/channel = \"{found}\"/channel = \"{expected}\"/' rust-toolchain.toml\n\
     \n\
     Or manually edit: rust-toolchain.toml, line 2\n\
     Change: channel = \"{found}\"\n\
     To:     channel = \"{expected}\""
);
```

#### 2. Add Context About Why It Matters

```rust
panic!(
    "GitHub Actions must use SHA pinning\n\
     \n\
     File: {filename}, line {line_num}\n\
     Found: uses: {action}@{tag}\n\
     \n\
     Why this is required:\n\
     - Tags like v1.2.3 are mutable and can be changed by maintainers\n\
     - Attackers who compromise a maintainer account can push malicious code\n\
     - SHA pinning locks to the exact commit, preventing supply chain attacks\n\
     \n\
     How to fix:\n\
     1. Go to https://github.com/{action}/releases/tag/{tag}\n\
     2. Click the commit hash for that release\n\
     3. Copy the full 40-character SHA\n\
     4. Update the workflow:\n\
        uses: {action}@<SHA> # {tag}\n\
     \n\
     Example:\n\
     uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2"
);
```

#### 3. Show Progress and Context

```rust
// Add diagnostic information to failures
panic!(
    "Markdown validation failed\n\
     \n\
     Files checked: {total_files}\n\
     Code blocks found: {total_blocks}\n\
     Violations: {violation_count}\n\
     Files with issues: {files_with_issues}\n\
     \n\
     Issues:\n\
     {violations}\n\
     \n\
     Fix all issues with:\n\
     ./scripts/check-markdown.sh fix\n\
     \n\
     Or fix manually by adding language identifiers after ```"
);
```

#### 4. Provide Multiple Fix Options

```rust
panic!(
    "Script permissions issue\n\
     \n\
     File: {path}\n\
     Current permissions: {mode:o}\n\
     Required: executable (755 or 744)\n\
     \n\
     Fix option 1 (quick):\n\
     chmod +x {path}\n\
     git add {path}\n\
     \n\
     Fix option 2 (git only):\n\
     git update-index --chmod=+x {path}\n\
     \n\
     Verify:\n\
     ls -la {path}  # Should show -rwxr-xr-x\n\
     git ls-files --stage {path}  # Should show 100755"
);
```

---

## Helper Functions to Extract

### Current Duplication

Many tests reimplement similar logic:

- File reading with error messages
- YAML/TOML parsing
- Workflow file iteration
- Markdown file finding
- Pattern matching

### Recommended Helper Functions

```rust
// 1. File operations
fn read_file_or_panic(path: &Path, context: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("Failed to read {}: {}\n  Context: {}", path.display(), e, context)
    })
}

fn find_files(root: &Path, extensions: &[&str], exclude: &[&str]) -> Vec<PathBuf> {
    // Recursive file finder with filtering
}

// 2. Config parsing
fn extract_config_value(content: &str, field: &str, format: ConfigFormat) -> Option<String> {
    match format {
        ConfigFormat::Toml => extract_toml_version(content, field),
        ConfigFormat::Yaml => extract_yaml_version(content, field),
        ConfigFormat::Json => extract_json_value(content, field),
    }
}

// 3. Workflow operations
struct Workflow {
    path: PathBuf,
    filename: String,
    content: String,
}

impl Workflow {
    fn all_in_dir(dir: &Path) -> Vec<Self> { /* ... */ }
    fn has_field(&self, field: &str) -> bool { /* ... */ }
    fn uses_action(&self, action: &str) -> bool { /* ... */ }
    fn action_references(&self) -> Vec<ActionReference> { /* ... */ }
}

struct ActionReference {
    action: String,
    reference: String,
    line_num: usize,
}

impl ActionReference {
    fn is_sha_pinned(&self) -> bool { /* ... */ }
    fn version_comment(&self) -> Option<String> { /* ... */ }
    fn version(&self) -> Option<(u32, u32, u32)> { /* ... */ }
}

// 4. Version comparison
fn version_matches(v1: &str, v2: &str, allow_shorthand: bool) -> bool {
    if allow_shorthand {
        let v1_short = to_major_minor(v1);
        let v2_short = to_major_minor(v2);
        v1_short == v2_short || v1 == v2
    } else {
        v1 == v2
    }
}

fn to_major_minor(version: &str) -> String {
    version.split('.').take(2).collect::<Vec<_>>().join(".")
}

// 5. Validation reporting
struct Violation {
    file: String,
    line: usize,
    severity: Severity,
    rule: String,
    message: String,
    suggestion: String,
    context: Option<String>,
}

impl Violation {
    fn format(&self) -> String {
        format!(
            "{}:{}: [{}] {}\n  Suggestion: {}\n  Context: {}",
            self.file,
            self.line,
            self.rule,
            self.message,
            self.suggestion,
            self.context.as_deref().unwrap_or("N/A")
        )
    }
}

fn report_violations(violations: &[Violation], category: &str) {
    if violations.is_empty() {
        return;
    }

    let (errors, warnings, info): (Vec<_>, Vec<_>, Vec<_>) = violations.iter()
        .partition(|v| match v.severity {
            Severity::Error => true,
            _ => false,
        });

    if !warnings.is_empty() {
        eprintln!("\nWarnings ({}):", category);
        for w in warnings {
            eprintln!("{}", w.format());
        }
    }

    if !info.is_empty() {
        eprintln!("\nInfo ({}):", category);
        for i in info {
            eprintln!("{}", i.format());
        }
    }

    if !errors.is_empty() {
        panic!("\n{} validation failed:\n\n{}", category,
            errors.iter().map(|e| e.format()).collect::<Vec<_>>().join("\n\n"));
    }
}

// 6. YAML/TOML validation
fn validate_yaml_syntax(content: &str) -> Vec<String> {
    let mut issues = Vec::new();

    // Check balanced quotes
    if content.matches('"').count() % 2 != 0 {
        issues.push("Unbalanced double quotes".to_string());
    }

    // Check required fields
    for field in &["name:", "on:", "jobs:"] {
        if !content.contains(field) {
            issues.push(format!("Missing required field: {}", field));
        }
    }

    issues
}

fn validate_toml_syntax(content: &str) -> Vec<String> {
    let mut issues = Vec::new();

    // Check balanced quotes
    if content.matches('"').count() % 2 != 0 {
        issues.push("Unbalanced double quotes".to_string());
    }

    // Check for common mistakes
    if content.contains("= \"true\"") || content.contains("= \"false\"") {
        issues.push("Boolean values should not be quoted".to_string());
    }

    issues
}
```

---

## New Test Recommendations

### Missing Edge Cases

1. **Empty File Handling**

   ```rust
   #[test]
   fn test_empty_config_files_are_rejected() {
       // Ensure config files aren't empty (common mistake)
   }
   ```

2. **Concurrent Workflow Runs**

   ```rust
   #[test]
   fn test_workflow_concurrency_configuration() {
       // Verify concurrency groups are properly scoped
       // Check for potential race conditions
   }
   ```

3. **Workflow Secret Usage**

   ```rust
   #[test]
   fn test_workflows_dont_expose_secrets() {
       // Check that secrets aren't accidentally echoed
       // Verify masked output for sensitive data
   }
   ```

4. **Caching Strategy**

   ```rust
   #[test]
   fn test_workflow_caching_strategies() {
       // Validate cache keys are properly scoped
       // Check for cache invalidation conditions
   }
   ```

5. **Dependency Pinning**

   ```rust
   #[test]
   fn test_workflow_dependency_pinning() {
       // Ensure npm/pip dependencies are pinned
       // Check for lock files in workflows that install packages
   }
   ```

6. **Fail-Fast Configuration**

   ```rust
   #[test]
   fn test_matrix_fail_fast_configuration() {
       // Verify fail-fast is set appropriately for different workflows
       // CI should fail-fast, nightly/scheduled tests should not
   }
   ```

7. **Workflow Triggers**

   ```rust
   #[test]
   fn test_workflow_trigger_configurations() {
       // Validate path filters are appropriate
       // Check for unnecessary workflow runs
   }
   ```

---

## Implementation Priority

### Phase 1: High-Impact Consolidation (Week 1)

1. **MSRV Tests** - Merge 4 → 2 tests
2. **Workflow Validation** - Merge 7 → 3 tests
3. **Workflow Best Practices** - Merge 3 → 1 test
   - **Impact:** Remove 11 tests, improve maintainability significantly

### Phase 2: Medium-Impact Improvements (Week 2)

1. **GitHub Actions Security** - Merge 3 → 1 test
2. **Markdown Validation** - Merge 7 → 3 tests
3. **Configuration Files** - Merge 6 → 3 tests
   - **Impact:** Remove 12 more tests, add data-driven patterns

### Phase 3: Helper Functions & Enhancement (Week 3)

1. Extract common helper functions
2. Improve error message quality
3. Add missing edge case tests
   - **Impact:** Reduce future maintenance burden, better diagnostics

### Phase 4: AWK Validation (Week 4)

1. **AWK Tests** - Merge 4 → 2 tests
2. Add AWK fixture testing infrastructure
   - **Impact:** Better AWK validation, easier to add new patterns

---

## Metrics

### Before Consolidation

- **Total Tests:** 35
- **Total Lines:** 2,492
- **Average Lines per Test:** 71
- **Code Duplication:** ~40% (estimated)
- **Data-Driven Tests:** 2-3 (~8%)

### After Consolidation (Projected)

- **Total Tests:** 19
- **Total Lines:** ~1,800 (estimated with helpers)
- **Average Lines per Test:** 95
- **Code Duplication:** ~10% (estimated)
- **Data-Driven Tests:** 12-15 (~70%)

### Benefits

- **45% fewer tests** to maintain
- **27% less code** overall
- **Easier to add new validations** (just add to data structures)
- **Consistent error messages** across all tests
- **Better separation of concerns**

---

## Conclusion

The CI configuration test suite is comprehensive and catches real issues, but it suffers from
duplication and lacks data-driven patterns. By consolidating tests and extracting helper functions,
we can significantly improve maintainability while retaining all current validation coverage.

The recommended consolidation reduces the test count from 35 to 19 (-45%) while making it easier
to add new validations and improving error message quality. The data-driven approach will make the
test suite more maintainable and consistent.

**Next Steps:**

1. Review this analysis with the team
2. Prioritize consolidation phases
3. Implement Phase 1 (high-impact consolidations)
4. Measure impact and adjust approach for remaining phases
