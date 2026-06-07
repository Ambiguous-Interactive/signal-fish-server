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
       |  2. Exchange legacy peer metadata        |
       +<-----------------------------------------+
       |                                          |
       |  3. Establish negotiated data path       |
       +<========================================>+
       |       (game traffic flows here)          |
```

1. Both clients connect to Signal Fish over WebSocket.
2. Signal Fish coordinates room creation, matchmaking, and relays each player's
   legacy peer metadata through `GameStarting`.
3. Protocol v3 clients use the negotiated `SessionPlan` for topology,
   transport, peers, relay fallback, and ICE only when the selected transport is
   WebRTC; v2 clients keep using the legacy `GameStarting` handoff.

## What Signal Fish Does

- **Room-based matchmaking** -- Players create rooms with auto-generated
  shareable codes. Others join with the code.
- **Lobby system with ready-up flow** -- Rooms transition through Waiting,
  Lobby, and Finalized states so everyone can signal readiness before the
  game starts.
- **Session handoff** -- v2 clients receive legacy, self-declared
  `GameStarting` metadata, while negotiated v3 clients use `SessionPlan` for
  topology, transport, peers, relay fallback, and WebRTC ICE when applicable.
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

The WebSocket endpoints are served at `/v2/ws` and `/v3/ws`. Both use the same
protocol; the `/v3/ws` alias defaults omitted `protocol_version` values to 3,
while `/v2/ws` defaults omitted versions to 2. Clients connect, optionally
authenticate, then create or join rooms.

For the complete list of client and server message types, see the
[Protocol](../protocol.md). For server configuration options, see
the [Configuration](../configuration.md).

## Next Steps

- [Rooms and Lobbies](rooms-and-lobbies.md) -- How rooms are created,
  filled, and readied up
- [Authority System](authority.md) -- Designating an authoritative host
- [Reconnection](reconnection.md) -- Handling dropped connections
- [Spectator Mode](spectator-mode.md) -- Adding read-only observers
