# CI/CD Troubleshooting - Ecosystem & Toolchain Patterns

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

**Quick audit** — look for cross-ecosystem indicators in workflows:

```bash
grep -r "pip\|requirements\.txt\|python" .github/workflows/  # Python patterns
grep -r "cargo\|Cargo\.toml\|rust" .github/workflows/        # Rust patterns
```

**Caveat:** Mixed-language projects are legitimate (e.g., `requirements-docs.txt` for MkDocs).

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

**Or via GitHub UI:** Repository → Actions → Caches → Delete problematic entries.

**Best practice:** Use `Swatinem/rust-cache@v2.7.5` which handles keys and invalidation
automatically, or version your manual cache keys (`-v2-`, `-v3-`, etc.).

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
- uses: dtolnay/rust-toolchain@v1
  with:
    toolchain: nightly-2025-02-21  # 360 days old!
```

### Solution

```yaml
# CORRECT: Recent nightly (within last 30 days)
- uses: dtolnay/rust-toolchain@v1
  with:
    toolchain: nightly-2026-02-01  # recent, acceptable
```

For stable MSRV issues, see [MSRV Management](../../msrv-management/SKILL.md).

### Staleness Thresholds

| Toolchain Type | Maximum Age | Action Required |
|----------------|-------------|-----------------|
| Stable MSRV | N/A | Update when dependencies require it |
| Pinned nightly | 6 months | Proactive update recommended |
| Action version tags | 1 year | Review for security updates |
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

**Keep vs Remove:** Remove unused deps unless behind a feature flag or a false positive
(mark with `# keep: used in macro`). Remove unmaintained deps (>1 year) immediately.
Add a weekly CI job with `cargo machete` for prevention.

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

Use `docker build --no-cache` in CI and `FROM --platform=$BUILDPLATFORM` for
multi-platform builds. Ensure `.dockerignore` excludes `target/`, `.git/`, `.github/`,
`*.md`, `.env*`, and `.vscode/`.

```bash
# Simulate CI environment locally
docker build --no-cache --progress=plain -t test .
```

---

## Pattern 7: `dtolnay/rust-toolchain@v1` Requires Explicit `toolchain` Input

### Symptom

```text
'toolchain' is a required input
```

Jobs fail at the "Install Rust toolchain" step.

### Root Cause

The `dtolnay/rust-toolchain@v1` action changed to require an explicit `toolchain` input
parameter. Previous behavior of auto-detecting from `rust-toolchain.toml` no longer works
with the `@v1` tag.

### Solution

Extract the channel from `rust-toolchain.toml` and pass it dynamically:

```yaml
- name: Read Rust toolchain
  id: toolchain
  run: |
    CHANNEL=$(bash scripts/read-toml-string.sh rust-toolchain.toml channel toolchain)
    echo "channel=$CHANNEL" >> "$GITHUB_OUTPUT"
- uses: dtolnay/rust-toolchain@v1
  with:
    toolchain: ${{ steps.toolchain.outputs.channel }}
```

**Important:** Do NOT use bare `stable`, `beta`, or `nightly` — these are moving aliases
rejected by CI validation tests. Always use the concrete version from `rust-toolchain.toml`.

---

## Related Skills

- [CI CD Troubleshooting Scripts](scripts-and-tests.md) — Shell scripts, Miri, test filtering
- [CI CD Troubleshooting Supply Chain](supply-chain.md) — Action ref policy, Dockerfile
- [CI CD Troubleshooting Categories](diagnostic-workflow.md) — Diagnostic workflow, quick ref
- [MSRV Management](../../msrv-management/SKILL.md) — MSRV updates and consistency
