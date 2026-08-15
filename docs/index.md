<img class="sf-home-logo" src="assets/logo-banner.svg" alt="Signal Fish Server">

**Signaling for peer-to-peer multiplayer**{ .sf-hero-tag }

# Signal Fish Server

Signal Fish Server helps players find and connect to each other. A player creates
a room, shares its short code, and the other players join over WebSocket.

It runs as one small, in-memory server. You do not need a database, message
broker, or other service.

[Start in five minutes](quickstart.md){ .md-button .md-button--primary .sf-home-action }
[View on GitHub](https://github.com/Ambiguous-Interactive/signal-fish-server){ .md-button .sf-home-action }

## How It Fits Into a Game

1. Your game clients connect to Signal Fish Server.
2. One player creates a room and shares the room code.
3. Other players join, ready up, and start the game.
4. Clients exchange game data through the relay or use the connection plan to
   establish peer-to-peer links.

Rooms live in memory and disappear when the server stops.

## Where to Go Next

- [Run the quick start](quickstart.md) to start a local server and join two
  players.
- [Build a client](guides/building-a-client.md) to add the flow to your game.
- [Read the protocol reference](protocol.md) for every message and field.
- [Configure and deploy the server](configuration.md) when you are ready to
  move beyond local development.
