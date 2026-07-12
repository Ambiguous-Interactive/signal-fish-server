# Single-Instance Deployment Contract

Signal Fish Server is a **single-instance, in-memory room server**. One running
process is the only authority for every room it hosts. Its room membership,
WebSocket routes, reconnection records, relay sequence/epoch counters, replay
ring, and active session plan are not shared with another process.

This is a correctness boundary, not merely a deployment recommendation:

> A room has exactly one home process. Every connection and reconnect for that
> room must reach that process for the room's lifetime. Losing the home process
> loses the room.

The server logs this contract at startup as structured fields:
`deployment_mode=single_instance`, `room_state=in_memory`,
`room_affinity_required=true`, and `session_handoff=false`.

## What one home means

Keep all of these operations on the same process:

- creating or joining a room;
- reconnecting with a room token;
- relay-floor `GameData` fan-out;
- v3 WebRTC `Signal` relay and session re-planning; and
- spectator joins and room lifecycle updates.

Connection-level cookie or source-IP stickiness is insufficient unless it also
guarantees the same room home. A client opens its WebSocket before it sends
`JoinRoom`, so a conventional load balancer cannot route the initial upgrade by
`room_code` unless an application-owned edge supplies that routing key outside
the WebSocket protocol. Reconnect tokens also carry no routable instance hint.

The safe supported deployment is one active Signal Fish process per routing
domain. You may run several isolated deployments when an application-owned
directory assigns each newly created room to one deployment and routes **every**
participant to that deployment before the WebSocket upgrade. Signal Fish does
not provide that directory, migrate a live room, or coordinate a room across
processes.

## Proven failure catalog

The nightly two-process H5 experiment
(`tests/split_brain_two_instances_e2e.rs`) runs the real binary twice and pins
the unsupported behavior:

| Misrouted operation | Observed result | Why |
| --- | --- | --- |
| Join the same `(game_name, room_code)` on process B after creation on process A | B silently creates a second room with the same public code and a different `room_id`; each room contains only its local player | Join-by-code creates an unknown room locally; there is no shared room directory |
| Present A's real reconnect identity and token to B | `ReconnectionFailed` / `RECONNECTION_FAILED`, “No disconnection record found” | Reconnection records and token claims are process-local |
| Signal from A's player to B's player ID | `SignalTargetNotFound`, “Signal target is not in any room” | B's player is absent from A's local registry, so A cannot even classify it as a different local room |

These are clean local responses, but together they are a silent split brain at
the application level. Health checks can remain green on both processes while
the logical room is partitioned.

The formal suite makes the same boundary executable. `SplitBrainStampBug` in
`SequencedRelay.tla` breaks gap accountability when two instances stamp one
logical sender independently. `SplitBrainCounterBug` in `ReconnectReplay.tla`
breaks replay faithfulness and status honesty when a reconnect lands on a fresh
instance. See [Single-instance theorems](formal-verification.md) for the model
scope and seeded counterexamples.

## Load balancer and drain behavior

For the supported single-active deployment:

1. Route readiness/health checks to `/v2/health`.
2. Before planned maintenance, remove the process from new connection routing.
3. Send `SIGTERM` (or Ctrl-C) and allow at least `server.drain_grace_secs`.
4. The server rejects new upgrades, sends v3 `GoingAway`, and closes remaining
   sockets with `4000 server_shutdown`.
5. Clients create or join a fresh room after drain; they must not reconnect to
   the exiting process, and the drain deliberately does not arm reconnect state.

If an external room directory fronts multiple isolated deployments, it must
stop assigning **new rooms** to a draining home before `SIGTERM`. Existing rooms
are not handed off: clients receive the drain boundary and rebuild elsewhere.
An unexpected process or host loss has the same room-loss outcome without the
advisory.

## What does not provide multi-instance safety

The `DistributedLock`, `MessageCoordinator`, `SequencedMessage`, `region_id`,
and room-code-prefix types are extension seams or local coordination tools.
Their shipped implementations are in-memory. They do not provide shared
storage, consensus, a cross-instance message bus, global deduplication, or
reconnection/session handoff.

Do not infer multi-instance support from those names. Supporting a room across
processes would require a separately designed and verified distributed room
authority, routing directory, message bus, shared reconnect state, and sequence
ownership protocol. That system is out of scope for the current zero-external-
runtime-dependency server.

## Related documents

- [Scaling architecture](scaling.md) — scale-up guidance and future extension
  seams within this contract
- [Deployment guide](../deployment.md) — process and container operation
- [Reconnection](../concepts/reconnection.md) — token lifetime and drain rules
- [Protocol](../protocol.md#goingaway) — `GoingAway` and close code `4000`
- [Formal verification](formal-verification.md) — executable single-instance
  theorem boundary
