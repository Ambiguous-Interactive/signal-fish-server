# Authority System

In a peer-to-peer game, every player runs their own copy of the game
logic. This works well for cooperative or turn-based games, but some
genres need a single source of truth -- one player whose copy of the
game state is considered **authoritative**. Signal Fish Server provides
an optional authority system that lets you designate exactly one player
per room as the authority.

## What is Authority?

Authority means one player acts as the trusted host for game-critical
decisions: physics resolution, score keeping, anti-cheat validation, or
any logic where conflicting results between peers would break the game.
The other players send their inputs to the authority and accept its
responses.

Signal Fish does not enforce what the authority player does with that
role -- it simply tracks who holds authority and notifies all players
when it changes. Your game code decides how to use it.

## Enabling Authority

Authority support is set at room creation time. Include
`supports_authority: true` in the `JoinRoom` message when creating a new
room:

```json
{
  "type": "JoinRoom",
  "data": {
    "game_name": "my-game",
    "player_name": "Alice",
    "max_players": 4,
    "supports_authority": true
  }
}
```

If `supports_authority` is `false` (the default) or omitted, authority
requests in that room are rejected with the `AUTHORITY_NOT_SUPPORTED`
error code.

## How Authority Works

### Initial Assignment

The first player to join a room with authority enabled becomes the
authority by default. When Alice creates the room above, her `RoomJoined`
response includes `"is_authority": true`, and all subsequent players who
join see her flagged as the authority in the `current_players` list.

### Requesting Authority

Any player in the room can request authority by sending an
`AuthorityRequest` message:

```json
{
  "type": "AuthorityRequest",
  "data": {
    "become_authority": true
  }
}
```

The server responds directly to the requesting player with an
`AuthorityResponse`:

```json
{
  "type": "AuthorityResponse",
  "data": {
    "granted": true
  }
}
```

If the request is denied (for example, because another player already
holds authority), the response includes a reason:

```json
{
  "type": "AuthorityResponse",
  "data": {
    "granted": false,
    "reason": "Another client has already claimed authority. Only one client can have authority at a time.",
    "error_code": "AUTHORITY_CONFLICT"
  }
}
```

### Broadcasting Authority Changes

When authority changes, **every player** in the room receives an
`AuthorityChanged` message. The `you_are_authority` field is personalized
per recipient:

**Sent to the new authority:**

```json
{
  "type": "AuthorityChanged",
  "data": {
    "authority_player": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
    "you_are_authority": true
  }
}
```

**Sent to all other players:**

```json
{
  "type": "AuthorityChanged",
  "data": {
    "authority_player": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
    "you_are_authority": false
  }
}
```

### Releasing Authority

A player can release authority by sending `become_authority: false`:

```json
{
  "type": "AuthorityRequest",
  "data": {
    "become_authority": false
  }
}
```

This clears the authority role. All players receive an `AuthorityChanged`
message with `authority_player` set to `null`:

```json
{
  "type": "AuthorityChanged",
  "data": {
    "authority_player": null,
    "you_are_authority": false
  }
}
```

### Authority Player Disconnects

If the authority player disconnects from the room, authority is
**cleared** -- there is no automatic reassignment. All remaining players
receive an `AuthorityChanged` message with `authority_player: null`. Your
game logic should handle this case, either by prompting another player to
claim authority or by pausing the game until someone does.

## Key Rules

- Only **one player** can hold authority at a time per room.
- Authority must be **enabled at room creation** via `supports_authority`.
- The **first player** to join becomes authority by default.
- Authority **transfers** are explicit via `AuthorityRequest`.
- **Disconnection clears** authority with no auto-reassignment.
- Authority status is included in the `GameStarting` peer connection data
  so clients know who the authority is at game start.

## Use Cases

- **Host migration** -- If the current host drops out, another player
  requests authority and becomes the new host.
- **Authoritative physics** -- The authority player runs the physics
  simulation and broadcasts results to peers.
- **Anti-cheat validation** -- The authority cross-checks player inputs
  against game rules before accepting them.
- **Asymmetric gameplay** -- One player acts as the game master or dungeon
  master with elevated control.

## When NOT to Use Authority

If your game is fully symmetric -- every peer runs identical logic and
resolves conflicts through deterministic lockstep or rollback netcode --
you do not need the authority system. Skipping it simplifies your room
setup and avoids unnecessary authority-related messages.

## Next Steps

- [Rooms and Lobbies](rooms-and-lobbies.md) -- Room lifecycle and the
  ready-up flow
- [Reconnection](reconnection.md) -- What happens to authority during
  reconnection
- [Spectator Mode](spectator-mode.md) -- Read-only observers who do not
  participate in authority
