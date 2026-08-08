# Fortress + Signal Fish Godot/WASM interoperability fixture

This standalone fixture is the browser half of the issue-242 regression. It
compiles the registry releases `fortress-rollback` 0.10.0,
`signal-fish-client` 0.9.0, and `signal-fish-client-godot` 0.9.0 into a Godot
4.5 GDExtension for
`wasm32-unknown-emscripten`, exports with the official no-thread web template,
and drives two independent browser processes through the server built from the
current checkout. Chromium is the required pull-request gate. A weekly Firefox
cell runs the same released-client and negative-control oracles to detect
browser-specific drift without lengthening every pull request.

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
templates, and Rust `nightly-2026-03-01` plus `rust-src`. The adapter's declared
Rust floor is 1.94.0, intentionally higher than the standalone server crate's
MSRV. The primary cell requires both peers to satisfy every healthy throughput,
progress, queue-age, conservation, rollback, checksum, cadence, and runtime
identity gate. Any missing report or invariant violation fails closed as
`BUSTED`; only the complete two-peer result prints `HEALTHY`. The hosted WASM
gate permits at most one recovered prediction-window-denied game callback and
fails on a second. Wait recommendations remain forbidden, as do every existing
loss, overflow, throughput, queue-age, lag, rollback, checksum, and conservation
violation. The native Fortress gate retains its stricter zero-stall boundary.

Exact-head CI with the released 0.9.0 graph passed the complete primary cell:
both reports satisfied every health and identity invariant and printed
`HEALTHY`. The following capped control printed its expected `BUSTED` verdict.

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

Set `FORTRESS_WASM_BROWSER=firefox` to reproduce the scheduled Firefox cell;
the default is `chromium`. Both browsers come from the exact `playwright-core`
version in `clients/browser/package-lock.json` and are installed afresh rather
than restored from a browser cache. A manual workflow dispatch also selects
Firefox, allowing exact-head verification before merge.

The Godot web export requires a WebGL2 context, and CI runners have no GPU.
Chromium reaches its own software rasterizer (SwiftShader) through
`--enable-webgl --ignore-gpu-blocklist`. Firefox has no such fallback: it asks
the platform for a GL device and, finding none, reports
`Exhausted GL driver options (FEATURE_FAILURE_WEBGL_EXHAUSTED_DRIVERS)`. It
needs two things:

1. `webgl.force-enabled`, to bypass the blocklist that refuses WebGL without an
   accelerated adapter;
2. an **X display**, which headless Firefox still uses to resolve a Mesa GL
   driver. `scripts/run-fortress-wasm-interop.sh` runs the Firefox harness under
   `xvfb-run`.

The display is what made the cell environment-dependent — a workstation with
`DISPLAY` set passes while a CI runner without one fails. Reproduce the CI
condition locally with `env -u DISPLAY`, which yields
`Exhausted GL driver options (FEATURE_FAILURE_WEBGL_EXHAUSTED_DRIVERS)`.

The workflow also pins the Mesa/EGL packages. That is defensive rather than
operative: the current `ubuntu-24.04` image already ships `libgl1-mesa-dri`,
`libglx-mesa0`, `xvfb`, and `xauth`, and a CI bisection showed those packages
alone did **not** fix the cell — only adding the display did. They are pinned so
the cell does not depend on what a future image happens to carry, since
`playwright install --with-deps firefox` contributes no GL packages of its own.

Both browsers are probed for a WebGL2 context before the export is loaded, so
that failure is reported with the browser's own
`webglcontextcreationerror` reason instead of a bare wait for a page global.
