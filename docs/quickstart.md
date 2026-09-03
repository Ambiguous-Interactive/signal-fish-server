# Quick Start

Start Signal Fish Server, create a room, and join it from a second WebSocket.

## 1. Start the Server

Choose Docker or Cargo.

<!-- markdownlint-disable MD046 -->

=== "Docker"

    The published image ships the secure compiled defaults, so this local
    trial opts into the open development posture explicitly. The command
    exposes it only on your computer, permits the browser page used below as
    a WebSocket origin, and disables the app-ID allowlist and metrics auth
    for the demo.

    ```bash
    docker run --rm -p 127.0.0.1:3536:3536 \
      -e SIGNAL_FISH__SECURITY__CORS_ORIGINS=http://localhost:3536 \
      -e SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=false \
      -e SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false \
      ghcr.io/ambiguous-interactive/signal-fish-server:latest
    ```

=== "Cargo"

    Install Rust 1.91+, then clone the repository. Copy the local
    development config before starting the server; the secure built-in defaults
    require settings that are not suitable for this first run.

    !!! warning "Trusted development networks only"

        `config.example.json` binds on all network interfaces and leaves browser
        origins, app IDs, and metrics open. Keep it behind a firewall on a
        trusted development network. Use the loopback-bound Docker command
        above instead if other machines could reach your development host.

    ```bash
    git clone https://github.com/Ambiguous-Interactive/signal-fish-server.git
    cd signal-fish-server
    cp config.example.json config.json
    cargo run
    ```

<!-- markdownlint-enable MD046 -->

The server listens on port 3536. Open
<http://localhost:3536/v2/health> in a browser to check it.

## 2. Create a Room

On that health page, open the browser's developer console and paste this
JavaScript:

```javascript
const alice = new WebSocket("ws://localhost:3536/v2/ws");
alice.onmessage = ({ data }) => console.log("Alice:", JSON.parse(data));
alice.onopen = () => alice.send(JSON.stringify({
  type: "JoinRoom",
  data: { game_name: "my-game", player_name: "Alice", max_players: 2 }
}));
```

Alice receives a `RoomJoined` message. Copy `data.room_code` from that message.
Leaving out `room_code` is what creates a new room.

## 3. Join the Room

In the same console, replace `ABC123` below with Alice's room code and paste:

```javascript
const bob = new WebSocket("ws://localhost:3536/v2/ws");
bob.onmessage = ({ data }) => console.log("Bob:", JSON.parse(data));
bob.onopen = () => bob.send(JSON.stringify({
  type: "JoinRoom",
  data: { game_name: "my-game", player_name: "Bob", room_code: "ABC123" }
}));
```

Bob receives `RoomJoined`, and Alice receives `PlayerJoined`. You now have two
players in the same room.

This example uses the open app-ID policy — disabled explicitly by the Docker
command's environment overrides above and by `config.example.json`, never by
the image itself. A production allowlist requires `Authenticate` before
`JoinRoom`; see [Application identification and access](authentication.md).

## Next Steps

- [Build a client](guides/building-a-client.md) to handle room events and game
  data.
- [Learn about rooms and lobbies](concepts/rooms-and-lobbies.md) to ready players
  and start a game.
- [Read the protocol reference](protocol.md) for all client and server messages.
- [Configure the server](configuration.md) before deploying it.
