# Protocol v2 vs v3

Signal Fish Server speaks two wire protocols on the same WebSocket handler. This page is the one-page mental
model: what each version is, exactly how they differ, the guarantees that let them interoperate, and a checklist
for migrating a v2 client to v3.

For the full message catalog see the [Protocol Reference](../protocol.md). For the server-side and client-side
design see [Handoff and Topologies](../architecture/handoff-and-topologies.md) and the
[Transport Fallback Contract](../architecture/transport-fallback.md).

## Mental model

**Protocol v2 is relay-only signaling.** The client authenticates, joins a room, and exchanges `GameData`
through the server's WebSocket relay. The server is the hub: every `GameData` frame is fanned out to the other
room members. There is no peer-to-peer data path — the relay carries everything. This is the universal floor and
it always works.

**Protocol v3 is additive capability negotiation on top of that floor.** A v3-capable client advertises which
transports and topologies it supports, and the server can _upgrade_ a room from relay to a peer-to-peer plan —
`host` or `mesh` topology over the `direct` or `webrtc` transport — when (and only when) every member supports
it. Everything v2 does still happens byte-for-byte; v3 only adds optional `Authenticate` fields, five new
message types, and an optional ICE list. The relay floor is always present underneath, and any peer that cannot
establish (or loses) its P2P path falls back to it. A v2 client on a v3 server observes pure v2 behavior.

## Comparison

| Dimension | v2 | v3 |
| --- | --- | --- |
| Endpoint | `/v2/ws` (default protocol version 2) | `/v3/ws` (default protocol version 3); same handler as `/v2/ws` |
| `Authenticate` fields | `app_id` (+ optional `sdk_version`, `platform`, `game_data_format`) | v2 fields **plus** optional `protocol_version`, `supported_transports`, `supported_topologies` |
| Message set | v2 messages (incl. `StartGame` to finalize the lobby) | v2 messages **plus** `SessionPlan`, `Signal`, `NewPeer`, `TransportStatus`, `PeerTransportStatus` |
| Topologies | `relay` only | `relay`, `host`, `mesh` (room-wide, chosen at finalization) |
| Transports | `relay` only | `relay`, `direct`, `webrtc` |
| ICE / TURN | none | STUN always in a WebRTC plan; ephemeral per-player TURN when `turn.enabled`; optional ICE pre-gather on `RoomJoined` / `Reconnected` |
| Capability handshake | none | `protocol_version` / transports / topologies negotiated, echoed in extended `ProtocolInfo` |
| Back-compat | baseline | purely additive; a v2 client never sends or receives a v3 message |

The negotiated version is clamped into the server's configured range:
`negotiated = clamp(client_max, protocol.min_protocol_version, protocol.max_protocol_version)`. A client that
advertises a higher version than the deployment speaks is clamped **down** to `protocol.max_protocol_version`; one
that omits `protocol_version` is negotiated from the endpoint default. Defaults: `protocol.min_protocol_version`
is `2`, `protocol.max_protocol_version` is `3`.

## The two invariants

These two rules are what make v2 and v3 clients interoperate, always.

**Relay-floor guarantee.** The server's WebSocket relay is the universal floor. It relays `GameData`
_unconditionally_, regardless of any peer's reported P2P state — even after a client reports
`TransportStatus { connected: false }`. P2P is an opt-in upgrade _on top of_ the floor, never a replacement the
server enforces. Every non-relay `SessionPlan` carries `fallback: "relay"`. A client that cannot establish (or
loses) its P2P path always has a working transport to fall back to.

**All-members-v3 required for any upgrade.** A non-relay plan requires _every_ member of the room to be
v3-capable _and_ to support the chosen topology and transport. A single v2 (or relay-only) member forces the
whole room to the relay floor, where **no** `SessionPlan` is emitted and no v3 message reaches any client. So a v3
control message can never be delivered to a v2 client, and a mixed room behaves exactly like v2.

The selection happens once, at lobby finalization, by walking a richest-first ladder and settling on the first
rung that fits the per-game desired ceiling, has its transport enabled in config, and is supported by every
member:

```text
mesh + webrtc      ← richest
host + webrtc
host + direct
relay (floor)      ← always available
```

The chosen topology and transport are **sticky for the session lifetime** — the ladder is never re-run
mid-session, even when departures widen the capability intersection.

## Migrating a v2 client to v3

A v2 client already works against a v3 server with no changes (it stays on the relay floor). To opt into
peer-to-peer upgrades, take these steps.

### 1. Add the capability fields to `Authenticate`

Connect to `/v3/ws` and add three optional fields to your first `Authenticate` message:

```json
{
  "type": "Authenticate",
  "data": {
    "app_id": "my-game",
    "protocol_version": 3,
    "supported_transports": ["relay", "direct", "webrtc"],
    "supported_topologies": ["relay", "host", "mesh"]
  }
}
```

- `protocol_version` — the highest version the client speaks. Omitting it uses the endpoint default (`/v3/ws`
  ⇒ 3).
- `supported_transports` — tokens from `relay`, `direct`, `webrtc`. **Absent means relay-only**, even on
  `/v3/ws`.
- `supported_topologies` — tokens from `relay`, `host`, `mesh`. **Absent means relay-only**, even on `/v3/ws`.

Advertise only what you can actually run. Always include `relay` so you remain a valid floor member. The
negotiated result comes back in the extended `ProtocolInfo` (`protocol_version`, `min_protocol_version`,
`max_protocol_version`, `transports`). For negotiated v3, `transports` currently advertises `["websocket"]`; it is
omitted with the other v3-only fields on negotiated v2 connections.

### 2. Handle the five new server messages

These arrive only on a negotiated v3 connection, and only when the room upgrades past the relay floor:

- `SessionPlan` (server → client) — your per-recipient session directive: `topology`, `transport`, the `peers`
  to connect to (each with an `initiate` flag), `ice_servers`, optional `host`, and `fallback: "relay"`.
- `Signal` (client ⇄ server) — opaque WebRTC signaling (`Offer` / `Answer` / `IceCandidate`) relayed verbatim to
  or from a named peer. Required only for the `webrtc` transport.
- `NewPeer` (server → client) — a late-join pairing delta telling existing members a peer is now available for a
  WebRTC connection; `you_initiate` designates the offerer.
- `TransportStatus` (client → server) — your report of your current data-path state (`transport`, `connected`).
  Informational; drives metrics. Optional.
- `PeerTransportStatus` (server → client) — a same-room peer's transport state changed. Informational. Optional
  to act on.

### 3. Know what is optional vs required

| Step | Required? |
| --- | --- |
| Add `Authenticate` capability fields | required to opt into v3 |
| Handle `SessionPlan` | required (else you never leave the relay floor usefully) |
| Send / handle `Signal` | required **only** for a `webrtc` plan |
| Handle `NewPeer` | required if you support late joins into a `webrtc` session |
| Send `TransportStatus` | optional (metrics only) |
| Handle `PeerTransportStatus` | optional |
| Pre-gather ICE from `RoomJoined` / `Reconnected` | optional latency optimization |

If you never implement P2P, you still gain nothing and lose nothing: the relay floor carries all `GameData`.

### 4. Follow the client contract

Two rules keep the client simple across finalization, late joins, reconnects, and host failover:

- **The latest `SessionPlan` wins.** On every `SessionPlan`, (re)configure the session and connect per
  `peers[].initiate`, tearing down peer connections no longer listed (for example a departed host). A plan can be
  re-issued mid-session — its topology and transport never change, only membership-derived fields (`peers`,
  `host`, `ice_servers`).
- **On `NewPeer`, additively connect** to that one peer using `you_initiate` to decide who offers. A relay-only
  room emits no `SessionPlan` at all, so a relay-only client just keeps using `GameData` over the WebSocket relay
  exactly as in v2.

For glare resolution, ICE/TURN credential composition, and the late-join decision table, see the
[Protocol v3 additions](../protocol.md#protocol-v3-additions).

## See also

- [Protocol Reference](../protocol.md) — every message, the selection ladder, and ICE pre-gather.
- [Handoff and Topologies](../architecture/handoff-and-topologies.md) — the finalization handoff seam and the
  three topology shapes.
- [Transport Fallback Contract](../architecture/transport-fallback.md) — the client-side state machine and the
  relay-floor guarantee.
- [Feature Availability Matrix](../reference/feature-matrix.md) — which config and client support each feature
  needs.
