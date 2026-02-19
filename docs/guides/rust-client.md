# Rust Client Guide

This guide walks through building a Rust client for Signal Fish Server using
`tokio-tungstenite`. Every section includes working code examples that
demonstrate the full lifecycle: connecting, creating and joining rooms,
exchanging game data, readying up, reconnecting, and spectating.

## Dependencies

Add the following to your `Cargo.toml`:

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.24"
futures-util = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["serde"] }
url = "2"
```

- **tokio** -- async runtime
- **tokio-tungstenite** -- WebSocket client built on tokio
- **futures-util** -- `StreamExt` and `SinkExt` traits for reading/writing
- **serde + serde_json** -- JSON serialization matching the server protocol
- **uuid** -- player and room identifiers are UUIDs
- **url** -- URL parsing for the WebSocket endpoint

## Connecting

Establish a WebSocket connection to the server's v2 endpoint.

```rust,ignore
use tokio_tungstenite::connect_async;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = Url::parse("ws://localhost:3536/v2/ws")?;
    let (ws_stream, _response) = connect_async(url).await?;
    println!("Connected to Signal Fish Server");

    // Split into sender and receiver halves
    use futures_util::StreamExt;
    let (write, read) = ws_stream.split();

    // Use `write` to send messages and `read` to receive them.
    drop(write);
    drop(read);

    Ok(())
}
```

`connect_async` returns a `WebSocketStream` that you split into a `SplitSink`
(for sending) and a `SplitStream` (for receiving) using `StreamExt::split`.

## Message Types

The server protocol uses JSON messages with a `type` field and an optional
`data` field. Define matching Rust types with serde's externally tagged
representation.

### Client Messages

Messages the client sends to the server:

```rust,ignore
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for players.
pub type PlayerId = Uuid;
/// Unique identifier for rooms.
pub type RoomId = Uuid;

/// Messages sent from the client to the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    /// Authenticate with an app ID (required when auth is enabled).
    Authenticate {
        app_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sdk_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        platform: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        game_data_format: Option<String>,
    },
    /// Join or create a room. Omit `room_code` to create a new room.
    JoinRoom {
        game_name: String,
        player_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        room_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_players: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        supports_authority: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        relay_transport: Option<String>,
    },
    /// Leave the current room.
    LeaveRoom,
    /// Send arbitrary game data to other players.
    GameData {
        data: serde_json::Value,
    },
    /// Signal readiness to start the game.
    PlayerReady,
    /// Request or release game authority.
    AuthorityRequest {
        become_authority: bool,
    },
    /// Provide connection info for P2P establishment.
    ProvideConnectionInfo {
        connection_info: serde_json::Value,
    },
    /// Heartbeat ping. Server responds with `Pong`.
    Ping,
    /// Reconnect after a disconnection using stored credentials.
    Reconnect {
        player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
    },
    /// Join a room as a read-only spectator.
    JoinAsSpectator {
        game_name: String,
        room_code: String,
        spectator_name: String,
    },
    /// Leave spectator mode.
    LeaveSpectator,
}
```

### Server Messages

Messages received from the server:

```rust,ignore
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Information about a player in a room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    pub is_authority: bool,
    pub is_ready: bool,
    pub connected_at: DateTime<Utc>,
}

/// Information about a spectator watching a room.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectatorInfo {
    pub id: PlayerId,
    pub name: String,
    pub connected_at: DateTime<Utc>,
}

/// Peer connection information provided when the game starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConnectionInfo {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub relay_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<serde_json::Value>,
}

/// Rate limit information for an authenticated app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub per_minute: u32,
    pub per_hour: u32,
    pub per_day: u32,
}

/// Messages sent from the server to the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    /// Authentication succeeded.
    Authenticated {
        app_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        organization: Option<String>,
        rate_limits: RateLimitInfo,
    },
    /// Authentication failed.
    AuthenticationError {
        error: String,
        error_code: String,
    },
    /// Successfully joined or created a room.
    RoomJoined {
        room_id: RoomId,
        room_code: String,
        player_id: PlayerId,
        game_name: String,
        max_players: u8,
        supports_authority: bool,
        current_players: Vec<PlayerInfo>,
        is_authority: bool,
        lobby_state: String,
        ready_players: Vec<PlayerId>,
        relay_type: String,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    /// Failed to join a room.
    RoomJoinFailed {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
    /// Successfully left the room.
    RoomLeft,
    /// Another player joined the room.
    PlayerJoined {
        player: PlayerInfo,
    },
    /// Another player left the room.
    PlayerLeft {
        player_id: PlayerId,
    },
    /// Game data relayed from another player.
    GameData {
        from_player: PlayerId,
        data: serde_json::Value,
    },
    /// Lobby state transitioned.
    LobbyStateChanged {
        lobby_state: String,
        ready_players: Vec<PlayerId>,
        all_ready: bool,
    },
    /// Game is starting with peer connection details.
    GameStarting {
        peer_connections: Vec<PeerConnectionInfo>,
    },
    /// Authority status changed in the room.
    AuthorityChanged {
        authority_player: Option<PlayerId>,
        you_are_authority: bool,
    },
    /// Response to an authority request.
    AuthorityResponse {
        granted: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
    /// Heartbeat response.
    Pong,
    /// Reconnection succeeded with current room state.
    Reconnected {
        room_id: RoomId,
        room_code: String,
        player_id: PlayerId,
        game_name: String,
        max_players: u8,
        supports_authority: bool,
        current_players: Vec<PlayerInfo>,
        is_authority: bool,
        lobby_state: String,
        ready_players: Vec<PlayerId>,
        relay_type: String,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
        missed_events: Vec<ServerMessage>,
    },
    /// Reconnection failed.
    ReconnectionFailed {
        reason: String,
        error_code: String,
    },
    /// Another player reconnected.
    PlayerReconnected {
        player_id: PlayerId,
    },
    /// Successfully joined as a spectator.
    SpectatorJoined {
        room_id: RoomId,
        room_code: String,
        spectator_id: PlayerId,
        game_name: String,
        current_players: Vec<PlayerInfo>,
        current_spectators: Vec<SpectatorInfo>,
        lobby_state: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Failed to join as a spectator.
    SpectatorJoinFailed {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
    /// Successfully left spectator mode.
    SpectatorLeft {
        #[serde(skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        room_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    /// A new spectator joined the room.
    NewSpectatorJoined {
        spectator: SpectatorInfo,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// A spectator disconnected.
    SpectatorDisconnected {
        spectator_id: PlayerId,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    /// General error message.
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
}
```

## Creating a Room

Send `JoinRoom` without a `room_code` to create a new room. The server responds
with `RoomJoined` containing the generated room code.

```rust,ignore
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = Url::parse("ws://localhost:3536/v2/ws")?;
    let (ws_stream, _) = connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Create a new room (no room_code)
    let join = ClientMessage::JoinRoom {
        game_name: "my-game".to_string(),
        player_name: "Player1".to_string(),
        room_code: None,
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let json = serde_json::to_string(&join)?;
    write.send(Message::Text(json)).await?;

    // Wait for RoomJoined response
    if let Some(Ok(Message::Text(text))) = read.next().await {
        let msg: ServerMessage = serde_json::from_str(&text)?;
        if let ServerMessage::RoomJoined {
            room_code,
            player_id,
            ..
        } = msg
        {
            println!("Created room: {room_code}");
            println!("My player ID: {player_id}");
            // Store player_id and room_id for reconnection
        }
    }

    Ok(())
}
```

The `room_code` in the response is a 6-character code (by default) that other
players use to join.

## Joining a Room

To join an existing room, include the `room_code` from the room creator:

```rust,ignore
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = Url::parse("ws://localhost:3536/v2/ws")?;
    let (ws_stream, _) = connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Join an existing room by code
    let join = ClientMessage::JoinRoom {
        game_name: "my-game".to_string(),
        player_name: "Player2".to_string(),
        room_code: Some("ABC123".to_string()),
        max_players: None,
        supports_authority: None,
        relay_transport: None,
    };
    let json = serde_json::to_string(&join)?;
    write.send(Message::Text(json)).await?;

    // Wait for response
    if let Some(Ok(Message::Text(text))) = read.next().await {
        let msg: ServerMessage = serde_json::from_str(&text)?;
        match msg {
            ServerMessage::RoomJoined {
                room_code,
                current_players,
                ..
            } => {
                println!("Joined room: {room_code}");
                println!(
                    "Players in room: {}",
                    current_players.len()
                );
            }
            ServerMessage::RoomJoinFailed {
                reason,
                error_code,
            } => {
                eprintln!(
                    "Failed to join: {reason} ({error_code:?})"
                );
            }
            other => {
                eprintln!("Unexpected response: {other:?}");
            }
        }
    }

    Ok(())
}
```

The `game_name` must match the game name used when the room was created.

## Handling Messages

Use a loop over the `SplitStream` to handle incoming server messages. Match on
the `ServerMessage` variants to react to each event.

```rust,ignore
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;

async fn message_loop(
    mut read: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<
                tokio::net::TcpStream,
            >,
        >,
    >,
) {
    while let Some(result) = read.next().await {
        let msg = match result {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<ServerMessage>(
                    &text,
                ) {
                    Ok(msg) => msg,
                    Err(e) => {
                        eprintln!("Parse error: {e}");
                        continue;
                    }
                }
            }
            Ok(Message::Close(_)) => {
                println!("Connection closed by server");
                break;
            }
            Err(e) => {
                eprintln!("WebSocket error: {e}");
                break;
            }
            _ => continue,
        };

        match msg {
            ServerMessage::RoomJoined { room_code, .. } => {
                println!("Joined room: {room_code}");
            }
            ServerMessage::PlayerJoined { player } => {
                println!(
                    "Player joined: {} ({})",
                    player.name, player.id
                );
            }
            ServerMessage::PlayerLeft { player_id } => {
                println!("Player left: {player_id}");
            }
            ServerMessage::GameData {
                from_player, data, ..
            } => {
                println!(
                    "Game data from {from_player}: {data}"
                );
            }
            ServerMessage::LobbyStateChanged {
                lobby_state,
                all_ready,
                ..
            } => {
                println!(
                    "Lobby: {lobby_state} \
                     (all ready: {all_ready})"
                );
            }
            ServerMessage::GameStarting {
                peer_connections,
            } => {
                println!(
                    "Game starting with {} peers",
                    peer_connections.len()
                );
            }
            ServerMessage::Pong => {
                println!("Pong received");
            }
            ServerMessage::Error {
                message,
                error_code,
            } => {
                eprintln!(
                    "Error: {message} ({error_code:?})"
                );
            }
            other => {
                println!("Received: {other:?}");
            }
        }
    }
}
```

For production clients, run the message loop in a separate tokio task so you can
send messages concurrently:

```rust,ignore
let (write, read) = ws_stream.split();
let write = std::sync::Arc::new(
    tokio::sync::Mutex::new(write),
);

// Spawn the reader task
let reader_handle = tokio::spawn(async move {
    message_loop(read).await;
});

// Send messages from the main task using `write`
// ...

reader_handle.await?;
```

## Sending Game Data

The `GameData` message carries an arbitrary JSON payload. Use
`serde_json::json!` for ad-hoc data or serialize your own game structs.

```rust,ignore
use futures_util::SinkExt;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;

/// Send a movement action to all other players in the room.
async fn send_move(
    write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<
                tokio::net::TcpStream,
            >,
        >,
        Message,
    >,
    x: f64,
    y: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let msg = ClientMessage::GameData {
        data: json!({
            "action": "move",
            "x": x,
            "y": y,
            "timestamp": 1234567890
        }),
    };
    let json = serde_json::to_string(&msg)?;
    write.send(Message::Text(json)).await?;
    Ok(())
}
```

When the server relays this to other players, the incoming `ServerMessage::GameData`
includes a `from_player` field identifying the sender:

```json
{
  "type": "GameData",
  "data": {
    "from_player": "550e8400-e29b-41d4-a716-446655440000",
    "data": {
      "action": "move",
      "x": 100.0,
      "y": 200.0,
      "timestamp": 1234567890
    }
  }
}
```

You can also define typed game data structs and serialize them into the
`serde_json::Value`:

```rust,ignore
use serde::Serialize;

#[derive(Serialize)]
struct PlayerMove {
    action: String,
    x: f64,
    y: f64,
    velocity_x: f64,
    velocity_y: f64,
}

let movement = PlayerMove {
    action: "move".to_string(),
    x: 150.0,
    y: 300.0,
    velocity_x: 1.5,
    velocity_y: -0.5,
};

let msg = ClientMessage::GameData {
    data: serde_json::to_value(&movement)?,
};
```

## Ready Up Flow

Players signal readiness by sending `PlayerReady`. The lobby transitions
through three states: `waiting`, `lobby`, and `finalized`.

```rust,ignore
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

async fn ready_up_and_wait(
    write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<
                tokio::net::TcpStream,
            >,
        >,
        Message,
    >,
    read: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<
                tokio::net::TcpStream,
            >,
        >,
    >,
) -> Result<(), Box<dyn std::error::Error>> {
    // Send ready signal
    let ready = ClientMessage::PlayerReady;
    let json = serde_json::to_string(&ready)?;
    write.send(Message::Text(json)).await?;
    println!("Marked as ready");

    // Listen for lobby state transitions
    while let Some(Ok(Message::Text(text))) = read.next().await
    {
        let msg: ServerMessage =
            serde_json::from_str(&text)?;
        match msg {
            ServerMessage::LobbyStateChanged {
                lobby_state,
                all_ready,
                ready_players,
            } => {
                println!(
                    "Lobby state: {lobby_state} \
                     ({}/{} ready)",
                    ready_players.len(),
                    if all_ready { "all" } else { "waiting" }
                );
                if lobby_state == "finalized" {
                    println!("Game has started");
                    break;
                }
            }
            ServerMessage::GameStarting {
                peer_connections,
            } => {
                println!("Game starting");
                for peer in &peer_connections {
                    println!(
                        "  Peer: {} ({})",
                        peer.player_name, peer.player_id
                    );
                }
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
```

The state machine flow:

1. **waiting** -- room is open, waiting for players to fill it
2. **lobby** -- room is full, players are coordinating readiness
3. **finalized** -- all players are ready, game is starting

If a player leaves during `lobby`, the room returns to `waiting`.

## Reconnection Handling

When a player disconnects, the server generates a reconnection token bound
to the player ID and room ID. Store your `player_id` and `room_id` from the
`RoomJoined` response. The `auth_token` for the `Reconnect` message is
provided through the reconnection mechanism at disconnect time.

```rust,ignore
use uuid::Uuid;

/// Credentials stored after joining a room, used for
/// reconnection.
struct ReconnectionCredentials {
    player_id: Uuid,
    room_id: Uuid,
    auth_token: String,
}

/// Attempt to reconnect to a room after losing the
/// connection.
async fn reconnect(
    credentials: &ReconnectionCredentials,
) -> Result<(), Box<dyn std::error::Error>> {
    let url =
        url::Url::parse("ws://localhost:3536/v2/ws")?;
    let (ws_stream, _) =
        tokio_tungstenite::connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Send reconnect message with stored credentials
    let reconnect_msg = ClientMessage::Reconnect {
        player_id: credentials.player_id,
        room_id: credentials.room_id,
        auth_token: credentials.auth_token.clone(),
    };
    let json = serde_json::to_string(&reconnect_msg)?;
    write
        .send(
            tokio_tungstenite::tungstenite::Message::Text(
                json,
            ),
        )
        .await?;

    // Wait for reconnection result
    use futures_util::StreamExt;
    if let Some(Ok(
        tokio_tungstenite::tungstenite::Message::Text(text),
    )) = read.next().await
    {
        let msg: ServerMessage =
            serde_json::from_str(&text)?;
        match msg {
            ServerMessage::Reconnected {
                room_code,
                current_players,
                lobby_state,
                missed_events,
                ..
            } => {
                println!(
                    "Reconnected to room {room_code}"
                );
                println!(
                    "Current players: {}",
                    current_players.len()
                );
                println!(
                    "Lobby state: {lobby_state}"
                );
                println!(
                    "Missed events: {}",
                    missed_events.len()
                );
                // Process missed events to catch up
                for event in &missed_events {
                    println!(
                        "  Missed: {event:?}"
                    );
                }
            }
            ServerMessage::ReconnectionFailed {
                reason,
                error_code,
            } => {
                eprintln!(
                    "Reconnection failed: \
                     {reason} ({error_code})"
                );
                // Fall back to joining as a new player
            }
            other => {
                eprintln!(
                    "Unexpected: {other:?}"
                );
            }
        }
    }

    Ok(())
}
```

The reconnection window is configurable on the server (default: 300 seconds).
After the window expires, you must join as a new player.

## Spectator Mode

Spectators observe a room without participating. They receive all game data
but cannot mark ready or send game data.

```rust,ignore
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = Url::parse("ws://localhost:3536/v2/ws")?;
    let (ws_stream, _) = connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Join as a spectator
    let spectate = ClientMessage::JoinAsSpectator {
        game_name: "my-game".to_string(),
        room_code: "ABC123".to_string(),
        spectator_name: "Observer1".to_string(),
    };
    let json = serde_json::to_string(&spectate)?;
    write.send(Message::Text(json)).await?;

    // Wait for confirmation
    if let Some(Ok(Message::Text(text))) = read.next().await {
        let msg: ServerMessage =
            serde_json::from_str(&text)?;
        match msg {
            ServerMessage::SpectatorJoined {
                room_code,
                current_players,
                lobby_state,
                ..
            } => {
                println!(
                    "Spectating room: {room_code}"
                );
                println!(
                    "Players: {}",
                    current_players.len()
                );
                println!(
                    "Lobby state: {lobby_state}"
                );
            }
            ServerMessage::SpectatorJoinFailed {
                reason,
                ..
            } => {
                eprintln!(
                    "Failed to spectate: {reason}"
                );
                return Ok(());
            }
            other => {
                eprintln!(
                    "Unexpected: {other:?}"
                );
            }
        }
    }

    // Watch game data as a spectator
    while let Some(Ok(Message::Text(text))) =
        read.next().await
    {
        if let Ok(msg) =
            serde_json::from_str::<ServerMessage>(&text)
        {
            match msg {
                ServerMessage::GameData {
                    from_player,
                    data,
                } => {
                    println!(
                        "[spectator] {from_player}: \
                         {data}"
                    );
                }
                ServerMessage::PlayerJoined {
                    player,
                } => {
                    println!(
                        "[spectator] Player joined: \
                         {}",
                        player.name
                    );
                }
                ServerMessage::PlayerLeft {
                    player_id,
                } => {
                    println!(
                        "[spectator] Player left: \
                         {player_id}"
                    );
                }
                _ => {}
            }
        }
    }

    Ok(())
}
```

To stop spectating, send `LeaveSpectator`:

```rust,ignore
let leave = ClientMessage::LeaveSpectator;
let json = serde_json::to_string(&leave)?;
write
    .send(
        tokio_tungstenite::tungstenite::Message::Text(json),
    )
    .await?;
```

## Error Handling

The server sends structured errors with an optional `error_code` field. Handle
common error codes to provide a good player experience.

### Common Error Codes

| Error Code | Meaning | Recommended Action |
|---|---|---|
| `ROOM_FULL` | Room reached max players | Show "room full" to the player |
| `ROOM_NOT_FOUND` | Room code does not exist | Prompt the player to check the code |
| `RATE_LIMIT_EXCEEDED` | Too many requests | Back off and retry after a delay |
| `AUTHENTICATION_REQUIRED` | Server requires authentication | Send `Authenticate` first |
| `INVALID_APP_ID` | Bad app ID | Check your app configuration |
| `ALREADY_IN_ROOM` | Player is already in a room | Leave the current room first |
| `NOT_IN_ROOM` | Action requires being in a room | Join a room before this action |
| `INVALID_GAME_NAME` | Game name validation failed | Check name length and characters |
| `INVALID_PLAYER_NAME` | Player name validation failed | Check name length and characters |
| `RECONNECTION_EXPIRED` | Reconnection window elapsed | Join as a new player |
| `RECONNECTION_TOKEN_INVALID` | Bad or expired token | Join as a new player |

### Handling Errors in Code

```rust,ignore
fn handle_server_message(msg: &ServerMessage) {
    match msg {
        ServerMessage::Error {
            message,
            error_code,
        } => {
            match error_code.as_deref() {
                Some("ROOM_FULL") => {
                    eprintln!("Room is full: {message}");
                    // Show UI to pick another room
                }
                Some("ROOM_NOT_FOUND") => {
                    eprintln!("Room not found: {message}");
                    // Prompt user to re-enter code
                }
                Some("RATE_LIMIT_EXCEEDED") => {
                    eprintln!(
                        "Rate limited: {message}"
                    );
                    // Wait and retry
                }
                Some("AUTHENTICATION_REQUIRED") => {
                    eprintln!(
                        "Auth required: {message}"
                    );
                    // Send Authenticate message
                }
                Some(code) => {
                    eprintln!(
                        "Error [{code}]: {message}"
                    );
                }
                None => {
                    eprintln!("Error: {message}");
                }
            }
        }
        ServerMessage::RoomJoinFailed {
            reason,
            error_code,
        } => {
            eprintln!(
                "Join failed: {reason} \
                 (code: {error_code:?})"
            );
        }
        ServerMessage::ReconnectionFailed {
            reason,
            error_code,
        } => {
            eprintln!(
                "Reconnection failed: {reason} \
                 ({error_code})"
            );
        }
        _ => {}
    }
}
```

## Complete Example

A full client struct that wraps all operations into a reusable API.

```rust,ignore
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{
    connect_async, MaybeTlsStream, WebSocketStream,
};
use url::Url;
use uuid::Uuid;

// --- Type aliases ---

pub type PlayerId = Uuid;
pub type RoomId = Uuid;
type WsStream =
    WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

// --- Protocol types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    Authenticate {
        app_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sdk_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        platform: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        game_data_format: Option<String>,
    },
    JoinRoom {
        game_name: String,
        player_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        room_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_players: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        supports_authority: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        relay_transport: Option<String>,
    },
    LeaveRoom,
    GameData {
        data: serde_json::Value,
    },
    PlayerReady,
    AuthorityRequest {
        become_authority: bool,
    },
    ProvideConnectionInfo {
        connection_info: serde_json::Value,
    },
    Ping,
    Reconnect {
        player_id: PlayerId,
        room_id: RoomId,
        auth_token: String,
    },
    JoinAsSpectator {
        game_name: String,
        room_code: String,
        spectator_name: String,
    },
    LeaveSpectator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    pub is_authority: bool,
    pub is_ready: bool,
    pub connected_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectatorInfo {
    pub id: PlayerId,
    pub name: String,
    pub connected_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConnectionInfo {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub relay_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub per_minute: u32,
    pub per_hour: u32,
    pub per_day: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    Authenticated {
        app_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        organization: Option<String>,
        rate_limits: RateLimitInfo,
    },
    AuthenticationError {
        error: String,
        error_code: String,
    },
    RoomJoined {
        room_id: RoomId,
        room_code: String,
        player_id: PlayerId,
        game_name: String,
        max_players: u8,
        supports_authority: bool,
        current_players: Vec<PlayerInfo>,
        is_authority: bool,
        lobby_state: String,
        ready_players: Vec<PlayerId>,
        relay_type: String,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    RoomJoinFailed {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
    RoomLeft,
    PlayerJoined {
        player: PlayerInfo,
    },
    PlayerLeft {
        player_id: PlayerId,
    },
    GameData {
        from_player: PlayerId,
        data: serde_json::Value,
    },
    LobbyStateChanged {
        lobby_state: String,
        ready_players: Vec<PlayerId>,
        all_ready: bool,
    },
    GameStarting {
        peer_connections: Vec<PeerConnectionInfo>,
    },
    AuthorityChanged {
        authority_player: Option<PlayerId>,
        you_are_authority: bool,
    },
    AuthorityResponse {
        granted: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
    Pong,
    Reconnected {
        room_id: RoomId,
        room_code: String,
        player_id: PlayerId,
        game_name: String,
        max_players: u8,
        supports_authority: bool,
        current_players: Vec<PlayerInfo>,
        is_authority: bool,
        lobby_state: String,
        ready_players: Vec<PlayerId>,
        relay_type: String,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
        missed_events: Vec<ServerMessage>,
    },
    ReconnectionFailed {
        reason: String,
        error_code: String,
    },
    PlayerReconnected {
        player_id: PlayerId,
    },
    SpectatorJoined {
        room_id: RoomId,
        room_code: String,
        spectator_id: PlayerId,
        game_name: String,
        current_players: Vec<PlayerInfo>,
        current_spectators: Vec<SpectatorInfo>,
        lobby_state: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    SpectatorJoinFailed {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
    SpectatorLeft {
        #[serde(skip_serializing_if = "Option::is_none")]
        room_id: Option<RoomId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        room_code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    NewSpectatorJoined {
        spectator: SpectatorInfo,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    SpectatorDisconnected {
        spectator_id: PlayerId,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default)]
        current_spectators: Vec<SpectatorInfo>,
    },
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_code: Option<String>,
    },
}

// --- Client ---

/// Signal Fish client wrapping a WebSocket connection.
pub struct SignalFishClient {
    write: Arc<Mutex<WsSink>>,
    player_id: Option<PlayerId>,
    room_id: Option<RoomId>,
    room_code: Option<String>,
}

impl SignalFishClient {
    /// Connect to a Signal Fish Server instance.
    pub async fn connect(
        server_url: &str,
    ) -> Result<(Self, WsSource), Box<dyn std::error::Error>>
    {
        let url = Url::parse(server_url)?;
        let (ws_stream, _) = connect_async(url).await?;
        let (write, read) = ws_stream.split();
        let client = Self {
            write: Arc::new(Mutex::new(write)),
            player_id: None,
            room_id: None,
            room_code: None,
        };
        Ok((client, read))
    }

    /// Send a client message over the WebSocket.
    async fn send(
        &self,
        msg: &ClientMessage,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_string(msg)?;
        self.write
            .lock()
            .await
            .send(Message::Text(json))
            .await?;
        Ok(())
    }

    /// Create a new room.
    pub async fn create_room(
        &self,
        game_name: &str,
        player_name: &str,
        max_players: u8,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send(&ClientMessage::JoinRoom {
            game_name: game_name.to_string(),
            player_name: player_name.to_string(),
            room_code: None,
            max_players: Some(max_players),
            supports_authority: Some(true),
            relay_transport: None,
        })
        .await
    }

    /// Join an existing room by code.
    pub async fn join_room(
        &self,
        game_name: &str,
        player_name: &str,
        room_code: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send(&ClientMessage::JoinRoom {
            game_name: game_name.to_string(),
            player_name: player_name.to_string(),
            room_code: Some(room_code.to_string()),
            max_players: None,
            supports_authority: None,
            relay_transport: None,
        })
        .await
    }

    /// Leave the current room.
    pub async fn leave_room(
        &self,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send(&ClientMessage::LeaveRoom).await
    }

    /// Send game data to other players.
    pub async fn send_game_data(
        &self,
        data: serde_json::Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send(&ClientMessage::GameData { data }).await
    }

    /// Signal readiness to start the game.
    pub async fn ready_up(
        &self,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send(&ClientMessage::PlayerReady).await
    }

    /// Request or release authority.
    pub async fn request_authority(
        &self,
        become_authority: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send(&ClientMessage::AuthorityRequest {
            become_authority,
        })
        .await
    }

    /// Send a heartbeat ping.
    pub async fn ping(
        &self,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send(&ClientMessage::Ping).await
    }

    /// Reconnect using stored credentials.
    pub async fn reconnect(
        &self,
        auth_token: String,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let player_id = self.player_id.ok_or(
            "No player_id stored for reconnection",
        )?;
        let room_id = self
            .room_id
            .ok_or("No room_id stored for reconnection")?;
        self.send(&ClientMessage::Reconnect {
            player_id,
            room_id,
            auth_token,
        })
        .await
    }

    /// Join a room as a spectator.
    pub async fn spectate(
        &self,
        game_name: &str,
        room_code: &str,
        spectator_name: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send(&ClientMessage::JoinAsSpectator {
            game_name: game_name.to_string(),
            room_code: room_code.to_string(),
            spectator_name: spectator_name.to_string(),
        })
        .await
    }

    /// Stop spectating.
    pub async fn leave_spectator(
        &self,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.send(&ClientMessage::LeaveSpectator).await
    }

    /// Update stored credentials from a RoomJoined
    /// response.
    pub fn on_room_joined(
        &mut self,
        player_id: PlayerId,
        room_id: RoomId,
        room_code: String,
    ) {
        self.player_id = Some(player_id);
        self.room_id = Some(room_id);
        self.room_code = Some(room_code);
    }
}

// --- Main ---

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut client, mut read) =
        SignalFishClient::connect(
            "ws://localhost:3536/v2/ws",
        )
        .await?;
    println!("Connected to Signal Fish Server");

    // Create a room
    client
        .create_room("my-game", "Player1", 4)
        .await?;

    // Process messages
    while let Some(result) = read.next().await {
        let text = match result {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) => break,
            Err(e) => {
                eprintln!("Error: {e}");
                break;
            }
            _ => continue,
        };

        let msg: ServerMessage =
            match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("Parse error: {e}");
                    continue;
                }
            };

        match msg {
            ServerMessage::RoomJoined {
                room_id,
                room_code,
                player_id,
                ..
            } => {
                println!(
                    "Room created: {room_code}"
                );
                client.on_room_joined(
                    player_id, room_id,
                    room_code,
                );
            }
            ServerMessage::PlayerJoined { player } => {
                println!(
                    "Player joined: {}",
                    player.name
                );
                // Ready up after another player joins
                client.ready_up().await?;
            }
            ServerMessage::GameStarting { .. } => {
                println!("Game starting");
                // Send initial game state
                client
                    .send_game_data(json!({
                        "action": "spawn",
                        "x": 0,
                        "y": 0
                    }))
                    .await?;
            }
            ServerMessage::GameData {
                from_player,
                data,
            } => {
                println!(
                    "Data from {from_player}: {data}"
                );
            }
            ServerMessage::Error {
                message,
                error_code,
            } => {
                eprintln!(
                    "Error: {message} \
                     ({error_code:?})"
                );
            }
            other => {
                println!("Received: {other:?}");
            }
        }
    }

    Ok(())
}
```

## Authentication (Optional)

When the server has `require_websocket_auth` enabled, you must send an
`Authenticate` message as the very first message after connecting. The server
will close the connection if authentication is not received within the
configured timeout (default: 10 seconds).

```rust,ignore
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = Url::parse("ws://localhost:3536/v2/ws")?;
    let (ws_stream, _) = connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Authenticate immediately after connecting
    let auth = ClientMessage::Authenticate {
        app_id: "my-game".to_string(),
        sdk_version: Some("1.0.0".to_string()),
        platform: Some("rust".to_string()),
        game_data_format: None,
    };
    let json = serde_json::to_string(&auth)?;
    write.send(Message::Text(json)).await?;

    // Wait for authentication response
    if let Some(Ok(Message::Text(text))) = read.next().await {
        let msg: ServerMessage =
            serde_json::from_str(&text)?;
        match msg {
            ServerMessage::Authenticated {
                app_name,
                rate_limits,
                ..
            } => {
                println!(
                    "Authenticated as: {app_name}"
                );
                println!(
                    "Rate limits: {}/min, {}/hr, {}/day",
                    rate_limits.per_minute,
                    rate_limits.per_hour,
                    rate_limits.per_day
                );
                // Now safe to join/create rooms
            }
            ServerMessage::AuthenticationError {
                error,
                error_code,
            } => {
                eprintln!(
                    "Auth failed: {error} \
                     ({error_code})"
                );
                return Err(error.into());
            }
            other => {
                eprintln!(
                    "Unexpected: {other:?}"
                );
            }
        }
    }

    Ok(())
}
```

The `app_id` is a public identifier, not a secret. It is safe to embed in
game builds. The server matches it against the `authorized_apps` list in the
server configuration.

## Next Steps

- [Protocol Reference](../protocol.md) -- complete message documentation
- [Features](../features.md) -- full feature overview
- [Authentication](../authentication.md) -- server-side auth configuration
