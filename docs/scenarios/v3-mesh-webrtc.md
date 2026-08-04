# v3: Mesh WebRTC

This scenario shows two protocol v3 clients negotiating a full-mesh WebRTC session: both advertise v3 capabilities,
the lobby finalizes, both receive `GameStarting` **plus** a per-recipient `SessionPlan`, they exchange WebRTC
offers/answers and trickle ICE through the server, the data channel opens, and both report
`TransportStatus{webrtc, true}`. It ends by showing the relay-floor fallback when the peer-to-peer path dies.

Throughout this page:

- Alice, id `00000000-0000-0000-0000-00000000000a` (the **lesser** UUID).
- Bob, id `00000000-0000-0000-0000-00000000000b`.
- Room `11111111-1111-1111-1111-111111111111`, code `ABC123`, `supports_authority: false`.

Both clients connect to `ws://localhost:3536/v3/ws`. By the mesh glare rule, the lesser UUID offers, so **Alice
offers to Bob**.

## 1. Both clients authenticate as v3

Intent: each client advertises the highest protocol version it speaks plus the transports and topologies it
supports. Absent these fields, the connection would be relay-only even on `/v3/ws`.

Alice sends:

```json
{
  "type": "Authenticate",
  "data": {
    "app_id": "mb_app_abc123",
    "sdk_version": "1.2.3",
    "platform": "unity",
    "protocol_version": 3,
    "supported_transports": ["relay", "direct", "webrtc"],
    "supported_topologies": ["relay", "host", "mesh"]
  }
}
```

The server replies with `Authenticated` (identical shape to the v2 scenario) followed by an **extended**
`ProtocolInfo` that echoes the negotiated version:

```json
{
  "type": "ProtocolInfo",
  "data": {
    "capabilities": ["reconnection", "spectators", "authority"],
    "game_data_formats": ["json", "message_pack"],
    "protocol_version": 3,
    "min_protocol_version": 2,
    "max_protocol_version": 3,
    "transports": ["websocket"]
  }
}
```

Bob authenticates with the identical `Authenticate` payload and receives the identical `ProtocolInfo`.

Next: both clients now know the connection negotiated v3. They join the room exactly as in the v2 flow (Alice
creates by omitting `room_code`, Bob joins with `room_code: "ABC123"`).

## 2. Lobby finalizes — GameStarting plus SessionPlan

Intent: both players ready up (`PlayerReady`, exactly as in the v2 flow). Readiness alone does not start the game —
once every current player is ready, a member finalizes the lobby with an explicit `StartGame`. The room was
created with `supports_authority: false`, so it has no designated authority and **any** member may start; here
Alice sends it:

```json
{
  "type": "StartGame"
}
```

When the lobby finalizes, because **every** member negotiated v3 plus the `mesh` topology and `webrtc` transport,
the server selects the richest rung — mesh + WebRTC — and emits a per-recipient `SessionPlan` alongside the
unchanged `GameStarting`. (`max_players` is a ceiling, not a required count — the room need not be full to start.)

Both players receive `GameStarting` just as in the [v2 relay scenario](v2-two-player-relay.md) — the move into
`finalized` is signaled by `GameStarting`, not by a `LobbyStateChanged`. Additionally, each v3 member receives its
**own** `SessionPlan`.

Alice receives a plan whose single peer is Bob, with `initiate: true` (Alice is the lesser UUID, so she offers):

```json
{
  "type": "SessionPlan",
  "data": {
    "generation": "00000000-0000-0000-0000-00000000000c",
    "topology": "mesh",
    "transport": "webrtc",
    "peers": [
      {
        "player_id": "00000000-0000-0000-0000-00000000000b",
        "player_name": "Bob",
        "is_authority": false,
        "initiate": true
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

Bob receives a plan whose single peer is Alice, with `initiate: false` (Bob answers). His TURN credential is minted
for **his** id:

```json
{
  "type": "SessionPlan",
  "data": {
    "generation": "00000000-0000-0000-0000-00000000000c",
    "topology": "mesh",
    "transport": "webrtc",
    "peers": [
      {
        "player_id": "00000000-0000-0000-0000-00000000000a",
        "player_name": "Alice",
        "is_authority": false,
        "initiate": false
      }
    ],
    "ice_servers": [
      { "urls": ["stun:stun.l.google.com:19302"] },
      {
        "urls": ["turn:turn.example.com:3478"],
        "username": "1700003600:00000000-0000-0000-0000-00000000000b",
        "credential": "Yb2pQ9m0c1Vd3R4t5U6w7X8y9Z0aA1B="
      }
    ],
    "fallback": "relay"
  }
}
```

Next: each client reads `peers[].initiate` to decide who offers. `is_authority` is `false` for every peer because
the room was created with `supports_authority: false`. `fallback` is always `relay` — the floor never closes.

## 3. Alice offers, Bob answers

Intent: the offerer (Alice) creates a WebRTC offer and relays it to Bob through the server's `Signal` channel. The
server never parses the payload; it forwards it verbatim to the named target.

Alice sends (`to` names the target peer):

```json
{
  "type": "Signal",
  "data": {
    "to": "00000000-0000-0000-0000-00000000000b",
    "generation": "00000000-0000-0000-0000-00000000000c",
    "signal": { "Offer": "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n..." }
  }
}
```

Bob receives the same opaque payload, now stamped with `from`:

```json
{
  "type": "Signal",
  "data": {
    "from": "00000000-0000-0000-0000-00000000000a",
    "generation": "00000000-0000-0000-0000-00000000000c",
    "signal": { "Offer": "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n..." }
  }
}
```

Bob sets the remote description, creates an answer, and signals it back:

```json
{
  "type": "Signal",
  "data": {
    "to": "00000000-0000-0000-0000-00000000000a",
    "generation": "00000000-0000-0000-0000-00000000000c",
    "signal": { "Answer": "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n..." }
  }
}
```

Alice receives:

```json
{
  "type": "Signal",
  "data": {
    "from": "00000000-0000-0000-0000-00000000000b",
    "generation": "00000000-0000-0000-0000-00000000000c",
    "signal": { "Answer": "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n..." }
  }
}
```

Next: both peers have exchanged SDP. They now trickle ICE candidates.

## 4. ICE trickle in both directions

Intent: as each peer's ICE agent gathers candidates, it relays them one at a time. Sending candidates individually
keeps each `Signal` payload small (well under the `security.max_signal_bytes` cap).

Alice sends a candidate:

```json
{
  "type": "Signal",
  "data": {
    "to": "00000000-0000-0000-0000-00000000000b",
    "generation": "00000000-0000-0000-0000-00000000000c",
    "signal": { "IceCandidate": "candidate:1 1 UDP 2130706431 10.0.0.5 54321 typ host" }
  }
}
```

Bob receives:

```json
{
  "type": "Signal",
  "data": {
    "from": "00000000-0000-0000-0000-00000000000a",
    "generation": "00000000-0000-0000-0000-00000000000c",
    "signal": { "IceCandidate": "candidate:1 1 UDP 2130706431 10.0.0.5 54321 typ host" }
  }
}
```

Bob trickles his own candidates back the same way (`to: Alice`, server delivers as `from: Bob`). This continues in
both directions until the agents converge.

Next: once a candidate pair succeeds, the WebRTC data channel opens.

## 5. Data channel open — both report TransportStatus

Intent: with the peer-to-peer data channel established, each client reports its data-path state so the server can
distinguish P2P-connected peers from relay-fallback peers. This is purely informational and drives metrics.

Each client sends:

```json
{
  "type": "TransportStatus",
  "data": {
    "transport": "webrtc",
    "connected": true
  }
}
```

Because this is the first report on each connection, the server records the state change and fans it out to the
**other** member as `PeerTransportStatus`. Bob receives Alice's status:

```json
{
  "type": "PeerTransportStatus",
  "data": {
    "peer_id": "00000000-0000-0000-0000-00000000000a",
    "transport": "webrtc",
    "connected": true
  }
}
```

and Alice receives Bob's:

```json
{
  "type": "PeerTransportStatus",
  "data": {
    "peer_id": "00000000-0000-0000-0000-00000000000b",
    "transport": "webrtc",
    "connected": true
  }
}
```

Next: gameplay traffic now flows directly peer-to-peer over the WebRTC data channel. The clients no longer need to
relay `GameData` through the server. (A duplicate `TransportStatus{webrtc, true}` would be dropped and fan out
nothing.)

## 6. Relay-floor fallback

Intent: the WebRTC path dies (a NAT rebinding, a network change, an ICE timeout). The client falls back to the
relay floor, which never closed. It reports the new state so the server and peers learn the data path changed.

Alice's data channel fails, so she reports it:

```json
{
  "type": "TransportStatus",
  "data": {
    "transport": "webrtc",
    "connected": false
  }
}
```

This is a real state transition, so the server fans it out to Bob:

```json
{
  "type": "PeerTransportStatus",
  "data": {
    "peer_id": "00000000-0000-0000-0000-00000000000a",
    "transport": "webrtc",
    "connected": false
  }
}
```

Alice now resumes sending gameplay over the relay, exactly as in the v2 flow:

```json
{
  "type": "GameData",
  "data": {
    "data": {
      "action": "move",
      "x": 140,
      "y": 205
    }
  }
}
```

Bob receives it through the server-relayed form:

```json
{
  "type": "GameData",
  "data": {
    "from_player": "00000000-0000-0000-0000-00000000000a",
    "data": {
      "action": "move",
      "x": 140,
      "y": 205
    }
  }
}
```

Next: the session continues over the relay floor. The reported `TransportStatus` never changed how the server
relays `GameData` — the floor was always available underneath the WebRTC upgrade. This is the relay-floor
guarantee: a failed peer-to-peer path degrades to v2 relay behavior without dropping the session.
