# Rooms and Lobbies

Rooms are the core coordination unit in Signal Fish Server. A room
groups players together for a single game session, manages a lobby where
everyone signals readiness, and distributes legacy peer metadata when the game
starts.

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
    "supports_authority": true,
    "current_players": [
      {
        "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "name": "Alice",
        "is_authority": true,
        "is_ready": false,
        "connected_at": "2025-01-15T10:30:00Z"
      }
    ],
    "is_authority": true,
    "lobby_state": "waiting",
    "ready_players": [],
    "relay_type": "matchbox",
    "current_spectators": []
  }
}
```

Alice now shares the room code `HK7T3W` with other players (via your
game's UI, a chat message, or any other channel).

`relay_type` is a deployment-defined legacy integration label. It is
informational only and does not select or prove the active transport; use the
authenticated WebSocket relay path or a negotiated v3 `SessionPlan` for
executable routing.

## `JoinRoom` Resolution Rules

`JoinRoom` is used for both creation and joining. The server resolves each
request using the `(game_name, room_code)` pair:

| `JoinRoom` payload | Server behavior | Result |
| --- | --- | --- |
| `room_code` omitted | Implicit room creation | New room + generated code in `RoomJoined` |
| `room_code` provided and room exists for `game_name` | Join existing room | `RoomJoined` + `PlayerJoined` broadcast to others |
| `room_code` provided and room does not exist for `game_name` | Explicit room creation | New room using requested `room_code` |

Creation-specific fields such as `max_players` and `supports_authority`
apply only when a room is created (implicit or explicit). When joining an
existing room, current room settings remain unchanged.

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

This leaves 32 unambiguous characters: `2-9` and `A-H, J-N, P-Z`.

The code length is configurable via the `room_code_length` protocol setting
(default: 6) and must be greater than zero. An optional
`server.room_code_prefix` is normalized to uppercase and prepended to generated
codes. Prefixes must be nonblank ASCII alphanumeric strings shorter than the
total code length; startup rejects any configuration that could generate a code
the join validator would refuse.

Automatic room creation tries at most eight independently generated candidates.
An occupied candidate is never treated as a request to join that room; the
server generates another candidate instead. All internal attempts count as one
client room-creation operation for rate limiting. Providing `room_code` retains
the explicit join/create behavior described above.

Let `s` be the random suffix length after the normalized prefix. The generator
has `32^s` possible suffixes. With `r` occupied codes for one game, a candidate's
collision probability is `r / 32^s`, and the probability that all eight uniform
candidates collide is `(r / 32^s)^8`. Keep expected active rooms below 1% of the
suffix namespace so collision retries remain exceptional. For example, four
random characters provide 1,048,576 suffixes; a two-character prefix with the
default six-character total length therefore comfortably supports the default
1,000-room per-game cap. Monitor `race_conditions.room_code_collisions` and the
dedicated `room_code_retry_operations`, `room_code_retry_successes`,
`room_code_retry_exhaustions`, and `room_code_retry_success_rate` fields when
selecting a longer prefix or shorter total length.

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
      |  Waiting  | <--- Empty room, waiting for the first player
      +-----------+
             |
             | First player joins
             v
      +-----------+
      |   Lobby   | <--- Players toggle ready/unready via PlayerReady
      +-----------+      (max_players is a ceiling; room need not be full)
             |
             | Every current player ready, a member sends StartGame
             v
      +-----------+
      | Finalized | ---> GameStarting sent to all players
      +-----------+
```

A player leaving during the lobby no longer regresses a partial room to
Waiting; the remaining members stay in the lobby.

### Waiting

The initial state. The room exists but is empty, awaiting its first player.

- No players are present yet.
- No ready-state tracking happens in this state.
- The room stays here until a player joins or it expires from inactivity.

### Lobby

Players are present and can coordinate readiness. `max_players` is a
**ceiling**, not a required count — the room enters the lobby with one or
more players and need not be full to start.

- Players send `PlayerReady` to toggle their ready/unready status.
- Each readiness change triggers a `LobbyStateChanged` broadcast so
  everyone sees who is ready; `all_ready` is set once every current player
  is ready.
- Readiness **does not** start the game on its own. Once every current
  player is ready, a member finalizes the lobby with an explicit
  [`StartGame`](../protocol.md#startgame).
- Readiness belongs to a **membership**: a player who leaves and joins the
  room again starts unready and must send `PlayerReady` again.
- A member who **reconnects** resumes the same membership, so it is not forced
  unready — but its lobby readiness can be dropped while it is away (any other
  member's `PlayerReady` prunes absent members from the ready set). Treat the
  `Reconnected` snapshot's `ready_players` / `is_ready` as authoritative rather
  than assuming the pre-disconnect state survived. A member that was in the room
  when the game **started** is still reported ready when it reconnects into that
  running game; a member that joins or returns to a room that started without it
  is reported unready, exactly like any other seat-filler.
- More players may keep joining until the room reaches `max_players`.

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

A member has sent [`StartGame`](../protocol.md#startgame) while every current
player was ready. The server finalizes the game:

`StartGame` is accepted only when every current player is ready
(`GAME_START_NOT_READY` otherwise) and the sender is permitted to start — the
room's authority player if it has one, else any member (`GAME_START_FORBIDDEN`
otherwise). On finalization the server:

- Records a finalization timestamp.
- Sends a `GameStarting` message to every player with legacy peer metadata.
- For every negotiated-v3 member, follows with a per-recipient `SessionPlan`
  that describes topology, transport, peers, relay fallback, and ICE only when
  the selected transport is WebRTC. The relay floor is an explicit no-peer
  `relay`/`relay` plan; v2 members receive no plan.

```json
{
  "type": "GameStarting",
  "data": {
    "peer_connections": [
      {
        "player_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
        "player_name": "Alice",
        "is_authority": true,
        "relay_type": "matchbox"
      },
      {
        "player_id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
        "player_name": "Bob",
        "is_authority": false,
        "relay_type": "matchbox"
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
     |-- StartGame ---------->|                       |
     |<-- GameStarting -------|--- GameStarting ----->|
     |   (legacy metadata)    |   (legacy metadata)   |
```

The move into `finalized` is signaled by `GameStarting` itself — the server
never broadcasts a `LobbyStateChanged` carrying `finalized`. The `finalized`
value appears only in the `lobby_state` field of a later `RoomJoined` /
`Reconnected` snapshot.

Once both players are ready, either member may send `StartGame` (this room has
no claimed authority, so any member may start). The game does not start until
that explicit message arrives.

## Single-Player and Solo Starts

`max_players` is a ceiling, so a room can start with fewer players than its
limit — including a single player. The lone member sends `PlayerReady` and
then `StartGame` to finalize a solo session and receive legacy `GameStarting`
peer metadata. A single ready member is always permitted to start (it is the
only member, so the "any member" authorization rule applies when no authority
is set).

## Configuration

Key room-related configuration options:

- `max_players` -- Maximum players per room. Default: `8`, maximum: `100`.
  Set per room at creation time via the `JoinRoom` message.
- `room_code_length` -- Nonzero length of generated room codes. Default: `6`.
- `room_code_prefix` -- Optional ASCII-alphanumeric prefix, shorter than the
  total code length.
- `room_cleanup_interval` -- Seconds between cleanup sweeps. Default: `60`.
- `empty_room_timeout` -- Seconds before an empty room is removed.
  Default: `300`.
- `inactive_room_timeout` -- Seconds before an inactive room is removed.
  Default: `3600`.

For the full list of server configuration options, see the
[Configuration](../configuration.md).

## Next Steps

- [Authority System](authority.md) -- Designating one player as the game
  host
- [Reconnection](reconnection.md) -- Handling dropped connections during a
  session
- [Spectator Mode](spectator-mode.md) -- Adding read-only observers to
  rooms
