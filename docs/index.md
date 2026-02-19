# Signal Fish Server

**A lightweight, in-memory WebSocket signaling server for peer-to-peer game networking.**

Built in Rust with axum and tokio, Signal Fish Server handles the hardest
part of multiplayer game networking: getting players connected. No database,
no message broker, no cloud services required.

## What It Does

Signal Fish Server is a WebSocket signaling server purpose-built for
multiplayer games. Players connect over WebSocket, create or join rooms
using shareable 6-character room codes, and coordinate through a built-in
lobby system with ready-up state management. Once all players are ready,
the server facilitates peer-to-peer connection establishment so your game
clients can communicate directly. Everything runs in-memory in a single
binary -- deploy it anywhere and start matchmaking in seconds.

## Key Features

- **Room codes for easy matchmaking** -- players share a short 6-character
  code to join the same room
- **Lobby system with ready-up state machine** -- tracks player readiness
  and manages Waiting, Lobby, and Finalized states automatically
- **Authority system** -- designate one player as the authoritative host
  for server-authoritative game logic
- **Spectator mode** -- observers can watch games without counting toward
  player limits
- **Reconnection with event replay** -- players who disconnect can rejoin
  and receive all events they missed
- **Rate limiting and metrics** -- built-in protection against abuse, with
  JSON and Prometheus metrics endpoints
- **Docker-ready, zero configuration needed** -- pull the image and run it;
  the defaults work out of the box

## Quick Taste

Here is a minimal Rust example that connects to the server and creates
a new room:

```rust,ignore
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let (mut ws, _) = connect_async("ws://localhost:3536/v2/ws")
        .await
        .expect("Failed to connect");

    let join_msg = serde_json::json!({
        "type": "JoinRoom",
        "data": {
            "game_name": "my-game",
            "player_name": "Player1",
            "max_players": 4
        }
    });
    ws.send(Message::Text(join_msg.to_string().into()))
        .await
        .expect("Failed to send");

    if let Some(Ok(msg)) = ws.next().await {
        println!("Server response: {}", msg);
        // Prints a RoomJoined message with your room_code
    }
}
```

## Getting Started

Ready to build multiplayer into your game? Start here:

- **[Quick Start](quickstart.md)** -- get a server running and two clients
  talking in under 5 minutes
- **[Rust Client Guide](guides/rust-client.md)** -- build a complete
  game client with room management, lobby flow, and data exchange
- **[Protocol Reference](protocol.md)** -- every message type, field,
  and flow documented
