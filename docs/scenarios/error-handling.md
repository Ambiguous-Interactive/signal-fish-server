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

Intent: a client sends messages faster than its app's configured rate limit (the `rate_limits` returned in
`Authenticated`). The server rejects the offending message.

After too many requests in a short window, the server returns an `Error`:

```json
{
  "type": "Error",
  "data": {
    "message": "Too many requests in a short time. Please slow down and try again later.",
    "error_code": "RATE_LIMIT_EXCEEDED"
  }
}
```

Next: the client must **back off** before retrying — stop sending, wait an increasing interval (exponential
backoff with jitter is recommended), then resume. The rate limits the client received in the `Authenticated`
message (`per_minute`, `per_hour`, `per_day`) tell it the ceilings to stay under. The connection itself stays open;
only the rate-limited message was dropped.

## SIGNAL_TOO_LARGE — split the payload

Intent: a v3 client relays a WebRTC `Signal` whose serialized JSON exceeds `security.max_signal_bytes` (default
16 KiB) — typically a bundled offer carrying every gathered ICE candidate at once.

The client sends an oversized signal:

```json
{
  "type": "Signal",
  "data": {
    "to": "00000000-0000-0000-0000-00000000000b",
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

This is not an error message — it is the absence of one. When the lobby finalizes (a member sends `StartGame`
once every current player is ready), both members receive the unchanged `GameStarting`. The v2 member receives
nothing more, exactly as in the [v2 two-player relay](v2-two-player-relay.md) flow. Critically, the **v3 member
receives no `SessionPlan`** either:

```text
v3 member's inbox at finalization:
  GameStarting       { peer_connections: [...] }
  (no SessionPlan)
```

Next: the v3 client must detect the missing `SessionPlan` and fall back to relayed `GameData` for the whole
session. The rule of thumb: a v3 client only attempts peer-to-peer transports if it actually received a
`SessionPlan` at finalization; otherwise it behaves exactly like a v2 client and relays everything through the
server. This is the relay-floor guarantee — v2 and v3 clients always interoperate, with the relay floor as the
universal default.
