# ADR: MSRV Consistency Enforcement

**Status**: Accepted

**Date**: 2026-02-16

**Context**: Recent CI/CD failures were caused by MSRV (Minimum Supported Rust Version)
inconsistencies between configuration files. When the Rust toolchain was updated from 1.87.0
to 1.88.0, multiple files needed coordinated updates, and missing updates caused CI failures.

**Decision**: Implement automated MSRV consistency enforcement at multiple levels to prevent
future toolchain-related CI/CD issues.

---

## Problem Statement

The project defines its MSRV in multiple configuration files:

- `Cargo.toml` (`rust-version` field) - Cargo's MSRV metadata
- `rust-toolchain.toml` (channel field) - Developer toolchain pinning
- `clippy.toml` (msrv field) - MSRV-aware linting
<!-- markdownlint-disable-next-line MD044 -- rust:X.Y is a Docker image name -->
- `Dockerfile` (`FROM rust:X.Y`) - Production build environment
- `.devcontainer/Dockerfile` - Development environment

When updating the MSRV, all files must be updated consistently. Manual updates are error-prone
and can cause:

- CI failures due to mismatched Rust versions
- Dependency incompatibilities (new dep requires newer Rust than specified MSRV)
- Inconsistent behavior between local development and CI
- Confusing error messages when builds fail

---

## Solution

### 1. Single Source of Truth

`Cargo.toml` `rust-version` field is the canonical MSRV definition. All other files derive
their Rust version from this field.

**Rationale**: Cargo's native MSRV field provides:

- Standard metadata for crates.io
- Validation when dependencies require newer Rust
- Integration with cargo-msrv tooling

### 2. Automated Consistency Validation

Implemented multi-layered validation:

#### CI Job: `msrv` (`.github/workflows/ci.yml`)

New dedicated CI job that:

1. Extracts MSRV from `Cargo.toml` (single source of truth)
2. Validates consistency across all configuration files
3. Compiles the project with exact MSRV version
4. Runs all tests with MSRV to ensure compatibility

**Impact**: Catches configuration drift and dependency incompatibilities on every PR.

#### Pre-commit Script: `scripts/check-msrv-consistency.sh`

Standalone script that:

- Verifies all configuration files match `Cargo.toml` MSRV
- Provides actionable error messages with fix instructions
- Can be run locally before committing
- Can be integrated into git hooks

**Usage**:

```bash
./scripts/check-msrv-consistency.sh

```

### 3. LLM Agent Guidance

Created comprehensive skill: `.llm/skills/msrv-and-toolchain-management.md`

This skill provides AI agents with:

- MSRV update checklist (all files that must be updated)
- Common pitfall detection (e.g., updating only Cargo.toml)
- Verification procedures before committing
- Troubleshooting guidance for MSRV-related errors
- Examples of proper commit messages

**Rationale**: AI agents frequently contribute to this codebase. Explicit guidance prevents
common mistakes and ensures consistent MSRV management practices.

### 4. Updated Existing Skills

Enhanced related skills with MSRV awareness:

**`dependency-management.md`**:

- Added MSRV compatibility check as first priority
- Documented options when dependency requires newer Rust
- Added MSRV verification to agent checklist

**`mandatory-workflow.md`**:

- Added `check-msrv-consistency.sh` to pre-push validation
- Updated PR checklist with MSRV verification step
- Documented MSRV update commit format

**`context.md`**:

- Added MSRV skill to quick decision tree
- Included in Security & Infrastructure skills table

### 5. Developer Documentation

Updated `docs/development.md` with:

- MSRV verification procedures
- Step-by-step MSRV update checklist
- Testing instructions for new MSRV
- CI integration details

---

## Configuration File Responsibilities

| File                        | Purpose                          | MSRV Policy                          |
| --------------------------- | -------------------------------- | ------------------------------------ |
| `Cargo.toml`                | MSRV source of truth             | Canonical definition                 |
| `rust-toolchain.toml`       | Developer toolchain pinning      | Must match Cargo.toml exactly        |
| `clippy.toml`               | Clippy MSRV-aware lints          | Must match Cargo.toml exactly        |
| `Dockerfile`                | Production build environment     | Must match Cargo.toml exactly        |
| `.devcontainer/Dockerfile`  | Development container            | May use newer Rust (CI enforces MSRV)|
| `.github/dependabot.yml`    | Dependency update policy         | Documents MSRV policy in comments    |

---

## MSRV Update Process

When a dependency requires a newer Rust version:

1. **Evaluate necessity**: Can we pin an older dependency version? Use alternatives?
2. **Update all files**: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, Dockerfile
3. **Verify consistency**: Run `./scripts/check-msrv-consistency.sh`
4. **Test thoroughly**: `cargo clean && cargo test --all-features`
5. **Document**: Update CHANGELOG.md and commit message

**Commit message format**:

```text
chore: update MSRV from 1.87.0 to 1.88.0

Update minimum supported Rust version to 1.88.0 to support the rand 0.10
dependency update. This change ensures compatibility with the latest stable
Rust ecosystem dependencies.

Changes:

- Update rust-version in Cargo.toml to 1.88.0
- Update rust-toolchain.toml to enforce Rust 1.88.0
- Update clippy.toml MSRV configuration to 1.88.0
- Update Dockerfile base image from rust:1.87 to rust:1.88
- Update documentation (README.md, docs/development.md)

Testing:

- All tests passing (cargo test --all-features)
- CI MSRV verification job passes


```

---

## CI Validation Details

The `msrv` CI job runs on every push and pull request:

**Steps**:

1. Extract MSRV from `Cargo.toml` using `grep` and `sed`
2. Validate `rust-toolchain.toml` channel matches (exact match: `1.88.0`)
3. Validate clippy.toml msrv matches (exact match: `1.88.0`)
4. Validate Dockerfile Rust version matches (normalized comparison: `1.88` ↔ `1.88.0`)
5. Install exact MSRV toolchain via dtolnay/Rust-toolchain
6. Build with `cargo check --locked --all-targets`
7. Test with `cargo test --locked --all-features`

**Version Format Normalization**:

Docker images use shortened version tags (`rust:1.88-bookworm`) while Cargo.toml uses
full semantic versioning (`1.88.0`). The CI validation normalizes both formats to
major.minor for comparison to avoid false failures.

**Why normalize instead of requiring exact format?**

1. **Docker Hub convention**: Official Rust images use `rust:1.88` not `rust:1.88.0`
2. **Automatic updates**: Docker tags like `rust:1.88` automatically pull latest patch (1.88.x)
3. **Semantic equivalence**: `1.88` and `1.88.0` refer to the same Rust version
4. **Maintenance burden**: Requiring `rust:1.88.0` forces manual updates for patch releases

The normalization logic:

```bash
# Extract major.minor from Dockerfile (already in 1.88 format)
DOCKERFILE_RUST=$(grep '^FROM rust:' Dockerfile | sed -E 's/FROM rust:([0-9]+\.[0-9]+).*/\1/')

# Normalize Cargo.toml MSRV from 1.88.0 to 1.88
MSRV_SHORT=$(echo "$MSRV" | sed -E 's/([0-9]+\.[0-9]+).*/\1/')

# Compare normalized versions
if [ "$DOCKERFILE_RUST" != "$MSRV_SHORT" ]; then
  echo "FAIL"
fi

```

**Failure scenarios**:

- Configuration drift: Files have different versions → Clear error with fix instructions
- Dependency incompatibility: Dep requires newer Rust than MSRV → Build fails with error
- Code uses features from newer Rust → Build fails with feature stability error

---

## Benefits

### Immediate

- **Prevent CI failures**: Catch MSRV inconsistencies before merge
- **Clear error messages**: Developers know exactly which files to update
- **Automated validation**: No manual verification needed
- **Fast feedback**: Local script runs in seconds

### Long-term

- **Reproducible builds**: Everyone uses same Rust version in CI and production
- **Dependency safety**: Can't accidentally pull in deps requiring newer Rust
- **AI agent reliability**: Explicit guidance prevents common mistakes
- **Onboarding**: New contributors have clear MSRV update procedures

---

## Alternatives Considered

### Alternative 1: Manual Reviews

**Rejected**: Error-prone, doesn't scale, easy to miss in code review.

### Alternative 2: Single `rust-toolchain.toml` (no `Cargo.toml` `rust-version`)

**Rejected**: Loses Cargo's native MSRV validation and crates.io metadata.

### Alternative 3: Script to Auto-Update All Files

**Rejected**: Could mask understanding of MSRV impact; prefer explicit manual updates
with validation.

### Alternative 4: Dependabot Auto-MSRV Updates

**Rejected**: MSRV updates should be deliberate, coordinated decisions (not automatic).
Current policy: Dependabot ignores Rust version updates; manual MSRV bumps only.

---

## Future Enhancements

### Considered for Future

1. **Pre-commit hook**: Automatically run `check-msrv-consistency.sh` on git commit
   - Pro: Catches issues even earlier
   - Con: May slow down commits; opt-in via `scripts/enable-hooks.sh`

2. **cargo-msrv integration**: Use `cargo-msrv` to automatically determine minimum

   required Rust version

   - Pro: Finds true minimum version (may be lower than current MSRV)
   - Con: Slow on large codebases; better as manual tool

3. **CHANGELOG automation**: Auto-update CHANGELOG.md when MSRV changes
   - Pro: Never forget to document MSRV bumps
   - Con: May be too opinionated; prefer manual CHANGELOG entries

---

## Related ADRs

- (None yet - this is the first infrastructure ADR)

---

## References

- [Cargo Book: `rust-version` field](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field)
- [Rust RFC 2495: Minimum Supported Rust Version](https://rust-lang.github.io/rfcs/2495-min-rust-version.html)
- [cargo-msrv tool](https://github.com/foresterre/cargo-msrv)
- GitHub Issue: (Link to original CI failure issue once created)
