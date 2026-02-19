# What is Signal Fish?

Signal Fish Server is a lightweight WebSocket signaling server that helps
multiplayer game clients find each other and establish peer-to-peer (P2P)
connections. It is built in Rust, runs entirely in memory, and ships as a
single binary with zero external runtime dependencies.

## The Problem

You are building a multiplayer game. Two (or more) players want to play
together, but they have no way to discover each other on the internet. Even
if they know each other exists, they still need to exchange connection
details -- IP addresses, ports, relay tokens -- before they can talk
directly.

This "finding each other" step is called **signaling**, and every P2P
multiplayer game needs it.

## How Signaling Works

Signal Fish sits between your game clients during the connection setup
phase. Once players have exchanged the information they need, they talk
directly to each other and the signaling server steps out of the way.

```text
 +-----------+                              +-----------+
 | Client A  |                              | Client B  |
 | (Player)  |                              | (Player)  |
 +-----+-----+                              +-----+-----+
       |                                          |
       |  1. Connect via WebSocket                |
       +----------> +------------------+ <--------+
                    | Signal Fish      |
                    | Server           |
                    | (coordination)   |
                    +------------------+
       |                                          |
       |  2. Exchange connection info             |
       +<-----------------------------------------+
       |                                          |
       |  3. Establish direct P2P connection      |
       +<========================================>+
       |       (game traffic flows here)          |
```

1. Both clients connect to Signal Fish over WebSocket.
2. Signal Fish coordinates room creation, matchmaking, and relays each
   player's connection details to the other.
3. Players use those details to establish a direct connection (WebRTC, UDP,
   TCP, or a relay) and start playing.

## What Signal Fish Does

- **Room-based matchmaking** -- Players create rooms with auto-generated
  shareable codes. Others join with the code.
- **Lobby system with ready-up flow** -- Rooms transition through Waiting,
  Lobby, and Finalized states so everyone can signal readiness before the
  game starts.
- **Connection info relay** -- Each player provides their connection
  details, and Signal Fish distributes them to all peers when the game
  starts.
- **Authority system** -- Optionally designate one player as the
  authoritative host for game logic decisions.
- **Spectator mode** -- Read-only observers can watch a room without
  affecting gameplay.
- **Reconnection** -- If a player drops, they can reconnect within a
  configurable window and receive all missed events.

## What Signal Fish Does NOT Do

Setting expectations early saves headaches later:

- **Not a game server** -- Signal Fish does not run game logic, physics, or
  scoring. It coordinates connections; your game clients (or an
  authoritative host) handle everything else.
- **Not a production relay for gameplay data** -- The `GameData` message
  type exists for convenience during development and prototyping, but in
  production you should establish direct P2P or relay connections for game
  traffic.
- **Not a STUN/TURN server** -- Signal Fish does not perform NAT traversal.
  You still need a STUN/TURN server (or a service like Unity Relay) if your
  players are behind NATs.

## Protocol Overview

Signal Fish uses a JSON-based protocol over WebSocket. Every message is a
JSON object with a `type` field and an optional `data` field.

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Alice"
  }
}
```

The WebSocket endpoint is served at `/v2/ws`. Clients connect, optionally
authenticate, then create or join rooms.

For the complete list of client and server message types, see the
[Protocol Reference](../protocol.md). For server configuration options, see
the [Configuration Guide](../configuration.md).

## Next Steps

- [Rooms and Lobbies](rooms-and-lobbies.md) -- How rooms are created,
  filled, and readied up
- [Authority System](authority.md) -- Designating an authoritative host
- [Reconnection](reconnection.md) -- Handling dropped connections
- [Spectator Mode](spectator-mode.md) -- Adding read-only observers
