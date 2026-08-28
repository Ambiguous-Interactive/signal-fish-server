# Error Handling and Recovery

This scenario shows worked recovery from the errors a client most often hits: a full room, a tripped rate limit, an
oversized WebRTC signal, and a v2 member forcing the whole room onto the relay floor. Each part shows the exact
failure message (with its `error_code`) and the concrete next action a client takes.

Throughout this page placeholder ids follow the same conventions as the other scenarios.

## ROOM_FULL — retry into a new room

Intent: a client tries to join a room by code, but the room has already reached `max_players`.

The client sends:

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Eve",
    "room_code": "ABC123"
  }
}
```

The server rejects the join with `RoomJoinFailed`:

```json
{
  "type": "RoomJoinFailed",
  "data": {
    "reason": "Room is full",
    "error_code": "ROOM_FULL"
  }
}
```

Next: the client recovers by creating a fresh room — it retries `JoinRoom` with **no** `room_code`, which mints a
new room and returns a `RoomJoined` (as in the [v2 two-player relay](v2-two-player-relay.md) flow):

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Eve"
  }
}
```

The new room's generated `room_code` can then be shared so others join Eve instead. (A join attempt against a code
that does not exist for the game would instead **create** that room, not fail — only a full or otherwise invalid
room returns `RoomJoinFailed`.)

## RATE_LIMIT_EXCEEDED — back off and retry

Intent: a configured request budget is exhausted. Two budgets surface this code, and they recover
differently.

**Room admission budgets.** Join attempts (and spectator joins) are counted per player within the
server's rate-limit window. An over-budget join is refused without mutating the room, and the
refusal arrives on the same connection:

```json
{
  "type": "RoomJoinFailed",
  "data": {
    "reason": "Join attempt rate limit exceeded. Try again in 42 seconds.",
    "error_code": "RATE_LIMIT_EXCEEDED"
  }
}
```

A spectator join over budget answers with `SpectatorJoinFailed` carrying the same code. Next: the
client must **back off** before retrying — wait an increasing interval (exponential backoff with
jitter is recommended), then resume. The connection itself stays open; only the refused request was
dropped.

**The per-app handshake budget.** When the app configures `rate_limit_per_minute`, every
`Authenticate` handshake counts against a budget shared across all connections using the same
public ID (the `per_minute` value returned in `Authenticated`; `per_hour` and `per_day` are legacy
advisory projections for client budgeting). An over-budget handshake is refused with an
`AuthenticationError`, and the connection is then closed — the refusal is a farewell on a socket
that is about to die:

```json
{
  "type": "AuthenticationError",
  "data": {
    "error": "RateLimitExceeded",
    "error_code": "RATE_LIMIT_EXCEEDED"
  }
}
```

Next: wait out the window before reconnecting. Each retry is itself a handshake and consumes the
same app-wide budget, so a tight reconnect loop extends the lockout for every player of the app —
back off and reconnect at a low, steady rate.

## SIGNAL_TOO_LARGE — split the payload

Intent: a v3 client relays a WebRTC `Signal` whose serialized JSON exceeds `security.max_signal_bytes` (default
16 KiB) — typically a bundled offer carrying every gathered ICE candidate at once.

The client sends an oversized signal:

```json
{
  "type": "Signal",
  "data": {
    "to": "00000000-0000-0000-0000-00000000000b",
    "generation": "00000000-0000-0000-0000-00000000000c",
    "signal": { "Offer": "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n... (a very large SDP) ..." }
  }
}
```

The server validates only the envelope (it never parses the SDP) and rejects the over-cap payload with an `Error`;
it is **not** relayed to the target:

```json
{
  "type": "Error",
  "data": {
    "message": "The signal payload exceeds the maximum allowed size. Send smaller SDP/ICE payloads.",
    "error_code": "SIGNAL_TOO_LARGE"
  }
}
```

Next: the client switches to **trickle ICE** — it sends the offer/answer SDP on its own, then relays each ICE
candidate in a separate, small `Signal{IceCandidate}` (as in the [mesh scenario](v3-mesh-webrtc.md) step 4) rather
than bundling them. Each individual candidate is well under the cap.

Two related signaling errors share the same recovery shape:

- `SIGNAL_TARGET_NOT_FOUND` — the `to` peer is not in the room (or did not negotiate WebRTC). Re-check the peer id
  against the current `SessionPlan` peer list.
- `SIGNAL_RATE_LIMITED` — too many signaling messages too quickly; slow the trickle-ICE rate and retry shortly.

## A v2 member forces the relay floor

Intent: a room mixes a v3 client (which would prefer mesh + WebRTC) with a v2 (or relay-only) client. A non-relay
plan requires **every** member to be v3-capable and to support the chosen topology and transport, so a single
v2 member pins the whole room to the relay floor.

This is not an error. When the lobby finalizes (a member sends `StartGame` once
every current player is ready), both members receive the unchanged
`GameStarting`. The v2 member receives nothing more, exactly as in the
[v2 two-player relay](v2-two-player-relay.md) flow. The v3 member then receives
an explicit relay-floor reset:

```text
v3 member's inbox at finalization:
  GameStarting       { peer_connections: [...] }
  SessionPlan        { generation: G, topology: relay, transport: relay, peers: [], fallback: relay }
```

Next: the v3 client applies the latest plan, tears down any stale P2P links, and
uses relayed `GameData` for the whole session. The v2 client reaches the same
data path without seeing a v3 message. This is the relay-floor guarantee: v2
and v3 clients always interoperate, with the relay floor as the universal
default.
