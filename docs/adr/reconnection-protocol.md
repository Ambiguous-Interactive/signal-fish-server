# Reconnection Protocol

## Status

ADR-001 - Accepted

## Context

WebSocket connections are inherently fragile and can be disrupted by network issues, mobile device sleep/wake
cycles, browser tab suspension, or temporary connectivity loss. In multiplayer gaming scenarios, dropping a
player entirely due to a brief network hiccup creates a poor user experience and can break game sessions.

Without reconnection support, any connection loss requires:

- Complete re-authentication
- Rejoining the room (which may be full or closed)
- Loss of game state and missed messages during disconnection
- Potential loss of authority role in the room

## Decision

We implemented a comprehensive reconnection protocol with the following components:

### 1. Reconnection Tokens

When a player disconnects, the server issues a time-limited UUID-based reconnection token that binds:

- Player ID
- Room ID
- Creation and expiration timestamps (configurable window, default 300 seconds)
- Player's authority status at disconnection time

### 2. Event Buffering

The server maintains per-room event buffers using a ring buffer (VecDeque) that:

- Stores recent ServerMessage events with sequence numbers and timestamps
- Has configurable maximum size (default 100 events)
- Automatically evicts oldest events when full
- Clears when the last disconnected player from a room reconnects or expires

### 3. Reconnection Flow

```
Player Disconnects → Server registers disconnection
                  → Generates reconnection token
                  → Buffers room events
                  → Starts expiration timer (300s default)

Player Reconnects → Client sends Reconnect message with:
                   - Original Player ID
                   - Room ID
                   - Reconnection token
                 → Server validates token and window
                 → Replays missed events since last_sequence
                 → Restores player to room with same ID
                 → Optionally restores authority role
                 → Notifies other players

Expiration       → Background cleanup task removes expired tokens
                 → Event buffers cleared when no pending reconnections
```

### 4. Security Considerations

- Tokens are single-use and expire after the reconnection window
- Tokens are validated against player_id and room_id to prevent token reuse
- Prevents players from reconnecting while already connected (duplicate connection guard)
- Room state validation ensures room still exists before allowing reconnection

### 5. Configuration

Reconnection is optional and can be disabled via:

```json
{
  "server": {
    "enable_reconnection": true,
    "reconnection_window": 300
  }
}
```

### 6. Metrics

The implementation exposes comprehensive metrics:

- `reconnection_tokens_issued` - Total tokens generated
- `reconnection_sessions_active` - Current pending reconnections
- `reconnection_completions` - Successful reconnections
- `reconnection_validation_failure` - Failed token validations
- `reconnection_events_buffered` - Total events buffered

## Implementation

Core implementation lives in:

- `src/reconnection.rs` - ReconnectionManager, token validation, event buffering
- `src/server/reconnection_service.rs` - Server-side reconnection message handling
- `src/protocol/messages.rs` - Reconnect client message, Reconnected/ReconnectionFailed server messages
- `src/protocol/error_codes.rs` - ReconnectionFailed, ReconnectionExpired, ReconnectionTokenInvalid

## Consequences

### Positive

- **Seamless recovery**: Players can recover from brief network interruptions without losing their session
- **Message continuity**: Event replay ensures no game state is lost during disconnection
- **Authority preservation**: Players can maintain their authority role across reconnections
- **Configurable**: Can be disabled for simpler deployments or enabled with custom window sizes
- **Observable**: Rich metrics enable monitoring of reconnection patterns and success rates

### Negative

- **Memory overhead**: Event buffers consume memory proportional to active rooms and buffer size
- **Complexity**: Adds state management for disconnected players and event replay logic
- **Security surface**: Introduces new attack vectors (token guessing, replay attacks) that must be mitigated
- **Race conditions**: Must handle edge cases like reconnecting while already connected, room deletion
  during reconnection, etc.

### Mitigations

- Event buffers use bounded ring buffers with automatic eviction
- Cleanup task runs periodically to remove expired tokens and buffers
- Tokens are cryptographically random UUIDs with short validity windows
- Comprehensive validation checks (room exists, token valid, player not already connected)
- Extensive test coverage including edge cases and concurrent operations

## Alternatives Considered

### 1. No Reconnection Support

**Rejected**: Poor user experience for mobile/unreliable networks. Industry standard for real-time multiplayer
games includes reconnection support.

### 2. Stateless Reconnection (Client-Side State)

**Rejected**: Would require clients to maintain and replay their own state, increasing client complexity and
opening security holes (clients can fabricate state).

### 3. Persistent Connection IDs

**Rejected**: Would require database persistence and doesn't align with our in-memory-only architecture.
Reconnection tokens with time-limited validity are sufficient for the target use case.

### 4. Full Room State Snapshot on Reconnect

**Rejected**: Wasteful for rooms with frequent small updates. Event replay is more bandwidth-efficient and
provides better message continuity.

## References

- [WebSocket Protocol Patterns](../../.llm/skills/websocket-protocol-patterns.md)
- Implementation: `src/reconnection.rs`
- Configuration: `src/config/server.rs` (enable_reconnection, reconnection_window)
- Metrics: `src/metrics.rs` (ReconnectionMetrics)
