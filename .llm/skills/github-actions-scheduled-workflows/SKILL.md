---
name: github-actions-scheduled-workflows
description: >-
  Apply project guidance for GitHub Actions scheduled workflows. Use when adding cron schedules
  to workflows, configuring security audits, or ensuring non-audit jobs do not run on schedule.
---

# GitHub Actions Scheduled Workflows

---

## When to Use

- Adding `schedule:` triggers to GitHub Actions workflows
- Configuring daily security audits with cargo-deny
- Preventing non-audit jobs from running on cron
- Setting up proactive monitoring for dependencies and link rot
- Preventing duplicate/overlapping scheduled runs

## When NOT to Use

- General workflow configuration (see [GitHub Actions Workflow Config](../github-actions-workflow-config/SKILL.md))
- Release gating logic (see [GitHub Actions Release](../github-actions-release/SKILL.md))

## TL;DR

- Add `schedule:` triggers to catch CVEs published between code changes
- Add `if: github.event_name != 'schedule'` to every job that should NOT run on cron
- Only the intended job (e.g., `deny`) omits the schedule guard
- Stagger cron times; do not run everything at midnight UTC
- Document schedule frequency choice with comments

---

## 1. The Problem: Reactive vs Proactive Security

Running security audits only on code changes is reactive (`on: push` +
`pull_request` only). New CVEs published overnight won't trigger the
workflow, advisory databases update independently of code, stale
dependencies accumulate, and nightly toolchains age out unseen.

---

## 2. The Solution: Scheduled Workflows

Add a `schedule:` trigger alongside the per-event ones (daily noon-UTC
audit in `.github/workflows/ci.yml`). Schedule guidance:

| Workflow Type                | Recommended Schedule | Rationale                              |
|------------------------------|----------------------|----------------------------------------|
| Security audits (cargo-deny) | Daily                | New CVEs published frequently          |
| Dependency updates           | Weekly               | Balance freshness with stability       |
| Link checking                | Weekly               | Catch external link rot                |
| Workflow hygiene             | Weekly               | Detect stale toolchains                |
| Unused dependencies          | Changes/manual       | Pinned inputs change in Git            |

Stagger crons (daily noon, nightly 02:00/03:00/04:00); never run
everything at midnight UTC.

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

# Deterministic source analysis: relevant changes plus manual dispatch
unused-deps:
  # No cron: pinned analyzers cannot discover new unused code until source or
  # manifests change.

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

## 8. Skipping Ticks That Re-Verify an Unchanged Head (issue #702)

When the head has not moved since the same workflow last verified it, a
scheduled tick can only reproduce the prior verdict. The shared guard is
[`.github/actions/skip-if-verified`](../../../.github/actions/skip-if-verified)
(reuse it; do not fork it). Review findings that define its contract:

- **Runner env formats: verify against the docs, never the assumption.**
  `GITHUB_WORKFLOW_REF` is owner-qualified
  (`owner/repo/.github/workflows/f.yml@ref`) while the Actions API `path` is
  repository-relative. Parsing without normalizing made the guard never
  match, and the harness masked it because its fixtures seeded the
  implementation's assumed format. Normalize both spellings; seed harness
  fixtures from the documented reality.
- **Listing workflow runs needs job-level `actions: read`.** A workflow
  default of `contents: read` makes every lookup 403, and a fail-open guard
  then silently disables itself. Declare `permissions:` on the guard's job
  and make the warning name the permission.
- **A guard's own output must not satisfy its predicate.** A skipped tick
  still concludes `success` at run level, so "a recent success exists" lets
  one skip sustain itself forever. Count a run as verification only when its
  jobs show a successful executed-work step (`anchor_step`: gating, must-pass,
  never the guard, never checkout). Generalizes to every cache/dedupe guard:
  anchor on evidence the real work ran.
- **Fail-open is a property of every command, not a wrapper.** Under
  `set -euo pipefail`, one unguarded `jq` aborts the step; a job-level caller
  survives via `!cancelled()`, but an in-job caller (unused-deps.yml) fails
  the job and skips the analyzers the guard protects. Guard each fallible
  command with a `|| { warn; stay fail-open; }` handler and validate
  sentinel-derived values before they reach `jq --argjson`.
- **Sentinel fallbacks must point in the fail-open direction.** A
  window-computation failure falling back to epoch `0` counts every prior
  success — a skip-biased guard. The correct fallback is to skip the lookup
  and run. And a documented `0 disables the window` needs its own branch:
  `date -d "-0 hours"` succeeds with cutoff "now", so `0` would otherwise
  never match anything.
- **Guard scripts must stay runner-portable.** GNU `date -d` fails under the
  BSD date of macOS runners, and the nextest cron lane executes test
  harnesses there. Compute cutoffs from `date +%s` with shell arithmetic and
  derive fixture timestamps from `jq` (a guard dependency anyway), never
  from GNU-only flags; the harness pins this with a portability assert.
- **Commit the behavioral harness; wiring pins cannot see runtime.**
  Session-310's ad-hoc run-block harness died with the session, and both
  PR-#704 Bugbot findings were runtime-only (unguarded abort, zero-window).
  `tests/skip_if_verified_action_tests.rs` executes the extracted `run:`
  block with a canned `gh` shim; extend it for any contract change. Extract
  the block from RAW text (`read_file`, not `read_live_file`): the stripped
  view hides full-line comments from ABSENCE pins (documented caveat in
  `tests/common/mod.rs`) and makes the harness execute bytes that differ
  from production.

---

## Best Practices Checklist

- [ ] `schedule:` trigger added to the audit workflow; frequency documented
- [ ] Every non-audit job has `if: github.event_name != 'schedule'`; the audit job omits it
- [ ] Different schedules used for different priorities (no everything-at-midnight)
- [ ] Deterministic source-only analyzers use path triggers plus manual dispatch, not cron
- [ ] Failure notifications configured; concurrency control prevents overlapping runs
- [ ] A config test validates the schedule cohort (e.g. `test_ci_schedule_only_runs_security_jobs`)
- [ ] Skip guards (issue #702): the guard's job declares `permissions:` with
      `actions: read`; anchor step is an executed-work step; harness fixtures
      use documented env formats; skipped runs never count as verification;
      every guard-script command is fail-open-guarded (payload parse failures
      exit 0); `max_success_age_hours=0` disables the window; the behavioral
      harness (`tests/skip_if_verified_action_tests.rs`) stays green

---

## Related Skills

- [GitHub Actions Workflow Config](../github-actions-workflow-config/SKILL.md) — Permissions, path filters, smoke tests
- [GitHub Actions Release](../github-actions-release/SKILL.md) — Release gating and preflight hardening
- [GitHub Actions Config Tests](../github-actions-config-tests/SKILL.md) — Automated validation of CI configuration
- [CI CD Troubleshooting Categories](../ci-cd-troubleshooting/references/diagnostic-workflow.md) — Diagnosing CI failures
