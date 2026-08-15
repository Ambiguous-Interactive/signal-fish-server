# Capabilities

Signal Fish Server provides room coordination and a supported WebSocket relay
floor, with optional protocol v3 peer-to-peer coordination. This page is a map
to the focused documentation; it does not repeat message shapes or
configuration keys.

For exact protocol-version, server-configuration, and client requirements, use
the [Feature Availability Matrix](reference/feature-matrix.md).

## Game flow

- [Rooms and Lobbies](concepts/rooms-and-lobbies.md) — room codes, membership,
  readiness, and starting a game
- [Authority System](concepts/authority.md) — choosing one player to make
  authoritative game decisions
- [Spectator Mode](concepts/spectator-mode.md) — observing a room without taking
  a player seat
- [Reconnection](concepts/reconnection.md) — restoring a player session and
  replaying buffered events

## Networking and delivery

- [Protocol v2 vs v3](concepts/protocol-versions.md) — the relay-only v2
  baseline, additive v3 capabilities, and mixed-room behavior
- [Handoff and Topologies](architecture/handoff-and-topologies.md) — relay,
  host, and mesh session plans over relay, Direct, or WebRTC transports
- [Transport Fallback](architecture/transport-fallback.md) — how clients keep
  the WebSocket relay floor available when a peer-to-peer path fails
- [Protocol Reference](protocol.md) — message formats, MessagePack support,
  delivery classes, and error responses
- [Self-hosted TURN](deployment-turn.md) — connecting Signal Fish to
  operator-run STUN and TURN infrastructure; Signal Fish does not operate those
  services

## Access and operations

- [Application Identification and Access](authentication.md) — app-ID
  allowlisting, quotas, metrics authentication, and the exact trust boundary
- [Configuration](configuration.md) — room limits, rate limits, WebSocket
  settings, session policy, browser origins, token binding, TLS, and metrics
  access
- [Run Modes](run-modes.md) — common relay, peer-to-peer, TLS, and monitoring
  setups
- [Deployment](deployment.md) — containers, reverse proxies, health checks,
  metrics, logging, and production guidance
- [Single-Instance Deployment](architecture/single-instance-deployment.md) —
  the consistency boundary for the in-memory server

Start with the [Quick Start](quickstart.md), then use
[Building a Client](guides/building-a-client.md) for the client contract.
