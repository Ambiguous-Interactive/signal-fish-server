# Session 056 — P13 released-client acceptance

## Trigger

The fixed Signal Fish client is now available on crates.io as 0.9.0 together
with the new lockstep `signal-fish-client-godot` 0.9.0 adapter. P13 had remained
intentionally pinned to client 0.8.0 and accepted only the known completion
bottleneck while waiting for these releases.

## Change

The standalone Godot/WASM fixture now exact-pins both 0.9.0 registry crates.
The core dependency enables only `polling-client`; the companion crate owns
`GodotWebSocketTransport`, matching the released breaking package boundary.
Runtime reports and structural policy tests attest both versions. Because the
adapter requires Rust 1.94, the fixture declares that honest standalone floor
and continues to build under its separately pinned nightly toolchain without
raising the server crate's MSRV.

The primary two-Chromium classifier no longer accepts the historical
completion bottleneck. Both peers must pass every existing P13 health and
identity invariant and print `HEALTHY`. The bounded one-admission-per-callback
run remains an expected-`BUSTED` negative control.

## Verification

Focused local verification passed:

- exact locked feature-tree inspection shows core 0.9.0, adapter 0.9.0,
  godot-rust 0.4.5, and Fortress 0.10.0 from the standalone graph;
- all 286 applicable `ci_config_tests` passed (one expensive matrix ignored);
- all six MSRV consistency script tests and the live repository check passed;
- targeted Clippy passed with warnings denied;
- root and fixture formatting, JavaScript and shell syntax, Cargo metadata,
  documentation consistency, internal links, and Markdown policy passed.

The authoritative completion evidence is the dedicated real Godot 4.5
no-thread Emscripten/two-Chromium workflow on the exact PR head.
