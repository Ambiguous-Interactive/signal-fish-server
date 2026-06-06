# Testing Requirements

See [Core Testing Patterns](skills/testing-core-patterns.md) and
[Testing Tools and Frameworks](skills/testing-tools-and-frameworks.md) for full methodology.

- Every feature/bugfix requires exhaustive tests (happy, negative, edge, concurrent, recovery)
- Data-driven/table-driven tests preferred for validation functions
- **Zero tolerance for flaky tests** -- every failure is a real bug to fix
- Test "the impossible" -- corrupted state, unknown message types, future compatibility
- Tests must pass: `cargo test --all-features` validates all changes before handoff
- Do not discard `tokio::time::timeout(...).await` results in async tests. For expected
  messages, unwrap/assert the timeout, stream, and frame result or use
  `tests/websocket_test_helpers` so failures report timeout/closed/error diagnostics.
  For expected silence, assign the timeout result to a named variable and assert it
  timed out.
- Do not use silent channel drains such as `let _ = rx.try_recv()`, `while let
  Ok(_) = rx.try_recv()`, or `rx.try_recv().is_ok()` before later assertions.
  Assert each expected setup message by type/content, or use an explicit helper
  that distinguishes empty channels from disconnected channels.
- For ordered protocol flows, read and assert the exact next message at each step.
  Avoid matchers that skip nonmatching `ServerMessage`s when stale or unexpected
  frames would indicate a broken test setup; assert no pending messages at phase
  boundaries when backlog would affect later checks.
