# Skill: CI/CD Troubleshooting Guide

<!--
  trigger: ci failure, ci error, workflow failure, GitHub actions failure, ci debug, cache error, configuration mismatch
  | Common CI failures and their solutions
  | Infrastructure
-->

**Trigger**: When debugging CI/CD pipeline failures, diagnosing workflow issues, or investigating configuration problems.

---

## When to Use

- CI workflow fails unexpectedly
- Local builds pass but CI fails (or vice versa)
- Cache-related errors in GitHub Actions
- Configuration drift between environments
- Dependency resolution failures in CI
- Toolchain version mismatches
- Docker build failures in CI
- Stale file references in Dockerfiles or workflows after cleanup
- Supply chain security concerns with GitHub Actions
- Silent failures masked by `continue-on-error: true`

---

## When NOT to Use

- Writing new workflows from scratch (see [GitHub-actions-best-practices](./github-actions-best-practices.md))
- Performance optimization (see [Rust-performance-optimization](./rust-performance-optimization.md))
- Security audits (see [supply-chain-security](./supply-chain-security.md))

---

## TL;DR

**Configuration & Consistency:**

- **Configuration mismatch** is the most common root cause of "works locally, fails in CI"
- **Check language-project alignment**: Python caching on Rust project = instant failure
- **Typos configuration**: Mixed-case names (HashiCorp) need `extend-identifiers`, not `extend-words`
- **Docker versions**: Use X.Y format (1.88) for Docker Hub, not X.Y.Z (1.88.0)
- **YAML indentation**: All workflow files must use 2-space indentation — 4-space from copied
  templates is the most common yamllint failure

**Staleness & Maintenance:**

- **Staleness kills**: Old toolchains (>6 months) cause subtle breakage
- **Cache invalidation** is hard - when in doubt, clear the cache
- **Always check dates**: Pinned versions/toolchains from >6 months ago need review

**Testing & Validation:**

- **Test configuration files**: Add CI tests to validate consistency (MSRV, typos.toml, etc.)
- **AWK patterns**: Use prefix matching (`/^```rust/`) for flexibility, not exact patterns

---

## Common CI Failure Patterns

### Pattern 1: Language/Ecosystem Mismatch

#### Symptom

```text
# CI fails with:
ERROR: Cache entry deserialization failed, entry ignored
ERROR: Unable to locate executable file: pip

```

#### Root Cause

Workflow uses caching/tooling for wrong language ecosystem:

```yaml
# ❌ WRONG: Python caching on a Rust project
- uses: actions/cache@v4

  with:
    path: ~/.cache/pip        # ← Python cache path
    key: ${{ runner.os }}-pip-${{ hashFiles('**/requirements.txt') }}

- run: cargo build            # ← Rust project, not Python!


```

#### Solution

**Use language-specific caching that matches the project:**

```yaml
# ✅ CORRECT: Rust caching for Rust project
- uses: Swatinem/rust-cache@v2.7.5

  with:
    prefix-key: "rust"

- run: cargo build


```

#### Prevention Checklist

- [ ] Does workflow configuration match the project's primary language?
- [ ] Are cache paths appropriate for the language ecosystem?
- [ ] Do dependency files referenced in cache keys exist? (`requirements.txt` for Python, `Cargo.lock` for Rust, etc.)
- [ ] Are tools/actions language-appropriate? (pip for Python, cargo for Rust, npm for Node)

#### Detection

**Red flags that indicate ecosystem mismatch:**

| Indicator | Wrong Ecosystem | Correct for Rust |
|-----------|-----------------|------------------|
| Cache paths | `~/.cache/pip`, `node_modules/`, `.bundle/` | `~/.cargo/`, `target/` |
| Hash files | `requirements.txt`, `package-lock.json`, `Gemfile.lock` | `Cargo.lock`, `Cargo.toml` |
| Install commands | `pip install`, `npm install`, `bundle install` | `cargo build`, `rustup component add` |
| Build commands | `python setup.py`, `npm run build`, `make` | `cargo build`, `cargo test` |

**Quick audit command:**

```bash
# Search for language-specific patterns in workflow files
cd .github/workflows || exit
grep -r "pip\|requirements\.txt\|python" .    # Python patterns
grep -r "npm\|package\.json\|node" .          # Node patterns
grep -r "bundle\|Gemfile\|ruby" .             # Ruby patterns
grep -r "cargo\|Cargo\.toml\|rust" .          # Rust patterns (should be present)

```

---

### Pattern 2: Cache Corruption / Deserialization Failures

#### Symptom

```text

ERROR: Cache entry deserialization failed, entry ignored
WARNING: Failed to restore cache, continuing without cache

```

#### Root Causes

1. **Cache format changed** (action/tool updated)
2. **OS mismatch** (cache from Linux restored on macOS)
3. **Cache key collision** (different projects using same key)
4. **Corrupted upload** (network error during cache save)

#### Solution

**Clear and rebuild cache:**

```yaml
# Temporary: Add cache-busting suffix to key
- uses: actions/cache@v4

  with:
    path: ~/.cargo
    key: ${{ runner.os }}-cargo-v2-${{ hashFiles('**/Cargo.lock') }}
    #                            ^^^ increment version to bust cache

```

**Or via GitHub UI:**

1. Go to repository → Actions → Caches
2. Delete problematic cache entries
3. Re-run workflow to rebuild fresh cache

#### Prevention

**Use versioned cache keys:**

```yaml
# ✅ GOOD: Versioned cache key
key: ${{ runner.os }}-rust-v1-${{ hashFiles('**/Cargo.lock') }}
#                          ^^^ version allows cache invalidation
```

**Use action-managed caching when available:**

```yaml
# ✅ BEST: Let Swatinem/rust-cache handle cache management
- uses: Swatinem/rust-cache@v2.7.5

  # Automatically manages cache keys, invalidation, and restoration

```

---

### Pattern 3: Toolchain Staleness

#### Symptom

```text

error: package `rand v0.10.0` cannot be built because it requires rustc 1.88.0 or newer,
while the currently active rustc version is 1.87.0

# OR

error[E0658]: use of unstable library feature 'foo'

```

#### Root Cause

**Pinned toolchain/nightly version is too old:**

```yaml
# ❌ PROBLEM: Nightly from 360 days ago
- uses: dtolnay/rust-toolchain@stable

  with:
    toolchain: nightly-2025-02-21  # ← 360 days old!

```

**Why this happens:**

- Dependencies update to require newer Rust
- Pinned nightly becomes increasingly stale
- Features stabilize but old toolchain doesn't have them
- Security fixes not included in old toolchain

#### Solution

**Update pinned toolchain to recent version:**

```yaml
# ✅ CORRECT: Recent nightly (within last 30 days)
- uses: dtolnay/rust-toolchain@stable

  with:
    toolchain: nightly-2026-01-15  # ← 32 days old, acceptable

```

**For stable MSRV issues, update MSRV across all files:**

See [msrv-and-toolchain-management](./msrv-and-toolchain-management.md) for full checklist.

#### Prevention

**Establish staleness thresholds:**

| Toolchain Type | Maximum Age | Action Required |
|----------------|-------------|-----------------|
| Stable MSRV | N/A | Update when dependencies require it |
| Pinned nightly | 6 months | Proactive update recommended |
| Action SHA pins | 1 year | Review for security updates |
| Docker base images | 6 months | Update for security patches |

**Add staleness checks to workflows:**

```yaml
# Document expected update frequency
# Nightly Version: nightly-2026-01-15
# Last Updated: 2026-02-16
# Review Frequency: Quarterly (every 3 months)
#
# Update Criteria:
#   - If >6 months old
#   - If security advisories affect this version
#   - If tool requires newer nightly features
```

**Quarterly review process:**

```bash
# Check age of pinned nightlies
grep -r "nightly-20" .github/workflows/ | while read -r line; do
  echo "$line"
  # Extract date and calculate age
done

# Check age of Rust stable version
MSRV=$(grep '^rust-version = ' Cargo.toml | sed -E 's/rust-version = "(.+)"/\1/')
rustc --version  # Compare with latest stable

```

---

### Pattern 4: Dependency Hygiene Drift

#### Symptom

```text

warning: unused dependency: `futures`
warning: unused dependency: `async-trait`
# ... 15+ unused dependencies
```

**Or worse: no warning at all**, just accumulating cruft over time.

#### Root Cause

- Dependencies added for experimental features, never removed
- Refactoring eliminates need for dependency, but it stays in Cargo.toml
- Feature flags changed, some dependencies no longer needed
- No regular audit process

#### Solution

**Run dependency audit tools:**

```bash
# Install tools
cargo install cargo-machete cargo-udeps

# Find unused dependencies (fast, stable)
cargo machete

# Find unused dependencies and features (slow, nightly, more thorough)
cargo +nightly udeps --all-targets

```

**Remove confirmed unused dependencies:**

Before:

```toml
[dependencies]
tokio = "1.49"
futures = "0.3"           # ← Unused
async-trait = "0.1"       # ← Unused
serde = { version = "1.0", features = ["derive"] }
rand = "0.10"
```

After:

```toml
[dependencies]
tokio = "1.49"
serde = { version = "1.0", features = ["derive"] }
rand = "0.10"
```

#### Prevention

**Establish regular audit schedule:**

```yaml
# .github/workflows/unused-deps.yml
on:
  schedule:

    - cron: '0 0 * * 1'  # Weekly on Monday


```

**CI enforcement:**

```yaml


- name: Check for unused dependencies

  run: cargo machete
  # Fails workflow if unused dependencies detected

```

**Keep vs Remove Decision Matrix:**

| Scenario | Decision | Rationale |
|----------|----------|-----------|
| Unused but actively maintained | Remove | Can re-add when needed |
| Unused behind feature flag | Keep | Optional dependency, may be used |
| Unused but recently added (<1 week) | Keep | May be work-in-progress |
| Unused, unmaintained (>1 year) | Remove immediately | Security liability |
| False positive from cargo-udeps | Keep | Mark with `# keep: used in macro` comment |

**Documentation pattern:**

```toml

[dependencies]
# Core async runtime
tokio = { version = "1.49", features = ["rt-multi-thread", "macros"] }

# keep: Used by serde derive macros (false positive from cargo-udeps)
serde_derive = "1.0"

```

---

### Pattern 5: "Works Locally, Fails in CI" (or Vice Versa)

#### Symptom

```text

Local: cargo test  → ✓ Passes
CI:    cargo test  → ✗ Fails with compilation errors

# OR

Local: cargo test  → ✗ Fails
CI:    cargo test  → ✓ Passes

```

#### Root Causes

**A. Different Rust versions:**

```text
# Local (using latest stable)
$ rustc --version
rustc 1.89.0

# CI (using MSRV from rust-toolchain.toml)
rustc 1.88.0

```

**B. Different feature flags:**

```bash
# Local
cargo test --all-features   # Tests WITH all features

# CI
cargo test                  # Tests WITHOUT features

```

**C. Different OS:**

```rust
// Local: macOS (case-insensitive filesystem)
use crate::Config;  // finds config.rs, Config.rs, or CONFIG.rs

// CI: Linux (case-sensitive filesystem)
use crate::Config;  // ONLY finds config.rs (exact match)

```

**D. Different environment variables:**

```bash
# Local (has env vars set in shell)
export DATABASE_URL=...
export RUST_LOG=debug

# CI (clean environment)
# No env vars unless explicitly set in workflow
```

#### Solution

**Reproduce CI environment locally:**

```bash
# 1. Use exact MSRV from rust-toolchain.toml
rustup install 1.88.0
cargo +1.88.0 test

# 2. Match CI feature flags exactly
cargo test --locked          # No features (matches CI default job)
cargo test --locked --all-features  # All features (matches CI all-features job)

# 3. Use Docker to match CI OS
docker run --rm -v $(pwd):/app -w /app rust:1.88-bookworm cargo test

# 4. Clear env vars
env -i PATH=$PATH HOME=$HOME cargo test

```

**Identify differences systematically:**

```bash
# Compare local vs CI:
# - Rust version: rustc --version
# - Cargo version: cargo --version
# - OS: uname -a
# - Features: cargo tree --features (in CI logs)
# - Env vars: env | grep -E 'RUST|CARGO' (in CI logs)
```

#### Prevention

**Use `rust-toolchain.toml` for version pinning:**

```toml
# rust-toolchain.toml — enforces exact version
[toolchain]
channel = "1.88.0"
components = ["rustfmt", "clippy"]

```

**Test both feature configurations locally:**

```bash
# Before pushing:
cargo test --locked                     # Default features
cargo test --locked --all-features      # All features
cargo test --locked --no-default-features  # Minimal features

```

**Document environment requirements:**

```markdown
# docs/development.md

## Environment Setup

Required:

- Rust 1.88.0 (enforced by rust-toolchain.toml)
- No additional environment variables needed

Optional (for integration tests):

- DATABASE_URL for postgres feature tests


```

---

### Pattern 6: Docker Build Failures (Local Success, CI Failure)

#### Symptom

```text

Local: docker build -t myapp .  → ✓ Success
CI:    docker build -t myapp .  → ✗ Fails with package not found

```

#### Root Causes

**A. Docker build cache differences:**

```dockerfile
# Local: Has cached layers from previous builds
# CI: Starts fresh every time
```

**B. Platform differences:**

```bash
# Local: Building for host architecture (e.g., arm64 on M1 Mac)
# CI: Building for linux/amd64
```

**C. Build context includes files that shouldn't be there:**

```text
# Local: .dockerignore not properly configured
COPY . /app  # Includes target/, .git/, etc. (breaks build)

# CI: Fails because copied files interfere
```

#### Solution

**Disable Docker build cache in CI:**

```yaml


- name: Build Docker image

  run: docker build --no-cache -t myapp:ci .

```

**Specify platform explicitly:**

```dockerfile
# Multi-platform support
FROM --platform=$BUILDPLATFORM rust:1.88-bookworm AS builder

```

**Improve .dockerignore:**

```text
# .dockerignore
target/
.git/
.github/
*.md
.env*
.vscode/
.idea/
**/.DS_Store

```

**Test Docker build in clean environment:**

```bash
# Simulate CI environment
docker build --no-cache --progress=plain -t test .

# Or use BuildKit (shows more details)
DOCKER_BUILDKIT=1 docker build --no-cache -t test .

```

---

### Pattern 7: Clippy Lints in Test Code

#### Symptom

```text

CI clippy step fails with:
error: this `if` statement can be collapsed
  --> src/room.rs:142:9
   |
   = help: for further information visit https://rust-lang.github.io/rust-clippy/master/index.html#collapsible_if
   = note: `-D clippy::collapsible-if` implied by `-D warnings`

```

#### Root Cause

The CI clippy command uses `--all-targets`, which compiles and lints test code
(`#[cfg(test)]` modules and integration tests) in addition to production code.
Lints like `collapsible_if`, `needless_return`, and `single_match` are commonly
introduced in test code because developers focus on correctness rather than style
when writing tests.

#### Solution

**Run clippy with `--all-targets` locally before pushing:**

```bash

cargo clippy --all-targets --all-features -- -D warnings

```

The `--all-targets` flag ensures test code, benchmarks, and examples are all
compiled and linted — matching what CI does.

---

## Diagnostic Workflow

When CI fails, work through this systematic diagnostic process:

### Step 1: Identify Failure Type

```text

CI Failure
    |
    ├─ Compilation error ──► Check Rust version, dependencies, features
    ├─ Test failure ───────► Check env vars, filesystem case, test data
    ├─ Lint failure ───────► Check clippy version, lint configuration
    ├─ Cache error ────────► Check cache keys, action versions
    ├─ Docker error ───────► Check base image, build context, COPY paths
    ├─ Workflow error ─────► Check syntax, permissions, secrets
    ├─ Exit code 127 ─────► Check script references exist in repo
    └─ Supply chain risk ──► Check action SHA pins (not tags)

```

### Step 2: Check Recent Changes

```bash
# What changed since last successful run?
git diff HEAD~1 HEAD -- .github/workflows/

# Did we update dependencies?
git diff HEAD~1 HEAD -- Cargo.toml Cargo.lock

# Did we change Rust version?
git diff HEAD~1 HEAD -- rust-toolchain.toml clippy.toml Dockerfile

```

### Step 3: Reproduce Locally

```bash
# Match CI environment exactly:
# 1. Rust version from rust-toolchain.toml
# 2. Clean build (no cache)
# 3. Exact feature flags from workflow

cargo clean
rustc --version  # Verify matches MSRV
cargo test --locked --all-features

```

### Step 4: Compare Configurations

```bash
# Check consistency across files
./scripts/check-msrv-consistency.sh

# Check for ecosystem mismatches
grep -r "pip\|npm\|bundle" .github/workflows/  # Should be empty for Rust-only project
grep -r "cargo\|rust" .github/workflows/       # Should be present

```

### Step 5: Check Staleness

```bash
# Check age of pinned versions
grep -E "nightly-[0-9]{4}-[0-9]{2}-[0-9]{2}" .github/workflows/
# Are any >6 months old?

# Check action SHA pins
grep -E "uses: .+@[a-f0-9]{40}" .github/workflows/
# Are any from >1 year ago?

# Check Docker base images
grep "FROM rust:" Dockerfile
# Is version current or outdated?
```

---

## Prevention Checklist (Agent Self-Review)

Before committing workflow changes, verify:

### Configuration Matching

- [ ] Workflow uses caching appropriate for project language (Rust = `rust-cache`, not pip/npm)
- [ ] All cache paths reference files that actually exist (e.g., Cargo.lock, not requirements.txt)
- [ ] Base images match project MSRV (Docker `FROM rust:X.Y` = Cargo.toml `rust-version`)
- [ ] No language-specific commands for wrong ecosystem (no `pip install` in Rust project)

### File Reference Integrity

- [ ] Dockerfile `COPY`/`ADD` sources all exist in the repository
- [ ] Workflow `run:` script references (`.sh` files) all exist in the repository
- [ ] Steps with `continue-on-error: true` are not masking missing file errors

### Version Consistency

- [ ] MSRV consistent across: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, Dockerfile
- [ ] Pinned nightly toolchains documented with age and update criteria
- [ ] All action `uses:` references are SHA-pinned (`@<40-char-sha> # vX.Y.Z`), not tag-only
- [ ] Action SHA pins are recent (<1 year) or have documented reason for age
- [ ] Docker base images are recent (<6 months) or have documented reason for age

### Dependency Hygiene

- [ ] Unused dependencies removed (cargo machete passes)
- [ ] All dependencies support project MSRV
- [ ] No unmaintained dependencies (>1 year inactive)
- [ ] cargo deny check passes (security, licenses)

### Documentation

- [ ] Workflow has header comment explaining purpose
- [ ] Pinned versions have comments explaining update criteria
- [ ] Magic numbers documented (timeouts, retry counts)
- [ ] Changes to workflows documented in commit message

### Testing

- [ ] Workflow tested in CI (push to branch, verify passes)
- [ ] Local reproduction verified (cargo commands match CI)
- [ ] Both feature configurations tested (default and --all-features)
- [ ] Cache invalidation tested (workflow runs correctly with cold cache)

---

## Quick Reference: Common Error Messages

| Error Message | Root Cause | Fix |
|---------------|------------|-----|
| `Cache entry deserialization failed` | Wrong cache type or corrupted | Use language-appropriate caching or bust cache |
| `Unable to locate executable file: pip` | Python tools on Rust project | Remove Python-specific actions/commands |
| `requires rustc X.Y.Z or newer` | Dependency needs newer Rust than MSRV | Update MSRV or pin older dependency version |
| `use of unstable library feature` | Code uses feature not in MSRV | Update MSRV or use stable alternative |
| `No such file or directory` (case-sensitive) | Linux CI vs macOS/Windows local | Fix import case to match filename exactly |
| `unused dependency` | Dependency in Cargo.toml but not used | Remove from Cargo.toml or document reason to keep |
| `Permission denied` | Workflow needs additional permissions | Add `permissions:` section to workflow |
| `Resource not accessible by integration` | GitHub token lacks permission | Grant required permission in `permissions:` |
| `regex parse error` in lychee | `.lychee.toml` `exclude` uses glob syntax instead of regex | Escape `.` as `\\.`, `{}` as `\\{\\}`, use `.*` not `*` |
| Script exits with code 1, no error message | `grep` found no matches under `set -euo pipefail` | Add `\|\| true` after grep, or use AWK instead |
| False positive broken links in code blocks | Link checker scans inside fenced code blocks | Use AWK with fence tracking and inline code stripping |
| YAML parse error in markdown file | Non-YAML content in `yaml`-fenced code block | Use `text` for logs, `bash` for shell; split mixed blocks |
| Lychee reports broken URL from `.lychee.toml` | Lychee scans its own config and extracts partial URLs from regex | Exclude `.lychee.toml` via `--exclude-path` or add truncated URL exclusions |
| Test assertion fails on config regex pattern | `contains("http://localhost")` vs regex `^https?://localhost` | Test regex behavior (compile + match), not literal substrings |
| `failed to calculate checksum of ref: "/path": not found` | Dockerfile `COPY` references a path removed from the repo | Remove or update stale `COPY` instructions in Dockerfile |
| Action behavior changes without workflow edit | `uses:` references a mutable tag (`@v4.2.2`) instead of SHA pin | Pin with `uses: owner/repo@<40-char-sha> # vX.Y.Z` |
| `No such file or directory` (exit code 127) in workflow | `run:` step calls a script that was deleted or renamed | Remove stale script reference or update path; audit `continue-on-error` steps |
| `toolchain 'X.Y.Z' is not installed` in cargo-deny | Docker-based action uses own toolchain; `rust-toolchain.toml` override fails | Set `RUSTUP_TOOLCHAIN: stable` env var on the action step |
| Lychee scans dotfiles despite config | lychee v0.21.0 bug #1936: hidden file option ignored by file matcher | Pin `lycheeVersion: v0.22.0` or later in the action config |
| `exclude_path` in `.lychee.toml` has no effect on glob-expanded files | `exclude_path` TOML entries do not apply to glob-expanded paths (confirmed bug) | Use `--exclude-path` CLI flags instead; separate flags from globs with `--` |
| TOML validator fails on "before/after" example block | Single TOML block with duplicate keys (e.g., two `[dependencies]` headers) | Split into separate fenced code blocks (one "before", one "after") |

---

## Pattern 7: Typos Configuration Issues (extend-words vs extend-identifiers)

### Symptom

```text
CI fails with:
ERROR: Typo found: HashiCorp (did you mean: Hashicorp?)
ERROR: Typo found in file.md:42: HashiCorp

```

Even though you've added `hashicorp = "hashicorp"` to `.typos.toml`.

### Root Cause

**Mixed-case company names and proper nouns MUST use `[default.extend-identifiers]`, not `[default.extend-words]`.**

The `typos` spell checker has two distinct configuration sections:

1. **`[default.extend-words]`** - For lowercase technical terms (e.g., `tokio`, `axum`, `websocket`)
2. **`[default.extend-identifiers]`** - For mixed-case identifiers (e.g., `HashiCorp`, `WebSocket`, `CamelCase`)

**Why this matters:**

- `extend-words` uses case-insensitive matching for lowercase terms
- `extend-identifiers` preserves exact case for mixed-case terms
- CamelCase and PascalCase names are treated as identifiers by typos
- Company names with specific capitalization (HashiCorp, GitHub) require exact case matching

### Solution

**A. Use `[default.extend-identifiers]` for mixed-case terms:**

```toml
# .typos.toml

[default.extend-words]
# Lowercase technical terms
axum = "axum"
tokio = "tokio"
websocket = "websocket"
rustc = "rustc"

[default.extend-identifiers]
# Mixed-case company names and proper nouns
HashiCorp = "HashiCorp"  # Company name (capital H, capital C)
GitHub = "GitHub"        # Company name (capital H)
WebSocket = "WebSocket"  # Protocol name (capital W, capital S)

```

**B. Add both lowercase and mixed-case variants if needed:**

```toml

[default.extend-words]
# Lowercase variant (for general use)
hashicorp = "hashicorp"
github = "github"
websocket = "websocket"

[default.extend-identifiers]
# Mixed-case variant (for proper nouns)
HashiCorp = "HashiCorp"
GitHub = "GitHub"
WebSocket = "WebSocket"

```

### Common Mixed-Case Terms That Need extend-identifiers

**Company names:**

- `HashiCorp` (Terraform, Vault)
- `GitHub` (platform name)
- `GitLab`
- `MongoDB`
- `PostgreSQL`

**Protocol/Technology names:**

- `WebSocket` (networking protocol)
- `WebRTC` (real-time communication)
- `JavaScript`
- `TypeScript`

**Project/Product names:**

- `CamelCase` identifiers in code
- `PascalCase` type names
- Mixed-case project names

### Prevention

**Pattern A: Always add mixed-case terms to extend-identifiers:**

```toml
# ❌ WRONG: Mixed-case in extend-words
[default.extend-words]
HashiCorp = "HashiCorp"  # Won't work - needs exact case matching

# ✅ CORRECT: Mixed-case in extend-identifiers
[default.extend-identifiers]
HashiCorp = "HashiCorp"  # Works - preserves exact case

```

**Pattern B: Organize .typos.toml by category:**

```toml

[default.extend-words]
# === Rust Crates ===
tokio = "tokio"
axum = "axum"

# === Build Tools ===
dockerfile = "dockerfile"
nightly = "nightly"

# === Technical Terms ===
websocket = "websocket"
async = "async"

[default.extend-identifiers]
# === Code Identifiers ===
params = "params"
consts = "consts"

# === Proper Nouns (Company Names) ===
HashiCorp = "HashiCorp"
GitHub = "GitHub"

# === Protocol Names ===
WebSocket = "WebSocket"
WebRTC = "WebRTC"

```

### Testing .typos.toml Configuration

**Run typos locally to verify configuration:**

```bash
# Check all files
typos

# Check specific file
typos path/to/file.md

# Show what would be fixed
typos --write-changes

# Verify configuration is valid
typos --dump-config

```

**Add CI test to validate .typos.toml exists:**

```rust

// tests/ci_config_tests.rs

#[test]
fn test_typos_config_exists_and_is_valid() {
    let typos_config = repo_root().join(".typos.toml");

    assert!(
        typos_config.exists(),
        ".typos.toml is required for spell checking in CI"
    );

    let content = read_file(&typos_config);

    // Verify both required sections exist
    assert!(
        content.contains("[default.extend-words]"),
        ".typos.toml must have [default.extend-words] section"
    );

    assert!(
        content.contains("[default.extend-identifiers]"),
        ".typos.toml must have [default.extend-identifiers] section"
    );
}

#[test]
fn test_typos_config_has_common_rust_terms() {
    let content = read_file(".typos.toml");

    // Verify common Rust terms are whitelisted
    let required_terms = vec![
        "tokio", "axum", "serde", "async",
        "rustc", "clippy", "rustfmt"
    ];

    for term in required_terms {
        assert!(
            content.contains(&format!("{} = \"{}\"", term, term)),
            ".typos.toml should include common Rust term: {}",
            term
        );
    }
}

```

### Key Insights

**Why extend-identifiers is needed:**

- Typos uses CamelCase splitting internally
- `HashiCorp` is treated as `Hash` + `I` + `Corp` (identifier components)
- `extend-words` only handles lowercase, unsplit words
- `extend-identifiers` handles case-sensitive, potentially-split identifiers

**When to use each section:**

| Term Type                  | Section               | Example                            |
|----------------------------|---------------------- |------------------------------------|
| Lowercase technical term   | `extend-words`        | `tokio`, `axum`, `rustc`           |
| Lowercase abbreviation     | `extend-words`        | `async`, `impl`, `config`          |
| Mixed-case company name    | `extend-identifiers`  | `HashiCorp`, `GitHub`              |
| Mixed-case protocol        | `extend-identifiers`  | `WebSocket`, `WebRTC`              |
| Code identifier            | `extend-identifiers`  | `params`, `consts`, `stdin`        |
| CamelCase code             | `extend-identifiers`  | `CamelCase`, `PascalCase`          |

**Documentation pattern:**

```toml
# Always comment why a term is whitelisted

[default.extend-words]
# Build tools and infrastructure
hashicorp = "hashicorp"  # HashiCorp (lowercase variant)
dockerfile = "dockerfile"

[default.extend-identifiers]
# Proper nouns and company names with mixed case
HashiCorp = "HashiCorp"  # Company name, proper capitalization

```

---

## Pattern 9: Documentation Quality Issues (Markdown Linting, Spell Checking)

### Symptom

```text

CI fails with:
ERROR: MD040/fenced-code-language: Fenced code blocks should have a language specified
ERROR: MD060/table-alignment: Table column alignment is inconsistent
ERROR: typos found: HashiCorp (did you mean: Hashicorp?)

```

### Root Causes

**A. Code blocks without language identifiers:**

```markdown

❌ WRONG: Missing language identifier

(triple backticks with no language)
some code here
(triple backticks)

✅ CORRECT: Language identifier specified

(triple backticks)bash
some code here
(triple backticks)

```

**B. Spell checker flagging technical terms:**

```text
# CI fails because .typos.toml doesn't whitelist technical terms
Error: Unknown word: HashiCorp
Error: Unknown word: WebSocket
Error: Unknown word: rustc

```

**C. Table formatting inconsistencies:**

```markdown

❌ WRONG: Inconsistent table column alignment
| Column | Value |
|--------|-------|
|  foo   | bar  |

```

**D. Markdown linting not run locally:**

- Developers push without validating markdown
- CI catches issues that could have been fixed locally
- No pre-commit hook to catch markdown issues
- No VS Code extension for real-time feedback

### Solution

**A. Add language identifiers to all code blocks:**

```bash
# Find code blocks without language identifiers
grep -r '^```$' --include="*.md" .

# Manual fix: Add language after opening backticks
# Examples: ```bash, ```rust, ```json, ```text

# Automated fix with markdownlint-cli2:
./scripts/check-markdown.sh fix

```

**B. Configure spell checker to whitelist technical terms:**

```toml
# .typos.toml - Add technical terms
[default.extend-words]
hashicorp = "hashicorp"  # HashiCorp company name
websocket = "websocket"  # WebSocket protocol
rustc = "rustc"          # Rust compiler
axum = "axum"            # Axum web framework
tokio = "tokio"          # Tokio async runtime

```

**C. Use automated markdown formatter:**

```bash
# Auto-fix table alignment and other issues
markdownlint-cli2 --fix '**/*.md' '#target/**' '#third_party/**'

```

**D. Set up local validation tools:**

```bash
# Install markdown linter
npm install -g markdownlint-cli2

# Add to pre-commit hook
echo "scripts/check-markdown.sh" >> .githooks/pre-commit

# Install VS Code extensions
# - davidanson.vscode-markdownlint
# - streetsidesoftware.code-spell-checker
```

### Prevention

**Create validation script:**

```bash
#!/usr/bin/env bash
# scripts/check-markdown.sh

set -euo pipefail

if ! command -v markdownlint-cli2 &> /dev/null; then
    echo "ERROR: markdownlint-cli2 not installed"
    echo "Install: npm install -g markdownlint-cli2"
    exit 2
fi

# Check markdown files (excluding build artifacts)
markdownlint-cli2 '**/*.md' '#target/**' '#third_party/**' '#node_modules/**'

```

**Add CI config validation tests:**

```rust

// tests/ci_config_tests.rs

#[test]
fn test_markdown_files_have_language_identifiers() {
    // Find all markdown files
    let markdown_files = find_markdown_files(&repo_root());

    for file in markdown_files {
        let content = read_file(&file);

        for (line_num, line) in content.lines().enumerate() {
            // Check for opening code fence without language
            let fence_marker = "```";  // Three backticks
            if line.trim_start().starts_with(fence_marker) {
                let fence_content = line.trim_start()
                    .trim_start_matches('`')
                    .trim();

                assert!(
                    !fence_content.is_empty(),
                    "{}:{}: Code block missing language identifier (MD040)",
                    file.display(),
                    line_num + 1
                );
            }
        }
    }
}

#[test]
fn test_typos_config_exists() {
    let typos_config = repo_root().join(".typos.toml");

    assert!(
        typos_config.exists(),
        ".typos.toml is required for spell checking in CI"
    );

    let content = read_file(&typos_config);

    assert!(
        content.contains("[default.extend-words]"),
        ".typos.toml must have [default.extend-words] section"
    );
}

#[test]
fn test_markdownlint_config_exists() {
    let config = repo_root().join(".markdownlint.json");

    assert!(
        config.exists(),
        ".markdownlint.json is required for markdown linting"
    );

    let content = read_file(&config);

    // Verify MD040 rule is configured
    assert!(
        content.contains("MD040"),
        ".markdownlint.json must include MD040 rule"
    );
}

```

**Document in pre-commit hook:**

```bash
# .githooks/pre-commit

# Check markdown files (if markdownlint-cli2 is installed)
if command -v markdownlint-cli2 >/dev/null 2>&1; then
    echo "[pre-commit] Checking markdown files..."
    if ! scripts/check-markdown.sh; then
        echo "[pre-commit] ERROR: Markdown linting failed"
        echo "[pre-commit] To auto-fix: ./scripts/check-markdown.sh fix"
        exit 1
    fi
else
    echo "[pre-commit] Skipping markdown check (markdownlint-cli2 not installed)"
fi

```

**Add VS Code integration:**

```jsonc

// .vscode/extensions.json
{
  "recommendations": [
    "davidanson.vscode-markdownlint",
    "streetsidesoftware.code-spell-checker"
  ]
}

// .vscode/settings.json
{
  "markdownlint.config": {
    "MD040": true,  // Require language identifiers on code blocks
    "MD013": false  // Disable line length (too strict for technical docs)
  },
  "cSpell.words": [
    "rustc",
    "tokio",
    "axum",
    "HashiCorp",
    "WebSocket"
  ]
}

```

### Key Patterns

**Pattern A: Check ALL markdown files, not just docs/:**

```bash
# ❌ WRONG: Only checks docs/ directory
markdownlint-cli2 'docs/**/*.md'

# ✅ CORRECT: Checks all markdown files in repository
markdownlint-cli2 '**/*.md' '#target/**' '#third_party/**'

```

**Pattern B: Markdown linting should be part of the standard workflow:**

```yaml
# .github/workflows/doc-validation.yml
jobs:
  markdownlint:
    runs-on: ubuntu-latest
    steps:

      - uses: actions/checkout@<SHA>

      - name: Setup Node.js

        uses: actions/setup-node@<SHA>
        with:
          node-version: '20'

      - name: Install markdownlint-cli2

        run: npm install -g markdownlint-cli2

      - name: Check markdown files

        run: markdownlint-cli2 '**/*.md' '#target/**' '#third_party/**'

```

**Pattern C: Configuration files need validation tests:**

```rust

// Always test that required config files exist and are valid
#[test]
fn test_required_config_files_exist() {
    assert!(Path::new(".typos.toml").exists());
    assert!(Path::new(".markdownlint.json").exists());
    assert!(Path::new(".githooks/pre-commit").exists());
}

```

**Pattern D: Auto-fix capability is essential:**

```bash
# Always provide an auto-fix option for markdown issues
./scripts/check-markdown.sh       # Check only
./scripts/check-markdown.sh fix   # Auto-fix where possible

```

### Common Markdown Linting Rules

| Rule | Description | Fix |
|------|-------------|-----|
| **MD040** | Code blocks must have language identifiers | Add language after \`\`\` (bash, Rust, json, text) |
| **MD060** | Table column alignment inconsistent | Use consistent spacing in table columns |
| **MD013** | Line length limit (often disabled for technical docs) | Break long lines or disable rule |
| **MD041** | First line must be top-level heading | Add `# Title` as first line |
| **MD046** | Code block style (fenced vs indented) | Use fenced code blocks (\`\`\`) consistently |

### Spell Checking Best Practices

**Technical terms that commonly need whitelisting:**

```toml
# .typos.toml
[default.extend-words]
# Rust ecosystem
rustc = "rustc"
tokio = "tokio"
axum = "axum"
serde = "serde"
clippy = "clippy"

# Build tools and infrastructure
hashicorp = "hashicorp"
github = "github"
gitlab = "gitlab"
dockerfile = "dockerfile"

# Game engines and networking
godot = "godot"
websocket = "websocket"
webrtc = "webrtc"
signaling = "signaling"

# Common technical abbreviations
msrv = "msrv"  # Minimum Supported Rust Version
cicd = "cicd"  # CI/CD
api = "api"
json = "json"
yaml = "yaml"
toml = "toml"

```

**Organization-specific terms:**

```toml

[default.extend-words]
# Project-specific terms
matchbox = "matchbox"
signalfish = "signalfish"

# Company names
ambiguous = "ambiguous"

```

---

## Pattern 10: Link Check Failures (lychee)

### Symptom

```text
CI fails with:
ERROR: Failed to check links in documentation
✗ [404] https://example.com/broken-link | docs/guide.md:42:15
✗ [FILE] docs/missing-file.md | README.md:10:5
```

### Root Causes

**A. Placeholder URLs in test fixtures or documentation examples:**

```markdown
<!-- Documentation example that shouldn't be validated -->
Example error message format:
  Broken link: https://github.com/owner/repo/issues/123
```

**B. Case-sensitive path mismatch (Linux vs macOS/Windows):**

```markdown
<!-- ❌ WRONG: Case mismatch -->
See [testing guide](Skills/testing-strategies.md)
<!-- Actual file: skills/testing-strategies.md -->
```

**C. External link rot (third-party sites changed/removed):**

```text
✗ [404] https://external-site.com/old-path
```

**D. Relative path errors:**

```markdown
<!-- ❌ WRONG: Incorrect relative path -->
[config](config.md)  <!-- Should be: ../config.md or ./docs/config.md -->
```

### Solution

**A. Exclude placeholder URLs by pattern in `.lychee.toml`:**

```toml
# .lychee.toml
exclude = [
    # Test fixture and example URLs (from tests/ci_config_tests.rs)
    "https://github.com/owner/repo/*",     # Template placeholder
    "https://github.com/%7B%7B%7D/*",      # URL-encoded {{{}}} placeholder
    "https://github.com/{}/*",             # Brace placeholder
    "https://example.com/*",               # RFC 2606 example domain
    "http://localhost*",                   # Local development
]
```

**Why exclude by pattern, not file path:**

- Allows placeholder URLs in test fixtures without excluding the entire file
- Other links in the same file are still validated
- Prevents false positives from documentation examples

**B. Fix case sensitivity issues:**

```bash
# Find actual filename case
find . -name "testing-strategies.md" -type f
# Output: ./skills/testing-strategies.md (lowercase 's')

# Fix link to match exactly
sed -i 's|Skills/testing-strategies.md|skills/testing-strategies.md|g' docs/*.md
```

**Prevention:**

```bash
# Use tab completion when creating links (respects case)
# Test on Linux before pushing (WSL, Docker, or CI)
```

**C. Update or remove broken external links:**

```markdown
<!-- If link is permanently broken, remove or replace -->
<!-- If temporarily broken, add to .lychee.toml temporarily -->
exclude = [
    "https://temporarily-down-site.com/*",
]
```

**D. Fix relative path issues:**

```markdown
✅ CORRECT: Relative path from current file
<!-- From: docs/guide.md -->
[config](../config.md)          <!-- Up one directory -->
[other](./development.md)       <!-- Same directory -->
```

### Prevention

**Add CI test to validate link configuration:**

```rust
// tests/ci_config_tests.rs

#[test]
fn test_lychee_config_exists_and_valid() {
    let lychee_config = repo_root().join(".lychee.toml");

    assert!(
        lychee_config.exists(),
        ".lychee.toml is required for link checking in CI"
    );

    let content = read_file(&lychee_config);

    // Verify critical exclusions are present
    assert!(
        content.contains("exclude = ["),
        ".lychee.toml must have exclusion patterns"
    );

    // Verify placeholder URL exclusions
    assert!(
        content.contains("https://example.com/*")
            || content.contains("localhost"),
        ".lychee.toml should exclude placeholder/localhost URLs"
    );
}

#[test]
fn test_markdown_links_case_sensitive() {
    // Verify all markdown links use correct case
    for md_file in find_markdown_files() {
        let content = read_file(&md_file);
        let links = extract_internal_links(&content);

        for (line_num, link) in links {
            let target = resolve_link_target(&md_file, &link);

            if let Some(target_path) = target {
                assert!(
                    target_path.exists(),
                    "{}:{}: Broken link (case sensitivity?): {}",
                    md_file.display(),
                    line_num,
                    link
                );
            }
        }
    }
}
```

**Document link validation in development workflow:**

```markdown
# docs/development.md

## Before Committing

lychee --config .lychee.toml './**/*.md'

# Fix broken links or add to exclusions
```

### Key Insights

**Link validation is environment-specific:**

1. **Filesystem case sensitivity** - macOS/Windows are case-insensitive, Linux is case-sensitive
2. **External link availability** - Sites change, get rate-limited, or go offline temporarily
3. **Placeholder vs real URLs** - Documentation examples shouldn't cause CI failures

**Lychee configuration best practices:**

- `exclude` field is for URL patterns (**regex**, not globs) -- see Pattern 13 below
- Use URL patterns to exclude placeholder links while keeping real ones
- File path filtering is done via CLI args, not config
- Test locally before pushing: `lychee --config .lychee.toml './**/*.md'`

**When to exclude vs fix:**

| Scenario | Action | Rationale |
|----------|--------|-----------|
| Placeholder URL in test fixture | Exclude by pattern | Intentional example, not a real link |
| Broken external link | Fix or replace | Real documentation should work |
| Temporarily unavailable site | Exclude temporarily | Re-enable when site returns |
| localhost/example.com URLs | Exclude permanently | RFC 2606 reserved domains |
| Case mismatch | Fix link case | Must work on Linux |

---

## Pattern 11: cargo-deny CVSS 4.0 Parsing Issue

### Symptom

```text
# cargo-deny fails with CVSS 4.0 parsing errors
Error: failed to parse advisory database
Error: CVSS v4.0 vectors are not supported by this version
ERROR: cargo-deny-action v2.0.5 cannot parse CVSS 4.0 entries

```

### Root Cause

**cargo-deny-action versions prior to v2.0.15 cannot parse CVSS 4.0 entries** in the RustSec advisory database.

The RustSec advisory database was updated to include CVSS 4.0 vulnerability scores.
Older versions of cargo-deny (using rustsec < 0.31) cannot parse these entries and fail.

**Why this matters:**

- CVSS 4.0 is the newest Common Vulnerability Scoring System standard
- RustSec advisory database now includes CVSS 4.0 scores for newer vulnerabilities
- Using old cargo-deny versions causes CI failures when new advisories are published
- Security audits become blocked, preventing detection of real vulnerabilities

### Solution

**Update to cargo-deny-action v2.0.15 or later:**

```yaml
# ❌ WRONG: Old version cannot parse CVSS 4.0
- uses: EmbarkStudios/cargo-deny-action@f20e90f289e90a40fd814d92ea2935d9db5da04f # v2.0.5

# ✅ CORRECT: v2.0.15+ includes rustsec 0.31 with CVSS 4.0 support
- uses: EmbarkStudios/cargo-deny-action@44db170f6a7d12a6e90340e9e0fca1f650d34b14 # v2.0.15

  with:
    arguments: --all-features

```

**Key changes in v2.0.15:**

1. Updates rustsec dependency to 0.31+ (CVSS 4.0 support)
2. Handles both CVSS 3.x and CVSS 4.0 advisory entries
3. Backward compatible with existing configurations

### Prevention

**Add test to enforce minimum cargo-deny-action version:**

```rust

// tests/ci_config_tests.rs

#[test]
fn test_cargo_deny_action_version_supports_cvss_4() {
    let ci_workflow = read_file(".github/workflows/ci.yml");

    // Extract cargo-deny-action version
    let deny_line = ci_workflow
        .lines()
        .find(|line| line.contains("cargo-deny-action@"))
        .expect("cargo-deny-action not found in ci.yml");

    // Version should be v2.0.15 or later (CVSS 4.0 support)
    assert!(
        deny_line.contains("v2.0.15")
            || deny_line.contains("v2.0.16")
            || deny_line.contains("v2.1")
            || deny_line.contains("v3"),
        "cargo-deny-action must be v2.0.15+ for CVSS 4.0 support.\n\
         Found: {}\n\
         Fix: Update to EmbarkStudios/cargo-deny-action@<SHA> # v2.0.15",
        deny_line.trim()
    );
}

```

**Document version requirement:**

```yaml
# .github/workflows/ci.yml
# cargo-deny v2.0.15+ required for CVSS 4.0 advisory parsing
# Earlier versions fail when RustSec DB includes CVSS 4.0 entries
- uses: EmbarkStudios/cargo-deny-action@44db170f6a7d12a6e90340e9e0fca1f650d34b14 # v2.0.15


```

### Key Insights

**Why version pinning matters for security tools:**

1. **Advisory database evolves** - New vulnerability scoring systems get added
2. **Old tools break** - Incompatible with new formats
3. **Security blocked** - Can't run audits when tool fails to parse
4. **Silent failures** - May not notice until a new CVSS 4.0 advisory is published

**When to update security tool versions:**

| Trigger                          | Action                                      | Urgency    |
|----------------------------------|---------------------------------------------|------------|
| Parsing error in CI              | Update immediately                          | Critical   |
| New CVSS version released        | Update proactively within 1 month           | High       |
| Security tool >6 months old      | Review for updates                          | Medium     |
| Quarterly maintenance            | Check for updates and improvements          | Low        |

**Testing pattern:**

```bash
# Test cargo-deny locally with latest advisory DB
cargo install cargo-deny --locked
cargo deny check advisories

# If this fails with CVSS 4.0 error, your cargo-deny is too old
# Update to 0.16+ which includes rustsec 0.31+
```

### Related Pattern: cargo-deny Docker Container Toolchain Mismatch

**Problem:** `cargo-deny-action` runs in its own Docker container with its own
Rust toolchain. If `rust-toolchain.toml` pins a specific version (e.g., 1.88.0),
the container may not have that exact version installed, causing build failures
or unexpected behavior.

**Symptom:**

```text
error: toolchain '1.88.0' is not installed
# OR
error: override toolchain 1.88.0 is not installed
```

**Root Cause:** The action's Docker image has a stable Rust toolchain, but
`rust-toolchain.toml` in the repo forces a different version inside the container.

**Fix:** Set `RUSTUP_TOOLCHAIN: stable` as an environment variable on the
cargo-deny step. This overrides `rust-toolchain.toml` inside the container.
This is safe because cargo-deny only inspects metadata and `Cargo.lock` -- it
does not compile code, so the exact Rust version is irrelevant.

```yaml
- name: Run cargo-deny
  uses: EmbarkStudios/cargo-deny-action@<SHA> # v2.0.15
  env:
    RUSTUP_TOOLCHAIN: stable  # Override rust-toolchain.toml inside container
  with:
    arguments: --all-features
```

**Key Insight:** Any Docker-based GitHub Action that runs its own Rust toolchain
may conflict with `rust-toolchain.toml`. Use `RUSTUP_TOOLCHAIN` env var to
override when the action does not need the project's exact Rust version.

---

### Related Pattern: Scheduled Security Audits

**Problem:** Security audits only ran on code changes, missing new CVEs published
between commits.

**Solution:** Add daily cron schedule for dependency audit job:

```yaml

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  schedule:
    # Daily security audit at noon UTC to catch new CVEs

    - cron: '0 12 * * *'


```

**Benefits:**

- Detects new vulnerabilities published overnight
- Catches advisories added to RustSec database
- Alerts team to new CVEs even without code changes
- Proactive security posture

**Best practice:** Run security tools on a schedule, not just on push/PR.

---

## Pattern 12: Git Hook Permission Issues

### Symptom

```text
# Git hooks fail to execute
error: cannot run .git/hooks/pre-commit: Permission denied

# OR in CI
ERROR: Script ./scripts/check-markdown.sh is not executable
Fix: chmod +x ./scripts/check-markdown.sh

```

### Root Cause

**Git hooks and scripts must be executable:**

```bash
# File exists but is not executable
ls -la .githooks/pre-commit
-rw-r--r--  1 user  staff  1401 Feb 16 18:56 pre-commit  # ← Missing +x

# Git doesn't track executable bit correctly on some systems
# Especially when copying files or cloning on Windows
```

### Solution

**A. Set file system permissions:**

```bash
# Make script executable
chmod +x .githooks/pre-commit
chmod +x scripts/check-markdown.sh
chmod +x scripts/*.sh

```

**B. Tell Git to track executable bit:**

```bash
# CRITICAL: Git needs to track the executable bit explicitly
git update-index --chmod=+x .githooks/pre-commit
git update-index --chmod=+x scripts/check-markdown.sh

# Verify it's set in Git
git ls-files -s .githooks/pre-commit
# Should show: 100755 (executable) not 100644 (regular file)
```

**C. Commit both changes:**

```bash
# Both steps are required!
chmod +x .githooks/pre-commit
git update-index --chmod=+x .githooks/pre-commit
git add .githooks/pre-commit
git commit -m "fix: ensure pre-commit hook is executable"

```

### Prevention

**Add CI test to validate script permissions:**

```rust

// tests/ci_config_tests.rs

#[test]
fn test_scripts_are_executable() {
    let directories = vec!["scripts", ".githooks"];

    for dir in directories {
        let dir_path = repo_root().join(dir);
        if !dir_path.exists() {
            continue;
        }

        for entry in std::fs::read_dir(&dir_path).unwrap() {
            let path = entry.unwrap().path();

            // Check .sh files and git hooks
            if path.extension().map(|ext| ext == "sh").unwrap_or(false)
                || (path.is_file() && path.extension().is_none())
            {
                let metadata = std::fs::metadata(&path).unwrap();

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = metadata.permissions().mode();
                    let is_executable = mode & 0o111 != 0;

                    assert!(
                        is_executable,
                        "{} is not executable.\n\
                         Fix: chmod +x {} && git update-index --chmod=+x {}",
                        path.display(),
                        path.display(),
                        path.display()
                    );
                }
            }
        }
    }
}

```

**Document the two-step process:**

```bash
# scripts/enable-hooks.sh

#!/bin/bash
set -euo pipefail

echo "Enabling git hooks..."

# Step 1: Set filesystem permissions
chmod +x .githooks/pre-commit

# Step 2: Tell Git to track executable bit
git update-index --chmod=+x .githooks/pre-commit

# Step 3: Configure Git to use .githooks directory
git config core.hooksPath .githooks

echo "Git hooks enabled successfully"

```

### Key Insights

**Two permissions are required:**

1. **Filesystem permission** (`chmod +x`) - Allows the file to be executed locally
2. **Git index permission** (`git update-index --chmod=+x`) - Tracks executable bit in Git

**Why both are needed:**

- `chmod +x` is not always tracked by Git (especially on Windows)
- `git update-index --chmod=+x` ensures executable bit is committed
- When others clone the repo, Git restores the executable bit
- Without both, hooks work locally but fail for others (or in CI)

**Common failure pattern:**

```bash
# Developer creates hook on macOS
touch .githooks/pre-commit
chmod +x .githooks/pre-commit  # ← Only sets filesystem permission
git add .githooks/pre-commit
git commit -m "Add pre-commit hook"

# CI clones on Linux
git clone repo
.githooks/pre-commit           # ← Permission denied!

```

**Correct pattern:**

```bash
# Developer creates hook
touch .githooks/pre-commit
chmod +x .githooks/pre-commit           # Filesystem permission
git update-index --chmod=+x .githooks/pre-commit  # Git permission
git add .githooks/pre-commit
git commit -m "Add pre-commit hook"

# CI clones on Linux
git clone repo
.githooks/pre-commit           # ← Works! Git restored executable bit

```

---

## Pattern 13: Lychee Config Regex Pitfalls (.lychee.toml)

### Symptom

```text
CI link checker reports errors on URLs that should be excluded,
or lychee fails to start with a regex compilation error:

ERROR: regex parse error: \.github/test-fixtures/{bad-link}.md
                                                 ^
ERROR: repetition quantifier expects a valid decimal
```

### Root Cause

**The `exclude` field in `.lychee.toml` takes regular expressions, NOT glob patterns.**

Common mistakes:

WRONG -- glob syntax where `{}` and `.` are regex metacharacters:

```toml
exclude = [
    ".github/test-fixtures/{bad-link}.md",
    "https://example.com/*.html",
]
```

CORRECT -- regex syntax with escaped metacharacters and anchored patterns:

```toml
exclude = [
    # Anchored regex with escaped metacharacters
    "^https://example\\.com/.*\\.html$",
    # Literal braces escaped
    "^\\.github/test-fixtures/\\{bad-link\\}\\.md$",
]
```

**Key metacharacters that need escaping:**

| Character | Glob meaning | Regex meaning | Fix |
|-----------|-------------|---------------|-----|
| `.` | Literal dot | Any character | Escape: `\\.` |
| `{}` | Brace expansion | Repetition quantifier | Escape: `\\{\\}` |
| `*` | Wildcard | Zero or more of previous | Use `.*` for wildcard |
| `?` | Single char | Zero or one of previous | Escape: `\\?` |
| `+` | Literal | One or more of previous | Escape: `\\+` |
| `()` | Literal | Capture group | Escape: `\\(\\)` |
| `[]` | Literal (some shells) | Character class | Escape: `\\[\\]` |

### Solution

**Always treat `.lychee.toml` `exclude` patterns as regex:**

```toml
# .lychee.toml

exclude = [
    # ✅ Escaped dots, anchored pattern
    "^https://example\\.com/",

    # ✅ Use .* instead of glob *
    "^https://github\\.com/owner/repo/.*",

    # ✅ Escaped braces for literal braces in URL
    "^https://github\\.com/%7B%7B%7D/.*",

    # ✅ Localhost patterns
    "^http://localhost",

    # ✅ Add comments explaining each exclusion
    # RFC 2606 reserved example domain
    "^https?://example\\.",
]
```

### Prevention

**Regex review checklist for `.lychee.toml`:**

- [ ] Every `.` in a domain name or file extension is escaped as `\\.`
- [ ] Patterns use `^` anchors to avoid unintended substring matches
- [ ] Globs like `*` are replaced with regex `.*`
- [ ] Literal braces `{}` are escaped as `\\{\\}`
- [ ] Each exclusion has a comment explaining why it exists
- [ ] Test patterns locally: `lychee --config .lychee.toml './**/*.md'`

---

## Pattern 14: Shell Script Validation Pitfalls (grep Exit Codes, Code Blocks)

### Symptom

```text
# CI script crashes unexpectedly
Error: Process completed with exit code 1.

# OR: CI reports broken links that are actually inside code blocks
ERROR: Broken internal link: ./nonexistent-example.md (docs/guide.md:42)
# But line 42 is inside a fenced code block example
```

### Root Causes

**A. `grep` returns exit code 1 when no matches are found:**

```bash
#!/usr/bin/env bash
set -euo pipefail   # ← -e exits on ANY non-zero exit code

# ❌ WRONG: grep returns 1 if no matches, killing the script
links=$(grep -oP '\[.*?\]\(.*?\)' "$file")

# ✅ CORRECT: Append || true to suppress no-match exit code
links=$(grep -oP '\[.*?\]\(.*?\)' "$file" || true)

# ✅ BETTER: Use AWK instead (always exits 0)
links=$(awk '/\[.*\]\(.*\)/' "$file")
```

**Why this is dangerous:**

- `set -euo pipefail` is best practice for shell scripts
- `grep` exit codes: 0 = match found, 1 = no match, 2 = error
- With `-e`, exit code 1 (no match) terminates the script identically to exit code 2 (error)
- Files with no links silently crash the validator

**B. Link extraction scans inside fenced code blocks:**

```bash
# ❌ WRONG: Scans ALL lines, including code block examples
grep -oP '\[([^\]]+)\]\(([^)]+)\)' "$file"

# ✅ CORRECT: Use AWK to skip fenced code blocks and inline code
awk '
  /^```/ { in_block = !in_block; next }
  in_block { next }
  {
    line = $0
    # Strip inline code spans before matching links
    gsub(/`[^`]+`/, "", line)
    # Now extract links from non-code content only
    while (match(line, /\[([^\]]+)\]\(([^)]+)\)/, arr)) {
      print arr[2]
      line = substr(line, RSTART + RLENGTH)
    }
  }
' "$file"
```

**D. Nested fence tracking -- simple toggle breaks on 4+ backtick fences:**

The simple `in_fence = !in_fence` toggle treats every ```` ``` ```` line as a
fence boundary. This breaks when a 4+ backtick outer fence contains 3-backtick
examples (common in documentation-about-documentation). For example, a
5-backtick fence wrapping a 3-backtick code block example: the inner
```` ``` ```` lines are **not** fence boundaries -- they are content of the
outer 5-backtick fence. The correct approach uses **fence-width tracking**:

```awk
# ✅ CORRECT: Width-based fence tracking
/^[ \t]*`{3,}/ {
    n = 0
    s = $0
    while (substr(s, 1, 1) == " " || substr(s, 1, 1) == "\t") s = substr(s, 2)
    while (substr(s, 1, 1) == "`") { n++; s = substr(s, 2) }

    if (!in_fence) {
        fence_width = n
        in_fence = 1
    } else if (n >= fence_width && s ~ /^[ \t]*$/) {
        # Closing fence: >= opening width AND no trailing content
        # (trailing whitespace is allowed per CommonMark spec)
        in_fence = 0
    }
    next
}
in_fence { next }
```

For validators (JSON/YAML/TOML/Bash) that extract content from inner fenced
blocks, use a separate `outer_fence` variable for 4+ backtick fences while the
inner block tracking handles 3-backtick fences:

```awk
# Track outer 4+ backtick fences separately from inner 3-backtick blocks
/^`{4,}/ {
    if (!outer_fence) { outer_fence = 1 } else { outer_fence = 0 }
    next
}
outer_fence { next }
# Now handle 3-backtick blocks normally for content extraction
/^```/ { in_block = !in_block; next }
```

**C. Link validator only checks files, not directories:**

```bash
# ❌ WRONG: Only checks if target is a file
if [ ! -f "$full_path" ]; then
    echo "ERROR: Broken link: $link"
fi

# ✅ CORRECT: Check both files and directories
if [ ! -f "$full_path" ] && [ ! -d "$full_path" ]; then
    echo "ERROR: Broken link: $link"
fi
```

### Prevention

**Shell script validation checklist:**

- [ ] Every `grep` in a `set -e` script has `|| true` or is wrapped in `if`
- [ ] Link extraction skips fenced code blocks (track `` ``` `` toggles)
- [ ] Link extraction strips inline code spans before matching
- [ ] Path validation checks both files (`-f`) and directories (`-d`)
- [ ] AWK is preferred over `grep` for pattern extraction in validation scripts
- [ ] Fence tracking handles nested fences (4+ backtick outer fences skip inner 3-backtick blocks)
- [ ] Closing fence check allows trailing whitespace (`s ~ /^[ \t]*$/` per CommonMark spec)

**Testing pattern:**

```bash
# Create test fixtures that exercise edge cases:
# 1. File with no links (tests grep exit code handling)
# 2. File with links inside code blocks (tests code block skipping)
# 3. File with links to directories (tests directory link handling)
# 4. File with inline code containing link-like syntax (tests inline code stripping)
```

---

## Pattern 15: Test Fixture Exclusion Consistency

### Symptom

```text
# One validator passes, another fails on the same test fixture
JSON validator: PASS (excludes .github/test-fixtures/)
YAML validator: FAIL on .github/test-fixtures/bad.yml
Link checker:   FAIL on .github/test-fixtures/broken-links.md
```

### Root Cause

**Test fixtures are excluded from some validators but not all.**

When adding test fixtures that contain intentionally invalid content, every
validator in the CI pipeline must be updated to exclude them. Common validators
that need exclusion:

1. JSON validator
2. YAML validator
3. TOML validator
4. Bash/shell validator
5. Markdown linter
6. Link checker (lychee and internal)
7. Spell checker (typos)

### Solution

**Audit ALL validators when adding test fixture directories:**

```yaml
# In doc-validation.yml or equivalent workflow

# JSON validator
- name: Validate JSON
  run: |
    find . -name '*.json' \
      ! -path './.github/test-fixtures/*' \
      ! -path './test-fixtures/*' \
      ! -path './target/*' \
      -exec jq . {} +

# YAML validator
- name: Validate YAML
  run: |
    find . -name '*.yml' -o -name '*.yaml' \
      ! -path './.github/test-fixtures/*' \
      ! -path './test-fixtures/*' \
      ! -path './target/*' \
      | xargs yamllint

# Internal link checker
- name: Check internal links
  run: |
    find . -name '*.md' \
      ! -path './.github/test-fixtures/*' \
      ! -path './test-fixtures/*' \
      ! -path './target/*' \
      | while read -r file; do
        # validate links...
      done
```

### Prevention

**Establish a single exclusion list pattern:**

```yaml
# Define exclusion paths once at the top of the workflow
env:
  EXCLUDE_PATHS: >-
    ! -path './.github/test-fixtures/*'
    ! -path './test-fixtures/*'
    ! -path './target/*'
    ! -path './node_modules/*'
```

**When adding a new test fixture directory:**

1. Search the workflow for ALL `find` commands and `grep` invocations
2. Add exclusion to every validator, not just the one you are testing
3. Verify by running the full CI pipeline, not just the modified job

**Key insight:** Test fixture exclusion is an all-or-nothing pattern. If you
exclude fixtures from one validator, you must exclude them from all validators,
because test fixtures often contain intentionally broken content across multiple
formats (invalid JSON inside a Markdown file, broken links, syntax errors, etc.).

---

## Pattern 16: YAML Validation Fails on Non-YAML Code Blocks

### Symptom

```text
CI fails with:
ERROR: YAML parse error in docs/guide.md
  mapping values are not allowed in this context
  at line 5, column 12
```

Even though the YAML in the document looks correct.

### Root Cause

**Code blocks with `yaml` language tags that contain non-YAML content**
(error logs, shell commands, AWK scripts, or mixed content) cause YAML
validators to fail when they extract and validate fenced code blocks by
language tag.

```markdown
<!-- ❌ WRONG: Tagged as yaml but contains shell + YAML mix -->
(triple backticks)yaml
$ kubectl get pods
NAME                    READY   STATUS
my-pod-abc123           1/1     Running

# config.yml output:
server:
  port: 8080
(triple backticks)
```

### Solution

**Use the correct language tag for the actual content:**

| Content Type | Correct Tag | Wrong Tag |
|--------------|-------------|-----------|
| Error logs, CLI output | `text` | `yaml` |
| Shell commands | `bash` | `yaml` |
| AWK scripts | `bash` or `awk` | `yaml` |
| Actual YAML config | `yaml` | `text` |
| Mixed shell + YAML | Split into separate blocks | Single `yaml` block |

**Split mixed-content blocks into separate blocks:**

```markdown
<!-- ✅ CORRECT: Separate blocks with appropriate tags -->
Run the command:

(triple backticks)bash
$ kubectl get pods
NAME                    READY   STATUS
my-pod-abc123           1/1     Running
(triple backticks)

The config file:

(triple backticks)yaml
server:
  port: 8080
(triple backticks)
```

### Prevention

- Before tagging a code block as `yaml`, `json`, `toml`, or `bash`,
  verify the **entire** block content is valid in that language
- Use `text` for output logs, error messages, or mixed content
- When documenting a workflow that mixes commands and config, split into
  multiple blocks rather than combining under one tag
- CI validators that extract code blocks by language tag will attempt to
  parse them -- incorrect tags cause false failures

---

## Pattern 17: Lychee Scans Its Own Config File

### Symptom

```text
CI link check reports broken URLs that don't appear in any documentation:

✗ [404] https://lib/ | .lychee.toml:8:5
✗ [404] https://crates/ | .lychee.toml:12:5
```

### Root Cause

**Lychee scans `*.toml` files and extracts partial URLs from regex
patterns inside `.lychee.toml` itself.** When the config contains exclude
patterns like `^https://lib\\.rs`, lychee extracts a truncated URL
(`https://lib/`) and attempts to check it.

```toml
# .lychee.toml
exclude = [
    "^https://lib\\.rs",     # ← lychee extracts "https://lib/" from this
    "^https://crates\\.io",  # ← lychee extracts "https://crates/" from this
]
```

### Solution

**Exclude `.lychee.toml` itself from lychee's scan, or exclude the
truncated URLs that lychee extracts from the regex patterns:**

```toml
# Option A: Exclude the config file via CLI
# lychee --exclude-path .lychee.toml ...

# Option B: Exclude the truncated URLs lychee extracts from its own patterns
exclude = [
    "^https://lib\\.rs",
    "^https://crates\\.io",
    # Self-referential: lychee extracts partial URLs from the regex
    # patterns above (e.g., "https://lib/", "https://crates/")
    "^https://lib/$",
    "^https://crates/$",
]
```

### Prevention

- When adding URL-based regex patterns to `.lychee.toml`, consider what
  partial URL lychee might extract from the pattern itself
- Use `--exclude-path .lychee.toml` in CI to avoid self-scanning entirely
- Test locally after changing `.lychee.toml`:
  `lychee --config .lychee.toml './**/*.md' './**/*.toml'`

---

## Pattern 18: Config Test Assertions vs Regex Patterns

### Symptom

```text
CI test fails:
assertion failed: content.contains("http://localhost")
  .lychee.toml should exclude localhost URLs
```

But `.lychee.toml` does exclude localhost -- via a regex pattern like
`^https?://localhost`, not a literal URL.

### Root Cause

**Test assertions use substring matching (`contains()`) against config
files that contain regex patterns, not literal strings.** The regex
`^https?://localhost` does not contain the substring `http://localhost`
as a literal match.

```rust
// ❌ WRONG: Substring check fails on regex patterns
assert!(content.contains("http://localhost"));
// The config has "^https?://localhost" which doesn't contain "http://localhost"

// ✅ CORRECT: Compile and test the regex pattern
let re = regex::Regex::new(r"^https?://localhost").unwrap();
assert!(re.is_match("http://localhost:8080"));
assert!(re.is_match("https://localhost"));
```

### Solution

**Tests that validate config files containing regex patterns should
either:**

1. Check for the regex pattern string itself (exact match)
2. Compile the regex and test it against representative URLs
3. Use a pattern match that accounts for regex syntax

```rust
// Option 1: Check for the actual regex string in the config
assert!(
    content.contains("localhost"),
    ".lychee.toml should have a localhost exclusion pattern"
);

// Option 2: Extract and compile regex patterns, then test behavior
let exclude_patterns = extract_exclude_patterns(&content);
let test_urls = vec!["http://localhost", "http://localhost:8080"];
for url in test_urls {
    assert!(
        exclude_patterns.iter().any(|re| re.is_match(url)),
        "No exclude pattern matches: {}", url
    );
}
```

### Prevention

- When testing config files that contain regex, grep, or glob patterns,
  never use `contains()` to check for a literal URL that should be matched
- Instead, test the **behavior** (does the pattern match the intended URLs?)
- Document in the test why regex-aware checking is needed

---

## Pattern 19: Lychee Version-Specific Bugs

### Bug A: Hidden File Matcher (lychee v0.21.0, #1936)

**Symptom:** Lychee scans dotfiles like `.lychee.toml` even when the hidden
file option is disabled. This causes false positives from regex patterns
extracted from the config file itself (see Pattern 17).

**Root Cause:** Lychee v0.21.0 (bundled with `lychee-action` v2.7.0) has
bug [#1936](https://github.com/lycheeverse/lychee/issues/1936): the file
matcher does not respect the hidden file option when using glob patterns
like `./**/*.toml`.

**Fix:** Pin `lycheeVersion: v0.22.0` or later in the action config:

```yaml
- name: Link Checker
  uses: lycheeverse/lychee-action@<SHA> # v2.7.0
  with:
    lycheeVersion: v0.22.0  # Fix for hidden file matcher bug #1936
    args: --verbose --no-progress './**/*.md' './**/*.toml'
```

### Bug B: `exclude_path` TOML Entries Ignored for Glob-Expanded Files

**Symptom:** Paths listed in `.lychee.toml` `exclude_path` are still scanned
when lychee receives files via glob expansion (e.g., `./**/*.md`).

**Root Cause:** `exclude_path` entries in `.lychee.toml` do **not** apply to
glob-expanded file paths. Only the CLI `--exclude-path` flags work correctly
for both directly specified and glob-expanded paths. This is a confirmed bug
in lychee v0.23.0 (and likely earlier versions).

**Fix:** Always provide `--exclude-path` on the CLI in addition to (or instead
of) the TOML config. Use `--` to separate CLI flags from positional glob
arguments:

```yaml
- name: Link Checker
  uses: lycheeverse/lychee-action@<SHA>
  with:
    args: >-
      --verbose --no-progress
      --exclude-path .lychee.toml
      --exclude-path target
      --exclude-path .github/test-fixtures
      --
      './**/*.md' './**/*.toml'
```

**Key Insight:** When excluding paths from lychee, never rely solely on
`.lychee.toml` `exclude_path`. Always duplicate critical exclusions as CLI
`--exclude-path` flags to ensure they apply regardless of how files are
discovered.

---

## Pattern 20: TOML Validation Fails on Before/After Example Blocks

### Symptom

```text
CI fails with:
ERROR: TOML parse error in docs/migration.md
  duplicate key `dependencies` at line 12
```

### Root Cause

**Documentation that shows "before/after" or "wrong/correct" TOML examples
in a single fenced code block creates invalid TOML** because the block
contains duplicate table headers (e.g., two `[dependencies]` sections).
CI validators that extract and parse `toml`-tagged code blocks will reject
this as a TOML syntax error.

```markdown
<!-- WRONG: Single block with duplicate [dependencies] -->
(triple backticks)toml
# Before:
[dependencies]
tokio = "1.48"
futures = "0.3"

# After:
[dependencies]
tokio = "1.49"
(triple backticks)
```

### Solution

**Split into separate fenced code blocks, each containing valid TOML:**

```markdown
Before:

(triple backticks)toml
[dependencies]
tokio = "1.48"
futures = "0.3"
(triple backticks)

After:

(triple backticks)toml
[dependencies]
tokio = "1.49"
(triple backticks)
```

### Prevention

- Every `toml`-tagged code block must be independently valid TOML
- "Before/after" comparisons must use separate fenced blocks
- This also applies to `json` and `yaml` blocks -- any language-tagged block
  is parsed by the corresponding CI validator

---

## Pattern 21: Dockerfile COPY Targets Referencing Non-Existent Directories

### Symptom

```text
ERROR: failed to calculate checksum of ref: "/vendor": not found
ERROR: failed to solve: failed to compute cache key: failed to calculate checksum
```

Docker build fails immediately at a `COPY` instruction because the source path
no longer exists in the repository.

### Root Cause

**When path dependencies or vendored directories are removed from the repo,
the Dockerfile `COPY` instructions that reference those paths are not updated.**

This commonly happens during dependency cleanup or refactoring:

```dockerfile
# ❌ PROBLEM: /vendor was removed from the repo but Dockerfile still copies it
COPY vendor/ /app/vendor/
COPY third_party/custom-lib/ /app/third_party/custom-lib/

# The build fails because Docker resolves COPY sources at build time
# against the build context -- missing paths are a hard error
```

**Why this is easy to miss:**

- Dependency removal PRs focus on `Cargo.toml` and source code
- Dockerfile changes are not flagged by `cargo` commands
- Local Docker builds may succeed if cached layers hide the missing path
- CI builds with `--no-cache` expose the failure immediately

### Solution

**Remove or update the stale `COPY` instructions:**

```dockerfile
# ✅ CORRECT: Only COPY paths that exist in the repository
COPY Cargo.toml Cargo.lock /app/
COPY src/ /app/src/
# Removed: COPY vendor/ /app/vendor/ (vendor directory was deleted)
```

**Audit all Dockerfile `COPY` and `ADD` instructions after removing files:**

```bash
# List all COPY/ADD sources referenced in Dockerfiles
grep -E '^\s*(COPY|ADD)\s' Dockerfile* | awk '{print $2}'

# Cross-reference against actual files in the repo
for src in $(grep -E '^\s*COPY\s' Dockerfile | awk '{print $2}'); do
    if [ ! -e "$src" ]; then
        echo "ERROR: Dockerfile references non-existent path: $src"
    fi
done
```

### Prevention

**Add CI test to validate Dockerfile `COPY` sources exist:**

```rust
// tests/ci_config_tests.rs

#[test]
fn test_dockerfile_copy_sources_exist() {
    let dockerfile = read_file("Dockerfile");

    for (line_num, line) in dockerfile.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("COPY") || trimmed.starts_with("ADD") {
            // Extract source path (second token, before the destination)
            let tokens: Vec<&str> = trimmed.split_whitespace().collect();
            if tokens.len() >= 3 {
                let source = tokens[1];
                // Skip --from= (multi-stage build references)
                if source.starts_with("--from=") {
                    continue;
                }
                let source_path = Path::new(source);
                assert!(
                    source_path.exists(),
                    "Dockerfile:{}: COPY source does not exist: {}\n\
                     Was this path removed without updating the Dockerfile?",
                    line_num + 1,
                    source
                );
            }
        }
    }
}
```

**Checklist when removing files or directories:**

- [ ] Search Dockerfiles for `COPY` and `ADD` references to the removed path
- [ ] Search `.dockerignore` for entries that reference the removed path
- [ ] Run `docker build --no-cache` locally to verify the build still works
- [ ] Check multi-stage builds -- intermediate stages may also reference the path

---

## Pattern 22: SHA Pinning Stripped from GitHub Actions Workflows

### Symptom

```yaml
# Workflow uses tag-based references instead of SHA pins
- uses: actions/checkout@v4.2.2
- uses: dtolnay/rust-toolchain@stable
```

No immediate CI failure, but the workflow is now vulnerable to supply chain
attacks. A compromised or mutable tag could silently change the action code
that runs in your CI pipeline.

### Root Cause

**When modifying workflow files, SHA pins are accidentally replaced with
tag-based references.** Tag references like `@v4.2.2` are mutable -- the
action maintainer (or an attacker who compromises their account) can point
the tag to different code at any time.

**Why SHA pinning matters:**

- Tags are Git references that can be moved to point to any commit
- An attacker who gains push access to an action repo can retag a release
- SHA pins are immutable -- a 40-character commit hash cannot be changed
- Supply chain attacks on GitHub Actions are a known threat vector

**Common ways SHA pins get stripped:**

1. Copying workflow snippets from documentation (docs use short tags)
2. Dependabot or Renovate updating to tag-only format
3. Manual edits that simplify the `uses:` line
4. IDE auto-completion suggesting tag format

### Solution

**Always use the full SHA pin with a version comment:**

```yaml
# ❌ WRONG: Mutable tag reference (supply chain risk)
- uses: actions/checkout@v4.2.2
- uses: dtolnay/rust-toolchain@stable

# ✅ CORRECT: Immutable SHA pin with version comment
- uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2
- uses: dtolnay/rust-toolchain@b3b07ba8b418998c39fb20f53e18c1f174353f47 # stable
```

**Find the SHA for a given action version:**

```bash
# Look up the commit SHA for a specific tag
gh api repos/actions/checkout/git/refs/tags/v4.2.2 --jq '.object.sha'

# Or clone and check locally
git ls-remote https://github.com/actions/checkout.git refs/tags/v4.2.2
```

**Audit existing workflows for missing SHA pins:**

```bash
# Find all action references that use tags instead of SHAs
grep -rn 'uses: .*@v[0-9]' .github/workflows/
grep -rn 'uses: .*@stable' .github/workflows/
grep -rn 'uses: .*@main' .github/workflows/
grep -rn 'uses: .*@master' .github/workflows/

# Find properly pinned actions (40-char hex SHA)
grep -rn 'uses: .*@[a-f0-9]\{40\}' .github/workflows/
```

### Prevention

**Add CI test to enforce SHA pinning:**

```rust
// tests/ci_config_tests.rs

#[test]
fn test_workflow_actions_are_sha_pinned() {
    let workflow_dir = repo_root().join(".github/workflows");

    for entry in std::fs::read_dir(&workflow_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().map(|e| e == "yml" || e == "yaml").unwrap_or(false) {
            let content = read_file(&path);

            for (line_num, line) in content.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.starts_with("uses:") || trimmed.starts_with("- uses:") {
                    // Extract the action reference after @
                    if let Some(at_pos) = trimmed.find('@') {
                        let after_at = &trimmed[at_pos + 1..];
                        let ref_part = after_at.split_whitespace().next().unwrap_or("");

                        // Must be a 40-char hex SHA
                        let is_sha = ref_part.len() == 40
                            && ref_part.chars().all(|c| c.is_ascii_hexdigit());

                        assert!(
                            is_sha,
                            "{}:{}: Action not SHA-pinned: {}\n\
                             Tags are mutable (supply chain risk).\n\
                             Fix: uses: owner/repo@<40-char-sha> # vX.Y.Z",
                            path.display(),
                            line_num + 1,
                            trimmed
                        );
                    }
                }
            }
        }
    }
}
```

**Document the SHA pin format in workflow headers:**

```yaml
# All action references MUST use SHA pins for supply chain security.
# Format: uses: owner/repo@<40-char-sha> # vX.Y.Z
# See: .llm/skills/supply-chain-security.md
```

---

## Pattern 23: Workflow Script References to Non-Existent Files

### Symptom

```text
# CI step fails (or silently passes with continue-on-error: true)
/home/runner/work/repo/repo/./scripts/verify-sccache.sh: No such file or directory
Error: Process completed with exit code 127.
```

A workflow `run:` step calls a local script that does not exist in the
repository.

### Root Cause

**When scripts are removed or renamed, workflow files that reference them
are not updated.** This is especially dangerous when the step has
`continue-on-error: true`, because the failure is silently ignored and
the workflow reports success.

```yaml
# ❌ PROBLEM: Script was deleted but workflow still calls it
- name: Verify sccache
  run: ./scripts/verify-sccache.sh
  continue-on-error: true  # ← Masks the "file not found" error!
```

**Common scenarios:**

1. Script renamed (e.g., `verify-sccache.sh` to `check-cache.sh`) without updating workflows
2. Script directory restructured (e.g., `scripts/` to `.github/scripts/`)
3. Script deleted as part of cleanup but workflow reference remains
4. Script only exists on a different branch (feature branch merged without the script)

### Solution

**Remove or update the stale script reference:**

```yaml
# Option A: Remove the step entirely if the script is no longer needed
# (deleted the verify-sccache step)

# Option B: Update the path to the new script location
- name: Verify sccache
  run: ./.github/scripts/verify-sccache.sh

# Option C: Inline the script if it was simple
- name: Verify sccache
  run: |
    if command -v sccache >/dev/null 2>&1; then
      sccache --show-stats
    else
      echo "sccache not installed, skipping"
    fi
```

**Audit all workflow script references:**

```bash
# Find all script references in workflow run: steps
grep -rn '\.sh\b' .github/workflows/ | grep -v '#' | while read -r line; do
    # Extract the script path
    script=$(echo "$line" | grep -oP '\./[^\s;|&"]+\.sh' || true)
    if [ -n "$script" ] && [ ! -f "$script" ]; then
        echo "WARNING: $line"
        echo "  Script not found: $script"
    fi
done
```

### Prevention

**Add CI test to validate workflow script references:**

```rust
// tests/ci_config_tests.rs

#[test]
fn test_workflow_script_references_exist() {
    let workflow_dir = repo_root().join(".github/workflows");

    for entry in std::fs::read_dir(&workflow_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().map(|e| e == "yml" || e == "yaml").unwrap_or(false) {
            let content = read_file(&path);

            for (line_num, line) in content.lines().enumerate() {
                // Look for script references in run: steps
                let trimmed = line.trim();
                if trimmed.starts_with("run:") || trimmed.starts_with("- run:") {
                    // Extract .sh file references
                    for word in trimmed.split_whitespace() {
                        if word.ends_with(".sh") && word.starts_with("./") {
                            let script_path = repo_root().join(
                                word.trim_start_matches("./")
                            );
                            assert!(
                                script_path.exists(),
                                "{}:{}: References non-existent script: {}\n\
                                 Was this script removed without updating the workflow?",
                                path.display(),
                                line_num + 1,
                                word
                            );
                        }
                    }
                }
            }
        }
    }
}
```

**Be cautious with `continue-on-error: true`:**

```yaml
# ❌ DANGEROUS: Silently ignores missing script
- name: Run validation
  run: ./scripts/validate.sh
  continue-on-error: true

# ✅ SAFER: Check script exists before running
- name: Run validation
  run: |
    if [ -f ./scripts/validate.sh ]; then
      ./scripts/validate.sh
    else
      echo "WARNING: ./scripts/validate.sh not found, skipping"
    fi
```

**Checklist when deleting or renaming scripts:**

- [ ] Search all workflow files for references to the old script path
- [ ] Search `Makefile`, `justfile`, and other task runners
- [ ] Search documentation for references to the script
- [ ] Update or remove `continue-on-error` steps that called the script
- [ ] Verify CI passes after the change (not just "green with silent failures")

---

## Lesson Learned: rustfmt --check on Documentation Code Blocks

`rustfmt --check` returns exit code 1 for **both** parse errors and formatting
differences, making it impossible to distinguish "not valid Rust" from "valid but
unformatted." When validating Rust code blocks in documentation, treat `rustfmt`
failures as **warnings**, not hard errors -- doc snippets are often fragments or
pseudo-code that won't parse. Reserve hard errors for `cargo clippy` / `cargo test`
on production code.

---

## Related Skills

- [GitHub-actions-best-practices](./github-actions-best-practices.md) — Workflow patterns and best practices
- [msrv-and-toolchain-management](./msrv-and-toolchain-management.md) — MSRV updates and consistency
- [dependency-management](./dependency-management.md) — Adding and auditing dependencies
- [supply-chain-security](./supply-chain-security.md) — Security audits and vulnerability scanning
- [container-and-deployment](./container-and-deployment.md) — Docker builds and deployment
- [agent-self-review-checklist](./agent-self-review-checklist.md) — Pre-commit verification checklist

---

## Real-World Examples

### Example 1: Python Cache on Rust Project (RESOLVED)

**Problem:**

```yaml
# CI workflow had Python caching for a Rust project
- uses: actions/cache@v4

  with:
    path: ~/.cache/pip
    key: ${{ runner.os }}-pip-${{ hashFiles('**/requirements.txt') }}

```

**Symptoms:**

- Cache deserialization failures
- `pip` executable not found
- CI slower than expected (cache always missing)

**Solution:**

```yaml
# Replaced with Rust-specific caching
- uses: Swatinem/rust-cache@v2.7.5


```

**Prevention:** Before adding caching, verify language ecosystem matches project.

### Example 2: 360-Day-Old Nightly Toolchain (RESOLVED)

**Problem:**

```yaml
# Nightly pinned to very old date
toolchain: nightly-2025-02-21  # 360 days old

```

**Symptoms:**

- Dependencies fail to compile with old nightly
- Features available in stable but not in old nightly
- Security vulnerabilities in old toolchain

**Solution:**

```yaml
# Updated to recent nightly
toolchain: nightly-2026-01-15  # 32 days old

```

**Prevention:** Document update criteria and review quarterly.

### Example 3: Accumulated Unused Dependencies (RESOLVED)

**Problem:**

- 15+ unused dependencies in Cargo.toml
- No regular audit process
- Dependencies added for experiments, never removed

**Solution:**

```bash
# Added weekly CI job
cargo machete  # Detect unused dependencies

# Removed unused dependencies in PR
```

**Prevention:** Weekly unused-deps workflow + document reason for optional deps.

---

## Escalation: When to Ask for Help

Self-service troubleshooting should resolve 90% of CI issues. Escalate when:

1. **Persistent cache corruption** (bust cache doesn't fix)
2. **GitHub Actions platform issues** (outage, service degradation)
3. **Upstream action breaking change** (action author made incompatible change)
4. **Security vulnerability** in pinned version (needs immediate attention)
5. **Resource limits hit** (workflow timeout, out of disk space, etc.)

---

## Summary: The Three Categories

Based on recent issues fixed in this project:

### Category 1: Configuration Mismatch

- **Example:** Python caching on Rust project
- **Detection:** Language-specific tools/paths in wrong ecosystem
- **Prevention:** Match configuration to project language
- **Fix Time:** Minutes (remove wrong config, add correct)

### Category 2: Dependency Hygiene

- **Example:** 15+ unused dependencies accumulating
- **Detection:** cargo-machete, cargo-udeps
- **Prevention:** Regular audits (weekly CI job)
- **Fix Time:** Hours (identify + remove + test)

### Category 3: Toolchain Staleness

- **Example:** Nightly from 360 days ago
- **Detection:** Age checks, compilation failures
- **Prevention:** Quarterly review, document update criteria
- **Fix Time:** Minutes (update version) to Hours (if dependencies broke)

### Category 4: Validation Script Fragility

- **Example:** `grep` exit code 1 kills script; link checker scans code blocks; lychee regex vs glob confusion
- **Detection:** CI failures with no error message, false positive broken links
- **Prevention:** Use AWK over grep in validation scripts; always `|| true` with grep;
  treat `.lychee.toml` exclude as regex; fence-aware link extraction
- **Fix Time:** Minutes (once root cause identified) but Hours (to diagnose initially)

### Category 5: Code Fence and Config File Mismatches

- **Example:** YAML validator fails on `yaml`-tagged code block containing shell output;
  lychee reports broken links from its own regex patterns; test uses `contains()` on regex config
- **Detection:** CI failures in validators that parse code blocks by language tag;
  phantom broken URLs from `.lychee.toml`; test assertion mismatches
- **Prevention:** Match code fence language tags to actual content; exclude `.lychee.toml`
  from self-scanning; test regex configs by compiling and matching, not substring search
- **Fix Time:** Minutes (once the mismatch pattern is recognized)

### Category 6: Stale File References and Supply Chain Gaps

- **Example:** Dockerfile `COPY vendor/` after vendor directory deleted; `uses: actions/checkout@v4.2.2`
  without SHA pin; workflow `run: ./scripts/verify-sccache.sh` after script removed
- **Detection:** Docker build checksum errors; security audit flagging mutable action refs;
  exit code 127 in workflow steps (or silent pass with `continue-on-error: true`)
- **Prevention:** Audit Dockerfiles and workflows when removing files; enforce SHA pinning
  for all `uses:` references; validate script paths exist; avoid `continue-on-error` masking real failures
- **Fix Time:** Minutes (update or remove stale references) but Hours (if silent failures went unnoticed)
