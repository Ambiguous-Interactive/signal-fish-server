# Testing Requirements

See [Core Testing Patterns](skills/testing/SKILL.md) and
[Testing Tools and Frameworks](skills/testing/references/tools-and-frameworks.md) for full methodology.

- Every feature/bugfix requires exhaustive tests (happy, negative, edge, concurrent, recovery)
- Data-driven/table-driven tests preferred for validation functions
- **Zero tolerance for flaky tests** -- every failure is a real bug to fix
  (full policy below: [Zero-Flakiness Policy](#zero-flakiness-policy-zero-tolerance))
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
- Test code must fail **loudly**. An error logged with `tracing::error!` must never
  be followed by a silent `return;`/`return Ok(());` (which lets CI pass while the
  setup/exchange actually failed). Use `panic!`/`assert!`/`.expect()`, or make the
  helper return `Result` and propagate with `?` (the wrapper panics on `Err`).
  Enforced repo-wide by `tests/loud_test_failures_scan.rs` — a `syn`-AST scan,
  self-tested with flags/allows fixtures, mirroring `tests/async_timeout_policy_scan.rs`
  (covers bare and `if`/`match`-guarded silent returns; `eprintln!` skip-notices are
  intentionally exempt).
- For ordered protocol flows, read and assert the exact next message at each step.
  Avoid matchers that skip nonmatching `ServerMessage`s when stale or unexpected
  frames would indicate a broken test setup; assert no pending messages at phase
  boundaries when backlog would affect later checks.
- When asserting that a raw JSON wire frame omits a field, parse the frame and
  inspect the exact JSON path (for example `/data/ice_servers`) instead of using
  string containment. Whole-frame substring checks can be false positives when
  payloads include nested messages such as `Reconnected.missed_events`.
- When asserting ordered composition from multiple sources (for example static
  `session.ice_servers` followed by `[turn]`-derived STUN/TURN entries), use
  source-distinguishable sentinel fixture values and assert the exact ordered
  sequence. If a test relies on defaults, add a fixture invariant so a future
  default/static value collision fails before it weakens the ordering assertion.

## Zero-Flakiness Policy (zero tolerance)

A **flaky test** is any test whose pass/fail outcome can change without a change
to the system under test — it depends on timing, ordering, scheduling, resource
contention, available CPU/ports, wall-clock, randomness, or test-execution order.
**A flake is a bug** (in the test or in the system), and this repository has
**zero tolerance** for them.

**Non-resolutions (forbidden).** None of these "fix" a flake — they hide it:

- Re-running until green, or relying on "it passes on retry".
- "It passes in isolation / on my machine" — a test that only passes when not
  contended is flaky. Every test MUST pass deterministically under BOTH
  `cargo nextest run` and a raw, **oversubscribed** `cargo test --all-features`
  (more concurrent tests than cores), because CI and developer machines run
  loaded.
- Adding nextest `retries`, `#[ignore]`, sprinkling `sleep`, or loosening an
  assertion to "make it pass".

**Resolution (required) — root-cause the non-determinism and eliminate it:**

- Replace fixed `sleep`s with **condition polling against a generous deadline**
  (a ceiling, not an expected wait): poll the actual readiness/state, return the
  instant it holds. Generous ceilings only bite under pathological load and do
  not slow the happy path.
- No ordering assumptions on concurrent producers; assert exact sequences or use
  the deadline-driven helpers in `tests/websocket_test_helpers` (never silent
  drains).
- **Resource-heavy / process-spawning / port-binding tests** (e.g. the
  multi-process suites that spawn the real server binary) must be (a) isolated
  from concurrency contention so a loaded runner cannot CPU/port-starve them past
  a deadline — use a nextest **test-group** with a bounded `max-threads`
  (`.config/nextest.toml`), not retries — and (b) written with
  saturation-tolerant ceilings (readiness/connect/message deadlines large enough
  that a starved-but-progressing process still completes). Allocate a unique
  ephemeral port / temp dir / id per test; never share mutable global state.
- No `Math.random` / `Instant::now`-derived values in assertions; pin fixtures.

**If a flake cannot be eliminated conclusively in the same change**, it is
**never left silent**: record it in `PLAN.md` with (1) the observed symptom and
exact reproduction conditions, (2) the root-cause hypothesis, (3) the mitigation
applied so far, and (4) the remaining research/fix items — and keep it open until
it is closed deterministically. A "known flaky" test without a tracked PLAN item
is a policy violation.

**Runner settings.** `.config/nextest.toml` sets `retries = 0` (implicitly — no
`retries` key) on purpose; do not add blanket retries. Use
`[[profile.*.overrides]]` + `[test-groups]` for per-test concurrency isolation
of resource-heavy tests instead.

**CI job timeouts are a flake source too.** A too-tight `timeout-minutes` on a
slow job cancels a healthy-but-loaded run, which is non-determinism by the same
definition above. This is why the mutation-testing shards keep a timeout _floor_
(generous headroom over the measured per-shard wall-clock) rather than the
tightest value that "usually" passes. See
[Mutation Testing Performance](skills/mutation-testing-performance/SKILL.md) for the
feasibility contract and `MUTPERF-001` in `PLAN.md`.
