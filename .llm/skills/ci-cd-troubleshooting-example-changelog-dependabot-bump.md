# Skill: CI/CD Troubleshooting Example - Changelog Gate vs Dependabot Bump

<!--
  trigger: ci example dependabot changelog gate, changelog false positive dependency bump
  | Example incident: changelog gate blocked a dependency-only update
  | Infrastructure
-->

**Trigger**: When changelog policy blocks dependency-only changes after bot PR merge workflows.

---

## Incident

**Problem:** A squash-merged Dependabot bump (`tempfile 3.26.0 -> 3.27.0`) triggered
the documentation consistency changelog gate.

---

## Symptoms

- `[ERROR] Detected non-internal changes without CHANGELOG.md update: Cargo.toml`
- Main branch push failed and downstream jobs were cancelled

---

## Root Cause

Actor-based bot detection was brittle after squash merge because the actor became
the human merger, not `dependabot[bot]`.

---

## Resolution

Add dependency-only detection that validates:

- non-internal changed files are only `Cargo.toml` and/or `Cargo.lock`
- commit message matches dependency-bump patterns

Then back this with data-driven tests for commit-message pattern matching and
argument parsing guards.

---

## Related Skills

- [CI CD Troubleshooting Categories](./ci-cd-troubleshooting-categories.md) — Category map and diagnostic workflow
- [Version Sync And Changelog Gates](./version-sync-and-changelog-gates.md) — Changelog gating policy
- [Testing Core Patterns](./testing-core-patterns.md) — Data-driven testing techniques
