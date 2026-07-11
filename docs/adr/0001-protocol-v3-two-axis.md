# Protocol v3: Two-Axis (Topology + Transport) Capability-Gated Signaling

## Status

ADR-0001 - Accepted

## Context

The v2 protocol is a pure WebSocket **relay**: `GameData` flows
client -> server -> clients via server-side fan-out. The `relay_type` and
`ConnectionInfo::WebRTC` strings present today are cosmetic labels; the server
never negotiates a real peer-to-peer transport and never stops relaying. A
handoff seam exists (the lobby finalization barrier emits `GameStarting` with
each peer's self-declared `ConnectionInfo` and an authority flag), but it only
advertises LAN/routable direct addresses and never brokers WebRTC.

We want to evolve the server into a capability-negotiated **signaling + relay**
server that can support true peer-to-peer (WebRTC) connections across browser,
native (Linux/Windows/macOS), mobile, and Steam clients, while keeping every
existing v2 client working byte-for-byte unchanged. We must lock the core design
principles before writing any v3 code so they can be treated as invariants.

## Decision

We adopt **protocol v3** as an additive, capability-gated evolution of v2 built
around two independent negotiated axes plus a universal relay floor.

### 1. Two negotiated axes plus a relay floor

```text
Axis 1 - data path:  relay (server fan-out)  |  signal (peers carry data)
Axis 2 - topology:   relay-hub | host (star) | mesh (everyone-to-everyone)

  upgrade  mesh + webrtc   (two browsers / natives)   <- opt-in, negotiated
           host + webrtc   (star, NAT-friendly)
           host + direct   (LAN / routable host)
  floor    relay (WebSocket fan-out)  <- v2, ALWAYS on  <- universal, mandatory
```

- **Topology set:** `{ Relay, Host, Mesh }`.
- **Transport set:** `{ Relay, Direct, WebRtc }`.

The server picks one plan per room from the intersection of what all members
advertise, hands it out at the finalization handoff, brokers the WebRTC
handshake (targeted offer/answer/ICE relay), and always keeps the relay live as
the fallback tier (P2P primary -> TURN relay -> WebSocket relay last resort).

### 2. Additive, capability-gated versioning (v2 frozen)

New message variants are added to the existing `ClientMessage` / `ServerMessage`
enums; new fields are added with `#[serde(skip_serializing_if = "Option::is_none")]`
and `#[serde(default)]` so existing v2 wire bytes are unchanged.

- The server **must not** emit a v3-only message (`Signal`, `NewPeer`,
  `SessionPlan`, `PeerTransportStatus`) to a connection that did not negotiate
  v3.
- Negotiation reuses the existing `game_data_format` mechanism: `Authenticate`
  gains optional `protocol_version`, `supported_transports`, and
  `supported_topologies`; the server computes
  `negotiated_version = clamp(client_max, min_protocol_version, max_protocol_version)`
  (defaults `min = 2`, `max = 3`; a client that omits its version is treated as
  `min_protocol_version`) and stores caps per connection.
- A non-relay v3 plan is only **chosen** for a room when **all** members are
  v3-capable for the required transport; otherwise the room is assigned
  `relay`. Each v3 member receives an explicit no-peer relay plan, while v2
  members retain their byte-identical plan-free behavior.

The v2 wire contract is **frozen**. Golden JSON and MessagePack snapshots
(`tests/v2_wire_golden.rs`) lock the current bytes; any diff is a breaking change
and must fail CI.

### 3. Relay is the floor (mandatory, always on)

Every client is relay-capable by definition. P2P is an opt-in upgrade that
gracefully degrades back to relay on failure, timeout, or capability mismatch.
`fallback` in the session plan is always `Transport::Relay`. The server relays
`GameData` unconditionally regardless of P2P state -- the floor never closes.

### 4. Opaque signals (the server never parses SDP or ICE)

The `signal` field carried by `ClientMessage::Signal` / `ServerMessage::Signal`
is opaque to the server (`serde_json::Value`). The server routes purely by
`to` / `from` `PlayerId` and **never parses SDP or ICE**, mirroring the matchbox
pattern: the server forwards, clients interpret. This keeps the signaling server
zero-dependency for the WebRTC media plane and payload-agnostic. Same-room
enforcement is applied on every relay hop.

### 5. Deterministic glare avoidance (offerer designation)

For each unordered peer pair, exactly one side is the offerer, decided by a
stateless rule -- no perfect-negotiation dance:

- **Mesh:** for a recipient `R` and each other peer `P`, `initiate = (R.id < P.id)`
  by lexicographic `PlayerId` UUID comparison. Exactly one of each pair offers.
- **Host (star):** every non-host client offers to the `host`; the host offers to
  none and answers all. Clients never signal each other.
- **Late join/reconnect:** every current v3 member receives a complete,
  per-recipient `SessionPlan` using the topology's offerer rule above -- the UUID
  compare in mesh (either side may offer), the fixed client-offers-to-host
  direction in star. The latest plan replaces prior peer state; `NewPeer`
  remains a compatibility wire shape, not the current membership-delta contract.

### 6. Signaling integrity

Signaling MUST run over `wss://` in production: an on-path attacker who can
rewrite DTLS fingerprints in the SDP defeats WebRTC's own encryption. The server
authenticates before any room op, enforces same-room signaling, rate-limits
signals per connection, and only ever mints/forwards ICE credentials -- it never
relays media itself.

## Consequences

### Positive

- **Byte-identical back-compat:** a v2 client never sends or receives a v3
  message; existing clients keep working with zero changes, enforced by golden
  snapshots in CI.
- **Single codebase:** no fork; v3 is additive on the existing enums.
- **Zero media dependency:** opaque signals keep the server out of the SDP/ICE
  parsing business and out of the media path.
- **Stateless, race-free offerer designation:** the glare rule needs no server
  arbitration and is trivially testable.
- **Graceful degradation:** every upgrade falls back to the relay floor.

### Negative

- **Two-axis state per connection/room** adds negotiation and selection
  complexity over the pure-relay model.
- **Opaque signals** mean the server cannot validate or normalize SDP/ICE; a
  misbehaving client can relay garbage to a peer (bounded by rate-limit, size
  cap, and same-room enforcement).
- **Frozen v2 wire** constrains future field renames; all evolution must be
  additive.

### Mitigations

- Golden v2 wire snapshots (JSON + MessagePack) fail CI on any drift.
- All v3 emission goes through a single `client_supports_v3` gate.
- Signal payload size cap, per-connection rate-limit, and same-room checks bound
  the opaque-signal attack surface.

## Alternatives Considered

### 1. Fork the codebase for v3

**Rejected:** doubles maintenance, drifts the two protocols apart, and breaks the
shared in-band negotiation. Additive capability gating keeps one codebase.

### 2. Server parses and validates SDP/ICE

**Rejected:** couples the server to WebRTC internals, adds a heavy dependency and
attack surface, and provides little value -- clients must interpret the payloads
regardless. Opaque routing mirrors the proven matchbox model.

### 3. Perfect negotiation (both sides may offer, rollback on glare)

**Rejected:** more client complexity and edge cases than a stateless
lexicographic-UUID / client-offers-to-host rule that designates exactly one
offerer per pair.

### 4. Drop the relay floor once P2P is established

**Rejected:** ~15-20% of P2P sessions need relay (symmetric NAT, CGNAT, strict
firewalls). Keeping relay as a mandatory floor guarantees connectivity and a
clean fallback.

## References

- Internal protocol v3 implementation plan - Sections 0, 1, 2; Appendices A, B, D, E
- [Matchbox Compatibility (ADR-0002)](0002-matchbox-compatibility.md)
- [Reconnection Protocol (ADR-001)](reconnection-protocol.md)
- Golden v2 wire freeze: `tests/v2_wire_golden.rs`
- [Matchbox](https://github.com/johanhelsing/matchbox) - payload-agnostic signaling reference
