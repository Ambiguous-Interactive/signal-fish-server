# Fortress + Signal Fish Godot/WASM interoperability fixture

This standalone fixture is the browser half of the issue-242 regression. It
compiles the registry releases `fortress-rollback` 0.10.0 and
`signal-fish-client` 0.8.0 into a Godot 4.5 GDExtension for
`wasm32-unknown-emscripten`, exports with the official no-thread web template,
and drives two independent Chromium processes through the server built from the
current checkout.

The Rust `FortressWasmPeer::process` callback owns the full game/network loop.
Each callback calls `SignalFishPollingClient::poll` exactly once, advances the
same deterministic workload and bounded relay adapter used by the native
fixture, and records callback cadence plus cross-peer application sequence
ledgers. GDScript only transfers injected configuration, creator room readiness,
and the final Rust-origin JSON report across `JavaScriptBridge`. Before handing
configuration to Rust, it adds the actual `Engine.get_version_info()` identity;
Rust validates that runtime and echoes the structured identity in its report.

Run the complete gate with:

```bash
bash scripts/run-fortress-wasm-interop.sh
```

The runner requires Emscripten 3.1.74, Godot 4.5 stable with its official web
templates, and Rust `nightly-2026-03-01` plus `rust-src`. It prints `HEALTHY`
only after the two 600-frame reports satisfy throughput, latency, conservation,
rollback, checksum, no-thread, no-worker, and process-identity gates. It then
runs a one-admission-per-callback negative control and requires the same healthy
validator to classify that run as the expected `BUSTED` result. The negative
control runs the same minimum 600 active callbacks as the healthy control and is
accepted as `BUSTED` only when its admission cap breaks completed-send
throughput or sends-per-callback gates. Browser logs, errors, diagnostics, and
any available partial report are retained before peer shutdown on failures.
