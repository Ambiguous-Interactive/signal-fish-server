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
   to it — its own `peers` list, per-recipient `initiate` flags, and ICE servers
   only when the selected transport is WebRTC. A relay-floor room emits **no**
   plan.
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
   +-- relay floor? --> emit nothing (clients keep relaying GameData)
   |
   +-- non-relay plan:
         for each v3 member:
             build per-recipient peers + initiate flags
             if transport == webrtc:
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

A peer joining or reconnecting _after_ finalization is brought into the session
the room is **actually running** — the stored active session plan, never a
recomputation over the current members (a departure can reopen a seat, so the
live membership can drift from the finalize-time membership; a recompute could
contradict the session every member already configured). With a stored non-relay
plan in a `Finalized` room:

- the **joiner** receives a fresh tailored `SessionPlan` — current peers,
  glare-correct `initiate` flags, the stored host, and freshly minted ICE for a
  WebRTC transport (a reconnector's original TURN credentials may have expired;
  a seat-filling joiner never had any). It is deliberately **not** sent
  `NewPeer`. A v3 joiner that cannot run the session — it did not negotiate the
  session's topology and transport (e.g. a relay-only seat-filler) — still
  receives the plan, but with an **empty** `peers` list — it has no P2P peers
  and participates via the relay floor;
- **existing members** receive the additive `NewPeer` delta, only when the
  stored transport is WebRTC and both ends of each announced pair can run the
  session (mesh: every session-capable member learns of the joiner; host: only
  along the star edge). A `host + direct` room has a non-relay topology but
  no WebRTC signaling transport, so it emits no `NewPeer`.

A room with no stored plan (it finalized to the relay floor, or pre-dates v3)
emits nothing at all on a late join. See the
[late-join decision table](../protocol.md#late-join-decision-table) for the
exact behavior. The key invariants: initial pairing is owned by the
finalize-time `SessionPlan`; `NewPeer` only covers post-finalization arrivals
and only ever targets existing members.

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
  per-recipient ICE for WebRTC. The departure itself is still signaled by the
  unchanged v2 `PlayerLeft`; an ex-host that later reconnects is paired as a
  _client_ of the re-elected host.
- **A late join / reconnect finds the stored host invalid.** The same
  re-election + full re-plan runs first (one re-plan event), delivering every
  current **v3** member — the joiner included, even one that cannot run the
  session itself (the heal is about the room; such a joiner's plan carries
  empty `peers`) — a fresh plan, in place of the normal joiner-plan + `NewPeer`
  emission (which would duplicate it). A normal late join with the host
  present and capable never re-plans.

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
elected, informational — and never appears in other members' lists (the
`NewPeer` gating applies this same predicate to both ends of every announced
pair). At finalization the filter is vacuous, because a plan is only selected
when every member supports it.

- **Any other departure** (a mesh member, a host-topology client while the host
  remains) re-emits nothing: `PlayerLeft` already tells peers to prune the
  departed member, and no plan parameter changed.
- **The last member departs** (or the room is cleaned up): the stored plan is
  dropped.

The client contract stays uniform: **the latest `SessionPlan` wins** — on
receipt, (re)configure the session and connect per `peers[].initiate`.

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
  (`Signal`, `NewPeer`, `SessionPlan`, `TransportStatus`, `PeerTransportStatus`),
  the selection ladder, the glare rule, and ICE/TURN.
- [TURN / STUN configuration](../configuration.md#turn--stun-ice-credentials-protocol-v3)
  — enabling ephemeral TURN credentials.
- [Platform Integration Guide](../guides/platform-integration.md) — which WebRTC stack
  to use per platform and the cross-stack interop traps.
- [ADR-0001: Protocol v3 two-axis design](../adr/0001-protocol-v3-two-axis.md).
