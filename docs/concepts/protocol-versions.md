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
it. The v2 shapes and default reliable behavior remain byte-for-byte; v3 adds optional `Authenticate` fields,
v3-only messages (`SessionPlan`, `Signal`, `NewPeer`, `TransportStatus`, `PeerTransportStatus`, `DeliveryReport`,
`GoingAway`, plus opt-in `RelayStats`), classified JSON delivery, and optional v3 fields such as ICE lists and
relay sequence metadata. The relay floor is always
present underneath, and any peer that cannot establish (or loses) its P2P path falls back to it. A v2 client on a
v3 server observes pure v2 behavior.

## Comparison

| Dimension | v2 | v3 |
| --- | --- | --- |
| Endpoint | `/v2/ws` (default protocol version 2) | `/v3/ws` (default protocol version 3); same handler as `/v2/ws` |
| `Authenticate` fields | `app_id` (+ optional `sdk_version`, `platform`, `game_data_format`) | v2 fields **plus** optional `protocol_version`, `supported_transports`, `supported_topologies` |
| Message set | v2 messages (incl. `StartGame` to finalize the lobby) | v2 messages **plus** `SessionPlan`, `Signal`, `NewPeer`, `TransportStatus`, `PeerTransportStatus`, `DeliveryReport`, `GoingAway`, and opt-in `RelayStats` |
| Topologies | `relay` only | `relay`, `host`, `mesh` (room-wide, chosen at finalization) |
| Transports | `relay` only | `relay`, `direct`, `webrtc` |
| Relay delivery | Reliable FIFO; raw binary is reliable | Default/reliable unchanged; JSON also supports keyed `latest` and `volatile`; raw binary remains reliable |
| Gap accountability | No sequence metadata | The union of prior exact `DeliveryReport` ranges authorizes continuing-connection gaps; aggregate `RelayStats` never does |
| ICE / TURN | none | STUN always in a WebRTC plan; ephemeral per-player TURN when `turn.enabled`; optional ICE pre-gather on `RoomJoined` / `Reconnected` |
| Capability handshake | none | `protocol_version` / transports / topologies negotiated, echoed in extended `ProtocolInfo` |
| Back-compat | baseline | purely additive; a v2 client never sends or receives a v3 message |

The negotiated version is capped at the server's configured ceiling:
`negotiated = min(client_max, protocol.max_protocol_version)`. A client that advertises a higher version than the
deployment speaks is clamped **down** to `protocol.max_protocol_version`; the server never raises a client above
its declared maximum. A result below `protocol.min_protocol_version` is rejected with
`UNSUPPORTED_PROTOCOL_VERSION`. An omitted `protocol_version` uses the endpoint default. Defaults:
`protocol.min_protocol_version` is `2`, `protocol.max_protocol_version` is `3`.

## The two invariants

These two rules are what make v2 and v3 clients interoperate, always.

**Relay-floor guarantee.** The server's WebSocket relay is the universal floor. It relays `GameData`
_unconditionally_, regardless of any peer's reported P2P state — even after a client reports
`TransportStatus { connected: false }`. P2P is an opt-in upgrade _on top of_ the floor, never a replacement the
server enforces. Every non-relay `SessionPlan` carries `fallback: "relay"`. A client that cannot establish (or
loses) its P2P path always has a working transport to fall back to.

**All-members-v3 required for any upgrade.** A non-relay plan requires _every_ member of the room to be
v3-capable _and_ to support the chosen topology and transport. A single v2 (or relay-only) member forces the
whole room to the relay floor. Each v3 member receives an explicit no-peer `relay`/`relay` `SessionPlan`; v2
members receive no v3 message. Every v3-only path has its own negotiated-v3 recipient gate. Mixed rooms keep the
authoritative `GameData` path on the v2-compatible relay floor. Delivery
class policy is one such per-recipient gate, not a room upgrade: a classified send may be lossy with exact reports
for a v3 recipient while remaining reliable FIFO with v3 fields stripped for a v2 recipient.

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

A v2 client already works against a v3 server with no changes (it stays on the
reliable relay floor). To use v3 peer-to-peer or classified-delivery features,
take these steps.

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

### 2. Handle the v3 server messages

These messages exist only on negotiated v3 connections. `SessionPlan` arrives
for every finalized session, including an explicit relay-floor reset. `Signal`
is WebRTC-transport-gated between same-room v3 peers; status, reliability, and
shutdown messages are independently gated by their features:

- `SessionPlan` (server → client) — your per-recipient session directive: `topology`, `transport`, the `peers`
  to connect to (each with an `initiate` flag), `ice_servers`, optional `host`, and `fallback: "relay"`.
- `Signal` (client ⇄ server) — opaque WebRTC signaling (`Offer` / `Answer` / `IceCandidate`) relayed verbatim to
  or from a named peer. Required only for the `webrtc` transport.
- `NewPeer` (server → client) — compatibility shape for an additive WebRTC peer
  directive. Current finalized membership changes use complete `SessionPlan`
  refreshes instead.
- `TransportStatus` (client → server) — your report of your current data-path state (`transport`, `connected`).
  Informational; drives metrics. Optional.
- `PeerTransportStatus` (server → client) — a same-room peer's transport state changed. Informational. Optional
  to act on.
- `RelayStats` (server → client) — optional delivery counters when
  `websocket.delivery_stats_interval_secs` is nonzero.
- `DeliveryReport` (server → client) — cumulative per-class outcomes and exact
  `(from_player, epoch, from_seq..=to_seq, reason)` ranges omitted for this
  connection. Gap-bearing reports are event-driven even when periodic stats are
  disabled.
- `GoingAway` (server → client) — shutdown-drain advisory before close code `4000 server_shutdown`.

### 3. Know what is optional vs required

| Step | Required? |
| --- | --- |
| Add `Authenticate` capability fields | required to opt into v3 |
| Handle `SessionPlan` | required; it also carries the authoritative relay-floor reset |
| Send / handle `Signal` | required **only** for a `webrtc` plan |
| Handle `NewPeer` | optional compatibility handling; current membership refreshes use `SessionPlan` |
| Send `TransportStatus` | optional (metrics only) |
| Handle `PeerTransportStatus` | optional |
| Handle `RelayStats` | optional diagnostics |
| Handle `DeliveryReport` | required for a conformant v3 receive loop; peers may send `latest` / `volatile` |
| Handle `GoingAway` | optional but recommended for clean shutdown UX |
| Pre-gather ICE from `RoomJoined` / `Reconnected` | optional latency optimization |

If you implement neither P2P nor classified delivery, omit the new fields and
retain v2-compatible reliable `GameData` behavior.

### 4. Apply classified delivery and sequence accountability

Only negotiated-v3 JSON `GameData` may carry delivery metadata. Omission or
`class: "reliable"` forbids `key`; `class: "latest"` requires a `u32` key; and
`class: "volatile"` forbids `key`. A well-typed illegal pairing is rejected
with `INVALID_DELIVERY_CLASS`; unknown tokens, wrong types, out-of-range keys,
and explicit `null` are malformed `INVALID_INPUT`. Raw binary frames remain
reliable.

Within a sender epoch, delivered `seq` values increase but can skip. A skip on a
continuing connection is valid only when the union of exact, causally prior
`DeliveryReport` ranges covers every missing sequence for the same sender and
epoch. Reports carry at most 256 ranges and roll over when necessary.
`RelayStats`, aggregate counter deltas, and the supplemental
`UNSUPPORTED_GAME_DATA_FORMAT` error cannot authorize a gap. After
`RoomJoined` or `SpectatorJoined`, paired `PlayerInfo.epoch` and `PlayerInfo.seq`
give the exact recipient-visible baseline; the next delivery or exact gap begins
at `seq + 1`.

Peer lifecycle control can overtake queued old-epoch data. Continue validating
that trailing data for accountability, but suppress it from application state
after `PlayerLeft` or a newer incarnation announcement. A future epoch must be
announced exactly by `PlayerJoined` or `PlayerReconnected`; after data advances,
older epochs are invalid. The recipient's own room/spectator transition is a
generation barrier and clears room cursors without resetting physical-
connection counters. After reconnect, discard old expectations and apply
`Reconnected.sender_watermarks`; a new physical connection resets report
counters. Always resynchronize application state after recipient absence.

### 5. Follow the client contract

Two rules keep the client simple across finalization, late joins, reconnects, and host failover:

- **The latest `SessionPlan` wins.** On every `SessionPlan`, (re)configure the session and connect per
  `peers[].initiate`, tearing down peer connections no longer listed (for example a departed host). A plan can be
  re-issued mid-session — its topology and transport never change, only membership-derived fields (`peers`,
  `host`, `ice_servers`).
- **Relay is explicit on v3.** A `relay`/`relay` plan with an empty peer list
  tears down stale P2P links and keeps `GameData` on the WebSocket relay. V2
  clients receive no plan and continue their frozen relay flow.

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
