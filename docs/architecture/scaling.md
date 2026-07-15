# Scaling Architecture

Signal Fish Server scales **up** within one process. The shipped server does not
provide transparent horizontal scaling: all room and reconnect state is
in-memory, and a WebSocket is assigned to a process before its `JoinRoom`
message supplies a room code. Read the
[single-instance deployment contract](single-instance-deployment.md) before
placing the server behind a load balancer.

## What state a process holds

A process keeps all authoritative state in memory:

- rooms, room codes, players, spectators, lobby and ready state;
- one registered delivery route per connected client;
- reconnection records, replay events, and token claims;
- relay `(epoch, seq)` counters and delivery-accountability state; and
- v3 session plans and transport status.

Every forwarding decision reads that local state. Relay-floor `GameData` fans
out over local channels; WebRTC `Signal` validates both peers against the local
room; session plans are computed from local membership. No shipped component
replicates those facts to another process.

## The room is the conceptual scaling unit

Rooms are independent within one process: two rooms never exchange messages,
and v3 enforces same-room signaling on every hop. That makes a room the natural
unit for a **future external routing layer**, but Signal Fish does not implement
that layer.

An application-owned directory may assign rooms to separate, isolated Signal
Fish deployments and give clients a deployment-specific WebSocket URL before
they connect. `server.room_code_prefix` can make generated codes carry a
deployment hint (for example `EU7K2X`), but the prefix is only a hint: the server
does not resolve it, redirect a WebSocket, or prevent another process from
creating the same code. Adding or removing homes while rooms are live requires
application-level drain and rebuild, not consistent-hash remapping.

Two consequences follow:

- Relay and signaling share the same boundary. A room's `GameData`, WebRTC
  signals, reconnects, and re-plans must all use its one home process.
- WebRTC can reduce server bandwidth after P2P establishment, but it does not
  make room control state portable. The WebSocket relay remains the fallback.

## Cross-process fan-out is not implemented

A message bus and shared database would be necessary but not sufficient for a
room to span processes. The code contains extension seams:

- `MessageCoordinator` abstracts send-to-player and broadcast-to-room;
- `SequencedMessage` is an envelope with origin and targeting metadata; and
- `DistributedLock` abstracts room-operation locking.

Their shipped implementations are in-memory. They do not provide a shared room
directory, consensus, sequence ownership, global deduplication, reconnect token
handoff, or cross-process delivery. Treat them as local coordination tools and
future seams, not as distributed behavior.

## Multi-region hints

`server.region_id` and `server.room_code_prefix` anticipate an external routing
layer:

- every room/player record carries the local region internally; and
- generated room codes may carry a deployment-specific prefix.

Neither changes protocol behavior. A prefix or region ID does not make a
reconnect token routable and does not establish room affinity by itself.

## Capacity drivers

- **Connections and rooms:** see the
  [resource requirements](../deployment.md#resource-requirements) for current
  per-process guidance.
- **Relay-floor bandwidth:** rooms that do not upgrade to P2P keep all
  `GameData` on the process, so message rate, payload size, and recipient fanout
  dominate network and queue pressure.
- **P2P signaling:** after WebRTC establishment, the process normally carries
  only control, signaling, and fallback traffic.
- **TURN:** TURN relay bandwidth is operated separately from the signaling
  process; see the [TURN deployment guide](../deployment-turn.md).

These are operational starting points, not measured saturation guarantees. The
P10.F2 throughput knee table remains pending the planned 16-player saturation
experiment.

## Directional partition detection

A WebSocket can fail in only one direction. Do not use successful inbound or
outbound application traffic as proof that the reverse path is healthy. The
server has two independent fail-loud mechanisms:

| Fault | What may still work | Default detection path | Authoritative result |
| --- | --- | --- | --- |
| client → server blackhole | The client can keep receiving room traffic and RFC 6455 Ping frames | The matching Pong cannot return; the next probe plus its deadline is roughly bounded by `server_ping_interval_secs + pong_timeout_secs` (10 + 5 seconds by default) | close `4003 activity_timeout`; `signal_fish_websocket_ping_timeouts_total` increments |
| server → client blackhole, otherwise idle | The server can keep receiving application `Ping` and game traffic | The client never receives the next protocol Ping, so no matching Pong returns; the same probe bound applies | close `4003 activity_timeout` |
| server → client blackhole under reliable relay pressure | Client writes can still reach the server while its outbound socket/data queue stops draining | Queue/socket fill plus `slow_consumer_timeout_ms` (5 seconds by default), or `max_sojourn_ms` (15 seconds by default); a socket probe can win first depending on probe phase | close `4002 slow_consumer` when delivery pressure wins, otherwise `4003 activity_timeout` |
| symmetric blackhole | Neither direction carries new traffic | The next protocol probe cannot complete | close `4003 activity_timeout` |

The close code describes the mechanism that won, not the physical direction of
the fault. In particular, application Pings crossing client → server do not
make a blocked server → client path healthy. Keep protocol Ping handling enabled
in the WebSocket stack, treat `4002` and `4003` as terminal for that physical
connection, and reconnect according to the client contract.

The directional real-socket experiments use shortened probe/grace values and
prove all three distinct cases: the unaffected direction carries traffic,
exactly the expected close metric fires, the surviving member observes
`PlayerLeft`, and a replacement member can join and receive relayed data. The
same suite confirms that E4's probe bound supersedes the older activity-reaper
estimate for these socket partitions.

## Related documents

- [Single-instance deployment contract](single-instance-deployment.md) — the
  supported topology and proven split-brain failure catalog
- [Deployment guide](../deployment.md) — process/container operation
- [TURN deployment guide](../deployment-turn.md) — relay capacity and cost
- [Transport fallback](transport-fallback.md) — the relay floor
- [Handoff and topologies](handoff-and-topologies.md) — session plan emission
