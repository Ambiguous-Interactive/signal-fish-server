# GitHub Actions Scheduled Workflows

**Applies to**: When adding cron schedules to workflows, configuring security audits, or ensuring
non-audit jobs do not run on schedule.

---

## When to Use

- Adding `schedule:` triggers to GitHub Actions workflows
- Configuring daily security audits with cargo-deny
- Preventing non-audit jobs from running on cron
- Setting up proactive monitoring for dependencies and link rot
- Preventing duplicate/overlapping scheduled runs

## When NOT to Use

- General workflow configuration (see [GitHub Actions Workflow Config](./github-actions-workflow-config.md))
- Release gating logic (see [GitHub Actions Release](./github-actions-release.md))

## TL;DR

- Add `schedule:` triggers to catch CVEs published between code changes
- Add `if: github.event_name != 'schedule'` to every job that should NOT run on cron
- Only the intended job (e.g., `deny`) omits the schedule guard
- Stagger cron times; do not run everything at midnight UTC
- Document schedule frequency choice with comments

---

## 1. The Problem: Reactive vs Proactive Security

Running security audits only on code changes is reactive:

```yaml
# ❌ REACTIVE: Only runs when code changes
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
```

**Issues:**

- New CVEs published overnight won't trigger the workflow
- Advisory databases update independently of code
- Stale dependencies accumulate between changes
- Nightly toolchains become outdated

---

## 2. The Solution: Scheduled Workflows

```yaml
# ✅ PROACTIVE: Runs on code changes AND on schedule
on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  schedule:
    # Daily security audit at noon UTC to catch new CVEs
    - cron: '0 12 * * *'
```

### When to Use Scheduled Workflows

| Workflow Type                | Recommended Schedule | Rationale                              |
|------------------------------|----------------------|----------------------------------------|
| Security audits (cargo-deny) | Daily                | New CVEs published frequently          |
| Dependency updates           | Weekly               | Balance freshness with stability       |
| Link checking                | Weekly               | Catch external link rot                |
| Workflow hygiene             | Weekly               | Detect stale toolchains                |
| Unused dependencies          | Weekly               | Proactive dependency cleanup           |

### Common Cron Schedules

```yaml
- cron: '0 12 * * *'   # Every day at noon UTC
- cron: '0 0 * * 1'    # Every Monday at midnight UTC
- cron: '0 6 * * 1'    # Every Monday at 6 AM UTC
- cron: '0 0 1 * *'    # First day of every month
- cron: '0 */6 * * *'  # Every 6 hours
```

---

## 3. Real-World Example: Daily Security Audit

From `.github/workflows/ci.yml`:

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  schedule:
    # Daily security audit at noon UTC to catch new CVEs
    - cron: '0 12 * * *'

jobs:
  deny:
    name: Dependency Audit
    runs-on: ubuntu-latest
    # Runs on push/PR and daily via schedule (see workflow triggers).
    steps:
      - name: Checkout repository
        uses: actions/checkout@v6.0.3

      - name: Run cargo-deny
        uses: EmbarkStudios/cargo-deny-action@v2.0.15
        with:
          arguments: --all-features
```

---

## 4. Job-Level `if:` Guards for Schedule Triggers

### The Problem

Adding a `schedule:` trigger causes **all jobs** in the workflow to run on cron, not just the intended one.

```yaml
jobs:
  deny:    # <-- Only this job should run on schedule
    # ...
  lint:    # <-- Will ALSO run on schedule without a guard!
    # ...
  nextest: # <-- This too!
    # ...
```

### The Solution: `if:` Guards on Non-Audit Jobs

```yaml
jobs:
  deny:
    name: Dependency Audit
    # No `if:` guard — runs on ALL triggers including schedule
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6.0.3
      - uses: EmbarkStudios/cargo-deny-action@v2.0.15

  lint:
    name: Lint
    if: github.event_name != 'schedule'  # Skip on daily cron
    runs-on: ubuntu-latest
    # ...

  nextest:
    name: Tests
    if: github.event_name != 'schedule'  # Skip on daily cron
    runs-on: ubuntu-latest
    # ...
```

**Validated by:** The test `test_ci_schedule_only_runs_audit` in `tests/ci_config_tests.rs` ensures
every non-audit job in `ci.yml` has a schedule-excluding `if:` condition.

---

## 5. Preventing Alert Fatigue

Use different schedules for different priorities:

```yaml
# High priority: Daily security audits
security-audit:
  schedule:
    - cron: '0 12 * * *'  # Daily at noon

# Medium priority: Weekly dependency cleanup
unused-deps:
  schedule:
    - cron: '0 0 * * 1'  # Weekly on Monday

# Low priority: Monthly workflow hygiene
workflow-hygiene:
  schedule:
    - cron: '0 6 1 * *'  # First of month at 6 AM
```

**Always add comments explaining schedule choices:**

```yaml
schedule:
  # Daily security audit at noon UTC to catch new CVEs
  # More frequent than code changes because advisory DB updates independently
  - cron: '0 12 * * *'
```

---

## 6. Failure Notifications for Scheduled Runs

```yaml
jobs:
  security-audit:
    runs-on: ubuntu-latest
    steps:
      - name: Run cargo-deny
        uses: EmbarkStudios/cargo-deny-action@v2.0.15
        with:
          arguments: --all-features

      # Send notification on failure (scheduled runs only)
      - name: Notify on failure
        if: failure() && github.event_name == 'schedule'
        uses: actions/github-script@v8.0.0
        with:
          script: |
            github.rest.issues.create({
              owner: context.repo.owner,
              repo: context.repo.repo,
              title: 'Scheduled security audit failed',
              body: 'Daily security audit detected new vulnerabilities.\n\n' +
                    'Workflow: ${{ github.server_url }}/${{ github.repository }}/' +
                    'actions/runs/${{ github.run_id }}',
              labels: ['security', 'automated']
            })
```

---

## 7. Preventing Duplicate Runs

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.event_name }}
  cancel-in-progress: true
```

This ensures:

- Scheduled run will not overlap with push/PR runs
- Multiple queued scheduled runs are cancelled (only latest proceeds)
- Resources are used efficiently

---

## Best Practices Checklist

- [ ] `schedule:` trigger added to security audit workflow
- [ ] Schedule frequency documented with comments explaining the choice
- [ ] Every non-audit job has `if: github.event_name != 'schedule'`
- [ ] The audit job (`deny`) omits the schedule guard
- [ ] Different schedules used for different priorities (no everything-at-midnight)
- [ ] Failure notifications configured for scheduled-run failures
- [ ] Concurrency control prevents overlapping scheduled runs
- [ ] `test_ci_schedule_only_runs_audit` test validates guard coverage

---

## Related References

- [GitHub Actions Workflow Config](./github-actions-workflow-config.md) — Permissions, path filters, smoke tests
- [GitHub Actions Release](./github-actions-release.md) — Release gating and preflight hardening
- [GitHub Actions Config Tests](./github-actions-config-tests.md) — Automated validation of CI configuration
- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Diagnosing CI failures
