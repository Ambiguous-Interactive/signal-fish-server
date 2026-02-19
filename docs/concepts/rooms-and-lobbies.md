# Rooms and Lobbies

Rooms are the core coordination unit in Signal Fish Server. A room
groups players together for a single game session, manages a lobby where
everyone signals readiness, and distributes peer connection information
when the game starts.

## Creating a Room

A room is created when a player sends a `JoinRoom` message **without** a
`room_code`. The server generates a unique room code and returns it in the
`RoomJoined` response.

**Client sends:**

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Alice",
    "max_players": 4
  }
}
```

**Server responds:**

```json
{
  "type": "RoomJoined",
  "data": {
    "room_id": "550e8400-e29b-41d4-a716-446655440000",
    "room_code": "HK7T3W",
    "player_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "game_name": "my-game",
    "max_players": 4,
    "supports_authority": false,
    "current_players": [
      {
        "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "name": "Alice",
        "is_authority": false,
        "is_ready": false,
        "connected_at": "2025-01-15T10:30:00Z"
      }
    ],
    "is_authority": false,
    "lobby_state": "waiting",
    "ready_players": [],
    "relay_type": "WebRTC",
    "current_spectators": []
  }
}
```

Alice now shares the room code `HK7T3W` with other players (via your
game's UI, a chat message, or any other channel).

## Joining an Existing Room

Other players join by including the `room_code` in their `JoinRoom`
message:

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "room_code": "HK7T3W",
    "player_name": "Bob"
  }
}
```

Bob receives a `RoomJoined` response with the current room state. All
existing players in the room receive a `PlayerJoined` broadcast:

```json
{
  "type": "PlayerJoined",
  "data": {
    "player": {
      "id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
      "name": "Bob",
      "is_authority": false,
      "is_ready": false,
      "connected_at": "2025-01-15T10:31:00Z"
    }
  }
}
```

## Room Codes

Room codes are auto-generated 6-character alphanumeric strings designed to
be easy to read aloud and type. The "clean" code generator excludes
characters that are visually confusing:

- `0` (zero) and `O` (letter O)
- `1` (one) and `I` (letter I)

This leaves 30 unambiguous characters: `2-9` and `A-H, J-N, P-Z`.

The code length is configurable via the `room_code_length` protocol setting
(default: 6).

## Room Lifecycle

Every room moves through a simple state machine. The following diagram
shows the three states and the transitions between them.

```text
+------------------------------------------+
|            ROOM LIFECYCLE                 |
+------------------------------------------+

  Room created (JoinRoom, no code)
             |
             v
      +-----------+
      |  Waiting  | <--- Players joining, fewer than max_players
      +-----------+
             |
             | Room fills up (players == max_players)
             v
      +-----------+
      |   Lobby   | <--- Players mark ready via PlayerReady
      +-----------+
         |     |
         |     | All players ready
         |     v
         |  +-----------+
         |  | Finalized | ---> GameStarting sent to all players
         |  +-----------+
         |
         | Player leaves (drops below max_players)
         | Ready states cleared
         v
      +-----------+
      |  Waiting  |
      +-----------+
```

### Waiting

The initial state. The room is open and accepting players.

- Player count is below `max_players`.
- No ready-state tracking happens in this state.
- The room stays here until it fills up or expires from inactivity.

### Lobby

The room is full. All `max_players` slots are occupied and players can
coordinate readiness.

- Players send `PlayerReady` to mark themselves as ready.
- Each readiness change triggers a `LobbyStateChanged` broadcast so
  everyone sees who is ready.
- If a player **leaves** during the Lobby state, the room reverts to
  Waiting and **all ready states are cleared**.
- New players cannot join (the room is full).

**LobbyStateChanged example (one player ready):**

```json
{
  "type": "LobbyStateChanged",
  "data": {
    "lobby_state": "lobby",
    "ready_players": [
      "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
    ],
    "all_ready": false
  }
}
```

### Finalized

All players in the lobby have marked ready. The server finalizes the game:

- Records a finalization timestamp.
- Sends a `GameStarting` message to every player with peer connection
  information so clients can establish direct connections.

```json
{
  "type": "GameStarting",
  "data": {
    "peer_connections": [
      {
        "player_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "player_name": "Alice",
        "is_authority": false,
        "relay_type": "WebRTC"
      },
      {
        "player_id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
        "player_name": "Bob",
        "is_authority": false,
        "relay_type": "WebRTC"
      }
    ]
  }
}
```

After finalization, the room is typically cleaned up by the server.

## Message Flow Example (2 Players)

Here is the full message sequence for a two-player game from room creation
to game start:

```text
Alice (Client)             Server              Bob (Client)
     |                        |                       |
     |-- JoinRoom (create) -->|                       |
     |<-- RoomJoined ---------|                       |
     |   (code: HK7T3W)      |                       |
     |                        |                       |
     |                        |<-- JoinRoom (join) ---|
     |<-- PlayerJoined -------|--- RoomJoined ------->|
     |<-- LobbyStateChanged --|-- LobbyStateChanged ->|
     |   (state: lobby)       |   (state: lobby)      |
     |                        |                       |
     |-- PlayerReady -------->|                       |
     |<-- LobbyStateChanged --|-- LobbyStateChanged ->|
     |                        |                       |
     |                        |<-- PlayerReady -------|
     |<-- LobbyStateChanged --|-- LobbyStateChanged ->|
     |   (all_ready: true)    |   (all_ready: true)   |
     |                        |                       |
     |<-- GameStarting -------|--- GameStarting ----->|
     |   (peer connections)   |   (peer connections)  |
```

## Single-Player Rooms

If `max_players` is set to `1`, the room does **not** enter the Lobby
state. The single player receives connection information immediately
without needing to go through the ready-up flow.

## Configuration

Key room-related configuration options:

- `max_players` -- Maximum players per room. Default: `8`, maximum: `100`.
  Set per room at creation time via the `JoinRoom` message.
- `room_code_length` -- Length of generated room codes. Default: `6`.
- `room_cleanup_interval` -- Seconds between cleanup sweeps. Default: `60`.
- `empty_room_timeout` -- Seconds before an empty room is removed.
  Default: `300`.
- `inactive_room_timeout` -- Seconds before an inactive room is removed.
  Default: `3600`.

For the full list of server configuration options, see the
[Configuration Guide](../configuration.md).

## Next Steps

- [Authority System](authority.md) -- Designating one player as the game
  host
- [Reconnection](reconnection.md) -- Handling dropped connections during a
  session
- [Spectator Mode](spectator-mode.md) -- Adding read-only observers to
  rooms
