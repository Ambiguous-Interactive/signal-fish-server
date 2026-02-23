# Skill: CI Configuration Test Patterns

<!--
  trigger: ci config test, configuration testing, preventative testing, msrv test, workflow test, placeholder url
  | Data-driven test patterns for CI/CD configuration validation
  | Testing
-->

**Trigger**: When writing data-driven tests that validate CI/CD configuration consistency,
workflow existence, or tool configuration.

---

## When to Use

- Writing configuration validation tests (MSRV, workflows, markdown)
- Implementing data-driven/table-driven CI config tests
- Validating configuration file consistency
- Preventing CI/CD issues through proactive testing

## When NOT to Use

- Fixture organization and structure (see [test-fixture-structure](./test-fixture-structure.md))
- Application unit tests (see [testing-core-patterns](./testing-core-patterns.md))
- CI smoke tests (see [testing-ci-coverage](./testing-ci-coverage.md))

---

## TL;DR

- CI config tests catch issues during `cargo test` (before pushing)
- Test intent (MSRV consistency), not implementation details (specific version values)
- Data-driven patterns make tests easy to extend — just add to the array
- Clear error messages with fix instructions

---

## Pattern 1: Configuration Consistency Tests

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

---

## Pattern 2: Required Files/Workflows Tests

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

---

## Pattern 3: Data-Driven Pattern Validation

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

---

## Pattern 4: Markdown Quality Tests

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

---

## Pattern 5: Configuration Format Validation

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

## Best Practices

### 1. Test Intent, Not Implementation

```rust
// GOOD: Tests that MSRV is consistent (intent)
assert_eq!(
    normalize_version(&dockerfile_version),
    normalize_version(&cargo_version),
    "Versions must match"
);

// BAD: Tests specific version value (implementation)
assert_eq!(cargo_version, "1.88.0", "Must use 1.88.0");
```

### 2. Make Tests Easy to Extend

```rust
// GOOD: Data-driven, easy to add new cases
let test_cases = vec![
    ("localhost", "Development placeholder"),
    ("example.com", "RFC 2606 domain"),
];

for (pattern, description) in test_cases {
    assert!(is_excluded(pattern), "{}", description);
}

// BAD: Hardcoded, requires copy-paste for new cases
assert!(is_excluded("localhost"), "localhost should be excluded");
assert!(is_excluded("example.com"), "example.com should be excluded");
```

### 3. Keep Tests Fast

**Target execution times:**

- Individual test: < 10ms
- Full test suite: < 1 second
- No network calls (use fixtures or offline mode)
- No external tools (pure Rust file reading)

---

## Prevention Checklist

Before committing new configuration tests:

- [ ] Test validates intent (consistency), not specific values
- [ ] Error messages include fix instructions
- [ ] Test is data-driven (easy to add new cases)
- [ ] Test executes in < 10ms (no external tools)
- [ ] Test has documentation comment explaining purpose
- [ ] Test is organized in appropriate module
- [ ] Test covers both positive and negative cases

---

## Related Skills

- [test-fixture-structure](./test-fixture-structure.md) — Fixture directory layout, naming, and documentation
- [testing-core-patterns](./testing-core-patterns.md) — Core testing methodology and patterns
- [testing-error-message-quality](./testing-error-message-quality.md) — Actionable failure messages
- [GitHub-actions-best-practices](./github-actions-workflow-config.md) — CI/CD workflow patterns
- [ci-cd-troubleshooting](./ci-cd-troubleshooting-categories.md) — Diagnosing CI failures
- [markdown-best-practices](./markdown-best-practices-formatting.md) — Markdown documentation standards
