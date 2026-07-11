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
  supports it; `GameData` is fanned out through the server. A relay room sends
  every v3 member an explicit no-peer `relay`/`relay` `SessionPlan`; v2 members
  receive no plan and its authoritative game-data path stays v2-compatible.
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

The handoff happens at lobby finalization — when a member sends `StartGame` while
every current player is ready, and the server broadcasts the unchanged
`GameStarting`. The sequence per recipient is strictly `GameStarting`
**then** `SessionPlan`:

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
   to it — its own `peers` list, per-recipient `initiate` flags, and ICE servers
   only when the selected transport is WebRTC. A relay-floor plan has no host,
   peers, or ICE and explicitly resets stale P2P state.
5. **Record the decision.** A non-relay decision is stored as the room's _active
   session plan_ (topology, transport, host) — the single source of truth for
   the session the room is running. Late joins, reconnects, and departures
   consult this stored decision; the ladder is never re-run mid-session.

```text
all ready
   |
   v
finalize ──> broadcast GameStarting (v2, unchanged)
   |
   v
choose_session_plan(members, config)         # ladder walk; all-members-v3 gate
   |
   +-- for each v3 member:
         build per-recipient peers + initiate flags
         if transport == webrtc:
             build per-recipient ICE servers (STUN + minted TURN)
         else if relay floor:
             use empty host/peers/ICE as an authoritative reset
         send SessionPlan  (after GameStarting, ordering preserved)
```

The all-members-v3 gate is the back-compat invariant for P2P upgrades: a single
v2 or relay-only member forces the room plan to the relay floor. The v3 members
receive the explicit relay reset; every v3-only message still has a
negotiated-v3 recipient gate, so it can never reach a v2 client. `Signal` itself
is same-room plus WebRTC-transport gated.

## Per-recipient peer lists

The same room produces a different plan for each recipient:

- **Mesh.** Each recipient's `peers` is every _other_ member. `initiate` follows
  the glare rule — the lesser UUID offers — so exactly one side of each pair sends
  the offer.
- **Host.** The host's `peers` is every client (each `initiate = false` — the host
  answers all). Each client's `peers` is just the host (`initiate = true` — clients
  offer to the host). Clients never appear in each other's peer lists.
- **Relay.** Every v3 recipient gets the same empty-peer `relay`/`relay` reset.

## Late join and reconnect

A peer joining or reconnecting _after_ finalization is brought into the session
the room is **actually running**. A non-relay active plan is rehydrated over the
current members, never selected again from the ladder; absence of a sticky plan
derives the explicit relay floor. Every current v3 member receives a complete,
tailored `SessionPlan`: current peers, glare-correct `initiate` flags, the stored
host, and freshly minted ICE for WebRTC. A v3 member that cannot run the session
still receives an empty-peer plan and participates through relay. Protocol-v2
members receive only their frozen lifecycle traffic.

The full refresh replaces the old additive `NewPeer` membership delta. It gives
incumbents and the joining actor one authoritative peer set and removes stale
links in the same transition. See the
[late-join decision table](../protocol.md#late-join-decision-table).

ICE can also arrive **before** any plan: an eligible v3 client — one that
negotiated the WebRTC transport and the game's desired topology — joining (or
reconnecting into) a non-finalized room of a non-relay-desired game receives the
same composed ICE list on its `RoomJoined` / `Reconnected` payload (the
[ICE pre-gather](../protocol.md#ice-pre-gather)), so candidate gathering can
start during the lobby wait. A client that negotiated only a rung below the
desired one forfeits the head start — the finalize-time `SessionPlan` still
delivers its ICE if the whole room settles there. The `SessionPlan` ICE always
supersedes it; joins and reconnects into a `Finalized` room never pre-gather —
their fresh ICE is the late-join `SessionPlan`'s job, exactly as described
above.

## Mid-session re-planning (host failover and self-heal)

Topology and transport are **sticky for the session lifetime**: the ladder runs
once at finalization and is never re-run mid-session, even though the capability
intersection can only widen as members depart — a mid-game data-path migration
would disrupt gameplay for zero correctness gain. Only an invalidated plan
_parameter_ triggers a re-emission: a `host`-topology session whose **stored
host can no longer anchor the session** — no longer a member, or (after a
reconnect that downgraded its negotiated capabilities) seated but no longer
capable of the session's topology/transport. Both membership-touching events
check for it:

- **A departure leaves the stored host invalid** (the usual case: the host
  itself departed; also any later departure after a wedged entry — e.g. a
  re-plan skipped by a transient storage error, or a host whose reconnect
  downgraded its capabilities). The server re-elects a host
  over the remaining members and sends every remaining v3 member a fresh
  per-recipient `SessionPlan` — same topology and transport, new `host`, fresh
  per-recipient ICE for WebRTC. The departure itself is still signaled by
  `PlayerLeft`; v3 adds the terminal delivery watermark while the v2 projection
  stays frozen. An ex-host that later reconnects is paired as a
  _client_ of the re-elected host.
- **A late join / reconnect finds the stored host invalid.** The same
  re-election + full re-plan runs first (one re-plan event), delivering every
  current **v3** member — the joiner included, even one that cannot run the
  session itself (the heal is about the room; such a joiner's plan carries
  empty `peers`) — a fresh plan. A normal late join with the host present and
  capable refreshes membership-derived plan fields without counting a host
  re-plan.

Re-election is **capability-aware**: only members that negotiated v3 plus the
stored sticky (topology, transport) pair are electable — a seat-filling v2 or
v3-relay-only member (which can even hold authority) is never named host of a
session it cannot run. The authority preference passes the same filter; among
qualifying members the rule is unchanged (authority preferred, else earliest
joiner, smaller-UUID tie-break). If **no** member qualifies, the stored plan is
dropped and nothing is emitted — the session is over and the relay floor
carries the room.

Re-issued and late-join plan **peer lists are filtered by the same predicate**
as election: `peers[]` names only members that negotiated v3 plus the session's
topology and transport, so a member that did not (e.g. a v3-relay-only
seat-filler, or one lacking the session's topology) receives its plan with an
empty `peers` list — it participates via the relay floor, with `host` kept as
elected, informational — and never appears in other members' lists. At
finalization the filter is vacuous for non-relay plans, because an upgrade is
selected only when every member supports it.

- **Any other departure** (a mesh member, a host-topology client while the host
  remains) re-emits nothing: `PlayerLeft` already tells peers to prune the
  departed member, and no plan parameter changed.
- **The last member departs** (or the room is cleaned up): the stored plan is
  dropped.

The client contract stays uniform: **the latest `SessionPlan` wins** — on
receipt, (re)configure the session and connect per `peers[].initiate`.

## Fallback to the floor

Every non-relay plan carries `fallback: "relay"`. P2P status never disables the
relay floor: the server keeps accepting `GameData` regardless of any peer's
reported state (`TransportStatus { connected: false }`). A client that cannot
establish -- or loses -- its P2P path can fall back without renegotiation. The
physical WebSocket can still close loudly when its delivery contract fails. The
full client-side state machine, including timeouts and the
`TransportStatus` signaling, lives in the
[Transport Fallback Contract](transport-fallback.md).

## See also

- [Transport Fallback Contract](transport-fallback.md) — client-side state machine
  and the relay-floor guarantee.
- [Protocol v3 additions](../protocol.md#protocol-v3-additions) — the wire messages
  (`Signal`, `SessionPlan`, `TransportStatus`, `PeerTransportStatus`),
  the selection ladder, the glare rule, and ICE/TURN.
- [TURN and STUN configuration](../configuration.md#turn-and-stun-ice-credentials-protocol-v3)
  — enabling ephemeral TURN credentials.
- [Platform Integration Guide](../guides/platform-integration.md) — which WebRTC stack
  to use per platform and the cross-stack interop traps.
- [ADR-0001: Protocol v3 two-axis design](../adr/0001-protocol-v3-two-axis.md).
