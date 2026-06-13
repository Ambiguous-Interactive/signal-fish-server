# Transport Fallback Contract (Protocol v3)

How Signal Fish Server upgrades a room from the universal relay floor to a
peer-to-peer data path, and how clients fall back when that path fails. This is
the client-side contract for the v3 session flow (PLAN Appendix G), plus the
server-side guarantees and the observability it exposes.

## The relay floor always stays open

The server's WebSocket relay is the **universal floor**: every client supports it,
and the server relays `GameData` through it **unconditionally**, regardless of any
peer-to-peer state. P2P (WebRTC or direct) is an opt-in _upgrade_ on top of the
floor — never a replacement the server enforces.

Concretely, the server keeps relaying `GameData` even after a client reports its
P2P path failed (`TransportStatus { connected: false }`). The floor never closes,
so a client that cannot establish (or loses) a P2P connection always has a working
transport to fall back to. This is the central invariant the rest of the contract
depends on.

## Client transport / fallback state machine

A v3 client drives its data-path transport from the per-recipient `SessionPlan` it
receives at lobby finalization (alongside the unchanged `GameStarting`). A room
that resolves to the relay floor emits **no** `SessionPlan`, so a relay-only client
simply keeps using the WebSocket relay exactly as in v2.

A `SessionPlan` is not necessarily a once-per-session event: the server re-issues
it mid-session on **host failover** (the host of a `host`-topology session is
gone — departed, detected missing on a later membership event, or seated but no
longer capable of the session after a capability-downgrading reconnect; every
remaining **v3** member gets a fresh plan naming the re-elected host — delivery
is v3-gated, like every plan emission) and to a **late joiner / reconnector**
entering an active non-relay session (the joiner alone gets its
tailored view with fresh ICE; existing members get a `NewPeer` delta instead —
unless the join itself healed an invalid host, in which case the failover re-plan
to every v3 member replaces both). Re-issued and late-join plan peer lists name
only peers that can run the session — that negotiated the session's topology and
transport: a member that did not (for example a v3 relay-only client that
seat-filled a `mesh + webrtc` room) receives its plan with an **empty** `peers`
list — truthful, it has no P2P peers; the relay floor is its data path — and
never appears in other members' `peers`, nor in any `NewPeer` pairing (the
`NewPeer` gating applies this same predicate). (At
finalization this filter is vacuous: a plan is only selected when every member
supports it.) Topology and transport never change across re-issues —
they are sticky for the session lifetime — so the client rule is simple: **the
latest `SessionPlan` wins**. Re-run the `on SessionPlan` logic below against the
new `peers` / `host` / `ice_servers`, tearing down peer connections that are no
longer listed (e.g. the departed host) and connecting per the new `initiate`
flags. On `NewPeer`, additively connect to that one peer.

```text
on SessionPlan(plan):
    if plan.transport == relay:
        use GameData over the WebSocket relay            # the floor
    else if plan.transport == direct:
        start direct host/client P2P using plan.host + plan.peers
        if direct path established within the timeout:
            (optionally) stop sending GameData over the relay
            emit ClientMessage::TransportStatus { transport, connected: true }
        else (failure or timeout):
            resume GameData over the WebSocket relay
            emit ClientMessage::TransportStatus { transport, connected: false }
    else if plan.transport == webrtc:
        start WebRTC P2P using plan + plan.ice_servers
        for each peer where initiate == true: send Offer
        for each peer where initiate == false: await Offer, then send Answer
        relay all Offer / Answer / IceCandidate via ClientMessage::Signal { to, signal }
        if WebRTC path established within the timeout:
            (optionally) stop sending GameData over the relay
            emit ClientMessage::TransportStatus { transport, connected: true }
        else (failure or timeout):
            resume GameData over the WebSocket relay
            emit ClientMessage::TransportStatus { transport, connected: false }

server: always relays GameData regardless of P2P state (the floor never closes)
```

Key points:

- **`initiate` resolves glare.** Exactly one side of each pair offers: in `mesh`
  the lesser UUID initiates; in `host` each client initiates to the host and the
  host answers. The plan's `peers[].initiate` flag is already tailored per
  recipient — the client never computes it.
- **`Signal` is opaque.** Offer / Answer / IceCandidate payloads are forwarded
  verbatim by the server (matchbox-compatible by convention) and are never
  inspected.
- **Stopping relay `GameData` is optional.** A client may keep dual-sending during
  the P2P warm-up and cut over only once the data channel is confirmed; that is a
  client-side latency/robustness choice, not a protocol requirement.
- **Fallback is always available.** On any P2P failure or timeout the client
  resumes `GameData` over the relay — which never stopped accepting it.

## Server's unconditional relay guarantee

The server's responsibilities are deliberately narrow:

- It relays `GameData` to room peers at all times, independent of any reported or
  inferred P2P state.
- For a WebRTC plan it relays opaque `Signal` messages between same-room peers
  (subject to the same-room, negotiated-transport, and rate-limit checks).
- It records each client's last-reported `TransportStatus`, updates metrics, and
  fans the accepted state change out to the reporter's room as
  `PeerTransportStatus` (v3 recipients only — see below).

It never tears down the relay path for a peer, and it never requires a peer to be
P2P-connected. `TransportStatus` is **purely informational**: it drives metrics
and the `PeerTransportStatus` peer notification (and, in the future, targeted
relay for stuck peers), but reporting `connected: false` does not change how the
server relays for that client.

## Data-channel configuration recommendation

For game traffic, a client should open **two** WebRTC data channels:

- One **reliable + ordered** channel for commands, chat, and critical events.
- One **unreliable + unordered** channel — `{ ordered: false, maxRetransmits: 0 }`
  — for movement and frequently-overwritten state, where the latest value matters
  more than guaranteed delivery.

This split works browser-to-native and native-to-native. The relay floor carries
all `GameData` reliably, so the unreliable channel is purely a P2P optimization.

## `TransportStatus` message (v3 only)

```json
{ "type": "TransportStatus", "data": { "transport": "webrtc", "connected": true } }
```

`transport` is one of `relay`, `direct`, or `webrtc`; `connected` is a boolean. The
message is **v3 only** — the server ignores it from any connection that did not
negotiate v3 (a v2 client can never legitimately send it). The reported transport
must also be present in that connection's negotiated transport set; unnegotiated
transport reports are ignored and do not update stored state or metrics. It is
purely informational and never causes the relay floor to close.

Server-side interpretation (drives the metrics below): duplicate reports of the
same `(transport, connected)` state are ignored; they leave stored
per-connection state unchanged and do not move counters. Counters move on the
first report for a connection and on later real per-connection state transitions.

- `connected: true` with a P2P transport (`direct` or `webrtc`) — a peer-to-peer
  data path came up; counts as **P2P established** when it is a first report or a
  transition from a different state.
- `connected: false` (for any named transport) — the client dropped back to the
  relay floor; counts as **relay fallback** when it is a first report or a
  transition from a different state.
- `connected: true` with `transport: relay` — "I am still on the floor"; this is
  neither a P2P establishment nor a fallback, so it moves **no** counter (only the
  per-connection state is updated).

## `PeerTransportStatus` peer fan-out (v3 only)

When an accepted report records a **real state change** (the first report on a
connection, or a `(transport, connected)` transition — the same dedup gate the
metrics use), the server fans it out to the reporter's current room as

```json
{ "type": "PeerTransportStatus", "data": { "peer_id": "<player-uuid>", "transport": "webrtc", "connected": true } }
```

so peers learn, for example, that the host's WebRTC path died and relay-path
traffic from it should be expected. The reporter itself is excluded; a duplicate
report fans out nothing; a report from a room-less client is recorded but fans
out nothing. Delivery is per-recipient v3-gated (a v2 member never observes it,
Appendix K) but deliberately **not** gated on the recipient's own transport
capabilities — it is informational status about a _peer_, useful to any v3
client, not an instruction to use that transport. Like the report it relays, it
never changes how the server relays `GameData`. Delivery is best-effort (like
`Signal` / `NewPeer`): a backpressured peer may miss a notice and re-syncs on
the next state change.

## Metrics exposed (PLAN §P5)

The server exposes Prometheus counters for the v3 transport surface so dashboards
can see how often the relay floor is upgraded to a peer-to-peer path:

- `signal_fish_transport_session_plans_emitted_total` — non-relay `SessionPlan`s
  emitted (one per finalized non-relay room; re-plans and late-join plans are
  counted separately below).
- `signal_fish_transport_session_replans_emitted_total` — mid-session host
  re-plan events (departure failover or late-join self-heal; one per event, not
  per recipient).
- `signal_fish_transport_session_plans_late_join_total` — `SessionPlan`s
  delivered to late joiners / reconnectors of already-active sessions (one per
  joiner that received a plan).
- `signal_fish_transport_topology_mesh_selected_total`,
  `signal_fish_transport_topology_host_selected_total`,
  `signal_fish_transport_topology_relay_selected_total` — chosen topology per
  finalized room (including the relay floor).
- `signal_fish_transport_webrtc_selected_total`,
  `signal_fish_transport_direct_selected_total`,
  `signal_fish_transport_relay_selected_total` — chosen data-path transport per
  finalized room.
- `signal_fish_transport_p2p_established_total` — first reports or state
  transitions where clients reported P2P paths as established via `TransportStatus`.
- `signal_fish_transport_relay_fallback_total` — first reports or state transitions
  where clients reported falling back to the relay floor via `TransportStatus`.
- `signal_fish_transport_signals_relayed_total` — opaque WebRTC `Signal` messages
  accepted for best-effort dispatch to same-room WebRTC peers.
- `signal_fish_transport_turn_credentials_issued_total` — ephemeral TURN
  credentials minted into `SessionPlan`s and pre-gather `RoomJoined` /
  `Reconnected` ICE lists.
- `signal_fish_transport_status_fanout_total` — `PeerTransportStatus` fan-out
  events: accepted `TransportStatus` state changes from in-room clients fanned
  out to v3 room peers (one per event, not per recipient).
- `signal_fish_transport_ice_pregather_emitted_total` — `RoomJoined` /
  `Reconnected` payloads that carried a non-empty ICE pre-gather list (one per
  carrying payload).

## Related documents

- [Protocol v3 two-axis design (ADR-0001)](../adr/0001-protocol-v3-two-axis.md)
- [Protocol reference](../protocol.md)
- [Configuration reference](../configuration.md)
- [Handoff and topologies](handoff-and-topologies.md)
