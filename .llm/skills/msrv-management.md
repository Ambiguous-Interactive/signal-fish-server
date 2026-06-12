# Skill: MSRV Management

<!--
  trigger: msrv, rust-version, minimum supported Rust version, cargo msrv, dependency incompatibility
  | Managing the Minimum Supported Rust Version and keeping all config files consistent
  | Infrastructure
-->

**Trigger**: When updating MSRV, adding dependencies that require newer Rust, or debugging MSRV
consistency failures in CI.

See also:

- [Toolchain Pinning](./toolchain-pinning.md) — `rust-toolchain.toml`, MSRV testing, developer workflow
- [Toolchain Nightly](./toolchain-nightly.md) — Nightly Rust for CI-only analysis tools

---

## TL;DR

- **MSRV is defined ONCE** in `Cargo.toml` (`rust-version` field) — this is the single source of truth
- **All other files must match**: `rust-toolchain.toml`, `clippy.toml`, `Dockerfile`,
  `clients/native/Cargo.toml` (the reference client pins the root MSRV), `.devcontainer/Dockerfile`
- **CI enforces consistency** with a dedicated MSRV verification job
- **Before updating MSRV**: Check all dependencies support the new version (`cargo msrv`)
- **MSRV updates are coordinated changes** affecting multiple files — use the checklist below

---

## What is MSRV?

**Minimum Supported Rust Version (MSRV)** is the oldest Rust compiler version that can build your project.
It's a contract with users and CI environments about toolchain requirements.

**Why MSRV Matters:**

- **Reproducible builds**: Everyone uses the same Rust version in CI and production
- **Dependency compatibility**: Prevents pulling in dependencies that require newer Rust
- **Security**: Enables use of newer dependencies with security fixes
- **Developer experience**: Clear requirements for contributors

**MSRV Policy:**

- MSRV is explicitly defined in `Cargo.toml` (`rust-version = "1.88.0"`)
- Production builds (Dockerfile) **MUST** match MSRV exactly
- Development environments (devcontainer) **MAY** use newer Rust for better tooling
- CI validates MSRV compliance on every PR
- MSRV bumps are deliberate, versioned decisions (not automatic)

---

## MSRV Single Source of Truth: Cargo.toml

```toml
# Cargo.toml — THE authoritative MSRV definition
[package]
name = "signal-fish-server"
rust-version = "1.88.0"  # ← Single source of truth
```

**All other configuration files derive their Rust version from this field.**

---

## Configuration Files That Must Match MSRV

| File | Purpose | Format | CI Validated? |
|------|---------|--------|--------------|
| `Cargo.toml` | MSRV source of truth | `rust-version = "1.88.0"` (full semver) | Yes |
| `rust-toolchain.toml` | Developer toolchain pinning | `channel = "1.88.0"` (full semver) | Yes |
| `clippy.toml` | Clippy MSRV-aware lints | `msrv = "1.88.0"` (full semver) | Yes |
| `Dockerfile` | Production build environment | `FROM rust:1.88-bookworm` (Docker format) | Yes (normalized) |
| `clients/native/Cargo.toml` | Reference client MSRV pin (ADR-0004) | `rust-version = "1.88.0"` (full semver) | Yes |
| `.devcontainer/Dockerfile` | Dev container (optional) | Comment or use MS base image | Optional |
| `.github/dependabot.yml` | Dependency update policy | MSRV in comments | No |
| `README.md` | User-facing documentation | Full semver in requirements section | No |
| `docs/development.md` | Developer setup guide | Full semver in setup steps | No |

---

## Docker Version Format: Why `1.88` Instead of `1.88.0`

The Dockerfile uses `rust:1.88` (major.minor) instead of `rust:1.88.0` (full semver).
This is **intentional** and follows Docker Hub conventions.

**Why Docker uses shortened versions:**

1. Docker Hub convention: Official Rust images use `rust:1.88` not `rust:1.88.0`
2. `rust:1.88` automatically pulls the latest patch (1.88.x)
3. `1.88` and `1.88.0` refer to the same Rust minor version

**CI Normalization Logic:**

```bash
# Cargo.toml has: rust-version = "1.88.0"
MSRV="1.88.0"
DOCKERFILE_RUST="1.88"   # FROM rust:1.88-bookworm

# Normalize MSRV to major.minor (1.88.0 → 1.88)
MSRV_SHORT=$(echo "$MSRV" | sed -E 's/([0-9]+\.[0-9]+).*/\1/')

# Compare: "1.88" == "1.88" ✓
```

**Rules:**

- **Cargo.toml**: Always use full semver (`1.88.0`)
- **Dockerfile**: Use Docker format (`1.88-bookworm`) — `rust:1.88.0-bookworm` is not a valid tag

---

## MSRV Verification in CI

The `.github/workflows/ci.yml` includes a dedicated `msrv` job that:

1. **Extracts MSRV** from `Cargo.toml` (single source of truth)
2. **Validates consistency** across all configuration files
3. **Compiles the project** with the exact MSRV version
4. **Runs tests** to ensure compatibility

```yaml
# .github/workflows/ci.yml
jobs:
  msrv:
    name: MSRV Verification
    runs-on: ubuntu-latest
    steps:
      - name: Extract MSRV from Cargo.toml
        id: msrv
        run: |
          MSRV=$(bash scripts/read-toml-string.sh Cargo.toml rust-version package)
          echo "msrv=$MSRV" >> "$GITHUB_OUTPUT"
      - name: Install Rust at MSRV
        uses: dtolnay/rust-toolchain@...
        with:
          toolchain: ${{ steps.msrv.outputs.msrv }}
      - name: Verify build and tests with MSRV
        run: |
          cargo check --locked --all-targets
          cargo test --locked --all-features
```

---

## How to Update MSRV (Checklist)

### Pre-Update Validation

```bash
# 1. Check current MSRV
bash scripts/read-toml-string.sh Cargo.toml rust-version package

# 2. Determine minimum required version
cargo update -p "$DEPENDENCY"
cargo check  # Will fail if dependency needs newer Rust

# 3. Use cargo-msrv to find minimum version
cargo msrv --min 1.80.0
```

### Update All Configuration Files

Checklist for MSRV update from `1.87.0` to `1.88.0`:

- [ ] **Cargo.toml**: `rust-version = "1.88.0"`
- [ ] **`rust-toolchain.toml`**: `channel = "1.88.0"`
- [ ] **`clippy.toml`**: `msrv = "1.88.0"`
- [ ] **`Dockerfile`**: `FROM rust:1.88-bookworm AS chef`
- [ ] **`clients/native/Cargo.toml`**: `rust-version = "1.88.0"`
- [ ] **`.devcontainer/Dockerfile`**: Add comment `# Project MSRV: 1.88.0`
- [ ] **`.github/dependabot.yml`**: Update MSRV comments
- [ ] **`README.md`**: Update "Requirements" section
- [ ] **`docs/development.md`**: Update developer setup instructions
- [ ] **`CHANGELOG.md`**: Document MSRV bump under `[Unreleased]`

### Verification

```bash
# Use the dedicated script; avoid copying exact-space TOML greps into ad hoc checks.
./scripts/check-msrv-consistency.sh
```

---

## MSRV and Dependabot

The `.github/dependabot.yml` is configured to prevent automatic MSRV drift:

```yaml
- package-ecosystem: "docker"
  directory: "/"
  ignore:
    - dependency-name: "rust"
      update-types: ["version-update:semver-minor", "version-update:semver-patch"]
```

**Rationale:** Production builds must match CI validation environment. MSRV bumps are
deliberate, coordinated changes — not automatic. Override only for critical security fixes.

---

## Common MSRV Issues

### Issue 1: Dependency Requires Newer Rust

```text
error: package `rand v0.10.0` cannot be built because it requires rustc 1.88.0 or newer
```

**Solution:** Either update MSRV (following the checklist), or pin the older dependency:

```toml
[dependencies]
rand = "=0.9.0"  # Pin to older version compatible with current MSRV
```

### Issue 2: CI Passes Locally But Fails in CI

**Root Cause:** Local Rust version is newer than MSRV; CI uses exact MSRV.

**Solution:** Test with exact MSRV locally: `cargo +1.88.0 check`

### Issue 3: MSRV Consistency Check Fails

```text
✗ FAIL: clippy.toml msrv=1.87.0 (expected 1.88.0)
✗ FAIL: Dockerfile rust:1.87 (expected rust:1.88)
```

**Solution:**

```bash
sed -i 's/msrv = "1.87.0"/msrv = "1.88.0"/' clippy.toml
sed -i 's/FROM rust:1.87/FROM rust:1.88/' Dockerfile
```

---

## Related Skills

- [Toolchain Pinning](./toolchain-pinning.md) — `rust-toolchain.toml`, testing, developer workflow
- [Toolchain Nightly](./toolchain-nightly.md) — Nightly Rust for CI-only tools
- [Dependency Management Cargo](./dependency-management-cargo.md) — Choosing and auditing dependencies
- [Container Docker](./container-docker.md) — Docker build configuration
