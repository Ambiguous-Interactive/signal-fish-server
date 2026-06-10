# Scaling Architecture (Multi-Node Signaling)

How to scale Signal Fish Server horizontally. The short version: the server holds
only per-room, in-memory peer-routing state, so the natural scaling unit is the
**room** — keep all of a room's peers on one node (room affinity) and every
forwarding path stays in-process. No cross-node infrastructure is required until
a single room must span nodes, which is explicitly out of scope today.

## What state a node actually holds

A signaling node is deliberately light. Per process it keeps, all in memory:

- **Room state** — rooms, room codes, players, lobby/ready state, held in the
  in-memory database (`InMemoryDatabase`, hash maps under async locks).
- **Peer routing** — one registered message channel per connected client, used to
  deliver `ServerMessage`s to that client's WebSocket.

Everything the server forwards is routed from that state:

- **Relay-floor `GameData`** fans out to the sender's room via
  `broadcast_to_room_except` — an in-process loop over the room's registered
  channels.
- **WebRTC `Signal` relay** (v3) delivers each opaque offer/answer/candidate to
  one target peer after validating, against local room state, that sender and
  target share a room.
- **`SessionPlan` emission** (finalization, host failover, late join) is computed
  from the room's members and delivered to them.

There is no per-message persistence and no cross-room state. When the last room
on a node empties, the node holds nothing worth migrating.

## The room is the scaling unit

Because every forwarding decision is "look up the room, write to its members'
channels", the only constraint a deployment must preserve is:

> **All peers of a room are connected to the same node.**

Within that constraint, rooms are independent — two rooms never exchange
messages, and the v3 protocol enforces same-room signaling on every hop. So
horizontal scale is achieved by **room affinity**: partition rooms across nodes
and route each client to the node that owns its room.

Practically, route on a stable room key at the load-balancer / session-affinity
layer — for example a consistent hash of the room identity (`game_name` +
`room_code`) supplied at connection time, so adding or removing nodes remaps a
minimal share of rooms. The server also helps the room code itself carry the
routing hint: `server.room_code_prefix` prepends a configured, deployment-specific
prefix to every generated room code (`generate_region_room_code`), so a code like
`EU7K2X` (prefix `EU` plus a random tail) tells the edge which deployment hosts
the room before any lookup.

Two consequences fall out of room affinity:

- **Relay and signaling share the same constraint.** The relay floor
  (`GameData` fan-out) and WebRTC signal forwarding both route within one room,
  so a deployment that satisfies affinity for one satisfies it for both — no new
  topology is introduced by enabling v3 upgrades.
- **WebRTC offloads the heavy traffic anyway.** Once a room upgrades to a P2P
  transport, game data flows peer-to-peer (or via TURN) and the signaling node
  carries only the occasional re-plan or fallback traffic.

## Cross-node fan-out is out of scope (but the seams exist)

Redis pub/sub (or any message bus) between signaling nodes is only needed if a
single room may span nodes. Today it may not: the shipped implementations are
in-memory and single-process by design — the zero-dependency posture.

The code does keep the seams a multi-node implementation would plug into, so the
path is anticipated rather than precluded:

- `MessageCoordinator` (`src/coordination/mod.rs`) abstracts
  send-to-player / broadcast-to-room behind a trait; the in-memory implementation
  (`InMemoryMessageCoordinator` in `src/server.rs`) also implements
  `handle_bus_message`, the entry point a cross-instance bus would call with a
  `SequencedMessage`.
- `SequencedMessage` (`src/distributed.rs`) wraps a `ServerMessage` with a
  sequence id, originating instance id, and room/player targeting — the envelope
  for cross-node delivery and deduplication.
- `DistributedLock` (`src/distributed.rs`) abstracts cross-instance locking; the
  in-memory `InMemoryDistributedLock` is what runs today.

Until a concrete multi-node room requirement exists, prefer adding nodes with
room affinity over wiring a bus: it preserves in-process forwarding latency and
keeps the server dependency-free.

## Multi-region anticipation: `region_id`

Multi-region routing is anticipated by existing `region_id` plumbing, which is
internal-only today:

- `server.region_id` (config, default `"default"`) identifies the deployment
  region a node belongs to; the running server exposes it via
  `GameServer::region_id()`.
- Every `Room` records the `region_id` of the deployment hosting it, and each
  stored `PlayerInfo` records the region currently hosting that player. The
  player-side field is marked internal-only and is never serialized to the wire.
- `server.room_code_prefix` complements this by letting generated room codes
  carry a region/deployment discriminator, so a future cross-region directory
  can route a join by code alone.

None of this changes protocol behavior yet — it is the bookkeeping a
region-aware routing layer would build on.

## Capacity notes

- **Signaling is light.** A node brokers small JSON/binary messages; see the
  [resource requirements](../deployment.md#resource-requirements) for per-node
  sizing (hundreds of rooms / thousands of players per instance).
- **TURN bandwidth dominates cost.** Plan for 15–20% of P2P connections to be
  relayed through TURN; that relay bandwidth, not signaling CPU, is the dominant
  infrastructure cost of a WebRTC deployment. See the
  [TURN deployment guide](../deployment-turn.md) for cost figures and setup.
- **The relay floor is the elastic dimension.** Rooms that never upgrade to P2P
  keep all `GameData` on the signaling node, so the share of relay-floor rooms —
  not raw room count — drives per-node bandwidth.

## Related documents

- [Deployment guide](../deployment.md) — scaling considerations, resources
- [TURN deployment guide](../deployment-turn.md) — relay capacity and cost
- [Transport Fallback Contract](transport-fallback.md) — the relay floor
- [Handoff and topologies](handoff-and-topologies.md) — session plan emission
