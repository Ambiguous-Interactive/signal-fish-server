# Architecture and File Reference

See [Detailed File Tables](context-file-reference.md) for full file tables.

## Architecture At-a-Glance

```text
+-----------------------------------------------------+
|  CLIENTS: Game Engines | Browser WebRTC | Custom    |
+------------------------+----------------------------+
                         |
                         v
+-----------------------------------------------------+
|  SIGNAL FISH SERVER (Rust) -- axum + tokio          |
|  WebSocket(/v2/ws) | Health(/v2/health) | Metrics   |
|  EnhancedGameServer (Room/Player/Authority Mgmt)    |
|  Storage: In-Memory Only                            |
+-----------------------------------------------------+
```

## Key Files at a Glance

`src/main.rs` (entry), `src/server.rs` (room/player logic),
`src/websocket/` (WS lifecycle), `src/protocol/` (messages and types),
`src/config/` (all config structs), `src/auth/` (auth and rate limiting).

`clients/` holds reference clients as standalone packages OUTSIDE the root
package (own lockfile/deny.toml; outside the root cargo build/test/coverage
gates, though the root panic/timeout policy scans do walk its sources and
`scripts/check-msrv-consistency.sh` pins every Rust client manifest to the
root MSRV). `clients/native/` is the webrtc-rs reference client +
multi-process interop suite (run via `scripts/run-webrtc-interop.sh`; see
`clients/native/README.md` and ADR-0004). `clients/browser/` is the
TypeScript browser reference client (real headless-Chromium
`RTCPeerConnection` via playwright-core, own `package-lock.json`); its
browser↔native interop cells live in the native crate behind the
`browser-interop` cargo feature and run via `scripts/run-browser-interop.sh`
(see `clients/browser/README.md` and ADR-0005). Both clients share one JSONL
stdout event contract and exit codes (the native README is canonical).
`clients/fortress/` is the native Fortress rollback interoperability fixture;
`clients/fortress-wasm/` reuses its deterministic workload and relay ledger in
a Godot 4.5, single-threaded Emscripten build exercised by two isolated browser
processes via `scripts/run-fortress-wasm-interop.sh`. The exact released 0.9.0
client + Godot adapter graph is required to pass every health gate in two
Chromium processes; the same harness retains an expected-`BUSTED`
one-admission-per-callback negative control. P13 is complete for the explicitly
scoped Chromium cell.

## Architectural Invariants

Protocol v3 routing invariant: `websocket::create_router()` is nest-safe and
must not expose `/v3/ws` by itself; production mounts it under `/v2` and adds
top-level `/v3/ws` separately. Standalone/library servers that serve Signal Fish
at the HTTP root should use `websocket::create_standalone_router()` when they
want both `/ws` and `/v3/ws`.

Signaling rate limits are split intentionally: `max_signals` counts validated
WebRTC `Signal` dispatch attempts, while `max_signal_errors` counts rejected
`Signal` attempts. Do not move target/transport validation in a way that lets
invalid traffic avoid `max_signal_errors` or consume the valid ICE budget.

Room finalization: borrow one player snapshot for peers, then move it into
`FinalizedRoom`; no deep clone. In-memory room-operation locks are operation
guards, not timing budgets; release them on success/error paths.

Reconnection claims are intentionally two-phase: `claim_reconnection` reserves
the pending record to prevent duplicate winners, but only successful reconnects
remove it. Every post-claim failure path must release the claim and roll back
room-side restoration so clients can retry with the same token until the
reconnection window expires. Do not make active claims stealable or reusable
while the original reconnect task can still continue. `handle_reconnect` uses
a drop guard to release abandoned claims and completes the claim as soon as
connection reassignment succeeds.

Session-plan topology/transport selection invariants are documented in
[Protocol v3 Session-Plan Selection](skills/protocol-v3-session-plan/SKILL.md).

Protocol v3 `TransportStatus` is informational, but still a negotiated-capability
boundary: accept reports only from v3 connections and only for transports present
in that connection's negotiated transport set. Invalid reports must not update
stored per-connection status or transport metrics. An accepted state change
(never a duplicate) fans out to the sender's room as `PeerTransportStatus` —
per-recipient v3-gated like every v3-only message, but deliberately NOT gated on
the recipient's transport capabilities (peer status is useful to any v3 client).

`ProvideConnectionInfo` / `GameStarting.peer_connections` is legacy,
self-declared metadata for the v2 handoff surface. It is preserved for backward
compatibility and must not be treated as proof of negotiated v3 `direct` or
`webrtc` transport capability. The one execution-readiness use is
`host + direct`: after the normal v3 capability intersection, host election
requires a syntactically usable Direct endpoint and copies it into
`SessionPlan.direct_endpoint`. The original metadata remains visible in legacy
player snapshots (including snapshots sent to spectators), so it is not a
v3-only privacy boundary.

Accepted sockets set `TCP_NODELAY`: every accepted WebSocket socket must disable
Nagle so small bidirectional relay frames are not stalled ~40-90 ms by the
Nagle x delayed-ACK interaction. `TCP_NODELAY` is per-connection and not reliably
inherited from the listen socket, so it is set on each accepted stream, not in
`bind_tcp_listener`. Both serve paths funnel through one seam — the plain
`axum::serve` stack via `websocket::bind_serve_listener` (`tap_io`) and the TLS
stack via `websocket::ConfiguredAcceptor` — both calling
`configure_accepted_socket`, so tests and production share identical semantics.
This complements (does not conflict with) the bounded-send-buffer
control-priority contract in `bind_tcp_listener`. Known gap: the optional
`legacy-fullmesh` matchbox port exposes no nodelay knob.

The outbound batch timer must never delay latency-sensitive traffic. Batching is
opt-in (`websocket.enable_batching` defaults to `false`). When enabled, only
`DeliveryClass::Latest` may wait for a fuller batch (to coalesce same-key
values); control, `Reliable`, and `Volatile` are released as soon as they reach
the front in `OutboundReceiver::try_pop_batched`. The non-batched `recv()` path
preserves every queue semantic (priority lanes, generations, latest coalescing,
gap reports, sojourn eviction) — the batch timer adds only the idle wait. The
per-hop latency budget lives in `docs/architecture/scaling.md`.
