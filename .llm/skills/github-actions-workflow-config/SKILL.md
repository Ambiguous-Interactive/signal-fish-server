---
name: github-actions-workflow-config
description: >-
  Configure and review GitHub Actions workflows, triggers, permissions, path filters, link checks,
  and smoke tests. Use when authoring workflow YAML or debugging workflow configuration behavior.
---

# GitHub Actions Workflow Configuration

---

## When to Use

- Configuring lychee link checker in workflows
- Debugging case-sensitive path failures on Linux CI
- Writing Docker smoke tests with health-check retry loops
- Setting minimal workflow permissions
- Configuring `on.push.paths` triggers and concurrency controls
- Using Docker-based actions (cargo-deny, cargo-audit) with `rust-toolchain.toml`

## When NOT to Use

- Language-specific caching (see [GitHub Actions Caching](../github-actions-caching/SKILL.md))
- Scheduled/cron workflow patterns (see [GitHub Actions Scheduled Workflows](../github-actions-scheduled-workflows/SKILL.md))
- Release gating and preflight (see [GitHub Actions Release](../github-actions-release/SKILL.md))

## TL;DR

- Lychee `include` is for URL regex filtering, not file glob patterns — use CLI args for file selection
- All file/link paths are case-sensitive on Linux CI; verify exact case matches
- Docker smoke tests need retry loops with `docker logs` on failure, not bare `sleep`
- Default permissions to `contents: read`; grant only what is needed
- For GHCR publishing, derive `images:` from repository owner/name; do not hard-code org names
- Always include the workflow file itself in `paths:` triggers
- Invoke local scripts through interpreters (`bash`, `pwsh -File`, `awk -f`,
  `node`); never use direct `run: scripts/foo.sh` execution

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

## 2. Case-Sensitive Filesystem Issues

Windows/macOS may ignore case locally, but Linux CI does not: `Skills/foo.md` fails if the real path is `skills/foo.md`.

**Prevention:** Use consistent lowercase paths, verify Markdown links and Rust `mod`
statements match actual file case, test on Linux before pushing.

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
  group: >-
    ${{ github.workflow }}-${{ github.event_name }}-${{
      github.event_name == 'pull_request' &&
      github.event.pull_request.number ||
      github.event_name == 'push' && github.ref ||
      github.run_id
    }}
  cancel-in-progress: true
```

Cancels superseded PR and branch-push runs while keeping scheduled and manual
runs independent. PR numbers avoid collisions between same-named fork branches;
the push ref avoids the unique-run-ID fallback that defeats push cancellation.

---

## 6. Native Cargo Audit Tools and Explicit Toolchains

Docker actions build their image before workflow steps run. A daemon mirror
step cannot repair a Docker Hub refusal during action preparation. Install
pinned native cargo-deny instead, and keep each graph's metadata toolchain
explicit. Retain the relevance gate on both installation and execution.

```yaml
- name: Install cargo-deny metadata toolchain
  uses: dtolnay/rust-toolchain@v1
  with:
    toolchain: ${{ steps.deny-msrv.outputs.version }}
- name: Install cargo-deny
  uses: taiki-e/install-action@v2.87.22
  with:
    tool: cargo-deny@0.20.2
- name: Run cargo-deny
  env:
    RUSTUP_TOOLCHAIN: ${{ steps.deny-msrv.outputs.version }}
  run: cargo deny --log-level warn --all-features check
```

Use the owning manifest for every separate Cargo graph. Install its explicit
Rust toolchain before selecting it. Keep compilation and lint jobs on their
existing toolchains. Cargo.lock v4 needs Cargo 1.78 or newer.

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

## Related Skills

- [GitHub Actions Caching](../github-actions-caching/SKILL.md) — Caching, action ref policy, Docker version formats
- [GitHub Actions Bash Scripts](../github-actions-bash-scripts/SKILL.md) — Shellcheck, Bash best practices
- [GitHub Actions Scheduled Workflows](../github-actions-scheduled-workflows/SKILL.md) — Cron schedules, monitoring
- [GitHub Actions Release](../github-actions-release/SKILL.md) — Release gating, preflight hardening
