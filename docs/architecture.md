# Architecture

Signal Fish Server is a single-process, in-memory coordination service for
multiplayer games. This page explains the choices that affect client design and
server operation. For message fields and configuration keys, use the linked
references.

## At a glance

```text
Game clients
    │ WebSocket
    ▼
Signal Fish Server
    ├─ rooms, lobby state, and reconnect state (in memory)
    ├─ WebSocket game-data relay
    └─ optional peer-to-peer coordination
             │
             └─ operator-provided STUN/TURN for WebRTC
```

Clients use one long-lived WebSocket for room operations and for the relay.
Protocol v2 always sends game data through that relay. Protocol v3 can select a
Direct or WebRTC peer-to-peer plan when every current player supports it, while
keeping the relay available as the fallback.

Read [Choose v2 or v3](concepts/protocol-versions.md) for the compatibility
rules and [Transport Fallback](architecture/transport-fallback.md) for the v3
client contract.

## The room is the ownership boundary

One running process owns a room and all of its live state:

- membership, readiness, spectators, and authority;
- reconnect tokens and buffered reconnect events;
- negotiated v3 session plans and signaling; and
- connection queues, rate-limit state, and operational counters.

That state is not copied to another server and does not survive a restart.
Putting several independent instances behind a generic load balancer can split
players with the same room code into different rooms. Run one active process
for a routing domain, or place an external room directory in front of Signal
Fish that assigns every room to exactly one process before the WebSocket opens.

The [Single-Instance Deployment Contract](architecture/single-instance-deployment.md)
defines the supported routing boundary. The [Consistency and Durability
Contract](architecture/consistency-and-durability.md) explains what clients can
and cannot recover after a disconnect or process loss.

## Data paths

### WebSocket relay

The relay is the simplest and universal path. A client sends `GameData`; the
room's process routes it to the other current players. Work grows with message
rate, encoded message size, and the number of recipients. Slow recipients are
bounded by per-connection queues and may be disconnected rather than allowing
memory use to grow without limit.

### Optional peer-to-peer path

With protocol v3, the server chooses one room-wide topology and transport from
the capabilities every player advertised. It tells each client which peers to
connect to and forwards opaque WebRTC signaling messages. Signal Fish does not
carry peer data on those direct connections and does not operate a STUN or TURN
service. Operators provide that infrastructure when required.

See [Handoff and Topologies](architecture/handoff-and-topologies.md) for plan
selection and [Self-hosted TURN](deployment-turn.md) for WebRTC infrastructure.

## Access and trust

The optional app-ID allowlist identifies a public application label for
admission, quotas, and metrics. It is not user authentication and an app ID is
not a secret. Browser-origin checks, message validation, rate limits, TLS, and
metrics authentication are separate controls.

Read [Application Identification and Access](authentication.md) for the exact
trust boundary and [Configuration](configuration.md) for the available limits.

## Capacity and operations

There is no fixed rooms, players, or messages-per-second recommendation that is
portable across games and hosts. Capacity depends especially on relay fanout,
message size and rate, slow clients, reconnect buffering, WebRTC/TURN use, and
the CPU and network available to the process.

Benchmark the release binary with your expected room sizes and traffic, then
monitor queue pressure, delivery latency, connection closes, CPU, memory, and
network egress. The [Scaling Architecture](architecture/scaling.md) provides
the sizing model and measured examples without treating them as deployment
promises. Use the [Deployment Guide](deployment.md) for health checks, metrics,
TLS, proxying, and shutdown behavior.

## Where to go next

- [Build a Client](guides/building-a-client.md) — required connection and room
  lifecycle
- [Configure the Server](configuration.md) — policy, limits, transports, and
  operational settings
- [Protocol Reference](protocol.md) — exact wire messages and errors
- [Library Usage](library-usage.md) — embed the server in a Rust application
