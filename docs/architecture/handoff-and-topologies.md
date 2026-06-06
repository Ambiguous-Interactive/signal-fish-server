# Handoff and Topologies (Protocol v3)

How a Signal Fish room moves from the universal relay floor to a peer-to-peer
topology at lobby finalization, what the three topologies mean, and how every
plan falls back to the floor. This is the server-side counterpart to the
[Transport Fallback Contract](transport-fallback.md), which covers the
client-side state machine; this document focuses on the **finalization handoff
seam** and the **topology shapes**. For the wire-level message details see the
[Protocol v3 additions](../protocol.md#protocol-v3-additions) section.

## The three topologies

A negotiated v3 room resolves to exactly one topology for its lifetime:

- **Relay** — the v2 server-relay hub, and the universal floor. Every client
  supports it; `GameData` is fanned out through the server. A relay room emits no
  `SessionPlan` and behaves byte-identically to v2.
- **Host** — a star around one elected authoritative peer (the host). Each client
  connects only to the host; clients never connect to each other. Used with the
  `webrtc` or `direct` transport.
- **Mesh** — full mesh: every peer connects to every other peer. Used with the
  `webrtc` transport.

Topology is one axis; the data-path **transport** (`relay`, `direct`, `webrtc`)
is the other. Only four pairs are legal — the three upgrade rungs plus the floor:

```text
mesh + webrtc      ← richest
host + webrtc
host + direct
relay (floor)      ← always available
```

## The finalization handoff seam

The handoff happens at lobby finalization — the moment all players are ready and
the server broadcasts the unchanged `GameStarting`. The sequence per recipient is
strictly `GameStarting` **then** `SessionPlan`:

1. **Finalize.** The coordinator finalizes the room and broadcasts `GameStarting`
   to every member, exactly as in v2.
2. **Select a plan.** The server computes a single room-wide plan by walking the
   richest-first ladder, settling on the first rung that fits the per-game desired
   ceiling, has its transport enabled, and is supported by **every** member.
   Otherwise it settles on the relay floor. This is the single source of truth in
   `src/server/session_policy.rs` (`UPGRADE_LADDER` + `RELAY_FLOOR`).
3. **Elect a host** (host topology only). Prefer the room's designated authority
   when present; otherwise the earliest joiner, breaking ties by the smaller UUID
   for determinism.
4. **Emit per-recipient `SessionPlan`s.** Each v3 member receives a plan tailored
   to it — its own `peers` list, per-recipient `initiate` flags, and freshly
   minted ICE credentials. A relay-floor room emits **no** plan.

```text
all ready
   |
   v
finalize ──> broadcast GameStarting (v2, unchanged)
   |
   v
choose_session_plan(members, config)         # ladder walk; all-members-v3 gate
   |
   +-- relay floor? --> emit nothing (clients keep relaying GameData)
   |
   +-- non-relay plan:
         for each v3 member:
             build per-recipient peers + initiate flags
             build per-recipient ICE servers (STUN + minted TURN)
             send SessionPlan  (after GameStarting, ordering preserved)
```

The all-members-v3 gate is the back-compat invariant: a single v2 or relay-only
member forces the whole room to the relay floor, so a v3 control message can never
reach a v2 client. This holds for finalize, late join, reconnect, and signal.

## Per-recipient peer lists

The same room produces a different plan for each recipient:

- **Mesh.** Each recipient's `peers` is every _other_ member. `initiate` follows
  the glare rule — the lesser UUID offers — so exactly one side of each pair sends
  the offer.
- **Host.** The host's `peers` is every client (each `initiate = false` — the host
  answers all). Each client's `peers` is just the host (`initiate = true` — clients
  offer to the host). Clients never appear in each other's peer lists.
- **Relay.** No plan is emitted (an empty peer list defensively otherwise).

## Late join and reconnect

A peer joining or reconnecting _after_ finalization re-runs the same selection and
is paired via `NewPeer` (not a fresh `SessionPlan`), gated on the room's
`lobby_state` and the resolved topology. See the
[late-join decision table](../protocol.md#late-join-decision-table) for the exact
behavior. The key invariant: initial pairing is owned by the finalize-time
`SessionPlan`; `NewPeer` only covers post-finalization arrivals.

## Fallback to the floor

Every non-relay plan carries `fallback: "relay"`. The relay floor never closes:
the server keeps relaying `GameData` unconditionally, regardless of any peer's
reported P2P state (`TransportStatus { connected: false }`). A client that cannot
establish — or loses — its P2P path always has a working transport to fall back
to. The full client-side state machine, including timeouts and the
`TransportStatus` signaling, lives in the
[Transport Fallback Contract](transport-fallback.md).

## See also

- [Transport Fallback Contract](transport-fallback.md) — client-side state machine
  and the relay-floor guarantee.
- [Protocol v3 additions](../protocol.md#protocol-v3-additions) — the wire messages
  (`Signal`, `NewPeer`, `SessionPlan`, `TransportStatus`), the selection ladder,
  the glare rule, and ICE/TURN.
- [TURN / STUN configuration](../configuration.md#turn--stun-ice-credentials-protocol-v3)
  — enabling ephemeral TURN credentials.
- [ADR-0001: Protocol v3 two-axis design](../adr/0001-protocol-v3-two-axis.md).
