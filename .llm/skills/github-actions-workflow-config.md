# Skill: GitHub Actions Workflow Configuration

<!--
  trigger: github actions, workflow, lychee, link checker, case sensitive, smoke test, docker smoke test, permissions, path filter, concurrency, yaml
  | Patterns for configuring GitHub Actions workflows: link checking, path filters, permissions, smoke tests | Infrastructure
-->

**Trigger**: When configuring workflow triggers, permissions, link checkers, smoke tests, or path filters.

---

## When to Use

- Configuring lychee link checker in workflows
- Debugging case-sensitive path failures on Linux CI
- Writing Docker smoke tests with health-check retry loops
- Setting minimal workflow permissions
- Configuring `on.push.paths` triggers and concurrency controls
- Using Docker-based actions (cargo-deny, cargo-audit) with `rust-toolchain.toml`

## When NOT to Use

- Language-specific caching (see [GitHub Actions Caching](./github-actions-caching.md))
- Scheduled/cron workflow patterns (see [GitHub Actions Scheduled Workflows](./github-actions-scheduled-workflows.md))
- Release gating and preflight (see [GitHub Actions Release](./github-actions-release.md))

## TL;DR

- Lychee `include` is for URL regex filtering, not file glob patterns — use CLI args for file selection
- All file/link paths are case-sensitive on Linux CI; verify exact case matches
- Docker smoke tests need retry loops with `docker logs` on failure, not bare `sleep`
- Default permissions to `contents: read`; grant only what is needed
- Always include the workflow file itself in `paths:` triggers

---

## 1. Lychee Link Checker Configuration

### The Problem

Lychee's `include` field in `.lychee.toml` is for **URL regex filtering**, not file glob patterns.
Using file globs in `include` silently fails to filter anything.

```toml
# ❌ WRONG: include is for URL patterns, not file paths
include = [
    "**/*.md",
    "src/**/*.rs",
]
```

### Solution: Use CLI Arguments for File Selection

```yaml
# ✅ CORRECT: File patterns as CLI args
- name: Link Checker
  uses: lycheeverse/lychee-action@a8c4c7cb88f0c7386610c35eb25108e448569cb0 # v2.7.0
  with:
    args: >-
      --verbose --no-progress --cache --max-cache-age 7d
      './**/*.md' './**/*.rs' './**/*.toml'
      --config .lychee.toml
```

### Lychee Config (.lychee.toml) Best Practices

```toml
# .lychee.toml — link validation rules, NOT file selection

accept = ["100..=103", "200..=299", "429"]  # 429 = rate limiting

max_retries = 3
retry_wait_time = 2
timeout = 20

# Exclude URL patterns (regex)
exclude = [
    "http://localhost",
    "http://127.0.0.1",
    "ws://localhost",
    "mailto:*",
]

# Exclude directories from internal link checking
exclude_path = ["target/", ".git/"]
exclude_link_local = true
```

### When Lychee Fails: Case-Sensitive Paths

Lychee follows filesystem case sensitivity. On Linux, `Skills/foo.md` != `skills/foo.md`.

```markdown
<!-- ❌ WRONG: Case mismatch breaks on Linux -->
See [testing guide](Skills/testing-core-patterns.md)

<!-- ✅ CORRECT: Exact case match -->
See [testing guide](skills/testing-core-patterns.md)
```

---

## 2. Case-Sensitive Filesystem Issues

### The Problem

Windows and macOS default to case-insensitive filesystems, but Linux CI runners are case-sensitive.
Links and imports that work locally may break in CI.

```bash
# Local (Windows/macOS): Works
ls Skills/testing.md    # Finds skills/testing.md

# CI (Linux): Fails
ls Skills/testing.md    # No such file or directory
```

### Prevention Checklist

- [ ] All file paths use consistent casing (prefer lowercase)
- [ ] All Markdown links match actual filename case exactly
- [ ] All `mod` statements in Rust match file case exactly
- [ ] Tested on Linux before pushing (WSL, Docker, or CI)

### Fix Script: Case Audit

```bash
# Find all Markdown links and verify targets exist (case-sensitive)
find . -name "*.md" -not -path "./target/*" | while read -r md_file; do
  grep -oE '\[([^]]+)\]\(([^)]+)\)' "$md_file" | while read -r link; do
    url=$(echo "$link" | sed -E 's/.*\(([^)]+)\).*/\1/')
    [[ "$url" =~ ^https?:// ]] && continue  # Skip external URLs
    file_part="${url%%#*}"
    [ -z "$file_part" ] && continue
    base_dir=$(dirname "$md_file")
    full_path=$(realpath -m "$base_dir/$file_part")
    if [ ! -f "$full_path" ]; then
      echo "Broken link in $md_file: $url"
    fi
  done
done
```

---

## 3. Docker Smoke Test Patterns

### The Problem

Bare `sleep` followed by `curl` is unreliable — the server may not be ready, causing false failures.

```bash
# ❌ WRONG: Fixed sleep is unreliable
docker run -d --name test-server -p 3536:3536 myapp:ci
sleep 3
curl -f http://localhost:3536/health  # May fail if server takes >3s
```

### Solution: Retry Loop with Diagnostics

```bash
# ✅ CORRECT: Retry loop with docker logs on failure
docker run -d --name test-server -p 3536:3536 myapp:ci

for i in $(seq 1 15); do
  if curl -sf http://localhost:3536/health; then
    echo "Health check passed on attempt $i/15"
    exit 0
  fi
  echo "Attempt $i/15: server not ready, retrying in 2s..."
  sleep 2
done

echo "ERROR: Server failed to become healthy after 30s"
echo "=== Docker logs ==="
docker logs test-server
exit 1
```

### Always Include Cleanup

```yaml
- name: Cleanup smoke test
  if: always()
  run: docker stop test-server && docker rm test-server || true
```

---

## 4. Minimal Permissions (Security)

### Default to Read-Only

```yaml
# NEVER omit permissions — defaults to full write access
permissions:
  contents: read
```

### Grant Only What Is Needed

```yaml
# If workflow creates issues or comments:
permissions:
  contents: read
  issues: write
  pull-requests: write
```

---

## 5. Workflow Path Filtering

### Trigger on Relevant Changes Only

```yaml
on:
  push:
    branches: [main]
    paths:
      - '**/*.md'
      - '**/*.rs'
      - 'Cargo.toml'
      - 'Cargo.lock'
      - '.github/workflows/this-workflow.yml'  # Always include self
  pull_request:
    branches: [main]
    paths:
      - '**/*.md'
      - '**/*.rs'
      - 'Cargo.toml'
      - 'Cargo.lock'
      - '.github/workflows/this-workflow.yml'
```

**Always include the workflow file itself** — changes to the workflow should trigger a run to validate them.

### Concurrency Control

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.head_ref || github.run_id }}
  cancel-in-progress: true
```

Prevents duplicate runs on rapid pushes to the same branch.

---

## 6. Docker-Based Actions and Toolchain Overrides

### The Problem

Some GitHub Actions (e.g., `cargo-deny-action`, `cargo-audit-action`) run inside their own Docker
container with a pre-installed Rust toolchain. If the repository's `rust-toolchain.toml` pins a
specific version, rustup inside the container tries to install that version — which may not be
available, causing the action to fail.

### Solution: `RUSTUP_TOOLCHAIN` Environment Variable

Override `rust-toolchain.toml` inside the container by setting `RUSTUP_TOOLCHAIN`:

```yaml
# ✅ CORRECT: Override toolchain for Docker-based actions
- name: Run cargo-deny
  uses: EmbarkStudios/cargo-deny-action@44db170f6a7d12a6e90340e9e0fca1f650d34b14 # v2.0.15
  env:
    RUSTUP_TOOLCHAIN: stable  # Use container's stable toolchain
  with:
    arguments: --all-features
```

### When to Use This Pattern

| Action Type                              | Needs Override? | Rationale                              |
|------------------------------------------|-----------------|----------------------------------------|
| Metadata-only (cargo-deny, cargo-audit)  | Yes             | Only reads lock files, no compilation  |
| Compilation actions (build, test)        | No              | Needs exact toolchain for correctness  |
| Linting actions (clippy)                 | No              | Lint results depend on Rust version    |
| Formatting actions (rustfmt)             | Depends         | Format output may vary by version      |

**Key Insight:** Actions that only inspect dependency metadata and lock files (not compile code)
do not need the project's exact Rust version. Overriding with `stable` avoids toolchain
installation failures in Docker containers.

---

## Related Skills

- [GitHub Actions Caching](./github-actions-caching.md) — Ecosystem-specific caching, SHA pinning, Docker version formats
- [GitHub Actions Bash Scripts](./github-actions-bash-scripts.md) — Shellcheck, Bash best practices
- [GitHub Actions Scheduled Workflows](./github-actions-scheduled-workflows.md) — Cron schedules, proactive monitoring
- [GitHub Actions Release](./github-actions-release.md) — Release gating, preflight hardening
- [ci-cd-troubleshooting](./ci-cd-troubleshooting-categories.md) — Diagnosing CI failures
