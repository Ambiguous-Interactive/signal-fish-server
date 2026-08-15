# What is Signal Fish?

Signal Fish Server helps multiplayer game clients find each other and start a
session. You run one small server, and each game client connects to it over a
WebSocket. No database or message broker is required.

## What players experience

1. One player creates a room and gets a short code.
2. Other players use that code to join.
3. Players ready up, and an allowed player starts the game.
4. Clients exchange game messages through the server. Advanced clients can
   also ask the server to help set up peer-to-peer connections.

The server keeps rooms, readiness, reconnect information, and messages waiting
to be delivered in memory. Restarting it removes that live state.

## What it handles

- Creating and joining rooms
- Lobby readiness and starting a game
- Choosing an authority player when your game needs one
- Spectators and reconnecting players
- Relaying game messages over WebSocket
- Helping compatible clients set up direct or WebRTC connections

Signal Fish does not run your simulation, physics, scoring, or permanent game
storage. Your game remains responsible for those. If you use WebRTC, you also
provide any STUN or TURN service your players need; Signal Fish only tells
clients how to reach it.

## Choose your next step

- [Run the Quick Start](../quickstart.md) to create and join a room locally.
- [Build a Client](../guides/building-a-client.md) to implement the required
  message flow in your game.
- [Choose v2 or v3](protocol-versions.md) when deciding between the simplest
  relay client and optional peer-to-peer features.
- [Configure and deploy](../configuration.md) when you are ready to run a
  server for players.
