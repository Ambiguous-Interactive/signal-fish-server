# Testing Requirements

See [Core Testing Patterns](skills/testing-core-patterns.md) and
[Testing Tools and Frameworks](skills/testing-tools-and-frameworks.md) for full methodology.

- Every feature/bugfix requires exhaustive tests (happy, negative, edge, concurrent, recovery)
- Data-driven/table-driven tests preferred for validation functions
- **Zero tolerance for flaky tests** -- every failure is a real bug to fix
- Test "the impossible" -- corrupted state, unknown message types, future compatibility
- Tests must pass: `cargo test --all-features` validates all changes before handoff
