# v3: Host (Star) Topology

This scenario shows a v3 host topology: a three-member room finalizes to a `host + webrtc` star around a single
elected host. Each client's `SessionPlan` targets only the host, and the host's plan lists every client. Clients
signal the host and never each other.

Throughout this page:

- Alice, id `00000000-0000-0000-0000-00000000000a` — the **elected host** (she is the room authority).
- Carol, id `00000000-0000-0000-0000-00000000000c` — a client.
- Dave, id `00000000-0000-0000-0000-00000000000d` — a client.
- Room `11111111-1111-1111-1111-111111111111`, code `ABC123`, `supports_authority: true`, `max_players: 3`.

All three connect to `ws://localhost:3536/v3/ws` and authenticate as v3 (advertising `host` among their
`supported_topologies`), exactly as in the [mesh scenario](v3-mesh-webrtc.md) step 1. The game's desired topology
is `host`, so when the lobby finalizes the server picks the `host + webrtc` rung.

## 1. Lobby finalizes — per-recipient SessionPlan around the host

Intent: all three players ready up (`PlayerReady`), and once every current player is ready the game is finalized by
an explicit `StartGame`. This room was created with `supports_authority: true` and Alice has claimed authority, so
**only the authority (Alice) may start** — a `StartGame` from Carol or Dave would be rejected with
`GAME_START_FORBIDDEN`. (`max_players: 3` is a ceiling, not a required count; the room need not be full to start.)

Alice (the authority) sends:

```json
{
  "type": "StartGame"
}
```

The server elects a host (authority preferred, else earliest joiner, smaller-UUID tie-break — here Alice, the
authority) and emits a per-recipient `SessionPlan`. Every member receives `GameStarting` exactly as in the
[v2 flow](v2-two-player-relay.md) (the move into `finalized` is signaled by `GameStarting`, not a
`LobbyStateChanged`); each v3 member additionally receives its tailored plan.

Carol's plan lists a single peer — the host, Alice — with `initiate: true` (each client offers to the host) and
`is_authority: true` (the host is the session authority). The `host` field names the elected host:

```json
{
  "type": "SessionPlan",
  "data": {
    "generation": "00000000-0000-0000-0000-00000000000d",
    "topology": "host",
    "transport": "webrtc",
    "host": "00000000-0000-0000-0000-00000000000a",
    "peers": [
      {
        "player_id": "00000000-0000-0000-0000-00000000000a",
        "player_name": "Alice",
        "is_authority": true,
        "initiate": true
      }
    ],
    "ice_servers": [
      { "urls": ["stun:stun.l.google.com:19302"] },
      {
        "urls": ["turn:turn.example.com:3478"],
        "username": "1700003600:00000000-0000-0000-0000-00000000000c",
        "credential": "C1c2D3d4E5e6F7f8G9g0H1h2I3i4J5j6="
      }
    ],
    "fallback": "relay"
  }
}
```

Dave's plan is the mirror of Carol's — its single peer is also the host Alice, `initiate: true`, with Dave's own
TURN credential:

```json
{
  "type": "SessionPlan",
  "data": {
    "generation": "00000000-0000-0000-0000-00000000000d",
    "topology": "host",
    "transport": "webrtc",
    "host": "00000000-0000-0000-0000-00000000000a",
    "peers": [
      {
        "player_id": "00000000-0000-0000-0000-00000000000a",
        "player_name": "Alice",
        "is_authority": true,
        "initiate": true
      }
    ],
    "ice_servers": [
      { "urls": ["stun:stun.l.google.com:19302"] },
      {
        "urls": ["turn:turn.example.com:3478"],
        "username": "1700003600:00000000-0000-0000-0000-00000000000d",
        "credential": "D1d2E3e4F5f6G7g8H9h0I1i2J3j4K5k6="
      }
    ],
    "fallback": "relay"
  }
}
```

The host Alice's plan lists **every client** (Carol and Dave), each with `initiate: false` (the host answers every
client) and `is_authority: false` (the clients are not the host):

```json
{
  "type": "SessionPlan",
  "data": {
    "generation": "00000000-0000-0000-0000-00000000000d",
    "topology": "host",
    "transport": "webrtc",
    "host": "00000000-0000-0000-0000-00000000000a",
    "peers": [
      {
        "player_id": "00000000-0000-0000-0000-00000000000c",
        "player_name": "Carol",
        "is_authority": false,
        "initiate": false
      },
      {
        "player_id": "00000000-0000-0000-0000-00000000000d",
        "player_name": "Dave",
        "is_authority": false,
        "initiate": false
      }
    ],
    "ice_servers": [
      { "urls": ["stun:stun.l.google.com:19302"] },
      {
        "urls": ["turn:turn.example.com:3478"],
        "username": "1700003600:00000000-0000-0000-0000-00000000000a",
        "credential": "7/zfauXrL6LSdBbV8YTfnCWafHk="
      }
    ],
    "fallback": "relay"
  }
}
```

Next: each client opens exactly one WebRTC connection (to the host); the host opens one per client. No client
connects to another client.

## 2. Each client offers to the host

Intent: in a star topology the direction is fixed — each client initiates to the host. Carol sends her offer to
Alice.

Carol sends:

```json
{
  "type": "Signal",
  "data": {
    "to": "00000000-0000-0000-0000-00000000000a",
    "generation": "00000000-0000-0000-0000-00000000000d",
    "signal": { "Offer": "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n..." }
  }
}
```

Alice (host) receives:

```json
{
  "type": "Signal",
  "data": {
    "from": "00000000-0000-0000-0000-00000000000c",
    "generation": "00000000-0000-0000-0000-00000000000d",
    "signal": { "Offer": "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n..." }
  }
}
```

Alice answers Carol:

```json
{
  "type": "Signal",
  "data": {
    "to": "00000000-0000-0000-0000-00000000000c",
    "generation": "00000000-0000-0000-0000-00000000000d",
    "signal": { "Answer": "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n..." }
  }
}
```

Carol receives:

```json
{
  "type": "Signal",
  "data": {
    "from": "00000000-0000-0000-0000-00000000000a",
    "generation": "00000000-0000-0000-0000-00000000000d",
    "signal": { "Answer": "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n..." }
  }
}
```

Dave performs the identical exchange with Alice (`to: Alice` offer, `from: Alice` answer). Carol and Dave **never**
signal each other. ICE trickle then proceeds along each client/host edge, identical in shape to the mesh
scenario's step 4.

Next: once each client/host data channel opens, every member reports `TransportStatus{webrtc, true}`, which the
server fans out as `PeerTransportStatus` to the other v3 members (same shapes as the mesh scenario).

## 3. On failure, fall back to the relay floor

Intent: if a client/host data channel fails, that client falls back to the relay floor for `GameData`, reporting
`TransportStatus{webrtc, false}`. The shapes are identical to the mesh fallback; the only difference is which edge
broke. The host keeps serving the still-connected clients over WebRTC while the disconnected client relays.

## Why host differs from mesh

The two topologies differ in two contractual ways:

- **Peer lists.** In a mesh plan every member's `peers` lists every other member. In a host plan a client's
  `peers` contains only the host, and only the host's `peers` lists all the clients. A star of N members has N − 1
  connections; a mesh has N × (N − 1) / 2.
- **The glare rule.** In mesh, the offerer is chosen per pair by the deterministic lesser-UUID rule, so `initiate`
  varies by peer. In host, the direction is fixed regardless of UUID order: every client initiates to the host
  (`initiate: true` on the host entry in client plans) and the host answers every client (`initiate: false` on the
  client entries in the host's plan). Clients never signal each other, so a star topology brokers only client/host
  signaling.

The `host` field is present only in `host`-topology plans (omitted entirely in mesh and relay plans), and
`is_authority` marks the elected host rather than mirroring a room-wide authority flag.
