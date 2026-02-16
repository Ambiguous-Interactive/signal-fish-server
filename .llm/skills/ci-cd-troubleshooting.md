# Skill: CI/CD Troubleshooting Guide

<!-- trigger: ci failure, ci error, workflow failure, GitHub actions failure, ci debug, cache error, configuration mismatch | Common CI failures and their solutions | Infrastructure -->

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

```yaml
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

```toml
# Before:
[dependencies]
tokio = "1.49"
futures = "0.3"           # ← Unused
async-trait = "0.1"       # ← Unused
serde = { version = "1.0", features = ["derive"] }
rand = "0.10"

# After:
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

```bash
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

```bash
# Local: macOS (case-insensitive filesystem)
use crate::Config;  // finds config.rs, Config.rs, or CONFIG.rs

# CI: Linux (case-sensitive filesystem)
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

**Use rust-toolchain.toml for version pinning:**

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

```bash
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
    ├─ Docker error ───────► Check base image, build context, platform
    └─ Workflow error ─────► Check syntax, permissions, secrets

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

- [ ] Workflow uses caching appropriate for project language (Rust = rust-cache, not pip/npm)
- [ ] All cache paths reference files that actually exist (e.g., Cargo.lock, not requirements.txt)
- [ ] Base images match project MSRV (Docker `FROM rust:X.Y` = Cargo.toml `rust-version`)
- [ ] No language-specific commands for wrong ecosystem (no `pip install` in Rust project)

### Version Consistency

- [ ] MSRV consistent across: Cargo.toml, rust-toolchain.toml, clippy.toml, Dockerfile
- [ ] Pinned nightly toolchains documented with age and update criteria
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

```json

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

```bash
# Check links locally
lychee --config .lychee.toml './**/*.md'

# Fix broken links or add to exclusions
```
```

### Key Insights

**Link validation is environment-specific:**
1. **Filesystem case sensitivity** - macOS/Windows are case-insensitive, Linux is case-sensitive
2. **External link availability** - Sites change, get rate-limited, or go offline temporarily
3. **Placeholder vs real URLs** - Documentation examples shouldn't cause CI failures

**Lychee configuration best practices:**
- `exclude` field is for URL patterns (regex), not file globs
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

The RustSec advisory database was updated to include CVSS 4.0 vulnerability scores. Older versions of cargo-deny (using rustsec < 0.31) cannot parse these entries and fail.

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

## Pattern 11: Git Hook Permission Issues

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
