---
name: ci-troubleshooting
description: Diagnose, repair, and prevent CI/CD and GitHub Actions failures in Signal Fish. Use for workflow YAML, Actions permissions or caching, lint and documentation failures, release or scheduled workflows, shell-in-CI bugs, stale references, and regression-test coverage for CI configuration.
---

<!-- markdownlint-disable MD013 -->

# CI Troubleshooting

Treat the failing check as data. Reproduce the narrowest failure, identify the contract it violates, fix the source, and add a guard that fails for the original defect.

## Route the task

- Start broad investigations with [ci-cd-troubleshooting-index.md](references/ci-cd-troubleshooting-index.md), [ci-cd-troubleshooting-categories.md](references/ci-cd-troubleshooting-categories.md), and [ci-cd-troubleshooting-ecosystem.md](references/ci-cd-troubleshooting-ecosystem.md).
- For workflow structure and preventive tests, read [GitHub-actions-workflow-config.md](references/github-actions-workflow-config.md), [GitHub-actions-config-tests.md](references/github-actions-config-tests.md), and [ci-config-live-view-tests.md](references/ci-config-live-view-tests.md).
- For caching, release, or schedules, read [GitHub-actions-caching.md](references/github-actions-caching.md), [GitHub-actions-release.md](references/github-actions-release.md), or [GitHub-actions-scheduled-workflows.md](references/github-actions-scheduled-workflows.md).
- For shell and AWK inside Actions, read [GitHub-actions-bash-scripts.md](references/github-actions-bash-scripts.md), [GitHub-actions-awk.md](references/github-actions-awk.md), and the `$shell-scripting` skill.
- For focused failures, read [linting](references/ci-cd-troubleshooting-linting.md), [links](references/ci-cd-troubleshooting-links.md), [scripts and tests](references/ci-cd-troubleshooting-scripts.md), or [supply-chain and stale references](references/ci-cd-troubleshooting-supply-chain.md).
- Load only the closest concrete precedent when it helps: [changelog gate versus Dependabot](references/ci-cd-troubleshooting-example-changelog-dependabot-bump.md), [Dependabot comment drift](references/ci-cd-troubleshooting-example-dependabot-comment-drift.md), [Python cache mismatch](references/ci-cd-troubleshooting-example-python-cache-mismatch.md), [stale nightly](references/ci-cd-troubleshooting-example-stale-nightly-toolchain.md), or [unused dependencies](references/ci-cd-troubleshooting-example-unused-dependencies.md).

## Validate

Run the smallest reproducer first, then the repository check that owns the contract. Add a data-driven assertion to `tests/ci_config_tests.rs` or the relevant focused test when static configuration could regress silently.
