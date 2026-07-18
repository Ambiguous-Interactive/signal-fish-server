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

These are operational starting points, not universal saturation guarantees.

## Size the relay floor

Relay cost grows with fan-out, not just sender rate. For a room with `N`
players where every player sends `S` messages per second, each client receives
`S × (N - 1)` messages per second and the server performs:

```text
recipient deliveries/second = N × S × (N - 1)
payload egress/second        = deliveries/second × payload bytes
```

WebSocket, protocol, JSON/MessagePack, IP, and TLS framing add to the payload
floor. Measure the encoded frames for capacity planning; do not provision only
the application payload number.

The H2 exact-ledger experiment fixes `N = 16`, a 1 KiB application payload,
JSON relay, production queue defaults, and one second at each sender rate. It
requires every `(receiver, sender, sequence)` delivery, valid v3 stamps, no
slow-consumer eviction, and reports p50/p95/p99/max latency, backpressure, wall
time, and process RSS. The registered load grid is:

| Per-player send rate | Per-client receive rate | Server recipient deliveries | Application-payload egress floor |
| ---: | ---: | ---: | ---: |
| 30 msg/s | 450 msg/s | 7,200/s | 7.03 MiB/s (56.3 Mbit/s) |
| 60 msg/s | 900 msg/s | 14,400/s | 14.06 MiB/s (112.5 Mbit/s) |
| 120 msg/s | 1,800 msg/s | 28,800/s | 28.13 MiB/s (225 Mbit/s) |
| 240 msg/s | 3,600 msg/s | 57,600/s | 56.25 MiB/s (450 Mbit/s) |
| 480 msg/s | 7,200 msg/s | 115,200/s | 112.50 MiB/s (900 Mbit/s) |

The first exact-head GitHub run measured the following on one shared-process
runner (server plus all 16 load clients):

| Target per player | Writer completion | Observed deliveries | p99 latency | Queue backpressure | RSS |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 30 msg/s | 1.001 s | 6,874/s | 45 ms | 0 | 22.8 MiB |
| 60 msg/s | 1.018 s | 13,505/s | 54 ms | 0 | 23.1 MiB |
| 120 msg/s | 1.591 s | 16,965/s | 149 ms | 0 | 24.2 MiB |
| 240 msg/s | 2.525 s | 17,818/s | 851 ms | 0 | 26.1 MiB |
| 480 msg/s | 3.334 s | 19,595/s | 2,722 ms | 0 | 28.2 MiB |

Every one of the 223,200 sweep deliveries was exact and no client was evicted.
The observed knee lies between the 120 and 240 msg/s targets: doubling offered
fan-out from 28,800/s to 57,600/s increased completed throughput by only 5.0%,
writer completion slipped beyond the scheduled second, and p99 rose from 149 ms
to 851 ms without filling the server's outbound queues. At the 480 target p99
reached 2.72 seconds. On this shared-process experiment the load clients/socket
ingress became the limiting boundary before the delivery queue. It does
**not** establish a standalone server maximum.

The bounded PR-lane matrix already proves the 30 msg/s point across JSON and
MessagePack with 2, 8, and 16 players: 17,880/17,880 deliveries, zero default-
queue backpressure, zero eviction, and measured p99 of 20–55 ms. Treat the
higher-rate nightly sweep as a trend measurement for its GitHub-hosted runner,
not a portable capacity promise. Benchmark the release binary on the intended
CPU, kernel, TLS termination, and network path, and retain headroom below the
first point where p99 or queue pressure bends sharply.

### Queue and freeze budget

For one recipient, estimate queue-fill time in messages, using encoded-message
rates:

```text
excess_rate = max(0, incoming_messages_per_second - drain_messages_per_second)
queue_fill_seconds = send_queue_capacity / excess_rate
fail_loud_bound ≈ queue_fill_seconds + slow_consumer_timeout
```

If `excess_rate` is zero, the queue does not fill under steady state; still
reserve burst headroom. A full reliable queue can pace room senders for at most
one `slow_consumer_timeout_ms` window before that recipient is closed loudly.
`max_sojourn_ms` independently bounds oldest reliable queued/batched work and
each control item's own queue age through write completion. Latest/volatile
queue age is handled by their loss policy; the value bounds their selected
socket write instead. Any of those deadlines may close the connection first.
`socket_send_buffer_bytes` bounds data already handed to TCP ahead of later
control (the 65536-byte default is a request; operating systems may clamp or
account it differently).

Do not multiply `send_queue_capacity × slow_consumer_timeout` and interpret the
result as seconds: queue slots and time have different units. With the defaults,
`1024 × 5000 ms` is only a message-time exposure measure; the actionable time
bound is queue fill **plus** the 5-second capacity wait. Raising queue capacity
absorbs a larger burst but retains more stale reliable work. Raising the
timeout tolerates a longer drain pause but directly raises the maximum room
freeze after the queue is full.

### Batching latency

With batching enabled, a sparse message may wait up to
`websocket.batch_interval_ms` (16 ms by default) for the timer; a full
`batch_size` flushes earlier. The low-rate two-player matrix measured roughly
20 ms p99 end to end, consistent with one default batch interval plus local
processing. This is not a universal latency floor: under dense traffic the
size trigger can flush sooner, while network and scheduler delay can dominate
it. Lower the interval only after measuring the syscall/throughput tradeoff.

## Directional partition detection

A WebSocket can fail in only one direction. Do not use successful inbound or
outbound application traffic as proof that the reverse path is healthy. The
server has two independent fail-loud mechanisms:

| Fault | What may still work | Default detection path | Authoritative result |
| --- | --- | --- | --- |
| client → server blackhole | The client can keep receiving room traffic and RFC 6455 Ping frames | A probe scheduled just before the last inbound activity may be skipped, so the worst-case fixed-tick bound is nearly `2 × server_ping_interval_secs + pong_timeout_secs` (25 seconds by default) | close `4003 activity_timeout`; `signal_fish_websocket_ping_timeouts_total` increments |
| server → client blackhole, both directions otherwise idle | No inbound non-Pong frame proves liveness | The client never receives the next protocol Ping, so no matching Pong returns; the same worst-case fixed-tick bound applies | close `4003 activity_timeout` |
| server → client blackhole while client → server traffic continues | Client writes can still reach the server, so idle-only probes are skipped or cancelled | Outbound queue/socket pressure, a socket write failure, or an application-level policy; upstream traffic alone cannot prove the reverse path | close `4002 slow_consumer` when delivery pressure wins; otherwise detection requires another policy |
| symmetric blackhole | Neither direction carries new traffic | The next protocol probe cannot complete | close `4003 activity_timeout` |

The close code describes the mechanism that won, not the physical direction of
the fault. Idle-only probes deliberately treat decoded inbound non-Pong traffic as
liveness so application backpressure cannot hide an already-arrived Pong. This
means continuous client → server traffic does not independently test the
reverse path; outbound pressure/failure or an application policy owns that
case. Keep protocol Ping handling enabled, treat `4002` and `4003` as terminal
for that physical connection, and reconnect according to the client contract.

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
