---
name: repo-source-hygiene-guards
description: >-
  Apply project guidance for repo source hygiene guards. Use when editing protocol docs,
  integration tests, or bootstrap/CI shell scripts — or when one of the guard tests below
  fails. Also use when ADDING a new src/ library module (Miri lane registration), a new
  production clock read (chrono classification), panic-capable production code
  (zero-panic policy), or a test that needs a compiled-in feature (feature gating).
---

# Repo Source Hygiene Guards

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
(no hand-kept list) over eyeballing. See [Documentation Accuracy Guarantees](../documentation/references/accuracy-and-drift.md).

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

### 5. New `src/` modules and new clock reads must register with the repo-wide scanners

Two scanners key on file inventory, so a change that only ADDS a module (or a clock read)
passes every scoped, edit-test-loop check and still fails hosted CI:

- **Miri lane coverage** (`test_ci_safety_miri_lane_filters_cover_every_library_module`,
  `tests/ci_config_tests.rs`): every library module must be named by a Miri lane filter in
  `.github/workflows/ci-safety.yml` (`core`: `websocket coordination server protocol`;
  `remaining`: everything else). A module named by no lane "would silently never run under
  Miri" — the guard names the missing module and the lane list. Register the new module in
  the SAME change; the fix is one word in the `remaining` lane's `filters` string unless the
  module is measured-heavy.
- **Clock-source classification** (`tests/clock_source_scan.rs`): every `chrono::Utc::now()`
  (and allowlisted `std` time type) in production `src/` needs its file in
  `chrono_clock_allowlist()` with the class stated (`durable record` / `embedder
  convenience` / `observability readout`) and a `Wall clock (...):` comment at the site;
  deadline and GC decisions use monotonic `tokio::time` or an injected `*_at(.., now)` seam
  instead. The allowlist entry must keep a live match (`allowlist_entries_stay_relevant`).
- **Zero-panic policy** (`scripts/check-no-panics.sh`, hosted Lint): production code denies
  `expect_used`/`unwrap_used`/`panic!` — test modules are exempt. A lock guard that must
  survive poisoning recovers the data: `.lock().unwrap_or_else(|error| error.into_inner())`
  (the `trace_validation.rs` pattern).

**Rule**: any change that adds a `src/` module, a production clock read, or panic-capable
code runs the scanners' owning targets locally in the same change
(`cargo nextest run --test ci_config_tests --test clock_source_scan` plus
`bash scripts/check-no-panics.sh`), or treats the hosted Lint/Nextest red as the
first signal. A scoped `-E 'test(<name>)'` loop on the changed feature's own target never
runs these guards.

### 6. Tests that need a compiled-in capability are feature-gated

Config that requires a compiled feature fail-closes at construction
(`security.transport.token_binding.required=true` needs the `tls` Cargo feature). A test
that builds such a config must carry `#[cfg(feature = "tls")]` (precedent:
`mtls_token_binding_e2e.rs`, and the `#[cfg(not(feature = "tls"))]`
`validate_config_rejects_tls_for_a_binary_without_tls_support` pin beside it), or the
`--no-default-features` suite is red even though every default-features run is green.

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

- [Documentation Accuracy Guarantees](../documentation/references/accuracy-and-drift.md)
- [Shell Scripting Patterns](../shell-scripting-patterns/SKILL.md)
- [Testing Core Patterns](../testing/SKILL.md)
