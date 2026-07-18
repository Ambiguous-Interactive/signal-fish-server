# Session 059 — P13 scheduled Firefox WASM interoperability

## Trigger

P13 deliberately limited its release-client acceptance claim to Chromium and
deferred a Firefox cell until the primary Godot/no-thread reproduction was
deterministic. Session 056 made the exact released 0.9.0 graph healthy under
that primary gate, so the deferred cross-browser check is now actionable.

## Change

- The existing fail-closed harness selects `chromium` by default or `firefox`
  through `FORTRESS_WASM_BROWSER`; both paths retain separate browser processes,
  executable hashing, PID/report binding, no-worker attestation, exact relay
  ledgers, and the same health classifier.
- Pull requests and pushes continue to run Chromium, preserving the established
  acceptance signal and runtime. Weekly schedules and manual dispatches run
  Firefox with the same released-client and one-admission negative-control
  cells, allowing exact-head verification before merge.
- The runner installs only the selected browser from the exact locked
  `playwright-core` version. Browser binaries remain uncached so a mutable local
  installation cannot satisfy CI.
- Structural policy coverage pins the schedule, event-to-browser mapping,
  selector, and exact selected-browser installation path.

## Verification

- Red-green focused `ci_config_tests` coverage for the scheduled selector and
  browser-agnostic harness path, followed by the full suite: 286 passed, one
  ignored.
- `node --check`, `bash -n`, ShellCheck, Actionlint, and workflow hygiene.
- Documentation consistency, Markdown lint, Cargo deny, and the documentation
  policy/script suites (five passed).
- Exact-head Chromium and manually dispatched Firefox workflow evidence is
  recorded after publication; the heavy Godot/WASM gate is intentionally left
  to CI under the goal's local-testing constraint.
