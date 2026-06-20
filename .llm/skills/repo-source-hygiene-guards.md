# Skill: Repo Source Hygiene Guards

<!--
  trigger: drift guard, hygiene guard, reason string drift, ignored test, exit vs return, python -m pip, source-of-truth
  | Static guards that keep docs, tests, and scripts from drifting from the source of truth
  | Core
-->

**Trigger**: When editing protocol docs, integration tests, or bootstrap/CI shell scripts —
or when one of the guard tests below fails.

---

## Why

Some bugs cannot be caught by `cargo test` on the application alone — they live in
_supporting material_ (docs examples, test expectations, shell scripts) that drifts from
the code it describes, often silently because nothing runs it. We catch these with small
**static guards** (plain `cargo test` files that read repo source and assert invariants).
Each guard below was added after a real drift escaped review. Treat a guard failure as a
real bug in the thing it checks — never weaken the guard to make it pass.

These complement the protocol drift guards in `tests/protocol_spec_consistency.rs` and
`tests/docs_site_consistency.rs` (which tie the AsyncAPI spec / MkDocs pages to the Rust
message + error-code enums).

---

## The guards

### 1. Documented wire strings must match the server (`tests/docs_site_consistency.rs`)

`reconnection_failure_docs_use_canonical_reason_strings` builds the set of reasons the
server can emit — the `ReconnectionError::Display` arms in `src/reconnection.rs` (the typed
path sends `error.to_string()`) PLUS every string literal in `src/server/reconnection_service.rs`
— and asserts every `ReconnectionFailed.reason` in `docs/**` is one of them. So an invented
paraphrase like "The reconnection token is invalid or malformed." lies about the contract a
client matches. Reasons reach the wire through several paths (an inline `reason:` field, a
`reason: &str` helper), so rather than track each path (brittle — it already missed one) the
guard takes ALL literals in that module: a deliberate superset that can never reject a real
reason while still catching invented strings.

**Rule**: A documented `reason`/`error_code` must be copied from its source of truth, not
paraphrased. When a server string lives in a single `Display`/enum, prefer a parsed guard
(no hand-kept list) over eyeballing. See [Documentation Accuracy Guarantees](./doc-accuracy-guarantees.md).

### 2. Integration tests must use the in-process harness (`tests/source_hygiene_guards.rs`)

`integration_tests_do_not_hardcode_ws_endpoints` forbids a literal `ws://host:port` in
`tests/`. A hardcoded endpoint can only reach a hand-started server, so the test ends up
`#[ignore]`d — and an ignored test silently drifts (a stale `lobby_e2e_tests.rs` kept
expecting a `LobbyStateChanged` the server had stopped broadcasting). Real tests dial the
embedded harness: `tests/e2e_tests.rs::start_test_server` (binds `127.0.0.1:0`, dial
`ws://{addr}`), or drive the server directly via `tests/test_helpers.rs::create_test_server`.

**Rule**: Never add an `#[ignore]`d test that targets an external server; run it in-process
instead. If a scenario is already covered by a running test, delete the dead duplicate.

### 3. Bootstrap best-effort functions `return`, never `exit` (`tests/source_hygiene_guards.rs`)

`bootstrap_recoverable_functions_do_not_exit` checks `.devcontainer/post-create.sh`: a
function reachable from a recovery site (`if ! step` / `step ||`) must not `exit` (directly
or transitively). An in-function `exit` aborts the whole container setup and defeats the
`if ! step; then warn; continue` handling (it killed the optional Codex install).

**Rule**: In best-effort bootstrap scripts, functions `return` a non-zero status and let the
top level decide. The guard is intentionally scoped to bootstrap scripts — fail-fast
checkers (`scripts/check-*.sh`) and fail-closed steps (`run-tla-model-check.sh` refusing an
unverified jar) `exit` from helpers on purpose and must NOT be flagged.

### 4. Install Python packages via `python3 -m pip` (`tests/source_hygiene_guards.rs`)

`scripts_install_python_packages_via_python_m_pip` forbids a bare `pip`/`pip3` command in
operational scripts. A bare `pip` may target a different interpreter than the `python3` that
imports the package, so the install can succeed yet the import fail. `python3 -m pip`
installs into the interpreter that runs it.

---

## Adding a guard (red → green)

1. Write the guard FIRST and run it — confirm it FAILS (red) on the current drift, naming
   every offender. A guard that never goes red proves nothing.
2. Derive the allowed/expected set from source (parse the enum / `Display` / harness), never
   a hand-kept list — that is what makes it self-maintaining.
3. Scope precisely to avoid false positives (e.g. the exit rule excludes fail-closed
   scripts); a noisy guard gets disabled, which is worse than no guard.
4. Fix the offenders and re-run — confirm GREEN.

---

## Related Skills

- [Documentation Accuracy Guarantees](./doc-accuracy-guarantees.md)
- [Shell Scripting Patterns](./shell-scripting-patterns.md)
- [Testing Core Patterns](./testing-core-patterns.md)
