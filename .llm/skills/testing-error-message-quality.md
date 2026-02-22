# Skill: Testing Error Message Quality

<!--
  trigger: test error message, assert message, test failure, actionable error, collect errors
  | Writing clear, actionable test failure messages
  | Core
-->

**Trigger**: When writing test assertions that need to produce helpful failure output.

---

## When to Use

- Writing `assert!`, `assert_eq!`, or `panic!` in tests
- Designing test failure messages for complex validation logic
- Making test failures self-diagnosing without needing to re-run with `--nocapture`
- Collecting multiple errors before failing

---

## When NOT to Use

- Core test patterns (see [testing-core-patterns](./testing-core-patterns.md))
- CI/CD config validation tests (see [testing-ci-coverage](./testing-ci-coverage.md))

---

## TL;DR

- Every test failure message must include: **what**, **why**, **expected**, **actual**, **how to fix**.
- Collect ALL errors before failing — never stop at the first one.
- Include copy-paste fix commands in the error message.
- Show before/after examples for complex fixes.

---

## The Problem: Unhelpful Test Failures

Test failures without context are hard to debug:

```rust
// BAD: Cryptic failure message
#[test]
fn test_msrv_consistency() {
    assert_eq!(dockerfile_version, cargo_version);
    // Failure: assertion failed: `(left == right)`
    // left: `1.88`, right: `1.88.0`
    // What do I do to fix this?
}
```

**Issues:**

- No explanation of what's being tested
- No guidance on how to fix the failure
- No context about expected vs actual values
- Developer wastes time investigating

---

## The Solution: Actionable Error Messages

```rust
// GOOD: Clear, actionable failure message
#[test]
fn test_msrv_consistency_across_config_files() {
    let msrv = extract_cargo_rust_version(&cargo_content);
    let dockerfile_version = extract_dockerfile_rust_version(&dockerfile_content);

    // Normalize versions for comparison (1.88.0 -> 1.88)
    let msrv_short = normalize_version(&msrv);
    let dockerfile_short = normalize_version(&dockerfile_version);

    assert_eq!(
        dockerfile_short, msrv_short,
        "Dockerfile Rust version must match Cargo.toml rust-version.\n\
         Expected: {} (from Cargo.toml)\n\
         Found: {} (from Dockerfile)\n\
         Note: Docker Hub uses X.Y format (e.g., 1.88, not 1.88.0)\n\
         Fix: Update Dockerfile line 7 to: FROM rust:{}-bookworm",
        msrv_short, dockerfile_short, msrv_short
    );
}
```

**Benefits:**

- Explains what's being validated
- Shows expected vs actual clearly
- Provides copy-paste fix instructions
- Includes contextual notes about why the check exists

---

## Pattern: Collect All Errors Before Failing

```rust
// BAD: Fails on first error (hides other issues)
#[test]
fn test_all_config_files_consistent() {
    assert_eq!(toolchain_version, msrv, "toolchain mismatch");
    assert_eq!(clippy_version, msrv, "clippy mismatch");
    assert_eq!(dockerfile_version, msrv, "dockerfile mismatch");
    // Only see first failure, have to run test 3 times to fix all
}

// GOOD: Collects all errors, shows complete picture
#[test]
fn test_all_config_files_consistent() {
    let msrv = extract_cargo_rust_version(&cargo_content);
    let mut errors = Vec::new();

    // Check rust-toolchain.toml
    if toolchain_version != msrv {
        errors.push(format!(
            "rust-toolchain.toml: expected {}, found {}",
            msrv, toolchain_version
        ));
    }

    // Check clippy.toml
    if clippy_version != msrv {
        errors.push(format!(
            "clippy.toml: expected {}, found {}",
            msrv, clippy_version
        ));
    }

    // Check Dockerfile
    if normalize_version(&dockerfile_version) != normalize_version(&msrv) {
        errors.push(format!(
            "Dockerfile: expected {}, found {}",
            normalize_version(&msrv), normalize_version(&dockerfile_version)
        ));
    }

    if !errors.is_empty() {
        panic!(
            "MSRV consistency check failed:\n\n{}\n\n\
             Fix: Update all files to match Cargo.toml rust-version = \"{}\"",
            errors.join("\n"),
            msrv
        );
    }
}
```

---

## Pattern: Include Examples in Error Messages

```rust
#[test]
fn test_github_actions_sha_pinning() {
    let mut unpinned_actions = Vec::new();

    for workflow_file in find_workflow_files() {
        let content = read_file(&workflow_file);

        for (line_num, line) in content.lines().enumerate() {
            if let Some(action) = extract_action_ref(line) {
                if !is_sha_pinned(&action) {
                    unpinned_actions.push(format!(
                        "{}:{}: {}",
                        workflow_file.display(),
                        line_num + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        unpinned_actions.is_empty(),
        "GitHub Actions must be pinned to SHA hashes for security and reproducibility.\n\n\
         Unpinned actions found:\n{}\n\n\
         Example fix:\n\
         WRONG: uses: actions/checkout@v4\n\
         CORRECT: uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2\n\n\
         Get SHA: gh api repos/actions/checkout/commits/v4.2.2 --jq .sha",
        unpinned_actions.join("\n")
    );
}
```

---

## Real-World Example: MSRV Consistency Test

From `/workspaces/signal-fish-server/tests/ci_config_tests.rs`:

```rust
#[test]
fn test_dockerfile_rust_version_matches_msrv() {
    let dockerfile = read_file("Dockerfile");
    let cargo_toml = read_file("Cargo.toml");

    let dockerfile_version = extract_dockerfile_rust_version(&dockerfile);
    let cargo_version = extract_cargo_rust_version(&cargo_toml);

    // Normalize to X.Y format for comparison
    let normalized_dockerfile = normalize_version(&dockerfile_version);
    let normalized_cargo = normalize_version(&cargo_version);

    assert_eq!(
        normalized_dockerfile, normalized_cargo,
        "Dockerfile Rust version must match Cargo.toml rust-version.\n\
         Expected: {} (from Cargo.toml)\n\
         Found: {} (from Dockerfile)\n\
         Note: Docker Hub uses X.Y format (e.g., 1.88, not 1.88.0)\n\
         Fix: Update Dockerfile to use Rust:{}-bookworm",
        normalized_cargo, normalized_dockerfile, normalized_cargo
    );
}
```

**This error message includes:**

1. Clear description of what's being tested
2. Expected vs actual values
3. Contextual note about Docker Hub version format
4. Copy-paste fix instruction with exact line to change

---

## Error Message Checklist

Every test failure message should include:

- [ ] **What** is being tested (e.g., "MSRV consistency across config files")
- [ ] **Why** the test failed (e.g., "Dockerfile version doesn't match Cargo.toml")
- [ ] **Expected** value (e.g., "Expected: 1.88")
- [ ] **Actual** value (e.g., "Found: 1.87")
- [ ] **How** to fix (e.g., "Fix: Update Dockerfile line 7 to: `FROM rust:1.88-bookworm`")
- [ ] **Context** if needed (e.g., "Note: Docker Hub uses X.Y format, not X.Y.Z")
- [ ] **Examples** for complex fixes (e.g., show before/after)

---

## Related Skills

- [testing-core-patterns](./testing-core-patterns.md) — Core test patterns (unit, async, table-driven)
- [testing-ci-coverage](./testing-ci-coverage.md) — CI/CD smoke tests and config validation tests
- [test-fixture-patterns](./test-fixture-structure.md) — Data-driven CI configuration tests
