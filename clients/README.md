# Reference Clients

This directory holds **reference clients** for the Signal Fish protocol: small, scriptable programs that prove the
documented protocol end to end against the real server and serve as executable documentation for client authors.

Each client is a **standalone package OUTSIDE the root `signal-fish-server` package** — its own `Cargo.lock`, its
own `deny.toml`, its own CI job — so the server crate's MSRV build, lockfile, and coverage gates are untouched,
and the root `Cargo.toml` excludes the whole `clients/` tree from `cargo package`, so the published server crate
ships none of it. (The root panic/timeout policy scans do walk the client sources. The native fixtures track the
server MSRV; the Godot/WASM fixture declares the higher floor required by its exact released adapter.)
See [ADR-0004](../docs/adr/0004-native-reference-client.md) for the full rationale and
[the platform integration guide](../docs/guides/platform-integration.md) for the support roadmap.

## Clients

| Client | Directory | Stack | Status |
|--------|-----------|-------|--------|
| Native | [`native/`](native/README.md) | Rust + [webrtc-rs](https://github.com/webrtc-rs/webrtc) 0.20 (real DTLS/SCTP data channels) | ✅ In-repo, CI-enforced (`.github/workflows/webrtc-interop.yml`) |
| Browser | [`browser/`](browser/README.md) | TypeScript + real headless-Chromium `RTCPeerConnection` (playwright-core) | ✅ In-repo, CI-enforced (`.github/workflows/browser-interop.yml`) |
| Fortress | [`fortress/`](fortress/README.md) | Rust + `fortress-rollback` 0.12.0 + released Signal Fish Rust client 0.8.0 | ✅ In-repo issue-242 regression, CI-enforced (`.github/workflows/fortress-interop.yml`) |
| Fortress WASM | [`fortress-wasm/`](fortress-wasm/README.md) | Godot 4.5 no-thread WASM + Fortress 0.12.0 + released client/adapter 0.9.0 | CI-enforced healthy Chromium acceptance plus expected-`BUSTED` negative control (`.github/workflows/fortress-wasm-interop.yml`) |

Both clients speak the same JSONL stdout event contract and exit codes (the
[native README](native/README.md) is canonical), so one Rust interop harness asserts over native and browser
processes interchangeably. The browser package is an npm package (own `package-lock.json`), not a crate; its
interop cells live in the native crate's test harness behind the `browser-interop` cargo feature. See
[ADR-0005](../docs/adr/0005-browser-reference-client.md).

## Quick start

```bash
bash scripts/run-webrtc-interop.sh    # native client: unit + native<->native interop suite
bash scripts/run-turn-interop.sh      # native clients through pinned local coturn, relay-only
bash scripts/run-browser-interop.sh   # browser client: lint/build + browser<->native interop cells
bash scripts/run-fortress-interop.sh  # two Fortress games through this checkout's server
bash scripts/run-fortress-wasm-interop.sh  # two independent Godot WASM peers in Chromium
```

The first builds the server binary, lints the native client, and runs its unit + multi-process WebRTC interop
suite (mesh, host-star, crippled-ICE fallback, late-join, mixed v2/v3). The second requires a provisioned,
digest-pinned coturn image and cached Cargo dependencies, then runs the relay-only positive and
mismatched-secret fallback controls entirely offline (see the [TURN deployment guide](../docs/deployment-turn.md#repository-turn-only-interoperability-proof)).
The third additionally builds the browser client and runs the browser cells
(mixed mesh, browser↔browser, host star with a browser client,
crippled-ICE browser fallback, the mDNS `.local` trap, pure-v2 browser, mid-handshake close handling, and
SIGTERM/SIGKILL Chromium teardown reaping). Everything runs over loopback; the
only network fetch is the cached Chromium headless-shell download at install time.

The Fortress fixture is a separate compatibility consumer rather than a
reference implementation of the wire protocol. It recreates a 60 Hz rollback
game loop with one polling-client callback per frame and gates throughput,
queue age, delivery conservation, rollback depth, and checksum agreement
against the real server binary.

| Fortress compatibility cell | Runtime | Result |
|---|---|---|
| Native | Two Rust processes over loopback WebSockets | CI-enforced healthy |
| WASM | Godot 4.5 no-thread export in two independent headless-Chromium processes | CI-enforced healthy with released 0.9.0 client/adapter; Chromium only |
