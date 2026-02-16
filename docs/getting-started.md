# Getting Started

Quick guide to get Signal Fish Server running and connect your first client.

## Installation

### Using Rust

```bash
cargo run
```

The server starts on port 3536 by default.

### Using Docker

```bash
docker run -p 3536:3536 ghcr.io/ambiguousinteractive/signal-fish-server:latest
```

### Using Docker Compose

```bash
docker compose up
```

## First Connection

Connect your WebSocket client to:

```text
ws://localhost:3536/v2/ws
```

## Basic Client Flow

Here's a minimal example showing a complete session:

```javascript
const ws = new WebSocket('ws://localhost:3536/v2/ws');

ws.onopen = () => {
  // Create a room by joining without a room code
  ws.send(JSON.stringify({
    type: 'JoinRoom',
    data: {
      game_name: 'my-game',
      player_name: 'Player1',
      max_players: 4
    }
  }));
};

ws.onmessage = (event) => {
  const message = JSON.parse(event.data);

  if (message.type === 'RoomJoined') {
    console.log('Room code:', message.data.room_code);
    // Share this code with other players
  }

  if (message.type === 'PlayerJoined') {
    console.log('Player joined:', message.data.player.name);
  }

  if (message.type === 'GameData') {
    console.log('Received game data:', message.data.data);
  }
};
```

## Joining an Existing Room

```javascript
ws.send(JSON.stringify({
  type: 'JoinRoom',
  data: {
    game_name: 'my-game',
    room_code: 'ABC123',
    player_name: 'Player2'
  }
}));
```

## Sending Game Data

```javascript
ws.send(JSON.stringify({
  type: 'GameData',
  data: {
    action: 'move',
    x: 100,
    y: 200
  }
}));
```

## Health Check

Verify the server is running:

```bash
curl http://localhost:3536/v2/health
```

## Next Steps

- [Configuration](configuration.md) - Customize server settings
- [Protocol Reference](protocol.md) - Complete message documentation
- [Authentication](authentication.md) - Secure your server
