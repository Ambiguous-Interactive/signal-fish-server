# Skill: CI/CD Troubleshooting Guide

<!-- trigger: ci failure, ci error, workflow failure, github actions failure, ci debug, cache error, configuration mismatch | Common CI failures and their solutions | Infrastructure -->

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

- Writing new workflows from scratch (see [github-actions-best-practices](./github-actions-best-practices.md))
- Performance optimization (see [rust-performance-optimization](./rust-performance-optimization.md))
- Security audits (see [supply-chain-security](./supply-chain-security.md))

---

## TL;DR

- **Configuration mismatch** is the most common root cause of "works locally, fails in CI"
- **Check language-project alignment**: Python caching on Rust project = instant failure
- **Staleness kills**: Old toolchains (>6 months) cause subtle breakage
- **Cache invalidation** is hard — when in doubt, clear the cache
- **Always check dates**: Pinned versions/toolchains from >6 months ago need review

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

## Pattern 7: Documentation Quality Issues (Markdown Linting, Spell Checking)

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

## Pattern 8: Git Hook Permission Issues

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

- [github-actions-best-practices](./github-actions-best-practices.md) — Workflow patterns and best practices
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
