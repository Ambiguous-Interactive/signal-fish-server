# Fortress + Signal Fish Godot/WASM interoperability fixture

This standalone fixture is the browser half of the issue-242 regression. It
compiles the registry releases `fortress-rollback` 0.13.0,
`signal-fish-client` 0.14.0, and `signal-fish-client-godot` 0.14.0 into a Godot
4.7.2 GDExtension for
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

Each report also retains the first 64 consumed Fortress wait recommendations in
`wait_observations.events` and counts later observations in `omitted`. Each event
records time since configuration, callback/poll counts, time since the previous
active callback, remote-poll duration, frames before and after that poll, confirmed
frame, frame advantage, both peer lag estimates, requested skips and relay state.
These values describe event consumption after remote polling; Fortress may have
generated the event during the preceding advance. Peer lag estimates are not
synchronized remote clock samples. A pending event can make the cumulative
`wait_recommendations` metric exceed the consumed observation count. The existing
zero-recommendation health gate remains unchanged. A second bounded trace,
`wait_generation_observations`, records each recommendation inside the advance
that produced it, even if the next callback never consumes the event. It includes
the advance duration and starting frame. The validator pairs the retained
prefixes by order and checks callback order, elapsed time and requested skips.
Generation counts must match the cumulative metric, including omitted events.
The released Fortress 0.13.0 source checks recommendations during `advance_frame`;
these traces distinguish that operation from the following remote poll.
A passing run does not explain issue #1312's retained failure. Hosted runs retain
control reports for 14 days on both success and failure. Failed runs also retain
the exported project.

Run the complete gate with:

```bash
bash scripts/run-fortress-wasm-interop.sh
```

Run this timing gate without concurrent builds.

The runner requires Emscripten 6.0.12, Godot 4.7.2 stable with its official web
templates, and Rust `nightly-2026-10-16` plus `rust-src`. The adapter's declared
Rust floor is 1.94.0, matching the standalone server crate's
MSRV. The primary cell requires both peers to satisfy every healthy throughput,
progress, queue-age, conservation, rollback, checksum, cadence, and runtime
identity gate. Any missing report or invariant violation fails closed as
`BUSTED`; only the complete two-peer result prints `HEALTHY`. The hosted WASM
gate bounds stalls as a rate — at most 20 per-mille of confirmed frames — so
isolated contention-induced stalls on a shared runner cannot fail an otherwise
healthy run, while any systematic stalling regression still fails. Wait
recommendations remain forbidden, as do every existing
loss, overflow, throughput, queue-age, lag, rollback, checksum, and conservation
violation. The native Fortress gate retains its stricter zero-stall boundary.

The runner then executes a one-admission-per-callback negative control and
requires the same healthy validator to classify that run as the expected
`BUSTED` result. The negative control runs the same minimum 600 active callbacks
as the released cell. It must drain at least 90% of the callback budget it
actually drove as enqueued relay frames and completed sends. The old gate
demanded an absolute 600 sends. That number sat exactly at the cap's
theoretical maximum, so a fully working 598/600 run on a loaded runner read
as vacuous (issue #639). The control must show an observed maximum of exactly
one admission per callback, and is accepted
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
`--enable-webgl --ignore-gpu-blocklist`. In a Wayland devcontainer, run
`env -u DISPLAY -u WAYLAND_DISPLAY bash scripts/run-fortress-wasm-interop.sh`
to keep the headless browser on this path. Firefox has no such fallback: it asks
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
