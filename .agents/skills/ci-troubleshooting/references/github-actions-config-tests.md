# CI Configuration Validation Tests

**Applies to**: When adding tests that validate CI configuration files, MSRV consistency,
required workflows, or coding standards.

---

## When to Use

- Fixing a recurring CI configuration issue (add a test to prevent recurrence)
- Validating MSRV consistency across `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, and `Dockerfile`
- Ensuring required workflows exist
- Enforcing explicit action version refs, markdown linting, or script permissions via tests
- Detecting ecosystem mismatches (Python cache on Rust project) automatically

## When NOT to Use

- Runtime test failures (see [Testing Core Patterns](../../testing-rust/references/testing-core-patterns.md))
- Application code (see [Rust Idioms And Patterns](../../rust-development/references/rust-idioms-and-patterns.md))

## TL;DR

- Add a `tests/ci_config_tests.rs` test every time a CI configuration issue is fixed
- Tests run in `cargo test` — sub-second feedback before pushing
- Error messages must include fix instructions, not just failure descriptions
- Test configuration requirements (file exists, key present), not exact content
- See `tests/ci_config_tests.rs` for canonical implementations

---

## 1. The Pattern: Preventative Configuration Tests

Instead of waiting for CI to fail, use tests in `tests/ci_config_tests.rs` to validate
configuration consistency during local development.

**Benefits:**

- Catch issues during `cargo test` (before pushing) — fast feedback < 1 second
- Actionable error messages with fix instructions
- Once an issue is fixed, a test prevents recurrence
- Tests serve as executable documentation of configuration requirements

### When to Add a Configuration Test

Add a test whenever you:

1. Fix a CI configuration issue — prevent recurrence
2. Add a new configuration file — validate it exists and is correct
3. Establish a consistency requirement (MSRV across files, naming conventions)
4. Add a new required workflow — test that it exists
5. Add a coding standard — markdown linting, spell checking, script permissions

---

## 2. Example Tests

### MSRV Consistency Across Config Files

```rust
// tests/ci_config_tests.rs

#[test]
fn test_msrv_consistency_across_config_files() {
    // Single source of truth: Cargo.toml rust-version
    let msrv = extract_toml_version(&cargo_content, "rust-version");

    // Validate rust-toolchain.toml
    let toolchain_version = extract_yaml_version(&toolchain_content, "channel");
    assert_eq!(
        toolchain_version, msrv,
        "rust-toolchain.toml channel must match Cargo.toml rust-version.\n\
         Expected: {msrv}\n\
         Found: {toolchain_version}\n\
         Fix: Update rust-toolchain.toml to use channel = \"{msrv}\""
    );

    // Validate clippy.toml
    let clippy_msrv = extract_toml_version(&clippy_content, "msrv");
    assert_eq!(clippy_msrv, msrv, "clippy.toml msrv must match Cargo.toml rust-version");
}
```

### Required Workflows Exist

```rust
#[test]
fn test_required_ci_workflows_exist() {
    let required_workflows = vec![
        "ci.yml",
        "yaml-lint.yml",
        "actionlint.yml",
        "unused-deps.yml",
        "workflow-hygiene.yml",
    ];

    for workflow in required_workflows {
        assert!(
            workflows_dir.join(workflow).exists(),
            "Required workflow missing: {}", workflow
        );
    }
}
```

### No Ecosystem Cache Mismatch

```rust
#[test]
fn test_no_language_specific_cache_mismatch() {
    let root = repo_root();
    let is_rust_project = root.join("Cargo.toml").exists();

    // Detect requirements-*.txt variants (e.g., requirements-docs.txt for MkDocs)
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
    let pip_cache_re = regex::Regex::new(
        r#"(?m)^[ \t]*cache[ \t]*:[ \t]*['"]?pip['"]?(?:[ \t]*(?:#.*)?)?$"#,
    )
    .expect("valid pip cache regex");

    for workflow_file in workflow_files {
        let content = read_file(&workflow_file);
        if !is_python_project && is_rust_project && pip_cache_re.is_match(&content) {
            panic!("Python pip cache found in Rust project workflow: {}", workflow_file);
        }
    }
}
```

### Markdown Files Have Language Identifiers (MD040)

```rust
#[test]
fn test_markdown_files_have_language_identifiers() {
    for file in find_markdown_files() {
        for (line_num, line) in content.lines().enumerate() {
            if line.trim_start().starts_with("```") {
                let fence = line.trim_start().trim_start_matches('`').trim();
                assert!(
                    !fence.is_empty(),
                    "{}:{}: Missing language identifier on code fence (MD040)",
                    file.display(), line_num + 1
                );
            }
        }
    }
}
```

### Scripts Are Executable

```rust
#[test]
fn test_scripts_are_executable() {
    for script in find_scripts(&["scripts", ".githooks"]) {
        #[cfg(unix)]
        {
            let mode = metadata.permissions().mode();
            let is_executable = mode & 0o111 != 0;
            assert!(
                is_executable,
                "{} is not executable.\nFix: chmod +x {} && git update-index --chmod=+x {}",
                script.display(), script.display(), script.display()
            );
        }
    }
}
```

### Typos Config Exists and Is Valid

```rust
#[test]
fn test_typos_config_exists_and_is_valid() {
    assert!(
        Path::new(".typos.toml").exists(),
        ".typos.toml is required for spell checking"
    );
    let content = read_file(".typos.toml");
    assert!(
        content.contains("[default.extend-words]"),
        ".typos.toml must have [default.extend-words] section for technical term whitelisting"
    );
}
```

---

## 3. Error Message Best Practice

**Error messages must include fix instructions:**

```rust
// ✅ GOOD: Actionable error with fix
assert_eq!(
    toolchain_version, msrv,
    "rust-toolchain.toml channel must match Cargo.toml rust-version.\n\
     Expected: {msrv}\n\
     Found: {toolchain_version}\n\
     Fix: Update rust-toolchain.toml to use channel = \"{msrv}\""
);

// ❌ BAD: Cryptic failure with no guidance
assert_eq!(toolchain_version, msrv);
```

---

## 4. Test Configuration Requirements, Not Exact Content

```rust
// ✅ GOOD: Test configuration requirements
#[test]
fn test_markdownlint_config_exists() {
    assert!(Path::new(".markdownlint.json").exists());
}

// ❌ BAD: Test implementation details (too brittle)
#[test]
fn test_markdownlint_config_exact_content() {
    let content = read_file(".markdownlint.json");
    assert_eq!(content, r#"{"MD040": true}"#); // Breaks on any whitespace change
}
```

When validating optional elements, keep policy and implementation aligned:

- Do not assert a non-empty set unless policy explicitly requires presence.
- For style checks (for example, Shields badge query parameters), validate all discovered
  items and let empty sets pass by default.
- If presence is required, encode it as an explicit strict mode or separate assertion with clear wording.

---

## 4a. Live Config Presence Checks

Presence assertions against comment-bearing config must use the live
comment-stripped view so a commented-out required line cannot satisfy a drift
guard. Use [CI Config Live View Tests](./ci-config-live-view-tests.md) for the
`read_live_file` / `strip_comment_lines` convention and the locked regression
tests.

---

## 5. Running Configuration Tests

```bash
# Run only configuration tests (fast — no compilation)
cargo test --test ci_config_tests

# Run as part of full test suite
cargo test --all-features  # Includes ci_config_tests

# Run a specific test
cargo test --test ci_config_tests test_msrv_consistency_across_config_files
```

**Fast execution:** No external dependencies, no network calls, parallel test execution.
Total time for all configuration tests: < 1 second.

---

## Related References

- [GitHub Actions Caching](./github-actions-caching.md) — Ecosystem-specific caching, action ref policy
- [GitHub Actions Scheduled Workflows](./github-actions-scheduled-workflows.md) — Schedule guard validation
- [GitHub Actions Release](./github-actions-release.md) — Release preflight tests
- [CI Config Live View Tests](./ci-config-live-view-tests.md) — Presence assertions against active config only
- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Diagnosing CI failures
