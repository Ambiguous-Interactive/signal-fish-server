<p align="center">
  <img
    src="https://raw.githubusercontent.com/Ambiguous-Interactive/signal-fish-server/main/docs/assets/logo-banner.svg"
    alt="Signal Fish Server" width="640">
</p>

<p align="center">
  <a href="https://crates.io/crates/signal-fish-server">
    <img src="https://img.shields.io/crates/v/signal-fish-server?style=for-the-badge"
         alt="crates.io">
  </a>
  <a href="https://ambiguous-interactive.github.io/signal-fish-server/">
    <img src="https://img.shields.io/badge/docs-GitHub%20Pages-blue?style=for-the-badge"
         alt="Documentation">
  </a>
  <a href="https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/rust-toolchain.toml">
    <img src="https://img.shields.io/badge/MSRV-1.91.0-blue.svg?style=for-the-badge"
         alt="MSRV">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-MIT-blue.svg?style=for-the-badge"
         alt="License: MIT">
  </a>
</p>

Signal Fish Server is a lightweight, in-memory WebSocket signaling server for
peer-to-peer multiplayer games. It puts players into rooms, coordinates lobby
state and peer setup, and can relay game data. It runs as one Rust binary with
no database, message broker, or cloud service required.

Built by [Ambiguous Interactive](https://github.com/Ambiguous-Interactive).

> **AI disclosure:** This project was developed with substantial assistance
> from Claude Opus 4.6 and Codex 5.3. Humans created the protocol concepts and
> core design and retained oversight of architecture and code review.

## What it is — and is not

Signal Fish Server handles connection coordination, not your game simulation.
It provides rooms, lobbies, reconnection, spectator and authority flows,
WebRTC signaling, rate limits, and operational metrics. WebSocket relay is
still available when v3 clients negotiate a peer-to-peer data path.

Live rooms, connections, reconnect state, and relay buffers are process-local
and are lost when the server restarts. The server does not make game state
durable or synchronize independent instances. Production deployments must
choose their own TLS, routing, capacity, and application-identification policy;
the operator guides below cover those decisions.

## Run the server

### Docker

The published image ships the secure compiled defaults (app-ID allowlist
enforcement and metrics auth on). For a local trial, opt into the open
development posture explicitly:

```bash
docker run --rm -p 127.0.0.1:3536:3536 \
  -e SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=false \
  -e SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false \
  ghcr.io/ambiguous-interactive/signal-fish-server:latest
```

Verify it is ready:

```bash
curl http://localhost:3536/v2/health
```

### From source

Source builds require Rust 1.91.0 or newer. Copy the development configuration so
the local server accepts clients without a configured app-ID allowlist:

```bash
git clone https://github.com/Ambiguous-Interactive/signal-fish-server.git
cd signal-fish-server
cp config.example.json config.json
cargo run
```

The server listens on port `3536` by default. `config.example.json` binds on all
network interfaces and leaves origins, app IDs, and metrics open. Use it only on
a trusted development network behind a firewall; use the loopback-bound Docker
command above when that is not appropriate.

## Create a room

Follow the [complete five-minute quick start](https://ambiguous-interactive.github.io/signal-fish-server/quickstart/)
to start a local server, open two browser WebSockets from an allowed origin,
create a room, and join it from a second client. It includes the exact commands
and messages to paste, plus the production allowlist difference.

The room-creation step uses this complete browser-console example:

```javascript
const socket = new WebSocket("ws://localhost:3536/v2/ws");
socket.addEventListener("open", () => {
  socket.send(JSON.stringify({
    type: "JoinRoom",
    data: {
      game_name: "my-game",
      player_name: "Alice",
      max_players: 2
    }
  }));
});
socket.addEventListener("message", ({ data }) => {
  const message = JSON.parse(data);
  if (message.type === "RoomJoined") {
    console.log("Room code:", message.data.room_code);
  }
});
```

## Protocol versions

- **v2** is the reliable WebSocket relay floor and the simplest place to start.
- **v3** adds capability negotiation for direct or WebRTC peer plans and
  delivery classes for JSON relay traffic. It is additive: v2 clients continue
  to use v2 message shapes, while v3 clients opt into the extra capabilities.

Use `/v2/ws` for the v2 default or `/v3/ws` for the v3 default. Read the
[protocol-version guide](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/docs/concepts/protocol-versions.md)
before advertising v3 capabilities.

## Go deeper

- [Build a client](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/docs/guides/building-a-client.md)
  for the required lifecycle, heartbeats, room flow, and v3 client rules.
- [Configure the server](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/docs/configuration.md)
  and [deploy it](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/docs/deployment.md)
  with production-oriented settings.
- Use the [protocol reference](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/docs/protocol.md)
  for message shapes, fields, and error behavior.
- See [library usage](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/docs/library-usage.md)
  to embed `signal_fish_server` in a Rust application.
- Follow the [contributor development guide](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/docs/development.md)
  to build, test, and work on the repository.

The complete user documentation is available on the
[Signal Fish Server documentation site](https://ambiguous-interactive.github.io/signal-fish-server/).

## License

Signal Fish Server is available under the
[MIT License](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/LICENSE).
