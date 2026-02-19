# Quick Start

Get Signal Fish Server running and two clients connected in under 5 minutes.

## Prerequisites

You need one of the following:

- **Rust 1.88+** -- to build and run from source
- **Docker** -- to run the pre-built container image

## Step 1: Start the Server

=== "Docker"

    Pull and run the container image. No configuration needed.

    ```bash
    docker run -p 3536:3536 ghcr.io/ambiguousinteractive/signal-fish-server:latest
    ```

=== "Cargo"

    Clone the repository and run with Cargo.

    ```bash
    git clone https://github.com/Ambiguous-Interactive/signal-fish-server.git
    cd signal-fish-server
    cargo run
    ```

The server starts on port 3536 by default. Verify it is running:

```bash
curl http://localhost:3536/v2/health
```

## Step 2: Connect and Create a Room

Open a new terminal and create a Rust project for your first client.
Add the following dependencies to your `Cargo.toml`:

```toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
tokio-tungstenite = "0.28"
futures-util = "0.3"
serde_json = "1.0"
serde = { version = "1.0", features = ["derive"] }
```

Create a file called `src/main.rs` with the following code. This client
connects to the server and creates a new room by sending a `JoinRoom`
message without a `room_code`:

```rust,ignore
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let url = "ws://localhost:3536/v2/ws";
    let (mut ws, _) = connect_async(url)
        .await
        .expect("Failed to connect");

    println!("Connected to Signal Fish Server");

    // Create a new room (no room_code means "create")
    let join_msg = serde_json::json!({
        "type": "JoinRoom",
        "data": {
            "game_name": "my-game",
            "player_name": "Player1",
            "max_players": 2
        }
    });
    ws.send(Message::Text(join_msg.to_string().into()))
        .await
        .expect("Failed to send JoinRoom");

    // Read the RoomJoined response
    if let Some(Ok(Message::Text(text))) = ws.next().await {
        let response: Value = serde_json::from_str(&text)
            .expect("Invalid JSON from server");
        let msg_type = response["type"].as_str().unwrap_or("unknown");
        let room_code = response["data"]["room_code"]
            .as_str()
            .unwrap_or("none");

        println!("Response type: {msg_type}");
        println!("Room code: {room_code}");
        println!("Share this room code with another player!");
    }
}
```

Run this client:

```bash
cargo run
```

You should see output like:

```text
Connected to Signal Fish Server
Response type: RoomJoined
Room code: A7X2K9
Share this room code with another player!
```

Copy the room code from the output. You will need it in the next step.

## Step 3: Join from Another Client

In a separate terminal, create a second Rust project. Use the same
`Cargo.toml` dependencies as Step 2, then create `src/main.rs` with
the following code. Replace `A7X2K9` with the room code from Step 2:

```rust,ignore
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let url = "ws://localhost:3536/v2/ws";
    let (mut ws, _) = connect_async(url)
        .await
        .expect("Failed to connect");

    println!("Connected to Signal Fish Server");

    // Join the existing room using the room code from Step 2
    let join_msg = serde_json::json!({
        "type": "JoinRoom",
        "data": {
            "game_name": "my-game",
            "room_code": "A7X2K9",
            "player_name": "Player2"
        }
    });
    ws.send(Message::Text(join_msg.to_string().into()))
        .await
        .expect("Failed to send JoinRoom");

    // Read the RoomJoined response
    if let Some(Ok(Message::Text(text))) = ws.next().await {
        let response: Value = serde_json::from_str(&text)
            .expect("Invalid JSON from server");
        let msg_type = response["type"].as_str().unwrap_or("unknown");
        let players = &response["data"]["current_players"];
        let player_count = players.as_array()
            .map(|a| a.len())
            .unwrap_or(0);

        println!("Response type: {msg_type}");
        println!("Players in room: {player_count}");
    }
}
```

Run the second client:

```bash
cargo run
```

You should see that the room now has 2 players:

```text
Connected to Signal Fish Server
Response type: RoomJoined
Players in room: 2
```

Meanwhile, the first client will receive a `PlayerJoined` notification
from the server indicating that Player2 has entered the room.

## Step 4: Exchange Data

Once both players are in the room, either client can send arbitrary game
data to the other using the `GameData` message type. The server relays
the data to all other players in the room.

Add a send-and-receive loop to your client after joining the room:

```rust,ignore
// Send game data to all other players in the room.
// The outer "data" is the serde content tag; the inner "data"
// is the GameData variant's field name.
let game_data = serde_json::json!({
    "type": "GameData",
    "data": {
        "data": {
            "action": "move",
            "x": 100,
            "y": 200
        }
    }
});
ws.send(Message::Text(game_data.to_string().into()))
    .await
    .expect("Failed to send GameData");

// Listen for incoming messages
while let Some(Ok(Message::Text(text))) = ws.next().await {
    let msg: Value = serde_json::from_str(&text)
        .expect("Invalid JSON");
    let msg_type = msg["type"].as_str().unwrap_or("unknown");

    match msg_type {
        "GameData" => {
            let from = msg["data"]["from_player"]
                .as_str()
                .unwrap_or("unknown");
            println!("Game data from {from}: {}", msg["data"]["data"]);
        }
        other => println!("Received: {other}"),
    }
}
```

The `data` field inside `GameData` accepts any valid JSON value. Use it
to send positions, inputs, chat messages, or any game state your
application needs.

## Step 5: Ready Up and Start

Signal Fish Server includes a lobby system that tracks when players are
ready. Once all players send `PlayerReady`, the lobby transitions through
`Lobby` to `Finalized` and the server sends a `GameStarting` event
with peer connection information.

After both clients have joined the room, send the ready signal:

```rust,ignore
// Signal that this player is ready
let ready_msg = serde_json::json!({
    "type": "PlayerReady"
});
ws.send(Message::Text(ready_msg.to_string().into()))
    .await
    .expect("Failed to send PlayerReady");

// Listen for lobby state changes
while let Some(Ok(Message::Text(text))) = ws.next().await {
    let msg: Value = serde_json::from_str(&text)
        .expect("Invalid JSON");
    let msg_type = msg["type"].as_str().unwrap_or("unknown");

    match msg_type {
        "LobbyStateChanged" => {
            let state = msg["data"]["lobby_state"]
                .as_str()
                .unwrap_or("unknown");
            println!("Lobby state: {state}");
        }
        "GameStarting" => {
            println!("Game is starting!");
            println!("Peer connections: {}", msg["data"]["peer_connections"]);
            break;
        }
        other => println!("Received: {other}"),
    }
}
```

When both players send `PlayerReady`, you will see the lobby transition:

```text
Lobby state: lobby
Lobby state: finalized
Game is starting!
```

At this point the server has done its job: players are matched, ready,
and have the information they need to establish direct peer-to-peer
connections.

## What's Next

Now that you have a working signaling flow, explore the deeper concepts
and build a production-ready integration:

- **[Rooms and Lobbies](concepts/rooms-and-lobbies.md)** -- understand
  room lifecycle, lobby states, and player management
- **[Authority System](concepts/authority.md)** -- designate a host
  player for server-authoritative game logic
- **[Reconnection](concepts/reconnection.md)** -- handle dropped
  connections gracefully with event replay
- **[Rust Client Guide](guides/rust-client.md)** -- build a complete,
  robust game client
- **[Configuration](configuration.md)** -- customize ports, limits,
  authentication, and more
- **[Protocol Reference](protocol.md)** -- every message type and field
  documented in detail
