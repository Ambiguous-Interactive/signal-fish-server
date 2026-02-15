use thiserror::Error;

/// Authentication errors that may be returned during app credential or ID
/// validation.
///
/// The `AppIdExpired`, `AppIdRevoked`, and `AppIdSuspended` variants are
/// reserved for future extension (e.g., app status management, admin-controlled
/// app suspension, or external auth backends). They are not currently returned
/// by the in-memory `AuthMiddleware` but are kept so that client error-handling
/// code paths remain stable when those features are introduced.
#[derive(Debug, Error)]
pub enum AuthError {
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("Rate limit exceeded")]
    RateLimitExceeded,
    #[error("Invalid app ID")]
    InvalidAppId,
    /// Reserved for future use: returned when an app ID has passed its
    /// expiration date in a backend that tracks app lifecycles.
    #[error("App ID expired")]
    AppIdExpired,
    /// Reserved for future use: returned when an admin has explicitly revoked
    /// an app ID.
    #[error("App ID revoked")]
    AppIdRevoked,
    /// Reserved for future use: returned when an app ID has been temporarily
    /// suspended by an admin.
    #[error("App ID suspended")]
    AppIdSuspended,
}
