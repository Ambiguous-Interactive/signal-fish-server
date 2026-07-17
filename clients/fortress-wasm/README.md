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
templates, and Rust `nightly-2026-03-01` plus `rust-src`. The exact released
graph produces a reproducible `BUSTED` result. The complete exact-head reports
reached confirmed frame 607, but client completions remained near one per
callback and missed the healthy throughput gates despite multi-send adapter
admission. Observed callback cadence also varies with runner load, and one peer
recorded a wait recommendation; those failures are accepted only alongside the
per-peer completion bottleneck. The characterization gate requires both peers
to reach at least 600 confirmed frames, preserve every conservation and
correctness gate, and avoid unrelated failures before the expected `BUSTED`
result can pass CI. An unexpectedly healthy release is surfaced rather than
silently classified as busted. P13 therefore remains open.

The runner then executes a one-admission-per-callback negative control and
requires the same healthy validator to classify that run as the expected
`BUSTED` result. The negative control runs the same minimum 600 active callbacks
as the released cell, must admit and complete at least 600 workload messages
with an observed maximum of exactly one admission per callback, and is accepted
only when that cap breaks both completed-send throughput and sends-per-callback
gates. Cap-induced waits and
runner-dependent slow cadence may accompany those required failures but cannot
substitute for them; neither can the expected frame-progress and checksum-sample
shortfalls after the fixed 600-callback budget. Browser logs, errors,
diagnostics, and any available partial report are retained before peer shutdown
on failures.
