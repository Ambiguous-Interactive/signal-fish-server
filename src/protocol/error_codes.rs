use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Error codes for structured error handling
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Archive, RkyvSerialize, RkyvDeserialize,
)]
#[rkyv(compare(PartialEq))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // Authentication errors (1xxx)
    Unauthorized,
    InvalidToken,
    AuthenticationRequired,
    InvalidAppId,
    AppIdExpired,
    AppIdRevoked,
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
    ServiceUnavailable,
}

impl ErrorCode {
    /// Returns a human-readable description of this error code.
    ///
    /// This method provides actionable error messages that SDK developers
    /// can display to end users or use for debugging.
    pub fn description(&self) -> &'static str {
        match self {
            // Authentication errors (1xxx)
            Self::Unauthorized => {
                "Access denied. Authentication credentials are missing or invalid."
            }
            Self::InvalidToken => {
                "The authentication token is invalid, malformed, or has expired. Please obtain a new token."
            }
            Self::AuthenticationRequired => {
                "This operation requires authentication. Please provide valid credentials."
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
                "Application ID is required but was not provided. Include your app ID in the request."
            }
            Self::AuthenticationTimeout => {
                "Authentication took too long to complete. Please try again."
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
        ];

        for error_code in &error_codes {
            let description = error_code.description();
            assert!(
                !description.is_empty(),
                "ErrorCode::{:?} has empty description",
                error_code
            );
            assert!(
                description.len() > 10,
                "ErrorCode::{:?} has suspiciously short description: '{}'",
                error_code,
                description
            );
        }
    }

    #[test]
    fn test_display_uses_description() {
        let error = ErrorCode::RoomNotFound;
        let display_output = format!("{}", error);
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
}
