# Test Consolidation Implementation Examples

This document provides concrete code examples for consolidating the CI configuration tests.

## Example 1: MSRV Consolidation

### Before (3 separate tests)

```rust
#[test]
fn test_msrv_consistency_across_config_files() {
    let root = repo_root();
    let cargo_toml = root.join("Cargo.toml");
    let cargo_content = read_file(&cargo_toml);
    let msrv = extract_toml_version(&cargo_content, "rust-version")
        .expect("Could not extract rust-version from Cargo.toml");

    // Check rust-toolchain.toml
    let rust_toolchain = root.join("rust-toolchain.toml");
    if rust_toolchain.exists() {
        let toolchain_content = read_file(&rust_toolchain);
        let toolchain_version = extract_yaml_version(&toolchain_content, "channel")
            .expect("Could not extract channel from rust-toolchain.toml");
        assert_eq!(toolchain_version, msrv, "...");
    }

    // Check clippy.toml
    let clippy_toml = root.join("clippy.toml");
    if clippy_toml.exists() {
        let clippy_content = read_file(&clippy_toml);
        if let Some(clippy_msrv) = extract_toml_version(&clippy_content, "msrv") {
            assert_eq!(clippy_msrv, msrv, "...");
        }
    }

    // Check Dockerfile
    let dockerfile = root.join("Dockerfile");
    if dockerfile.exists() {
        let dockerfile_content = read_file(&dockerfile);
        let rust_version_in_dockerfile = /* ... complex extraction ... */;
        // ... more code ...
    }
}

#[test]
fn test_ci_workflow_msrv_normalization() {
    let root = repo_root();
    let ci_workflow = root.join(".github/workflows/ci.yml");
    let content = read_file(&ci_workflow);

    assert!(content.contains("MSRV_SHORT=$(echo \"$MSRV\" | sed -E 's/([0-9]+\\.[0-9]+).*/\\1/')"), "...");
    assert!(content.contains("if [ \"$DOCKERFILE_RUST\" != \"$MSRV_SHORT\" ]"), "...");
}

#[test]
fn test_msrv_script_consistency_with_ci() {
    let root = repo_root();
    let script = root.join("scripts/check-msrv-consistency.sh");
    let ci_workflow = root.join(".github/workflows/ci.yml");

    let script_content = read_file(&script);
    let ci_content = read_file(&ci_workflow);

    let normalization_pattern = "sed -E 's/([0-9]+\\.[0-9]+).*/\\1/'";

    assert!(script_content.contains(normalization_pattern), "...");
    assert!(ci_content.contains(normalization_pattern), "...");
}
```

### After (1 consolidated data-driven test)

```rust
/// Configuration for files that must match the MSRV
struct MsrvConfigFile {
    path: &'static str,
    field: &'static str,
    extractor: fn(&str, &str) -> Option<String>,
    allow_major_minor: bool,
    fix_command: &'static str,
}

const MSRV_CONFIG_FILES: &[MsrvConfigFile] = &[
    MsrvConfigFile {
        path: "rust-toolchain.toml",
        field: "channel",
        extractor: extract_yaml_version,
        allow_major_minor: false,
        fix_command: "sed -i 's/channel = \".*\"/channel = \"{msrv}\"/' rust-toolchain.toml",
    },
    MsrvConfigFile {
        path: "clippy.toml",
        field: "msrv",
        extractor: extract_toml_version,
        allow_major_minor: false,
        fix_command: "sed -i 's/msrv = \".*\"/msrv = \"{msrv}\"/' clippy.toml",
    },
    MsrvConfigFile {
        path: "Dockerfile",
        field: "FROM rust:",
        extractor: extract_dockerfile_version,
        allow_major_minor: true,
        fix_command: "sed -i 's/FROM rust:[0-9.]*/FROM rust:{msrv_short}/' Dockerfile",
    },
];

const MSRV_SCRIPTS: &[&str] = &[
    ".github/workflows/ci.yml",
    "scripts/check-msrv-consistency.sh",
];

#[test]
fn test_msrv_consistency() {
    let root = repo_root();

    // 1. Extract MSRV from Cargo.toml (source of truth)
    let cargo_toml = root.join("Cargo.toml");
    let msrv = extract_toml_version(&read_file(&cargo_toml), "rust-version")
        .unwrap_or_else(|| {
            panic!(
                "MSRV not defined in Cargo.toml\n\
                 \n\
                 Add to [package] section:\n\
                 rust-version = \"1.88.0\"\n\
                 \n\
                 This is the source of truth for all MSRV checks."
            )
        });

    let msrv_short = to_major_minor(&msrv);
    let mut violations = Vec::new();

    // 2. Validate all config files
    for config in MSRV_CONFIG_FILES {
        let path = root.join(config.path);
        if !path.exists() {
            continue;
        }

        let content = read_file(&path);
        let found = match (config.extractor)(&content, config.field) {
            Some(v) => v,
            None => {
                violations.push(format!(
                    "{}: Field '{}' not found",
                    config.path, config.field
                ));
                continue;
            }
        };

        let matches = if config.allow_major_minor {
            found == msrv || found == msrv_short
        } else {
            found == msrv
        };

        if !matches {
            let expected = if config.allow_major_minor { &msrv_short } else { &msrv };
            let fix_cmd = config.fix_command
                .replace("{msrv}", &msrv)
                .replace("{msrv_short}", &msrv_short);

            violations.push(format!(
                "{}: MSRV mismatch\n\
                 \n\
                 Expected: {}\n\
                 Found:    {}\n\
                 Field:    {}\n\
                 \n\
                 Fix with:\n\
                 {}\n\
                 \n\
                 Or manually edit {} and update the {} field.",
                config.path, expected, found, config.field,
                fix_cmd, config.path, config.field
            ));
        }
    }

    // 3. Validate normalization scripts
    let normalization_pattern = r#"sed -E 's/([0-9]+\.[0-9]+).*/\1/'"#;

    for script_path in MSRV_SCRIPTS {
        let path = root.join(script_path);
        if !path.exists() {
            violations.push(format!(
                "Missing MSRV script: {}\n\
                 This script is required for MSRV validation.",
                script_path
            ));
            continue;
        }

        let content = read_file(&path);

        if !content.contains(normalization_pattern) {
            violations.push(format!(
                "{}: Missing MSRV normalization pattern\n\
                 \n\
                 Required pattern: {}\n\
                 \n\
                 This normalizes version strings from X.Y.Z to X.Y for Docker compatibility.",
                script_path, normalization_pattern
            ));
        }

        if !content.contains("MSRV_SHORT") {
            violations.push(format!(
                "{}: Should use MSRV_SHORT variable for normalized version",
                script_path
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "MSRV consistency check failed:\n\n{}\n\n\
         Files checked: {}\n\
         Scripts checked: {}\n\
         Source of truth: Cargo.toml (rust-version = \"{}\")",
        violations.join("\n\n"),
        MSRV_CONFIG_FILES.len(),
        MSRV_SCRIPTS.len(),
        msrv
    );
}

// Helper function
fn to_major_minor(version: &str) -> String {
    version.split('.').take(2).collect::<Vec<_>>().join(".")
}

fn extract_dockerfile_version(content: &str, _field: &str) -> Option<String> {
    content
        .lines()
        .find(|line| line.trim().starts_with("FROM rust:"))
        .and_then(|line| {
            line.split(':')
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.split('-').next())
                .map(String::from)
        })
}
```

**Benefits:**
- Single test instead of 3
- Easy to add new files (just add to `MSRV_CONFIG_FILES`)
- Consistent error messages
- Better diagnostic output

---

## Example 2: Workflow Validation Consolidation

### Before (5+ separate tests)

```rust
#[test]
fn test_required_ci_workflows_exist() {
    let required_workflows = vec![
        ("ci.yml", "Main CI pipeline"),
        ("yaml-lint.yml", "YAML validation"),
        // ...
    ];
    // ... validation code ...
}

#[test]
fn test_link_check_workflow_exists_and_is_configured() {
    let workflow = root.join(".github/workflows/link-check.yml");
    assert!(workflow.exists(), "...");
    let content = read_file(&workflow);
    assert!(content.contains("lycheeverse/lychee-action"), "...");
    // ...
}

#[test]
fn test_markdownlint_workflow_exists_and_is_configured() {
    let workflow = root.join(".github/workflows/markdownlint.yml");
    assert!(workflow.exists(), "...");
    // ...
}

#[test]
fn test_ci_workflow_has_required_jobs() {
    let required_jobs = vec![("check", "Code formatting"), /* ... */];
    // ... validation code ...
}
```

### After (1 consolidated data-driven test)

```rust
/// Specification for a required workflow file
struct WorkflowSpec {
    filename: &'static str,
    description: &'static str,
    required: bool,
    required_actions: Vec<&'static str>,
    required_jobs: Vec<(&'static str, &'static str)>, // (job_name, description)
    required_env_vars: Vec<&'static str>,
    required_config_files: Vec<&'static str>,
    should_have_schedule: bool,
}

const WORKFLOW_SPECS: &[WorkflowSpec] = &[
    WorkflowSpec {
        filename: "ci.yml",
        description: "Main CI pipeline (tests, clippy, etc.)",
        required: true,
        required_actions: vec![],
        required_jobs: vec![
            ("check", "Code formatting and linting"),
            ("test", "Unit and integration tests"),
            ("deny", "Security audits and license checks"),
            ("msrv", "MSRV verification"),
            ("docker", "Docker build and smoke test"),
        ],
        required_env_vars: vec![],
        required_config_files: vec![],
        should_have_schedule: false,
    },
    WorkflowSpec {
        filename: "link-check.yml",
        description: "Link checking with lychee",
        required: true,
        required_actions: vec!["lycheeverse/lychee-action"],
        required_jobs: vec![],
        required_env_vars: vec!["GITHUB_TOKEN"],
        required_config_files: vec![".lychee.toml"],
        should_have_schedule: true,
    },
    WorkflowSpec {
        filename: "markdownlint.yml",
        description: "Markdown linting",
        required: true,
        required_actions: vec!["DavidAnson/markdownlint-cli2-action"],
        required_jobs: vec![],
        required_env_vars: vec![],
        required_config_files: vec![],
        should_have_schedule: false,
    },
    WorkflowSpec {
        filename: "yaml-lint.yml",
        description: "YAML syntax validation",
        required: true,
        required_actions: vec![],
        required_jobs: vec![],
        required_env_vars: vec![],
        required_config_files: vec![],
        should_have_schedule: false,
    },
    WorkflowSpec {
        filename: "actionlint.yml",
        description: "GitHub Actions syntax validation",
        required: true,
        required_actions: vec![],
        required_jobs: vec![],
        required_env_vars: vec![],
        required_config_files: vec![],
        should_have_schedule: false,
    },
    WorkflowSpec {
        filename: "unused-deps.yml",
        description: "Unused dependency detection",
        required: true,
        required_actions: vec![],
        required_jobs: vec![],
        required_env_vars: vec![],
        required_config_files: vec![],
        should_have_schedule: false,
    },
    WorkflowSpec {
        filename: "workflow-hygiene.yml",
        description: "Workflow configuration validation",
        required: true,
        required_actions: vec![],
        required_jobs: vec![],
        required_env_vars: vec![],
        required_config_files: vec![],
        should_have_schedule: false,
    },
];

#[test]
fn test_workflow_configurations() {
    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    assert!(
        workflows_dir.exists(),
        "Workflows directory not found: {}",
        workflows_dir.display()
    );

    let mut violations = Vec::new();
    let mut stats = ValidationStats::new();

    for spec in WORKFLOW_SPECS {
        stats.total_specs += 1;
        let path = workflows_dir.join(spec.filename);

        // Check existence
        if !path.exists() {
            if spec.required {
                violations.push(format!(
                    "Missing required workflow: {}\n\
                     \n\
                     Description: {}\n\
                     Expected path: {}\n\
                     \n\
                     Create this workflow or mark it as not required.",
                    spec.filename, spec.description, path.display()
                ));
            }
            continue;
        }

        stats.workflows_found += 1;
        let content = read_file(&path);

        // Validate required actions
        for action in &spec.required_actions {
            if !content.contains(action) {
                violations.push(format!(
                    "{}: Missing required action\n\
                     \n\
                     Required: {}\n\
                     \n\
                     Add to workflow:\n\
                     - uses: {}@<sha> # vX.Y.Z",
                    spec.filename, action, action
                ));
            }
        }

        // Validate required jobs
        for (job_name, job_desc) in &spec.required_jobs {
            let job_pattern = format!("  {}:", job_name);
            if !content.contains(&job_pattern) {
                violations.push(format!(
                    "{}: Missing required job: {}\n\
                     \n\
                     Description: {}\n\
                     \n\
                     Add to jobs: section:\n\
                     {}:\n\
                     \x20\x20runs-on: ubuntu-latest\n\
                     \x20\x20steps:\n\
                     \x20\x20\x20\x20# ... job steps ...",
                    spec.filename, job_name, job_desc, job_name
                ));
            }
        }

        // Validate required environment variables
        for env_var in &spec.required_env_vars {
            if !content.contains(env_var) {
                violations.push(format!(
                    "{}: Missing required environment variable: {}",
                    spec.filename, env_var
                ));
            }
        }

        // Validate config file references
        for config_file in &spec.required_config_files {
            if !content.contains(config_file) {
                violations.push(format!(
                    "{}: Should reference config file: {}\n\
                     \n\
                     Add --config {} to the action args",
                    spec.filename, config_file, config_file
                ));
            }
        }

        // Validate schedule
        if spec.should_have_schedule {
            if !content.contains("schedule:") && !content.contains("cron:") {
                violations.push(format!(
                    "{}: Should run on schedule for proactive checks\n\
                     \n\
                     Add to 'on:' section:\n\
                     schedule:\n\
                     \x20\x20- cron: '0 0 * * 1'  # Weekly on Monday",
                    spec.filename
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Workflow configuration validation failed:\n\n\
         {}\n\n\
         Statistics:\n\
         - Workflow specs: {}\n\
         - Workflows found: {}\n\
         - Violations: {}",
        violations.join("\n\n"),
        stats.total_specs,
        stats.workflows_found,
        violations.len()
    );
}

struct ValidationStats {
    total_specs: usize,
    workflows_found: usize,
}

impl ValidationStats {
    fn new() -> Self {
        Self {
            total_specs: 0,
            workflows_found: 0,
        }
    }
}
```

**Benefits:**
- Single source of truth for workflow requirements
- Easy to add new workflows or requirements
- Comprehensive validation in one test
- Better reporting with statistics

---

## Example 3: GitHub Actions Security Consolidation

### Before (3 separate tests)

```rust
#[test]
fn test_github_actions_are_pinned_to_sha() {
    // Loop through workflows
    // Check each uses: line
    // Validate SHA pinning
}

#[test]
fn test_cargo_deny_action_minimum_version() {
    // Find cargo-deny-action
    // Check version >= 2.0.15
}

#[test]
fn test_action_version_comments_exist() {
    // Loop through workflows
    // Check each SHA-pinned action
    // Validate version comment exists
}
```

### After (1 consolidated test)

```rust
/// Security policy for a GitHub Action
struct ActionSecurityPolicy {
    action: &'static str,
    min_version: Option<(u32, u32, u32)>,
    require_sha_pinning: bool,
    allow_tag_reference: bool,
}

const ACTION_SECURITY_POLICIES: &[ActionSecurityPolicy] = &[
    ActionSecurityPolicy {
        action: "EmbarkStudios/cargo-deny-action",
        min_version: Some((2, 0, 15)),
        require_sha_pinning: true,
        allow_tag_reference: false,
    },
    ActionSecurityPolicy {
        action: "actions/checkout",
        min_version: None,
        require_sha_pinning: true,
        allow_tag_reference: false,
    },
    ActionSecurityPolicy {
        action: "actions/cache",
        min_version: None,
        require_sha_pinning: true,
        allow_tag_reference: false,
    },
    // Default policy for all other actions
    ActionSecurityPolicy {
        action: "*",
        min_version: None,
        require_sha_pinning: true,
        allow_tag_reference: false,
    },
];

#[test]
fn test_github_actions_security() {
    let root = repo_root();
    let workflows_dir = root.join(".github/workflows");

    let mut violations = Vec::new();
    let mut stats = SecurityStats::new();

    for entry in std::fs::read_dir(&workflows_dir).unwrap() {
        let path = entry.ok().unwrap().path();
        if !is_yaml_file(&path) {
            continue;
        }

        let content = read_file(&path);
        let filename = path.file_name().unwrap().to_string_lossy();

        for (line_num, line) in content.lines().enumerate() {
            let line_num = line_num + 1;
            let trimmed = line.trim();

            if !trimmed.starts_with("uses:") {
                continue;
            }

            let action_ref = ActionReference::parse(trimmed);

            // Skip local actions and docker references
            if action_ref.is_local() || action_ref.is_docker() {
                continue;
            }

            stats.total_actions += 1;

            // Find applicable policy
            let policy = ACTION_SECURITY_POLICIES
                .iter()
                .find(|p| p.action == action_ref.owner_repo() || p.action == "*")
                .unwrap();

            // Check SHA pinning
            if policy.require_sha_pinning && !action_ref.is_sha_pinned() {
                violations.push(SecurityViolation {
                    file: filename.to_string(),
                    line: line_num,
                    action: action_ref.owner_repo(),
                    violation_type: ViolationType::MissingSHAPinning,
                    found: action_ref.reference.clone(),
                    message: format!(
                        "Action not pinned to SHA: {}\n\
                         \n\
                         Current: uses: {}@{}\n\
                         \n\
                         Why SHA pinning is required:\n\
                         - Tags (v1, v1.2.3) are mutable\n\
                         - Compromised maintainer accounts can modify tags\n\
                         - SHA pinning locks to exact commit\n\
                         \n\
                         How to fix:\n\
                         1. Go to https://github.com/{}/releases\n\
                         2. Find the tag {}\n\
                         3. Click the commit SHA\n\
                         4. Copy the full 40-character SHA\n\
                         5. Update: uses: {}@<SHA> # {}",
                        action_ref.owner_repo(),
                        action_ref.owner_repo(),
                        action_ref.reference,
                        action_ref.owner_repo(),
                        action_ref.reference,
                        action_ref.owner_repo(),
                        action_ref.reference
                    ),
                });
                continue;
            }

            // Check version comment
            if action_ref.is_sha_pinned() && action_ref.version_comment.is_none() {
                violations.push(SecurityViolation {
                    file: filename.to_string(),
                    line: line_num,
                    action: action_ref.owner_repo(),
                    violation_type: ViolationType::MissingVersionComment,
                    found: action_ref.reference.clone(),
                    message: format!(
                        "SHA-pinned action missing version comment\n\
                         \n\
                         Current: uses: {}@{}\n\
                         Required: uses: {}@{} # vX.Y.Z\n\
                         \n\
                         Version comments help identify what version is being used",
                        action_ref.owner_repo(),
                        action_ref.reference,
                        action_ref.owner_repo(),
                        action_ref.reference
                    ),
                });
                continue;
            }

            // Check minimum version
            if let Some(min_ver) = policy.min_version {
                if let Some(ver) = action_ref.parse_version() {
                    if ver < min_ver {
                        violations.push(SecurityViolation {
                            file: filename.to_string(),
                            line: line_num,
                            action: action_ref.owner_repo(),
                            violation_type: ViolationType::VersionTooOld,
                            found: format!("v{}.{}.{}", ver.0, ver.1, ver.2),
                            message: format!(
                                "Action version too old: {}\n\
                                 \n\
                                 Minimum required: v{}.{}.{}\n\
                                 Found: v{}.{}.{}\n\
                                 \n\
                                 Update to the latest version for security fixes",
                                action_ref.owner_repo(),
                                min_ver.0, min_ver.1, min_ver.2,
                                ver.0, ver.1, ver.2
                            ),
                        });
                    }
                }
            }

            stats.actions_validated += 1;
        }
    }

    assert!(
        violations.is_empty(),
        "GitHub Actions security violations:\n\n\
         {}\n\n\
         Statistics:\n\
         - Actions found: {}\n\
         - Actions validated: {}\n\
         - Violations: {}",
        format_security_violations(&violations),
        stats.total_actions,
        stats.actions_validated,
        violations.len()
    );
}

struct ActionReference {
    owner_repo: String,
    reference: String,
    version_comment: Option<String>,
}

impl ActionReference {
    fn parse(uses_line: &str) -> Self {
        let uses_value = uses_line.trim_start_matches("uses:").trim();
        let parts: Vec<&str> = uses_value.split('@').collect();

        let owner_repo = parts[0].to_string();
        let after_at = parts.get(1).map(|s| *s).unwrap_or("");

        let reference = after_at
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();

        let version_comment = if after_at.contains('#') {
            after_at.split('#').nth(1).map(|s| s.trim().to_string())
        } else {
            None
        };

        Self {
            owner_repo,
            reference,
            version_comment,
        }
    }

    fn is_local(&self) -> bool {
        self.owner_repo.starts_with("./")
    }

    fn is_docker(&self) -> bool {
        self.owner_repo.starts_with("docker://")
    }

    fn is_sha_pinned(&self) -> bool {
        self.reference.len() == 40
            && self.reference.chars().all(|c| c.is_ascii_hexdigit())
    }

    fn owner_repo(&self) -> String {
        self.owner_repo.clone()
    }

    fn parse_version(&self) -> Option<(u32, u32, u32)> {
        let version_str = self.version_comment.as_ref()?.trim_start_matches('v');
        let parts: Vec<u32> = version_str
            .split('.')
            .filter_map(|s| s.parse().ok())
            .collect();

        if parts.len() >= 3 {
            Some((parts[0], parts[1], parts[2]))
        } else {
            None
        }
    }
}

struct SecurityViolation {
    file: String,
    line: usize,
    action: String,
    violation_type: ViolationType,
    found: String,
    message: String,
}

enum ViolationType {
    MissingSHAPinning,
    MissingVersionComment,
    VersionTooOld,
}

struct SecurityStats {
    total_actions: usize,
    actions_validated: usize,
}

impl SecurityStats {
    fn new() -> Self {
        Self {
            total_actions: 0,
            actions_validated: 0,
        }
    }
}

fn is_yaml_file(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext == "yml" || ext == "yaml")
        .unwrap_or(false)
}

fn format_security_violations(violations: &[SecurityViolation]) -> String {
    violations
        .iter()
        .map(|v| format!("{}:{}: {}\n{}", v.file, v.line, v.action, v.message))
        .collect::<Vec<_>>()
        .join("\n\n")
}
```

**Benefits:**
- All security policies in one place
- Comprehensive action validation
- Extensible policy system
- Rich violation reporting

---

## Key Patterns for All Consolidations

### 1. Data-Driven Configuration

```rust
// Define specifications as data structures
struct ValidationSpec {
    // ... fields
}

const VALIDATION_SPECS: &[ValidationSpec] = &[
    // ... specs
];

// Single test iterates over specs
#[test]
fn test_validations() {
    for spec in VALIDATION_SPECS {
        // ... validate
    }
}
```

### 2. Rich Error Messages

```rust
violations.push(format!(
    "{}: Issue description\n\
     \n\
     Current state: {}\n\
     Expected: {}\n\
     \n\
     Why this matters:\n\
     - Reason 1\n\
     - Reason 2\n\
     \n\
     How to fix:\n\
     Command: {}\n\
     Or manually: {}",
    file, current, expected, fix_cmd, manual_steps
));
```

### 3. Statistics and Reporting

```rust
struct ValidationStats {
    total_checked: usize,
    violations: usize,
    warnings: usize,
}

// Report in assertion
assert!(
    violations.is_empty(),
    "Validation failed:\n\n{}\n\nStats: {}",
    violations.join("\n\n"),
    stats
);
```

### 4. Helper Functions

```rust
// Extract common operations
fn read_file_or_panic(path: &Path, context: &str) -> String {
    // ...
}

fn validate_format(content: &str, format: Format) -> Vec<String> {
    // ...
}

fn format_violations(violations: &[Violation]) -> String {
    // ...
}
```

---

## Migration Strategy

### Phase 1: Add New Tests (Don't Remove Old)
1. Implement consolidated test
2. Run both old and new tests
3. Ensure new test catches all issues old tests catch

### Phase 2: Enhance New Test
1. Add missing validations
2. Improve error messages
3. Add statistics

### Phase 3: Remove Old Tests
1. Mark old tests with `#[ignore]`
2. Run CI for a week
3. Remove old tests if no issues

### Phase 4: Document
1. Update test documentation
2. Add examples to CLAUDE.md
3. Update contribution guide

---

## Testing the Consolidation

```bash
# Run specific test
cargo test test_msrv_consistency -- --nocapture

# Run all CI config tests
cargo test --test ci_config_tests

# Check coverage (optional)
cargo tarpaulin --test ci_config_tests
```

---

## Conclusion

These consolidation patterns provide:
- **Single source of truth** for validation rules
- **Easy extensibility** (just add to data structures)
- **Consistent error messages** across all tests
- **Better maintainability** (less duplication)
- **Comprehensive reporting** (statistics, context)

The examples show how to reduce 35 tests to 19 while improving quality and maintainability.
