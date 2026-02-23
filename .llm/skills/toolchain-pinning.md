# Skill: Rust Toolchain Pinning and MSRV Testing

<!--
  trigger: rust-toolchain.toml, toolchain pinning, cargo msrv, MSRV testing, developer workflow
  | Pinning the Rust toolchain, testing against MSRV, and developer workflow
  | Infrastructure
-->

**Trigger**: When setting up `rust-toolchain.toml`, running MSRV tests locally, or establishing
developer workflow for a Rust project.

See also:

- [msrv-management](./msrv-management.md) — MSRV definition, config files, update checklist
- [toolchain-nightly](./toolchain-nightly.md) — Nightly Rust for CI-only analysis tools

---

## TL;DR

- `rust-toolchain.toml` pins the exact Rust version for all developers and CI automatically
- Run `cargo +1.88.0 check` to test against MSRV without changing system toolchain
- Use `cargo-msrv` to find the actual minimum version required by your dependency tree
- Never update only `Cargo.toml` — all files must change together

---

## Toolchain Pinning: `rust-toolchain.toml`

```toml
[toolchain]
channel = "1.88.0"
components = ["rustfmt", "clippy"]
targets = []
```

**Effect:**

- `cargo` commands automatically use this version — no need to specify `cargo +1.88.0`
- CI picks this up automatically via rustup's toolchain file detection
- Ensures every developer uses the same compiler

**vs `rust-version` in Cargo.toml:**

| Field | Purpose |
|-------|---------|
| `rust-version` in `Cargo.toml` | Minimum required Rust (metadata + validation) |
| `rust-toolchain.toml` `channel` | Active toolchain to use (enforces exact version) |

**Best practice:** Set both to the same value for consistency.

---

## MSRV Testing Strategy

### Local Testing

```bash
# Install the MSRV version
rustup install 1.88.0

# Test with exact MSRV
cargo +1.88.0 check --all-targets
cargo +1.88.0 test --all-features
cargo +1.88.0 clippy --all-targets --all-features

# Test with latest stable (regression detection)
rustup install stable
cargo +stable check --all-targets
```

### CI Testing

The CI pipeline runs **two separate validation tracks**:

1. **Main CI jobs** (check, test): Use `rust-toolchain.toml` (enforced MSRV)
2. **MSRV job**: Explicitly validates MSRV from `Cargo.toml`

This dual validation ensures:

- Code compiles with MSRV (MSRV job)
- Configuration is consistent (MSRV job)
- All lints/tests pass (main CI jobs)

---

## cargo-msrv Tool (Optional)

Install `cargo-msrv` for automated MSRV detection:

```bash
cargo install cargo-msrv

# Find minimum Rust version for current codebase
cargo msrv

# Check if specific version works
cargo msrv --min 1.80.0

# List incompatible dependencies
cargo msrv --output-format json | jq '.dependencies'
```

**Use cases:**

- Determining minimum version after adding dependencies
- Validating an MSRV bump is necessary
- Finding which dependency requires newer Rust

---

## MSRV Bump Timing Strategy

| Scenario | Action | Urgency |
|----------|--------|---------|
| Security fix in dependency | Bump MSRV immediately | High |
| New dependency requires newer Rust | Evaluate alternatives first | Medium |
| Ecosystem majority moved to newer | Consider bump (not urgent) | Low |
| New Rust feature improves performance | Measure impact, then decide | Low |
| MSRV is >6 months old | Review ecosystem, consider bump | Low |

**When NOT to bump MSRV:**

- Just because a new Rust version is released
- For convenience features (unless significant value)
- Without checking dependency compatibility
- Without updating all configuration files simultaneously

---

## Developer Workflow

### First-Time Setup

```bash
# 1. Clone repository
git clone https://github.com/Ambiguous-Interactive/signal-fish-server.git
cd signal-fish-server

# 2. Rust toolchain is auto-selected via rust-toolchain.toml
# Verify correct version:
rustc --version
# Should output: rustc 1.88.0 (...)

# 3. Install components (if not already present)
rustup component add rustfmt clippy

# 4. Build and test
cargo build
cargo test --all-features
```

### Daily Development

```bash
# Standard workflow automatically uses MSRV from rust-toolchain.toml
cargo fmt
cargo clippy --all-targets --all-features
cargo test --all-features

# No need to specify +1.88.0 — rust-toolchain.toml handles it
```

### Testing with Newer Rust

```bash
# Install latest stable
rustup install stable

# Test with newer Rust (check for future compatibility)
cargo +stable check --all-targets

# If it fails with stable, likely using unstable features
```

---

## Agent Checklist: MSRV Updates

- [ ] `Cargo.toml`: `rust-version` updated
- [ ] `rust-toolchain.toml`: `channel` updated
- [ ] `clippy.toml`: `msrv` updated
- [ ] `Dockerfile`: `FROM rust:X.Y` updated
- [ ] `.devcontainer/Dockerfile`: Comment updated (version may differ)
- [ ] `.github/dependabot.yml`: Comments updated
- [ ] `README.md`: Requirements section updated
- [ ] `docs/development.md`: Setup instructions updated
- [ ] `CHANGELOG.md`: MSRV bump documented
- [ ] **Local verification**: `cargo clean && cargo test --all-features`
- [ ] **Docker verification**: `docker build -t test .`
- [ ] **MSRV consistency check**: `./scripts/check-msrv-consistency.sh`
- [ ] **CI verification**: Push to branch, ensure MSRV job passes

---

## Common Mistakes to Avoid

### Updating Only Cargo.toml

```bash
# ❌ WRONG: Only update Cargo.toml
sed -i 's/1.87.0/1.88.0/' Cargo.toml
git commit -m "Update MSRV"
# CI MSRV verification job will fail due to inconsistency
```

**Correct:** Update all files using the checklist in [msrv-management](./msrv-management.md).

### Using Different Versions in Different Files

```toml
# ❌ WRONG: Inconsistent versions
# Cargo.toml: rust-version = "1.88.0"
# rust-toolchain.toml: channel = "1.87.0"  ← Inconsistent!
```

### Assuming Devcontainer Must Match MSRV

- **Production (Dockerfile)**: MUST match MSRV exactly
- **Development (devcontainer)**: MAY use newer Rust for better tooling
- **Rationale:** Developers benefit from latest diagnostics; CI enforces MSRV

### Skipping CI Validation Locally

Run `./scripts/check-msrv-consistency.sh` before pushing to catch mismatches in
`clippy.toml`, `Dockerfile`, or other files.

---

## Commit Message Format

```text
chore: update MSRV from 1.87.0 to 1.88.0

Update minimum supported Rust version to 1.88.0 to support the rand 0.10
dependency update.

Changes:
- Update rust-version in Cargo.toml to 1.88.0
- Update rust-toolchain.toml to enforce Rust 1.88.0
- Update clippy.toml MSRV configuration to 1.88.0
- Update Dockerfile base image from rust:1.87 to rust:1.88
- Update documentation (README.md, docs/development.md)
- Update CHANGELOG.md with MSRV update documentation
```

---

## Related Skills

- [msrv-management](./msrv-management.md) — MSRV definition, config files, update checklist, CI
- [toolchain-nightly](./toolchain-nightly.md) — Nightly Rust for CI-only tools
- [GitHub-actions-best-practices](./github-actions-workflow-config.md) — CI/CD workflow patterns
