---
name: testing-rust
description: Design, implement, diagnose, and optimize deterministic tests for Signal Fish Rust code and repository configuration. Use for unit, integration, end-to-end, async, property, fuzz, fixture, CI coverage, assertion-message, test-organization, or mutation-testing tasks.
---

<!-- markdownlint-disable MD013 -->

# Rust Testing

Every behavior change requires tests. Cover the happy path, negative and error paths, boundary cases, cleanup, and concurrency where applicable. Treat every failure as a defect to explain; do not label failures flaky.

## Route the task

- Read [project-testing.md](references/project-testing.md) for repository-specific requirements and commands.
- Read [testing-core-patterns.md](references/testing-core-patterns.md) for test design and naming.
- Read [testing-tools-and-frameworks.md](references/testing-tools-and-frameworks.md) when selecting proptest, fuzzing, nextest, coverage, or other tools.
- Read [testing-ci-coverage.md](references/testing-ci-coverage.md) for CI and coverage design.
- Read [test-fixture-structure.md](references/test-fixture-structure.md) and [test-fixture-ci-patterns.md](references/test-fixture-ci-patterns.md) for fixture-driven checks.
- Read [testing-error-message-quality.md](references/testing-error-message-quality.md) for actionable assertions; load the [MSRV consistency assertion example](references/testing-error-message-example-msrv-consistency.md) only when useful.
- Read [mutation-testing-performance.md](references/mutation-testing-performance.md) before changing mutation shards, timeouts, or linker strategy.

Run the narrowest affected test during iteration, then the full mandatory Rust validation before handoff.
