# Error Codes Reference

Signal Fish Server uses structured error codes to communicate failures
to clients. Every error response includes a human-readable `message`
field and an `error_code` string that clients can match on
programmatically.

## How Errors Are Delivered

Errors arrive as server messages over the WebSocket connection. The
server uses several message types depending on the context:

- `Error` -- general errors that can occur at any point
- `RoomJoinFailed` -- errors specific to joining a room
- `ReconnectionFailed` -- errors specific to reconnection attempts
- `AuthenticationError` -- errors during authentication
- `SpectatorJoinFailed` -- errors when joining as a spectator
- `AuthorityResponse` -- authority request denials (with `granted: false`)

All of these message types include an `error_code` field containing one
of the codes documented below. The `error_code` field is optional in
some message types, but when present it is always a `SCREAMING_SNAKE_CASE`
string.

### Example Error Message

```json
{
  "type": "Error",
  "data": {
    "message": "The room has reached its maximum player capacity.",
    "error_code": "ROOM_FULL"
  }
}
```

### Example RoomJoinFailed Message

```json
{
  "type": "RoomJoinFailed",
  "data": {
    "reason": "Room is full",
    "error_code": "ROOM_FULL"
  }
}
```

---

## Error Code Categories

Error codes are organized into categories. The category ranges (1xxx,
2xxx, etc.) appear in source comments for organizational purposes; the
wire format uses string codes, not numeric values.

### Authentication Errors (1xxx)

Errors related to credentials, application identity, and session
establishment.

| Error Code | Description |
|---|---|
| `UNAUTHORIZED` | Access denied. Credentials are missing or invalid. |
| `INVALID_TOKEN` | The authentication token is invalid, malformed, or expired. |
| `AUTHENTICATION_REQUIRED` | The operation requires authentication. Provide valid credentials. |
| `INVALID_APP_ID` | The provided application ID is not recognized. |
| `APP_ID_EXPIRED` | The application ID has expired. Renew your application registration. |
| `APP_ID_REVOKED` | The application ID has been revoked by an administrator. |
| `APP_ID_SUSPENDED` | The application ID has been suspended by an administrator. |
| `MISSING_APP_ID` | Application ID is required but was not provided in the request. |
| `AUTHENTICATION_TIMEOUT` | Authentication took too long to complete. |
| `SDK_VERSION_UNSUPPORTED` | The SDK version is no longer supported. Upgrade to the latest version. |
| `UNSUPPORTED_GAME_DATA_FORMAT` | The requested game data format is not supported by this server. |

### Validation Errors (2xxx)

Errors caused by invalid input, malformed messages, or constraint
violations.

| Error Code | Description |
|---|---|
| `INVALID_INPUT` | The provided input is invalid or malformed. |
| `INVALID_GAME_NAME` | The game name is invalid. Must be non-empty and follow naming requirements. |
| `INVALID_ROOM_CODE` | The room code is invalid or malformed. |
| `INVALID_PLAYER_NAME` | The player name is invalid. Must be non-empty and meet length requirements. |
| `INVALID_MAX_PLAYERS` | The maximum player count is invalid. Must be a positive number within limits. |
| `MESSAGE_TOO_LARGE` | The message size exceeds the maximum allowed limit. |

### Room Errors (3xxx)

Errors related to room lifecycle, capacity, and membership.

| Error Code | Description |
|---|---|
| `ROOM_NOT_FOUND` | The requested room could not be found. It may have been closed. |
| `ROOM_FULL` | The room has reached its maximum player capacity. |
| `ALREADY_IN_ROOM` | You are already in a room. Leave the current room first. |
| `NOT_IN_ROOM` | You are not currently in any room. Join a room first. |
| `ROOM_CREATION_FAILED` | Failed to create the room. Try again later. |
| `MAX_ROOMS_PER_GAME_EXCEEDED` | The maximum number of rooms for this game has been reached. |
| `INVALID_ROOM_STATE` | The room is in an invalid state for this operation. |

### Authority Errors (4xxx)

Errors related to the authority system, which designates a single
client as the authoritative source of game state.

| Error Code | Description |
|---|---|
| `AUTHORITY_NOT_SUPPORTED` | Authority features are not enabled on this server. |
| `AUTHORITY_CONFLICT` | Another client has already claimed authority in this room. |
| `AUTHORITY_DENIED` | You do not have permission to claim authority in this room. |

### Rate Limiting Errors (5xxx)

Errors triggered by exceeding request or connection limits.

| Error Code | Description |
|---|---|
| `RATE_LIMIT_EXCEEDED` | Too many requests in a short time. Slow down and retry later. |
| `TOO_MANY_CONNECTIONS` | Too many active connections. Close some before opening new ones. |

### Reconnection Errors (6xxx)

Errors that occur when a client attempts to rejoin a room after a
disconnection using a stored reconnection token.

| Error Code | Description |
|---|---|
| `RECONNECTION_FAILED` | Failed to reconnect. The session may have expired or the room closed. |
| `RECONNECTION_TOKEN_INVALID` | The reconnection token is invalid or malformed. |
| `RECONNECTION_EXPIRED` | The reconnection window has expired. Join as a new player. |
| `PLAYER_ALREADY_CONNECTED` | This player is already connected from another session. |

### Spectator Errors (7xxx)

Errors related to spectator (read-only observer) mode.

| Error Code | Description |
|---|---|
| `SPECTATOR_NOT_ALLOWED` | Spectator mode is not enabled for this room. |
| `TOO_MANY_SPECTATORS` | The room has reached its maximum spectator capacity. |
| `NOT_A_SPECTATOR` | You are not a spectator in this room. |
| `SPECTATOR_JOIN_FAILED` | Failed to join as a spectator. The room may be full or spectating disabled. |

### Server Errors (9xxx)

Internal server failures. These typically indicate transient issues
that may resolve on retry.

| Error Code | Description |
|---|---|
| `INTERNAL_ERROR` | An internal server error occurred. Try again or contact support. |
| `STORAGE_ERROR` | A storage error occurred while processing the request. |
| `SERVICE_UNAVAILABLE` | The service is temporarily unavailable. Try again in a few moments. |

---

## Handling Errors in Client Code

Error codes are delivered as strings, so clients can match on them
directly. The following Rust example demonstrates a pattern for
handling error codes received from the server.

```rust,ignore
/// Represents an error response from Signal Fish Server.
struct ServerError {
    message: String,
    error_code: Option<String>,
}

fn handle_server_error(error: &ServerError) {
    let Some(code) = &error.error_code else {
        eprintln!("Server error (no code): {}", error.message);
        return;
    };

    match code.as_str() {
        "ROOM_FULL" => {
            println!("Room is full. Try a different room or create a new one.");
        }
        "RATE_LIMIT_EXCEEDED" => {
            println!("Rate limited. Backing off before retrying.");
        }
        "RECONNECTION_EXPIRED" => {
            println!("Reconnection window expired. Joining as a new player.");
        }
        "ROOM_NOT_FOUND" => {
            println!("Room not found. It may have been closed.");
        }
        "AUTHENTICATION_REQUIRED" | "UNAUTHORIZED" | "INVALID_TOKEN" => {
            println!("Authentication error. Re-authenticating.");
        }
        code if code.starts_with("INTERNAL") || code.starts_with("SERVICE") => {
            println!("Server issue. Retrying after a delay.");
        }
        _ => {
            eprintln!("Unhandled error code {code}: {}", error.message);
        }
    }
}
```

---

## Common Scenarios

### Room is full (`ROOM_FULL`)

The room has reached its `max_players` limit. To resolve this, wait for
a player to leave and retry, or create a new room by sending a
`JoinRoom` message without a `room_code`.

### Rate limited (`RATE_LIMIT_EXCEEDED`)

Your client is sending messages faster than the server allows. Implement
exponential backoff: wait 1 second, then 2, then 4, and so on before
retrying. The `Authenticated` response includes a `rate_limits` object
with your per-minute, per-hour, and per-day limits.

### Reconnection expired (`RECONNECTION_EXPIRED`)

The reconnection window has closed since the client disconnected. The
stored `auth_token` is no longer valid. The client must rejoin the room
as a new player by sending a fresh `JoinRoom` message.

### Invalid input (`INVALID_INPUT`)

The message format or content does not meet validation requirements.
Check that:

- `game_name` is non-empty and within length limits
- `player_name` meets the server's naming rules (see `ProtocolInfo`)
- `room_code` follows the expected format
- `max_players` is a positive number within allowed limits
- The overall message size does not exceed the server's limit

### Authentication failures (`UNAUTHORIZED`, `INVALID_APP_ID`)

Verify that your `app_id` is correct and has not expired, been revoked,
or been suspended. Send an `Authenticate` message before attempting
room operations when authentication is enabled.

### Spectator join failed (`SPECTATOR_JOIN_FAILED`, `SPECTATOR_NOT_ALLOWED`)

Spectator mode must be enabled for the room. Verify the room exists,
supports spectators, and has not reached its spectator capacity limit.

---

## See Also

- [Protocol Reference](../protocol.md) -- full message format documentation
- [Getting Started](../getting-started.md) -- basic usage examples
- [Features](../features.md) -- complete feature overview
