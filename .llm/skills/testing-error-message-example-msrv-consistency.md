# Skill: Testing Error Message Example - MSRV Consistency Assertion

<!--
  trigger: test error message example msrv, msrv assertion message example
  | Example test failure message for MSRV consistency checks
  | Core
-->

**Trigger**: When writing or reviewing assertion failure text for toolchain/MSRV consistency tests.

---

## Incident Pattern

Mismatch between Dockerfile Rust version and `Cargo.toml` `rust-version` can cause
CI/build drift and confusing failures.

---

## Example Assertion

From [tests/ci_config_tests.rs](../../tests/ci_config_tests.rs):

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

---

## Why This Works

- States exactly what invariant failed
- Includes expected and actual values
- Adds domain context (Docker Hub version format)
- Provides copy-paste remediation

---

## Related Skills

- [Testing Error Message Quality](./testing-error-message-quality.md) — Error message design guidelines
- [MSRV Management](./msrv-management.md) — MSRV policy and consistency rules
- [Testing Core Patterns](./testing-core-patterns.md) — General testing patterns
