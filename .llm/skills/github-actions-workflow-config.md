# Skill: GitHub Actions Workflow Configuration
<!--
  trigger: GitHub actions, workflow, lychee, link checker,
  case sensitive, smoke test, Docker smoke test, permissions,
  path filter, concurrency, yaml
  | Patterns for configuring GitHub Actions workflows: link checking, path
  filters, permissions, smoke tests | Infrastructure
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
- For GHCR publishing, derive `images:` from repository owner/name; do not hard-code org names
- Always include the workflow file itself in `paths:` triggers
- Invoke local scripts through interpreters (`bash`, `pwsh -File`, `awk -f`,
  `node`); never use direct `run: scripts/foo.sh` execution

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
  uses: lycheeverse/lychee-action@v2.7.0
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

Windows/macOS may ignore case locally, but Linux CI does not: `Skills/foo.md` fails if the real path is `skills/foo.md`.

**Prevention:** Use consistent lowercase paths, verify Markdown links and Rust `mod`
statements match actual file case, test on Linux before pushing.

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

Declare permissions explicitly (org defaults vary). Start with `contents: read` and grant only what is needed:

```yaml
permissions:
  contents: read
  issues: write        # Only if workflow creates issues/comments
  pull-requests: write # Only if workflow comments on PRs
```

For GHCR publish workflows, derive `images:` from repository context via step outputs instead of hard-coded org paths.

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

Some GitHub Actions (e.g., `cargo-deny-action`, `cargo-audit-action`) run inside
their own Docker container with a pre-installed Rust toolchain. If the repo's
`rust-toolchain.toml` pins a specific version, rustup inside the container tries
to install it — which may not be available, causing the action to fail.

### Solution: Explicit `rust-version` Input

Prefer the action input `rust-version` so the container installs a concrete
toolchain before executing:

```yaml
# ✅ CORRECT: install an explicit toolchain for Docker-based actions
- name: Extract MSRV
  id: deny-msrv
  run: |
    MSRV=$(bash scripts/read-toml-string.sh Cargo.toml rust-version package)
    echo "version=$MSRV" >> "$GITHUB_OUTPUT"

- name: Run cargo-deny
  uses: EmbarkStudios/cargo-deny-action@v2.0.15
  with:
    arguments: --all-features
    rust-version: ${{ steps.deny-msrv.outputs.version }}
```

### When to Use This Pattern

| Action Type                              | Needs Override? | Rationale                              |
|------------------------------------------|-----------------|----------------------------------------|
| Metadata-only (cargo-deny, cargo-audit)  | Yes             | Only reads lock files, no compilation  |
| Compilation actions (build, test)        | No              | Needs exact toolchain for correctness  |
| Linting actions (clippy)                 | No              | Lint results depend on Rust version    |
| Formatting actions (rustfmt)             | Depends         | Format output may vary by version      |

**Key Insight:** Metadata-only actions still need a deterministic toolchain setup.
`with.rust-version` is more reliable than environment alias overrides
(`RUSTUP_TOOLCHAIN=stable`), which can fail when `stable` is not preinstalled.

---

## 7. Schedule Trigger Guards

Workflows with `schedule:` triggers run all jobs by default on cron events. The
pre-commit hook validates that every scheduled workflow either:

1. Contains `# all-jobs-run-on-schedule` within the first 30 lines, **or**
2. Has per-job `if: github.event_name != 'schedule'` guards on non-scheduled jobs

### Adding the Directive

Place `# all-jobs-run-on-schedule` in the workflow header comment when **all**
jobs should run on the cron schedule:

```yaml
name: CI Safety
# all-jobs-run-on-schedule
on:
  schedule:
    - cron: '30 6 * * 1'
  # ...other triggers
```

If only some jobs should run on schedule, add per-job guards instead:

```yaml
jobs:
  build:
    if: github.event_name != 'schedule'
    # ...
  scheduled-audit:
    # Runs on all triggers including schedule
```

The hook also recognizes per-job comments: `# runs-on-schedule`, `# schedule`,
`# security`, `# audit`, `# daily`; those jobs do not need an `if:` guard.

---

## Related Skills

- [GitHub Actions Caching](./github-actions-caching.md) — Caching, action ref policy, Docker version formats
- [GitHub Actions Bash Scripts](./github-actions-bash-scripts.md) — Shellcheck, Bash best practices
- [GitHub Actions Scheduled Workflows](./github-actions-scheduled-workflows.md) — Cron schedules, monitoring
- [GitHub Actions Release](./github-actions-release.md) — Release gating, preflight hardening
