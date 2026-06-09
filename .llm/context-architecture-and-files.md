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
[Protocol v3 Session-Plan Selection](skills/protocol-v3-session-plan.md).

Protocol v3 `TransportStatus` is informational, but still a negotiated-capability
boundary: accept reports only from v3 connections and only for transports present
in that connection's negotiated transport set. Invalid reports must not update
stored per-connection status or transport metrics.

`ProvideConnectionInfo` / `GameStarting.peer_connections` is legacy,
self-declared metadata for the v2 handoff surface. It is preserved for backward
compatibility and must not be treated as proof of negotiated v3 `direct` or
`webrtc` transport capability.
