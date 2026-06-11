# Reference Clients

This directory holds **reference clients** for the Signal Fish protocol: small, scriptable programs that prove the
documented protocol end to end against the real server and serve as executable documentation for client authors.

Each client is a **standalone package OUTSIDE the root `signal-fish-server` package** — its own `Cargo.lock`, its
own `deny.toml`, its own CI job — so the server crate's MSRV build, lockfile, and coverage gates are untouched,
and the root `Cargo.toml` excludes the whole `clients/` tree from `cargo package`, so the published server crate
ships none of it. (The root panic/timeout policy scans do walk the client sources, and the root MSRV-consistency
check pins the native client's `rust-version` to the server's — those root disciplines apply identically.)
See [ADR-0004](../docs/adr/0004-native-reference-client.md) for the full rationale and PLAN.md P7 for the roadmap
context.

## Clients

| Client | Directory | Stack | Status |
|--------|-----------|-------|--------|
| Native | [`native/`](native/README.md) | Rust + [webrtc-rs](https://github.com/webrtc-rs/webrtc) 0.17 (real DTLS/SCTP data channels) | ✅ In-repo, CI-enforced (`.github/workflows/webrtc-interop.yml`) |
| Browser | — | WASM / `web-sys` (planned) | Planned next (PLAN P7 task 3) |

## Quick start

```bash
bash scripts/run-webrtc-interop.sh
```

That builds the server binary, lints the native client, and runs its unit + multi-process WebRTC interop suite
(mesh, host-star, crippled-ICE fallback, late-join, mixed v2/v3 — all over loopback, zero external network).
