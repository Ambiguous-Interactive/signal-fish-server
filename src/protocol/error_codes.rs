use serde::{Deserialize, Serialize};
use std::fmt;

/// Error codes for structured error handling
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // Authentication errors (1xxx)
    Unauthorized,
    /// Reserved for source and decode compatibility. Signal Fish Server does
    /// not emit this legacy token; invalid reconnect credentials use
    /// [`Self::ReconnectionTokenInvalid`].
    InvalidToken,
    /// Reserved for source and decode compatibility. Signal Fish Server does
    /// not emit this legacy token; a required app-ID handshake that was not
    /// supplied uses [`Self::MissingAppId`].
    AuthenticationRequired,
    InvalidAppId,
    /// Reserved for source and decode compatibility with potential external
    /// authentication backends. The shipped in-memory allowlist cannot emit it.
    AppIdExpired,
    /// Reserved for source and decode compatibility with potential external
    /// authentication backends. The shipped in-memory allowlist cannot emit it.
    AppIdRevoked,
    /// Reserved for source and decode compatibility with potential external
    /// authentication backends. The shipped in-memory allowlist cannot emit it.
    AppIdSuspended,
    MissingAppId,
    AuthenticationTimeout,
    SdkVersionUnsupported,
    UnsupportedGameDataFormat,

    // Validation errors (2xxx)
    InvalidInput,
    InvalidGameName,
    InvalidRoomCode,
    InvalidPlayerName,
    InvalidMaxPlayers,
    MessageTooLarge,

    // Room errors (3xxx)
    RoomNotFound,
    RoomFull,
    AlreadyInRoom,
    NotInRoom,
    RoomCreationFailed,
    MaxRoomsPerGameExceeded,
    InvalidRoomState,

    // Authority errors (4xxx)
    AuthorityNotSupported,
    AuthorityConflict,
    AuthorityDenied,

    // Rate limiting (5xxx)
    RateLimitExceeded,
    TooManyConnections,

    // Reconnection errors (6xxx)
    ReconnectionFailed,
    ReconnectionTokenInvalid,
    ReconnectionExpired,
    PlayerAlreadyConnected,

    // Spectator errors (7xxx)
    SpectatorNotAllowed,
    TooManySpectators,
    NotASpectator,
    SpectatorJoinFailed,

    // Server errors (9xxx)
    InternalError,
    StorageError,
    /// Reserved for source and decode compatibility. Signal Fish Server does
    /// not emit this token on WebSockets; HTTP admission can return status 503,
    /// while socket shutdown uses [`Self::ServerDraining`].
    ServiceUnavailable,

    // Signaling errors (8xxx). Variants stay in the order they were added:
    // the wire encoding is name-based (SCREAMING_SNAKE_CASE serde tokens), so
    // order is free, but this file's grouping-by-category comments only make
    // sense read as a history of appends.
    CrossRoomSignal,
    UnsupportedTransport,
    SignalTargetNotFound,
    SignalRateLimited,
    /// Signaling errors (8xxx), continued: the serialized `Signal` payload
    /// exceeds `security.max_signal_bytes`.
    SignalTooLarge,

    // Connection lifecycle errors (1xxx category, appended at the END — see
    // the signaling-errors note above).
    ConnectionIdleTimeout,

    // Game-start errors (3xxx category, appended at the END — see the
    // signaling-errors note above). Raised by the explicit `StartGame` flow.
    /// `StartGame` was sent before every current player was ready.
    GameStartNotReady,
    /// `StartGame` was sent by a player not permitted to start the game (the
    /// room has a designated authority and the sender is not it).
    GameStartForbidden,

    // Connection lifecycle errors (1xxx category, appended at the END — see
    // the signaling-errors note above).
    /// The connection's outbound queue stayed full past
    /// `websocket.slow_consumer_timeout_ms`, so the server disconnected it
    /// rather than silently dropping relayed messages. Sent best-effort as a
    /// final `Error` frame before the close.
    SlowConsumer,
    /// The server's activity reaper (`server.ping_timeout`) evicted this
    /// connection because no messages of any kind were received within the
    /// window. Distinct from [`Self::ConnectionIdleTimeout`], which is the
    /// socket-level `websocket.idle_timeout_secs` close. Sent best-effort as
    /// a final `Error` frame before the close.
    ActivityTimeout,
    /// The server is draining for shutdown and is rejecting new room creation.
    /// Existing connections will close with WebSocket close code 4000
    /// (`server_shutdown`) at the drain deadline.
    ServerDraining,

    // Delivery-class validation (2xxx category). Appended at the END; see the
    // signaling-errors note above.
    /// The requested delivery class/key combination is invalid.
    InvalidDeliveryClass,

    // Authentication errors (1xxx category). Appended at the END; see the
    // signaling-errors note above.
    /// The client cannot speak this deployment's minimum protocol version.
    UnsupportedProtocolVersion,
}

impl ErrorCode {
    /// Variants retained for Rust source and wire-decoding compatibility that
    /// the shipped server does not emit.
    ///
    /// Code generators should exclude these from the server-emitted contract.
    pub const NON_EMITTED: &'static [Self] = &[
        Self::InvalidToken,
        Self::AuthenticationRequired,
        Self::AppIdExpired,
        Self::AppIdRevoked,
        Self::AppIdSuspended,
        Self::ServiceUnavailable,
    ];

    /// Returns a human-readable description of this error code.
    ///
    /// This method provides actionable error messages that SDK developers
    /// can display to end users or use for debugging.
    pub fn description(&self) -> &'static str {
        match self {
            // Authentication errors (1xxx)
            Self::Unauthorized => {
                "Access denied by the app-ID handshake policy."
            }
            Self::InvalidToken => {
                "The authentication token is invalid, malformed, or has expired. Please obtain a new token."
            }
            Self::AuthenticationRequired => {
                "Complete the legacy Authenticate handshake before this operation."
            }
            Self::InvalidAppId => {
                "The provided application ID is not recognized. Verify your app ID is correct."
            }
            Self::AppIdExpired => {
                "The application ID has expired. Please renew your application registration."
            }
            Self::AppIdRevoked => {
                "The application ID has been revoked. Contact the administrator for assistance."
            }
            Self::AppIdSuspended => {
                "The application ID has been suspended. Contact the administrator for assistance."
            }
            Self::MissingAppId => {
                "The required app-ID handshake was not completed. Send Authenticate before application messages."
            }
            Self::AuthenticationTimeout => {
                "The app-ID and protocol handshake took too long to complete. Please try again."
            }
            Self::SdkVersionUnsupported => {
                "The SDK version you are using is no longer supported. Please upgrade to the latest version."
            }
            Self::UnsupportedGameDataFormat => {
                "The requested game data format is not supported by this server. Falling back to JSON encoding."
            }

            // Validation errors (2xxx)
            Self::InvalidInput => {
                "The provided input is invalid or malformed. Check your request parameters."
            }
            Self::InvalidGameName => {
                "The game name is invalid. Game names must be non-empty and follow naming requirements."
            }
            Self::InvalidRoomCode => {
                "The room code is invalid or malformed. Room codes must follow the required format."
            }
            Self::InvalidPlayerName => {
                "The player name is invalid. Player names must be non-empty and meet length requirements."
            }
            Self::InvalidMaxPlayers => {
                "The maximum player count is invalid. It must be a positive number within allowed limits."
            }
            Self::MessageTooLarge => {
                "The message size exceeds the maximum allowed limit. Please send a smaller message."
            }

            // Room errors (3xxx)
            Self::RoomNotFound => {
                "The requested room could not be found. It may have been closed or the code is incorrect."
            }
            Self::RoomFull => {
                "The room has reached its maximum player capacity. Try joining a different room."
            }
            Self::AlreadyInRoom => {
                "You are already in a room. Leave the current room before joining another."
            }
            Self::NotInRoom => {
                "You are not currently in any room. Join a room before performing this action."
            }
            Self::RoomCreationFailed => {
                "Failed to create the room. Please try again or contact support if the issue persists."
            }
            Self::MaxRoomsPerGameExceeded => {
                "The maximum number of rooms for this game has been reached. Please try again later."
            }
            Self::InvalidRoomState => {
                "The room is in an invalid state for this operation. Try refreshing or rejoining the room."
            }

            // Authority errors (4xxx)
            Self::AuthorityNotSupported => {
                "Authority features are not enabled on this server. Check your server configuration."
            }
            Self::AuthorityConflict => {
                "Another client has already claimed authority. Only one client can have authority at a time."
            }
            Self::AuthorityDenied => {
                "You do not have permission to claim authority in this room."
            }

            // Rate limiting (5xxx)
            Self::RateLimitExceeded => {
                "Too many requests in a short time. Please slow down and try again later."
            }
            Self::TooManyConnections => {
                "You have too many active connections. Close some connections before opening new ones."
            }

            // Reconnection errors (6xxx)
            Self::ReconnectionFailed => {
                "Failed to reconnect to the room. The session may have expired or the room may be closed."
            }
            Self::ReconnectionTokenInvalid => {
                "The reconnection token is invalid or malformed. You may need to join the room again."
            }
            Self::ReconnectionExpired => {
                "The reconnection window has expired. You must join the room again as a new player."
            }
            Self::PlayerAlreadyConnected => {
                "This player is already connected to the room from another session."
            }

            // Spectator errors (7xxx)
            Self::SpectatorNotAllowed => {
                "Spectator mode is not enabled for this room. Only players can join."
            }
            Self::TooManySpectators => {
                "The room has reached its maximum spectator capacity. Try again later."
            }
            Self::NotASpectator => {
                "You are not a spectator in this room. This action is only available to spectators."
            }
            Self::SpectatorJoinFailed => {
                "Failed to join as a spectator. The room may be full or spectating may be disabled."
            }

            // Signaling errors (8xxx)
            Self::CrossRoomSignal => {
                "Cannot signal a peer in a different room. WebRTC signaling is restricted to peers within the same room."
            }
            Self::UnsupportedTransport => {
                "Signaling requires the WebRTC transport, which was not negotiated for this connection. Re-authenticate advertising WebRTC support."
            }
            Self::SignalTargetNotFound => {
                "The signal target peer could not be found in your room, or does not support WebRTC. Verify the peer id and that the peer is connected."
            }
            Self::SignalRateLimited => {
                "Too many signaling messages in a short time. Please slow down trickle-ICE and try again shortly."
            }
            Self::SignalTooLarge => {
                "The signal payload exceeds the maximum allowed size. Send smaller SDP/ICE payloads, e.g. individual trickle-ICE candidates."
            }

            // Connection lifecycle errors (1xxx)
            Self::ConnectionIdleTimeout => {
                "The connection was closed because no messages were received within the idle timeout. Send periodic Ping messages to keep the connection alive."
            }

            // Server errors (9xxx)
            Self::InternalError => {
                "An internal server error occurred. Please try again or contact support if the issue persists."
            }
            Self::StorageError => {
                "A storage error occurred while processing your request. Please try again later."
            }
            Self::ServiceUnavailable => {
                "The service is temporarily unavailable. Please try again in a few moments."
            }

            // Game-start errors (3xxx)
            Self::GameStartNotReady => {
                "The game cannot start yet. Every current player must be ready before StartGame is accepted."
            }
            Self::GameStartForbidden => {
                "You are not permitted to start the game. Only the room's authority player may start it."
            }

            // Connection lifecycle errors (1xxx), continued
            Self::SlowConsumer => {
                "This connection could not keep up with the messages sent to it, so the server closed it instead of silently dropping data. Drain messages faster (or reconnect) and consider pacing senders."
            }
            Self::ActivityTimeout => {
                "The server received no messages from this connection within its activity window and evicted it. Send periodic Ping messages (or any traffic) to stay connected."
            }
            Self::ServerDraining => {
                "The server is draining for shutdown and is not accepting new room creation. Retry on another instance or after the drain deadline."
            }
            Self::InvalidDeliveryClass => {
                "The game-data delivery class is invalid: latest requires a key, while reliable and volatile must not include one."
            }
            Self::UnsupportedProtocolVersion => {
                "The client's highest supported protocol version is below this server's configured minimum. Upgrade the client or connect to a compatible deployment."
            }
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_error_codes_have_descriptions() {
        // Ensure all error codes have non-empty descriptions
        let error_codes = [
            ErrorCode::Unauthorized,
            ErrorCode::InvalidToken,
            ErrorCode::AuthenticationRequired,
            ErrorCode::InvalidAppId,
            ErrorCode::AppIdExpired,
            ErrorCode::AppIdRevoked,
            ErrorCode::AppIdSuspended,
            ErrorCode::MissingAppId,
            ErrorCode::AuthenticationTimeout,
            ErrorCode::SdkVersionUnsupported,
            ErrorCode::UnsupportedGameDataFormat,
            ErrorCode::InvalidInput,
            ErrorCode::InvalidGameName,
            ErrorCode::InvalidRoomCode,
            ErrorCode::InvalidPlayerName,
            ErrorCode::InvalidMaxPlayers,
            ErrorCode::MessageTooLarge,
            ErrorCode::RoomNotFound,
            ErrorCode::RoomFull,
            ErrorCode::AlreadyInRoom,
            ErrorCode::NotInRoom,
            ErrorCode::RoomCreationFailed,
            ErrorCode::MaxRoomsPerGameExceeded,
            ErrorCode::InvalidRoomState,
            ErrorCode::AuthorityNotSupported,
            ErrorCode::AuthorityConflict,
            ErrorCode::AuthorityDenied,
            ErrorCode::RateLimitExceeded,
            ErrorCode::TooManyConnections,
            ErrorCode::ReconnectionFailed,
            ErrorCode::ReconnectionTokenInvalid,
            ErrorCode::ReconnectionExpired,
            ErrorCode::PlayerAlreadyConnected,
            ErrorCode::SpectatorNotAllowed,
            ErrorCode::TooManySpectators,
            ErrorCode::NotASpectator,
            ErrorCode::SpectatorJoinFailed,
            ErrorCode::InternalError,
            ErrorCode::StorageError,
            ErrorCode::ServiceUnavailable,
            ErrorCode::CrossRoomSignal,
            ErrorCode::UnsupportedTransport,
            ErrorCode::SignalTargetNotFound,
            ErrorCode::SignalRateLimited,
            ErrorCode::SignalTooLarge,
            ErrorCode::ConnectionIdleTimeout,
            ErrorCode::GameStartNotReady,
            ErrorCode::GameStartForbidden,
            ErrorCode::SlowConsumer,
            ErrorCode::ActivityTimeout,
            ErrorCode::ServerDraining,
            ErrorCode::InvalidDeliveryClass,
            ErrorCode::UnsupportedProtocolVersion,
        ];

        for error_code in &error_codes {
            let description = error_code.description();
            assert!(
                !description.is_empty(),
                "ErrorCode::{error_code:?} has empty description"
            );
            assert!(
                description.len() > 10,
                "ErrorCode::{error_code:?} has suspiciously short description: '{description}'"
            );
        }
    }

    #[test]
    fn non_emitted_error_codes_remain_serializable_and_decodable() {
        assert_eq!(ErrorCode::NON_EMITTED.len(), 6);
        for code in ErrorCode::NON_EMITTED {
            let wire = serde_json::to_string(code).unwrap();
            assert_eq!(serde_json::from_str::<ErrorCode>(&wire).unwrap(), *code);
        }
    }

    #[test]
    fn test_display_uses_description() {
        let error = ErrorCode::RoomNotFound;
        let display_output = format!("{error}");
        let description_output = error.description();
        assert_eq!(display_output, description_output);
    }

    #[test]
    fn test_sample_descriptions() {
        // Verify a few specific descriptions to ensure they're actionable
        assert!(ErrorCode::InvalidToken
            .description()
            .contains("authentication token"));
        assert!(ErrorCode::RoomFull.description().contains("maximum"));
        assert!(ErrorCode::RateLimitExceeded
            .description()
            .contains("Too many requests"));
        assert!(ErrorCode::AuthorityConflict
            .description()
            .contains("already claimed"));
    }

    #[test]
    fn test_serialization_unchanged() {
        // Ensure adding descriptions doesn't change serialization
        let error = ErrorCode::RoomNotFound;
        let json = serde_json::to_string(&error).unwrap();
        assert_eq!(json, "\"ROOM_NOT_FOUND\"");
    }

    #[test]
    fn invalid_delivery_class_uses_the_append_only_wire_token() {
        let encoded = serde_json::to_string(&ErrorCode::InvalidDeliveryClass).unwrap();
        assert_eq!(encoded, "\"INVALID_DELIVERY_CLASS\"");
        assert_eq!(
            serde_json::from_str::<ErrorCode>(&encoded).unwrap(),
            ErrorCode::InvalidDeliveryClass
        );
    }
}
