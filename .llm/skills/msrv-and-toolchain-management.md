# Skill: MSRV and Toolchain Consistency Management

<!-- trigger: msrv, rust-version, toolchain, rust version, dependency incompatibility, cargo msrv, minimum supported rust version | Managing MSRV and ensuring toolchain version consistency | Infrastructure -->

**Trigger**: When updating Rust version, adding dependencies, or encountering toolchain-related CI failures.

---

## When to Use

- Updating the Minimum Supported Rust Version (MSRV)
- Adding a new dependency that requires a newer Rust version
- Encountering CI failures related to Rust toolchain version mismatches
- Setting up new development or deployment environments
- Reviewing dependency updates from Dependabot
- Debugging compilation errors that work locally but fail in CI

---

## When NOT to Use

- Performance optimization unrelated to dependencies (see [rust-performance-optimization](./rust-performance-optimization.md))
- Dependency security audits (see [supply-chain-security](./supply-chain-security.md))
- General dependency management (see [dependency-management](./dependency-management.md))

---

## TL;DR

- **MSRV is defined ONCE** in `Cargo.toml` (`rust-version` field) — this is the single source of truth
- **All other files must match**: `rust-toolchain.toml`, `clippy.toml`, `Dockerfile`, `.devcontainer/Dockerfile`
- **CI enforces consistency** with dedicated MSRV verification job (`.github/workflows/ci.yml`)
- **Before updating MSRV**: Check all dependencies support the new version (`cargo msrv`)
- **MSRV updates are coordinated changes** affecting multiple files — use checklist below

---

## What is MSRV?

**Minimum Supported Rust Version (MSRV)** is the oldest Rust compiler version that can build your project. It's a contract with users and CI environments about toolchain requirements.

### Why MSRV Matters

- **Reproducible builds**: Everyone uses the same Rust version in CI and production
- **Dependency compatibility**: Prevents pulling in dependencies that require newer Rust
- **Security**: Enables use of newer dependencies with security fixes
- **Developer experience**: Clear requirements for contributors

### MSRV Policy for This Project

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
```rust

**All other configuration files derive their Rust version from this field.**

---

## Configuration Files That Must Match MSRV

| File                        | Purpose                          | How to Update                            | Format Notes                | CI Validated? |
| --------------------------- | -------------------------------- | ---------------------------------------- | --------------------------- | ------------- |
| `Cargo.toml`                | MSRV source of truth             | `rust-version = "1.88.0"`                | Full semver (1.88.0)        | ✓             |
| `rust-toolchain.toml`       | Developer toolchain pinning      | `channel = "1.88.0"`                     | Full semver (1.88.0)        | ✓             |
| `clippy.toml`               | Clippy MSRV-aware lints          | `msrv = "1.88.0"`                        | Full semver (1.88.0)        | ✓             |
| `Dockerfile`                | Production build environment     | `FROM rust:1.88-bookworm`                | Docker format (1.88)        | ✓ (normalized)|
| `.devcontainer/Dockerfile`  | Development container (optional) | Comment or use `mcr.microsoft.com/...`   | Full semver in comment      | ⚠ (optional)  |
| `.github/dependabot.yml`    | Dependency update policy         | Document MSRV in ignore rules comments   | Full semver in comment      | ✗             |
| `README.md`                 | User-facing documentation        | Update installation requirements section | Full semver                 | ✗             |
| `docs/development.md`       | Developer setup guide            | Update toolchain installation steps      | Full semver                 | ✗             |

---

## Docker Version Format: Why 1.88 Instead of 1.88.0

**Important:** The Dockerfile uses `rust:1.88` (major.minor) instead of `rust:1.88.0` (full semver).
This is **intentional** and follows Docker Hub conventions.

### Why Docker Uses Shortened Versions

1. **Docker Hub convention**: Official Rust images use `rust:1.88` not `rust:1.88.0`
2. **Automatic patch updates**: `rust:1.88` automatically pulls the latest patch (1.88.x)
3. **Semantic equivalence**: `1.88` and `1.88.0` refer to the same Rust minor version
4. **Maintenance benefit**: Dockerfile gets security patches without manual updates

### CI Normalization Logic

The CI MSRV verification **normalizes both formats** before comparison to avoid false failures:

```bash
# Cargo.toml has: rust-version = "1.88.0"
MSRV="1.88.0"

# Dockerfile has: FROM rust:1.88-bookworm
DOCKERFILE_RUST="1.88"

# Normalize MSRV to major.minor (1.88.0 → 1.88)
MSRV_SHORT=$(echo "$MSRV" | sed -E 's/([0-9]+\.[0-9]+).*/\1/')

# Compare: "1.88" == "1.88" ✓
if [ "$DOCKERFILE_RUST" != "$MSRV_SHORT" ]; then
  echo "FAIL"
fi
```

### What This Means for You

- **Cargo.toml**: Always use full semver (`1.88.0`)
- **Dockerfile**: Use Docker format (`1.88-bookworm`)
- **CI will normalize**: Both formats are considered equivalent
- **Don't use 1.88.0 in Dockerfile**: It's not a valid Docker tag

### Common Mistake: Using Full Semver in Dockerfile

**Wrong:**

```dockerfile
FROM rust:1.88.0-bookworm  # ❌ Not a valid Docker tag
```bash

**Correct:**

```dockerfile
FROM rust:1.88-bookworm    # ✓ Valid Docker tag
```

The CI script normalizes both to `1.88` for comparison, so this mismatch is **expected and correct**.

---

## MSRV Verification in CI

The `.github/workflows/ci.yml` includes a dedicated `msrv` job that:

1. **Extracts MSRV** from `Cargo.toml` (single source of truth)
2. **Validates consistency** across all configuration files
3. **Compiles the project** with the exact MSRV version
4. **Runs tests** to ensure compatibility

### CI MSRV Validation Steps

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
          MSRV=$(grep '^rust-version = ' Cargo.toml | sed -E 's/rust-version = "(.+)"/\1/')
          echo "msrv=$MSRV" >> "$GITHUB_OUTPUT"

      - name: Verify MSRV consistency
        run: |
          # Checks rust-toolchain.toml, clippy.toml, Dockerfile
          # Fails if any file has a different version

      - name: Install Rust at MSRV
        uses: dtolnay/rust-toolchain@...
        with:
          toolchain: ${{ steps.msrv.outputs.msrv }}

      - name: Verify build and tests with MSRV
        run: |
          cargo check --locked --all-targets
          cargo test --locked --all-features
```bash

**This job catches:**
- Configuration drift (files with mismatched versions)
- Dependencies requiring newer Rust than MSRV
- Code using features from newer Rust versions

---

## How to Update MSRV (Checklist)

When a dependency requires a newer Rust version, follow this coordinated update process:

### Pre-Update Validation

```bash
# 1. Check current MSRV
grep '^rust-version = ' Cargo.toml

# 2. Identify why MSRV bump is needed
# Usually: dependency update requires newer Rust
cargo update -p <dependency>
cargo check  # Will fail if dependency needs newer Rust

# 3. Determine minimum required version
# Option A: Read dependency's Cargo.toml rust-version field
# Option B: Use cargo-msrv (install: cargo install cargo-msrv)
cargo msrv --min 1.80.0  # Check if a specific version works
```

### Update All Configuration Files

**Checklist for MSRV update from `1.87.0` to `1.88.0` (example):**

- [ ] **Cargo.toml**: Update `rust-version = "1.88.0"`
- [ ] **rust-toolchain.toml**: Update `channel = "1.88.0"`
- [ ] **clippy.toml**: Update `msrv = "1.88.0"`
- [ ] **Dockerfile**: Update `FROM rust:1.88-bookworm AS chef`
- [ ] **.devcontainer/Dockerfile**: Add comment `# Project MSRV: 1.88.0` (devcontainer may use newer)
- [ ] **.github/dependabot.yml**: Update MSRV comments in ignore rules documentation
- [ ] **README.md**: Update "Requirements" section if present
- [ ] **docs/development.md**: Update developer setup instructions
- [ ] **CHANGELOG.md**: Document MSRV bump under `[Unreleased]` or next version

### Verification Steps

#### Recommended: Use the Verification Script

The project includes a dedicated script for MSRV consistency validation:

```bash
./scripts/check-msrv-consistency.sh
```bash

This script validates all configuration files and provides clear, color-coded output.

#### Manual Verification (Alternative)

If you prefer to verify manually or the script is not available:

```bash
# 1. Clean build from scratch
cargo clean
rm -rf target/

# 2. Verify build with new MSRV
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features

# 3. Verify Docker build (uses Dockerfile MSRV)
docker build -t test-msrv .

# 4. Check for warnings about MSRV in dependencies
cargo tree --all-features | grep -i "requires rustc"

# 5. Run MSRV consistency check manually
# Extract MSRV from Cargo.toml
MSRV=$(grep '^rust-version = ' Cargo.toml | sed -E 's/rust-version = "(.+)"/\1/')
echo "Checking MSRV consistency: $MSRV"

# Verify rust-toolchain.toml
grep "channel = \"$MSRV\"" rust-toolchain.toml || echo "FAIL: rust-toolchain.toml"

# Verify clippy.toml
grep "msrv = \"$MSRV\"" clippy.toml || echo "FAIL: clippy.toml"

# Verify Dockerfile
grep "FROM rust:$MSRV" Dockerfile || echo "FAIL: Dockerfile"
```

### Commit Message Format

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
- Update CHANGELOG.md with MSRV update documentation

Testing:
- All 224 tests passing (cargo test --all-features)
- Zero clippy warnings (cargo clippy --all-targets --all-features)
- Docker build successful
- CI MSRV verification job passes
```rust

---

## Common MSRV Issues and Solutions

### Issue 1: Dependency Requires Newer Rust

**Symptom:**

```text
error: package `rand v0.10.0` cannot be built because it requires rustc 1.88.0 or newer,
while the currently active rustc version is 1.87.0
```

**Solution:**
1. Check if you actually need the newer dependency version
2. If yes, update MSRV following the checklist above
3. If no, pin the older dependency version in `Cargo.toml`:
   ```toml
   [dependencies]
   rand = "=0.9.0"  # Pin to older version compatible with current MSRV
   ```text

### Issue 2: CI Passes Locally But Fails in CI

**Symptom:**

```text
Local: cargo test → ✓ Passes
CI:    cargo test → ✗ Fails with "requires rustc X.Y.Z or newer"
```

**Root Cause:** Local Rust version is newer than MSRV, CI uses exact MSRV.

**Solution:**
1. Install exact MSRV locally: `rustup install 1.88.0`
2. Test with MSRV: `cargo +1.88.0 check`
3. Update MSRV if needed (see checklist above)

### Issue 3: MSRV Consistency Check Fails in CI

**Symptom:**

```text
✗ FAIL: clippy.toml msrv=1.87.0 (expected 1.88.0)
✗ FAIL: Dockerfile rust:1.87 (expected rust:1.88)
```rust

**Solution:** Update the mismatched files to match `Cargo.toml`:
```bash
# Fix clippy.toml
sed -i 's/msrv = "1.87.0"/msrv = "1.88.0"/' clippy.toml

# Fix Dockerfile
sed -i 's/FROM rust:1.87/FROM rust:1.88/' Dockerfile
```

### Issue 4: Using Features From Newer Rust

**Symptom:**

```text
error[E0658]: use of unstable library feature 'foo'
```bash

**Root Cause:** Code uses a feature stabilized after MSRV.

**Solutions:**
- **Option A**: Update MSRV to the version that stabilized the feature
- **Option B**: Use alternative code compatible with current MSRV
- **Option C**: Use feature gates: `#[cfg(feature = "unstable")]`

---

## MSRV and Dependabot

The `.github/dependabot.yml` is configured to prevent automatic MSRV drift:

```yaml
# Dockerfile: Ignore Rust image updates (MSRV policy)
- package-ecosystem: "docker"
  directory: "/"
  ignore:
    - dependency-name: "rust"
      update-types: ["version-update:semver-minor", "version-update:semver-patch"]
```

**Rationale:**
- Production builds must match CI validation environment (MSRV)
- Prevents accidental use of features newer than MSRV
- MSRV bumps are deliberate, coordinated changes (not automatic)
- Security fixes override this policy (manual review)

**When to Override:**
- Critical security fix in Rust compiler/std
- Major performance improvement in newer rustc (evaluate carefully)
- Dependency ecosystem forces MSRV bump

---

## Toolchain Pinning: rust-toolchain.toml

The `rust-toolchain.toml` file pins the exact Rust version for developers and CI:

```toml
[toolchain]
channel = "1.88.0"
components = ["rustfmt", "clippy"]
targets = []
```rust

**Effect:**
- `cargo` commands automatically use this version
- Developers don't need to remember to use `cargo +1.88.0`
- CI uses this file (via `rust-toolchain.toml` detection)

**vs `rust-version` in Cargo.toml:**
- `rust-version`: Minimum required Rust (metadata + validation)
- `rust-toolchain.toml`: Active toolchain to use (enforces exact version)
- **Best practice**: Set both to the same value for consistency

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
```rust

**Use cases:**
- Determining minimum version after adding dependencies
- Validating MSRV bump is necessary
- Finding which dependency requires newer Rust

---

## MSRV Bump Timing Strategy

**When to bump MSRV:**

| Scenario                              | Action                           | Urgency   |
| ------------------------------------- | -------------------------------- | --------- |
| Security fix in dependency            | Bump MSRV immediately            | High      |
| New dependency requires newer Rust    | Evaluate alternatives first      | Medium    |
| Ecosystem majority moved to newer     | Consider bump (not urgent)       | Low       |
| New Rust feature improves performance | Measure impact, then decide      | Low       |
| MSRV is >6 months old                 | Review ecosystem, consider bump  | Low       |

**When NOT to bump MSRV:**
- Just because a new Rust version is released
- For convenience features (unless significant value)
- Without checking dependency compatibility
- Without updating all configuration files simultaneously

---

## Developer Workflow: Working with MSRV

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
```bash

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
- [ ] **MSRV consistency check**: All files match (see verification script above)
- [ ] **CI verification**: Push to branch, ensure MSRV job passes

---

## Common Mistakes to Avoid

### ❌ Updating Only Cargo.toml

**Wrong:**
```bash
# Only update Cargo.toml
sed -i 's/1.87.0/1.88.0/' Cargo.toml
git commit -m "Update MSRV"
```bash

**Why it fails:** CI MSRV verification job will fail due to inconsistency.

**Correct:** Update all files using the checklist above.

---

### ❌ Using Different Versions in Different Files

**Wrong:**
```toml
# Cargo.toml
rust-version = "1.88.0"

# rust-toolchain.toml
channel = "1.87.0"  # ← Inconsistent!
```

**Why it fails:** CI enforces consistency; local builds use wrong version.

**Correct:** All files must use the same version (see single source of truth).

---

### ❌ Assuming Devcontainer Must Match MSRV

**Wrong assumption:**
> "Devcontainer Rust version must exactly match production MSRV."

**Actual policy:**
- **Production (Dockerfile)**: MUST match MSRV exactly
- **Development (devcontainer)**: MAY use newer Rust for better tooling
- **CI validates MSRV**: So devcontainer can be newer without risk

**Rationale:** Developers benefit from latest diagnostics; CI enforces MSRV.

---

### ❌ Skipping CI Validation Locally

**Wrong:**
```bash
# Update MSRV files, push immediately
git add Cargo.toml rust-toolchain.toml
git commit -m "Update MSRV"
git push  # ← CI will fail!
```rust

**Why it fails:** Forgot to update clippy.toml, Dockerfile, etc.

**Correct:** Run local consistency check before pushing (see verification script).

---

## Nightly-Only CI Tools

### Overview

Some CI analysis tools require nightly Rust because they use unstable compiler features. This is acceptable **only
for CI-only tools** that never build production artifacts.

### When Nightly is Acceptable

#### Acceptable: CI-only analysis tools

- cargo-udeps (unused dependency detection)
- cargo-miri (undefined behavior detection)
- cargo-fuzz (fuzzing infrastructure)
- Tools that use unstable compiler APIs for analysis

#### Not Acceptable: Production builds

- Building the application binary
- Building Docker images for deployment
- Building release artifacts
- Any code that users depend on

### Current Nightly Usage

This project uses nightly Rust **only** for:

| Tool        | Purpose                     | Workflow File                           | Nightly Version    |
| ----------- | --------------------------- | --------------------------------------- | ------------------ |
| cargo-udeps | Unused dependency detection | `.github/workflows/unused-deps.yml`     | nightly-2026-01-15 |

### Nightly Version Policy

#### Pinning Strategy

- We pin to a specific nightly date (e.g., `nightly-2026-01-15`)
- We do NOT use rolling `nightly` (always latest)
- Pinning provides reproducibility and stability

#### Update Criteria

Update the nightly version when:

1. **Age**: Nightly version is >6 months old
2. **Security**: Security advisories affect this version
3. **Features**: Tool requires newer nightly features
4. **Availability**: Nightly version becomes unavailable/broken

#### Update Frequency

- Review nightly versions quarterly
- Update proactively before staleness causes issues
- Document update date in workflow file

### Pinned vs Rolling Nightly

| Aspect          | Pinned (nightly-YYYY-MM-DD)      | Rolling (nightly)                |
| --------------- | -------------------------------- | -------------------------------- |
| Reproducibility | ✅ Same version every CI run     | ❌ Changes daily                 |
| Stability       | ✅ No surprise breakage          | ❌ May break unexpectedly        |
| Freshness       | ⚠️ Becomes stale over time       | ✅ Always latest                 |
| Maintenance     | ⚠️ Requires periodic updates     | ✅ No updates needed             |
| Recommendation  | ✅ Use for CI tools              | ❌ Avoid for stability           |

**Decision:** This project uses **pinned nightly** for reproducibility and stability.

### Nightly Update Checklist

When updating a nightly version in CI:

- [ ] **Identify current nightly version** (check workflow file)
- [ ] **Choose new nightly version** (within last 30 days preferred)
- [ ] **Update workflow file** (change `toolchain: nightly-YYYY-MM-DD`)
- [ ] **Update documentation** (change "Last Updated: YYYY-MM-DD" comment)
- [ ] **Update all references** (search for old nightly date in workflow)
- [ ] **Test in CI** (push to branch, verify workflow succeeds)
- [ ] **Document in commit** (explain reason for nightly update)

### Example: Updating cargo-udeps Nightly

```bash
# 1. Check current nightly version
grep -n "nightly-" .github/workflows/unused-deps.yml

# 2. Update workflow file (all occurrences)
sed -i 's/nightly-2025-02-21/nightly-2026-01-15/g' .github/workflows/unused-deps.yml

# 3. Update "Last Updated" comment
sed -i 's/Last Updated: .*/Last Updated: 2026-02-16/' .github/workflows/unused-deps.yml

# 4. Verify changes
git diff .github/workflows/unused-deps.yml

# 5. Commit with explanation
git add .github/workflows/unused-deps.yml
git commit -m "$(cat <<'EOF'
chore: update cargo-udeps nightly from 2025-02-21 to 2026-01-15

Update nightly toolchain for cargo-udeps to nightly-2026-01-15 (from
nightly-2025-02-21, which was 360 days old). The new nightly is 32 days
old and within our 6-month staleness threshold.

cargo-udeps requires nightly Rust for unstable compiler features used in
dependency analysis. This does not affect production builds, which continue
to use stable MSRV (1.88.0) as defined in Cargo.toml.

See .github/workflows/unused-deps.yml for nightly version policy and update
criteria.
EOF
)"
```

### Documentation Requirements

Every nightly usage **must** be documented in the workflow file:

```yaml
# cargo-udeps requires nightly Rust because it uses unstable compiler features
# to analyze dependency usage at a deeper level than stable tools can provide.
#
# Nightly Version: nightly-2026-01-15
# Last Updated: 2026-02-16
#
# Update Criteria (when to update this nightly version):
#   - If the nightly version is >6 months old
#   - If security advisories affect this version
#   - If cargo-udeps requires newer nightly features
#   - If the nightly version becomes unavailable or broken
#
# Policy:
#   - Production code MUST use stable MSRV (see Cargo.toml rust-version)
#   - CI-only analysis tools MAY use nightly if required by the tool
#   - Nightly is NEVER used for building production artifacts
```bash

### Nightly vs MSRV Relationship

**Key Principle:** Nightly for CI tools is **independent** of production MSRV.

```text
Production Code (Stable MSRV)
  ↓
  rust-version = "1.88.0" in Cargo.toml  ← Single source of truth
  ↓
  Used for: Building binaries, Docker images, production artifacts

CI Analysis Tools (Nightly)
  ↓
  nightly-2026-01-15 in workflow files  ← Independent of MSRV
  ↓
  Used for: cargo-udeps, cargo-miri (analysis only, no artifacts)
```

#### Relationship

- Nightly version can be NEWER than stable MSRV (usually is)
- Nightly version can be OLDER than stable MSRV (if recently updated MSRV)
- No requirement for nightly to match MSRV
- Nightly is updated independently based on tool needs

#### Common Confusion (Avoid)

- "Nightly must be newer than MSRV" (incorrect)
- "If MSRV is 1.88, nightly must be from after 1.88 release" (incorrect)
- "Nightly is for CI tools only; MSRV is for production code" (correct)
- "Update nightly based on staleness/tool needs, not MSRV changes" (correct)

### Future Consideration: Rolling Nightly

**Current Policy:** Pinned nightly (e.g., `nightly-2026-01-15`)

**Alternative (Not Currently Used):** Rolling nightly (`nightly`)

#### Pros of Rolling

- Always latest features
- Zero maintenance (no updates needed)
- Never stale

#### Cons of Rolling

- Unpredictable breakage
- Non-reproducible CI runs
- Harder to bisect failures

#### When to Reconsider Rolling

- If pinned nightly requires frequent updates (>monthly)
- If CI tool explicitly recommends rolling nightly
- If stability issues become rare/non-existent

**Decision:** Continue with pinned nightly unless evidence suggests rolling is more reliable.

### Agent Workflow: Nightly Version Updates

When asked to update nightly version:

1. **Verify nightly is needed**: Check if tool still requires nightly
2. **Choose recent nightly**: Within last 30 days (e.g., `nightly-2026-01-15`)
3. **Update all occurrences**: Search workflow file for old nightly date
4. **Update documentation**: Change "Last Updated: YYYY-MM-DD" comment
5. **Explain in workflow file**: Maintain comprehensive comments
6. **Document in this skill**: Reference workflow file as example
7. **Test in CI**: Verify workflow passes with new nightly
8. **Commit with context**: Explain age of old nightly, reason for update

---

## Related Skills

- [dependency-management](./dependency-management.md) — Choosing and auditing dependencies
- [supply-chain-security](./supply-chain-security.md) — Dependency security scanning
- [github-actions-best-practices](./github-actions-best-practices.md) — CI/CD workflow patterns
- [mandatory-workflow](./mandatory-workflow.md) — Pre-commit validation workflow
- [container-and-deployment](./container-and-deployment.md) — Docker build configuration

---

## References

- [Cargo Book: rust-version field](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field)
- [Rust Toolchain Files](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file)
- [cargo-msrv documentation](https://github.com/foresterre/cargo-msrv)
- [Clippy MSRV Configuration](https://doc.rust-lang.org/clippy/configuration.html#msrv)
