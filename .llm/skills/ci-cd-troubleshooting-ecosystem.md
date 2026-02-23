# Skill: CI/CD Troubleshooting - Ecosystem & Toolchain Patterns

<!--
  trigger: ci failure, language mismatch, cache error, toolchain staleness,
  unused dependencies, works locally fails ci, Docker build failure
  | Patterns 1-6: language/ecosystem mismatch, cache corruption, toolchain
  staleness, dependency hygiene, local-vs-CI divergence, Docker failures
  | Infrastructure
-->

**Trigger**: When debugging ecosystem mismatch, cache errors, stale toolchains, unused
dependencies, local-vs-CI divergence, or Docker build failures.

See also: [ci-cd-troubleshooting-linting.md](./ci-cd-troubleshooting-linting.md),
[ci-cd-troubleshooting-scripts.md](./ci-cd-troubleshooting-scripts.md),
[ci-cd-troubleshooting-supply-chain.md](./ci-cd-troubleshooting-supply-chain.md),
[ci-cd-troubleshooting-categories.md](./ci-cd-troubleshooting-categories.md)

---

## TL;DR

- **Configuration mismatch** is the most common "works locally, fails in CI" root cause
- **Python caching on Rust project** = instant failure — check ecosystem alignment
- **Staleness kills**: Old toolchains (>6 months) cause subtle breakage
- **Cache invalidation** is hard — when in doubt, clear the cache and add a version suffix

---

## Pattern 1: Language/Ecosystem Mismatch

### Symptom

```text
ERROR: Cache entry deserialization failed, entry ignored
ERROR: Unable to locate executable file: pip
```

### Root Cause

Workflow uses caching/tooling for wrong language ecosystem:

```yaml
# WRONG: Python caching on a Rust project
- uses: actions/cache@v4
  with:
    path: ~/.cache/pip        # Python cache path
    key: ${{ runner.os }}-pip-${{ hashFiles('**/requirements.txt') }}

- run: cargo build            # Rust project, not Python!
```

### Solution

```yaml
# CORRECT: Rust caching for Rust project
- uses: Swatinem/rust-cache@v2.7.5
  with:
    prefix-key: "rust"

- run: cargo build
```

### Detection

| Indicator | Wrong Ecosystem | Correct for Rust |
|-----------|-----------------|------------------|
| Cache paths | `~/.cache/pip`, `node_modules/`, `.bundle/` | `~/.cargo/`, `target/` |
| Hash files | `requirements.txt`, `package-lock.json`, `Gemfile.lock` | `Cargo.lock`, `Cargo.toml` |
| Install commands | `pip install`, `npm install`, `bundle install` | `cargo build`, `rustup component add` |

**Quick audit command:**

```bash
grep -r "pip\|requirements\.txt\|python" .github/workflows/  # Python patterns
grep -r "cargo\|Cargo\.toml\|rust" .github/workflows/        # Rust patterns
```

**Caveat:** Mixed-language projects are legitimate. Check `requirements*.txt` variants
(e.g., `requirements-docs.txt` for MkDocs) before flagging as mismatch.

---

## Pattern 2: Cache Corruption / Deserialization Failures

### Symptom

```text
ERROR: Cache entry deserialization failed, entry ignored
WARNING: Failed to restore cache, continuing without cache
```

### Root Causes

1. **Cache format changed** (action/tool updated)
2. **OS mismatch** (cache from Linux restored on macOS)
3. **Cache key collision** (different projects using same key)
4. **Corrupted upload** (network error during cache save)

### Solution

**Clear and rebuild cache:**

```yaml
# Temporary: Add cache-busting suffix to key
- uses: actions/cache@v4
  with:
    path: ~/.cargo
    key: ${{ runner.os }}-cargo-v2-${{ hashFiles('**/Cargo.lock') }}
    #                            ^^^ increment version to bust cache
```

**Or via GitHub UI:** Go to repository → Actions → Caches → Delete problematic entries.

### Prevention

```text
# GOOD: Versioned cache key
key: ${{ runner.os }}-rust-v1-${{ hashFiles('**/Cargo.lock') }}

# BEST: Let Swatinem/rust-cache handle cache management
- uses: Swatinem/rust-cache@v2.7.5
  # Automatically manages cache keys, invalidation, and restoration
```

---

## Pattern 3: Toolchain Staleness

### Symptom

```text
error: package `rand v0.10.0` cannot be built because it requires rustc 1.88.0 or newer
error[E0658]: use of unstable library feature 'foo'
```

### Root Cause

```yaml
# PROBLEM: Nightly from 360 days ago
- uses: dtolnay/rust-toolchain@stable
  with:
    toolchain: nightly-2025-02-21  # 360 days old!
```

### Solution

```yaml
# CORRECT: Recent nightly (within last 30 days)
- uses: dtolnay/rust-toolchain@stable
  with:
    toolchain: nightly-2026-02-01  # recent, acceptable
```

For stable MSRV issues, see [msrv-management](./msrv-management.md).

### Staleness Thresholds

| Toolchain Type | Maximum Age | Action Required |
|----------------|-------------|-----------------|
| Stable MSRV | N/A | Update when dependencies require it |
| Pinned nightly | 6 months | Proactive update recommended |
| Action SHA pins | 1 year | Review for security updates |
| Docker base images | 6 months | Update for security patches |

```bash
# Check age of pinned nightlies — are any >6 months old?
grep -r "nightly-20" .github/workflows/
```

---

## Pattern 4: Dependency Hygiene Drift

### Symptom

```text
warning: unused dependency: `futures`
warning: unused dependency: `async-trait`
# ... 15+ unused dependencies
```

### Solution

```bash
cargo install cargo-machete cargo-udeps

cargo machete              # Find unused dependencies (fast, stable)
cargo +nightly udeps --all-targets  # More thorough (nightly)
```

**Keep vs Remove Decision Matrix:**

| Scenario | Decision | Rationale |
|----------|----------|-----------|
| Unused but actively maintained | Remove | Can re-add when needed |
| Unused behind feature flag | Keep | Optional dependency |
| Unused, unmaintained (>1 year) | Remove immediately | Security liability |
| False positive from cargo-udeps | Keep | Mark with `# keep: used in macro` |

```toml
# keep: Used by serde derive macros (false positive from cargo-udeps)
serde_derive = "1.0"
```

### Prevention

Add a weekly CI job with `cargo machete`.

---

## Pattern 5: "Works Locally, Fails in CI"

### Root Causes

- **Different Rust versions**: Local uses latest stable, CI uses MSRV
- **Different feature flags**: `--all-features` locally vs none in CI
- **Different OS**: macOS case-insensitive vs Linux case-sensitive filesystem
- **Different environment variables**: Local has `DATABASE_URL`, CI has clean env

### Solution

```bash
# 1. Use exact MSRV from rust-toolchain.toml
rustup install 1.88.0
cargo +1.88.0 test

# 2. Match CI feature flags exactly
cargo test --locked
cargo test --locked --all-features

# 3. Use Docker to match CI OS
docker run --rm -v $(pwd):/app -w /app rust:1.88-bookworm cargo test

# 4. Clear env vars
env -i PATH=$PATH HOME=$HOME cargo test
```

### Prevention

```toml
# rust-toolchain.toml — enforces exact version
[toolchain]
channel = "1.88.0"
components = ["rustfmt", "clippy"]
```

```bash
# Before pushing:
cargo test --locked                        # Default features
cargo test --locked --all-features         # All features
cargo test --locked --no-default-features  # Minimal features
```

---

## Pattern 6: Docker Build Failures (Local Success, CI Failure)

### Root Causes

- **Docker build cache differences**: Local has cached layers, CI starts fresh
- **Platform differences**: Local arm64 vs CI linux/amd64
- **Build context pollution**: Missing `.dockerignore` entries

### Solution

```yaml
# Disable Docker build cache in CI
- name: Build Docker image
  run: docker build --no-cache -t myapp:ci .
```

```dockerfile
# Multi-platform support
FROM --platform=$BUILDPLATFORM rust:1.88-bookworm AS builder
```

```text
# .dockerignore — add to avoid polluting build context
target/
.git/
.github/
*.md
.env*
.vscode/
```

```bash
# Simulate CI environment locally (use --no-cache)
docker build --no-cache --progress=plain -t test .
```

---

## Related Skills

- [ci-cd-troubleshooting-scripts.md](./ci-cd-troubleshooting-scripts.md) — Shell scripts, Miri, test filtering
- [ci-cd-troubleshooting-supply-chain.md](./ci-cd-troubleshooting-supply-chain.md) — SHA pinning, Dockerfile
- [ci-cd-troubleshooting-categories.md](./ci-cd-troubleshooting-categories.md) — Diagnostic workflow and quick reference
- [msrv-management](./msrv-management.md) — MSRV updates and consistency
