# Skill: Test Fixture Structure and Organization

<!--
  trigger: test fixture, fixture organization, fixture naming, fixture documentation
  | Organizing and documenting test fixtures for CI/CD validation
  | Testing
-->

**Trigger**: When creating or organizing test fixtures for CI/CD workflow or configuration testing.

---

## When to Use

- Creating test fixtures for CI/CD workflows
- Organizing fixture directories and files
- Documenting what each fixture tests
- Naming fixture files descriptively

## When NOT to Use

- Application unit tests (see [testing-core-patterns](./testing-core-patterns.md))
- Writing the data-driven test patterns (see [test-fixture-ci-patterns](./test-fixture-ci-patterns.md))
- Performance benchmarks (see [Rust Performance Optimization](./rust-performance-optimization.md))

---

## TL;DR

- Store fixtures in `.github/test-fixtures/` with a README.md
- Use descriptive names that indicate what issue is demonstrated
- Document each fixture's purpose and the test that uses it
- Keep fixtures minimal — only what demonstrates the pattern

---

## Directory Structure

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

---

## Fixture Documentation

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
```

---

## Fixture Naming Convention

**Use descriptive names that indicate what's being tested:**

```text
GOOD: Clear purpose
- valid-msrv-config.toml
- invalid-cache-mismatch.yml
- missing-language-identifier.md
- placeholder-url-example.yml

BAD: Generic names
- test1.yml
- example.md
- config.toml
- fixture.yml
```

---

## Helper Functions for Fixture Use

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

## Test Organization by Module

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

---

## Test Execution Order

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

## Related Skills

- [test-fixture-ci-patterns](./test-fixture-ci-patterns.md) — Data-driven CI configuration test patterns
- [testing-core-patterns](./testing-core-patterns.md) — Core testing methodology and patterns
- [testing-error-message-quality](./testing-error-message-quality.md) — Actionable test failure messages
- [GitHub-actions-best-practices](./github-actions-workflow-config.md) — CI/CD workflow patterns
