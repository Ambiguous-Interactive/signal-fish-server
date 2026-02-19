# Skill: Test Fixture Patterns and Data-Driven CI Testing

<!--
  trigger: test fixture, data-driven test, ci config test, configuration testing, preventative testing
  | Creating test fixtures and data-driven tests for CI/CD validation
  | Testing
-->

**Trigger**: When creating test fixtures, writing configuration tests,
or implementing data-driven test patterns for CI/CD validation.

---

## When to Use

- Creating test fixtures for CI/CD workflows
- Writing configuration validation tests (MSRV, workflows, markdown)
- Implementing data-driven/table-driven tests
- Validating configuration file consistency
- Preventing CI/CD issues through proactive testing
- Testing that CI configurations match actual requirements

## When NOT to Use

- Application unit tests (see [testing-strategies](./testing-strategies.md))
- Integration tests (see [testing-tools-and-frameworks](./testing-tools-and-frameworks.md))
- Performance benchmarks (see [Rust Performance Optimization](./rust-performance-optimization.md))

---

## TL;DR

**Test Configuration Files, Not Just Code:**

- CI config tests catch issues during `cargo test` (before pushing)
- Data-driven patterns make tests easy to extend
- Test intent (MSRV consistency), not implementation details
- Clear error messages with fix instructions

**Fixture Organization:**

- Store fixtures in `.github/test-fixtures/` for workflow testing
- Document fixture purpose in README.md
- Use realistic examples that match actual use cases
- Version fixtures alongside code changes

---

## Test Fixture Patterns

### 1. Directory Structure

```text
.github/test-fixtures/
├── README.md                    # Purpose and usage documentation
├── workflows/
│   ├── valid-workflow.yml       # Example of correct configuration
│   ├── invalid-cache.yml        # Example of cache mismatch
│   └── missing-timeout.yml      # Example of missing timeout
├── config/
│   ├── valid-cargo.toml         # Correct MSRV configuration
│   └── invalid-cargo.toml       # MSRV mismatch example
└── markdown/
    ├── valid-example.md         # Properly formatted markdown
    └── missing-language.md      # MD040 violation example
```

### 2. Fixture Documentation

**Always include a README.md in test-fixtures:**

```markdown
# Test Fixtures

This directory contains test fixtures for validating CI/CD configuration.

## Purpose

These fixtures are used by `tests/ci_config_tests.rs` to validate:

- Workflow configuration patterns
- MSRV consistency across config files
- Markdown formatting requirements
- Link checking configuration

## Organization

- `workflows/` - GitHub Actions workflow examples
- `config/` - Configuration file examples (Cargo.toml, etc.)
- `markdown/` - Markdown formatting examples

## Usage

Tests reference these fixtures to validate detection of specific issues:

```rust

// Example: Test detects placeholder URLs
let fixture = read_fixture("workflows/invalid-placeholder.yml");
assert!(contains_placeholder_url(&fixture));

```

## Maintenance

- Keep fixtures minimal (only what's needed to demonstrate the pattern)
- Update fixtures when configuration format changes
- Document why each fixture exists (what issue it demonstrates)

### 3. Fixture Naming Convention

**Use descriptive names that indicate what's being tested:**

```text
✅ GOOD: Clear purpose
- valid-msrv-config.toml
- invalid-cache-mismatch.yml
- missing-language-identifier.md
- placeholder-url-example.yml

❌ BAD: Generic names
- test1.yml
- example.md
- config.toml
- fixture.yml
```

---

## Data-Driven CI Configuration Tests

### Pattern 1: Configuration Consistency Tests

**Test MSRV consistency across all config files:**

```rust
// tests/ci_config_tests.rs

#[test]
fn test_msrv_consistency_across_config_files() {
    // Single source of truth
    let cargo_content = read_file("Cargo.toml");
    let msrv = extract_toml_version(&cargo_content, "rust-version");

    // Validate all other files match
    let files_to_check = vec![
        ("rust-toolchain.toml", "channel"),
        ("clippy.toml", "msrv"),
        ("Dockerfile", "rust"),
    ];

    for (file, field) in files_to_check {
        let content = read_file(file);
        let version = extract_version(&content, field);

        assert_eq!(
            normalize_version(&version),
            normalize_version(&msrv),
            "{} {} must match Cargo.toml rust-version.\n\
             Expected: {}\n\
             Found: {}\n\
             Fix: Update {} to use {}",
            file, field, msrv, version, file, msrv
        );
    }
}
```

**Key Features:**

- Single source of truth (Cargo.toml)
- Version normalization handles Docker Hub format (1.88 vs 1.88.0)
- Clear error messages with fix instructions
- Tests intent (consistency), not specific values

### Pattern 2: Required Files/Workflows Tests

**Test that required CI workflows exist:**

```rust
#[test]
fn test_required_ci_workflows_exist() {
    let required_workflows = vec![
        ("ci.yml", "Main CI pipeline"),
        ("yaml-lint.yml", "YAML validation"),
        ("actionlint.yml", "GitHub Actions linting"),
        ("unused-deps.yml", "Dependency hygiene"),
        ("workflow-hygiene.yml", "Workflow validation"),
    ];

    for (workflow, description) in required_workflows {
        let path = Path::new(".github/workflows").join(workflow);
        assert!(
            path.exists(),
            "Required workflow missing: {} ({})\n\
             This workflow is required for: {}",
            workflow, path.display(), description
        );
    }
}
```

### Pattern 3: Data-Driven Pattern Validation

**Test placeholder URL exclusions:**

```rust
#[test]
fn test_lychee_excludes_placeholder_urls() {
    let lychee_content = read_file(".lychee.toml");

    // Data-driven test cases
    let test_cases = vec![
        ("http://localhost", "Localhost URLs are development placeholders"),
        ("https://github.com/owner/repo", "Generic GitHub placeholder pattern"),
        ("https://github.com/{}", "Template placeholder pattern"),
        ("https://example.com", "RFC 2606 example domain"),
    ];

    for (pattern, description) in test_cases {
        assert!(
            lychee_content.contains(pattern) || is_pattern_excluded(&lychee_content, pattern),
            ".lychee.toml must exclude placeholder URL: {} ({})\n\
             Add to exclude section:\n  \"{}\",",
            pattern, description, pattern
        );
    }
}
```

**Benefits:**

- Easy to add new test cases (just add to array)
- Self-documenting (description explains why)
- Clear failure messages (includes fix instructions)

### Pattern 4: Markdown Quality Tests

**Test code blocks have language identifiers:**

```rust
#[test]
fn test_markdown_files_have_language_identifiers() {
    let markdown_files = find_markdown_files(&repo_root());

    for file in markdown_files {
        let content = read_file(&file);

        for (line_num, line) in content.lines().enumerate() {
            if line.trim_start().starts_with("```") {
                let fence = line.trim_start().trim_start_matches('`').trim();

                assert!(
                    !fence.is_empty(),
                    "{}:{}: Code block missing language identifier (MD040)\n\
                     Add language after opening fence:\n\
                     - ```rust (for Rust code)\n\
                     - ```bash (for shell scripts)\n\
                     - ```json (for JSON data)\n\
                     - ```text (for plain text)",
                    file.display(),
                    line_num + 1
                );
            }
        }
    }
}
```

### Pattern 5: Configuration Format Validation

**Test typos configuration structure:**

```rust
#[test]
fn test_typos_config_exists_and_is_valid() {
    let typos_config = repo_root().join(".typos.toml");

    assert!(
        typos_config.exists(),
        ".typos.toml is required for spell checking in CI"
    );

    let content = read_file(&typos_config);

    // Validate required sections exist
    let required_sections = vec![
        "[default.extend-words]",
        "[default.extend-identifiers]",
    ];

    for section in required_sections {
        assert!(
            content.contains(section),
            ".typos.toml must have {} section.\n\
             extend-words: lowercase technical terms (e.g., tokio, axum)\n\
             extend-identifiers: mixed-case proper nouns (e.g., HashiCorp, GitHub)",
            section
        );
    }
}
```

---

## Helper Functions

### Version Extraction and Normalization

```rust
/// Extract version from TOML file
fn extract_toml_version(content: &str, field: &str) -> String {
    let pattern = format!(r#"{} = "([^"]+)""#, regex::escape(field));
    let re = regex::Regex::new(&pattern).unwrap();

    re.captures(content)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
        .expect(&format!("Field '{}' not found in TOML", field))
}

/// Normalize version for comparison (1.88.0 -> 1.88)
fn normalize_version(version: &str) -> String {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        version.to_string()
    }
}
```

### File Discovery

```rust
/// Find all markdown files, excluding build artifacts
fn find_markdown_files(root: &Path) -> Vec<PathBuf> {
    let exclude_patterns = vec!["target", ".git", "node_modules", "third_party"];

    WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            !exclude_patterns.iter().any(|p| e.path().to_string_lossy().contains(p))
        })
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "md")
                .unwrap_or(false)
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}
```

### Pattern Matching

```rust
/// Check if lychee config excludes a pattern
fn is_pattern_excluded(lychee_content: &str, pattern: &str) -> bool {
    // Simple check: look for pattern in exclude section
    // More robust: parse TOML and check exclude array
    let in_exclude_section = lychee_content
        .lines()
        .skip_while(|line| !line.contains("exclude = ["))
        .take_while(|line| !line.contains("]"))
        .any(|line| line.contains(pattern));

    in_exclude_section
}
```

---

## Error Message Design

### Good Error Messages

**Characteristics:**

1. **Explain what's wrong**: "MSRV mismatch between Cargo.toml and Dockerfile"
2. **Show expected vs actual**: "Expected: 1.88.0, Found: 1.87.0"
3. **Provide fix instructions**: "Fix: Update Dockerfile to use `rust:1.88.0-bookworm`"
4. **Explain why it matters**: "This ensures consistent Rust version across local and CI builds"

**Example:**

```rust
assert_eq!(
    dockerfile_version, msrv,
    "Dockerfile Rust version must match Cargo.toml rust-version.\n\
     Expected: {} (from Cargo.toml)\n\
     Found: {} (from Dockerfile)\n\
     \n\
     Fix: Update Dockerfile line 7:\n\
       FROM rust:{}-bookworm AS chef\n\
     \n\
     Why: This ensures consistent Rust version across Docker builds and local development",
    msrv, dockerfile_version, msrv
);
```

### Bad Error Messages

```rust
// ❌ BAD: Vague, no context
assert_eq!(version1, version2, "Versions don't match");

// ❌ BAD: No fix instructions
assert!(path.exists(), "File not found");

// ❌ BAD: Technical jargon without explanation
assert!(valid, "Predicate failed on invariant");
```

---

## Test Organization

### File Structure

```rust
// tests/ci_config_tests.rs

mod helpers {
    // Shared helper functions
    pub fn repo_root() -> PathBuf { ... }
    pub fn read_file(path: &str) -> String { ... }
    pub fn extract_toml_version(...) -> String { ... }
}

mod msrv_tests {
    use super::helpers::*;

    #[test]
    fn test_msrv_consistency_across_config_files() { ... }

    #[test]
    fn test_dockerfile_rust_version_matches_msrv() { ... }
}

mod workflow_tests {
    use super::helpers::*;

    #[test]
    fn test_required_workflows_exist() { ... }

    #[test]
    fn test_workflow_hygiene_requirements() { ... }
}

mod markdown_tests {
    use super::helpers::*;

    #[test]
    fn test_markdown_files_have_language_identifiers() { ... }
}
```

### Test Execution Order

**Fast tests first:**

```rust
// 1. Existence checks (< 1ms each)
#[test] fn test_config_files_exist() { ... }

// 2. Simple validation (< 10ms each)
#[test] fn test_msrv_consistency() { ... }

// 3. Content parsing (< 100ms each)
#[test] fn test_markdown_language_identifiers() { ... }

// 4. Complex validation (< 1s)
#[test] fn test_all_links_in_documentation() { ... }
```

---

## Integration with CI

### Local Development

```bash
# Run all CI config tests
cargo test --test ci_config_tests

# Run specific test module
cargo test --test ci_config_tests msrv_tests

# Run with verbose output
cargo test --test ci_config_tests -- --nocapture
```

### CI Pipeline

```yaml
# .github/workflows/ci.yml

jobs:
  config-tests:
    name: Configuration Tests
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<SHA>

      - name: Setup Rust
        uses: dtolnay/rust-toolchain@stable

      - name: Run CI config tests
        run: cargo test --test ci_config_tests
        # These tests validate:
        # - MSRV consistency across all config files
        # - Required workflows exist
        # - Markdown formatting (MD040 compliance)
        # - Spell checker configuration
        # - Link checker configuration
```

---

## Real-World Example: Signal Fish Server

### Problem: Multiple CI Failures

**Issues encountered:**

1. Lychee tried to check placeholder URLs (`https://github.com/owner/repo`)
2. Markdown files had code blocks without language identifiers (MD040 violations)
3. MSRV mismatch between Cargo.toml (1.88.0) and Dockerfile (1.87.0)
4. Typos config didn't whitelist "HashiCorp" (mixed-case company name)

### Solution: Comprehensive CI Config Tests

**Created 31 tests in `tests/ci_config_tests.rs`:**

```rust
// 1. Link checking (5 tests)
test_lychee_config_exists_and_is_valid()
test_lychee_excludes_placeholder_urls()
test_no_actual_placeholder_urls_in_docs()
test_link_check_workflow_uses_lychee_config()
test_lychee_config_format_is_valid_toml()

// 2. Markdown linting (5 tests)
test_markdown_files_have_language_identifiers()
test_markdown_no_capitalized_filenames_in_links()
test_markdown_technical_terms_consistency()
test_markdown_common_patterns_are_correct()
test_markdown_config_exists()

// 3. CI workflows (4 tests)
test_link_check_workflow_exists_and_is_configured()
test_markdownlint_workflow_exists_and_is_configured()
test_doc_validation_workflow_has_shellcheck()
test_workflow_hygiene_requirements()

// 4. Configuration consistency (15 tests)
test_msrv_consistency_across_config_files()
test_typos_config_exists_and_is_valid()
test_required_ci_workflows_exist()
test_scripts_are_executable()
// ... and more
```

**Results:**

- All 31 tests pass in < 1 second
- Catches entire categories of issues (not just specific bugs)
- Clear error messages with fix instructions
- Easy to extend with new test cases

---

## Best Practices

### 1. Test Intent, Not Implementation

```rust
// ✅ GOOD: Tests that MSRV is consistent (intent)
assert_eq!(
    normalize_version(&dockerfile_version),
    normalize_version(&cargo_version),
    "Versions must match"
);

// ❌ BAD: Tests specific version value (implementation)
assert_eq!(cargo_version, "1.88.0", "Must use 1.88.0");
```

### 2. Make Tests Easy to Extend

```rust
// ✅ GOOD: Data-driven, easy to add new cases
let test_cases = vec![
    ("localhost", "Development placeholder"),
    ("example.com", "RFC 2606 domain"),
];

for (pattern, description) in test_cases {
    assert!(is_excluded(pattern), "{}", description);
}

// ❌ BAD: Hardcoded, requires copy-paste for new cases
assert!(is_excluded("localhost"), "localhost should be excluded");
assert!(is_excluded("example.com"), "example.com should be excluded");
```

### 3. Provide Actionable Error Messages

```rust
// ✅ GOOD: Clear fix instructions
assert!(
    path.exists(),
    "{} is missing.\n\
     Create it with: touch {}\n\
     Then add it to git: git add {}",
    path.display(), path.display(), path.display()
);

// ❌ BAD: Vague error
assert!(path.exists(), "File not found");
```

### 4. Keep Tests Fast

**Target execution times:**

- Individual test: < 10ms
- Full test suite: < 1 second
- No network calls (use fixtures or offline mode)
- No external tools (pure Rust file reading)

### 5. Document Test Purpose

```rust
/// Tests that all code blocks in markdown files have language identifiers.
///
/// This prevents MD040 violations which cause markdownlint to fail in CI.
/// Language identifiers enable proper syntax highlighting and are required
/// for documentation quality.
///
/// Example violations:
/// - ` ```\n code here\n``` ` (missing language)
///
/// Example fixes:
/// - ` ```rust\n code here\n``` ` (correct)
#[test]
fn test_markdown_files_have_language_identifiers() { ... }
```

---

## Prevention Checklist

Before committing new configuration tests:

- [ ] Test validates intent (consistency), not specific values
- [ ] Error messages include fix instructions
- [ ] Test is data-driven (easy to add new cases)
- [ ] Test executes in < 10ms (no external tools)
- [ ] Test has documentation comment explaining purpose
- [ ] Test is organized in appropriate module
- [ ] Test failure messages are clear and actionable
- [ ] Test covers both positive and negative cases

---

## Related Skills

- [testing-strategies](./testing-strategies.md) — Core testing methodology and patterns
- [testing-tools-and-frameworks](./testing-tools-and-frameworks.md) — Testing tools and coverage
- [`github-actions-best-practices`](./github-actions-best-practices.md) — CI/CD workflow patterns
- [ci-cd-troubleshooting](./ci-cd-troubleshooting.md) — Diagnosing CI failures
- [markdown-best-practices](./markdown-best-practices.md) — Markdown documentation standards

---

## Summary

**Test fixtures and data-driven CI tests prevent entire categories of issues:**

1. **Configuration consistency** - MSRV, version formats, file existence
2. **Documentation quality** - Markdown formatting, link validity, spell checking
3. **Workflow validation** - Required jobs, timeouts, permissions
4. **Development experience** - Fast feedback, clear errors, easy to extend

**Key patterns:**

- Store fixtures in `.github/test-fixtures/` with documentation
- Use data-driven tests for easy extension
- Test intent (consistency), not implementation (specific values)
- Provide clear error messages with fix instructions
- Keep tests fast (< 1 second total)
- Integrate with CI pipeline and pre-commit hooks
