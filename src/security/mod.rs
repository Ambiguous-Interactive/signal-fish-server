/// Security and cryptography utilities
///
/// This module provides security-related functionality including:
/// - TLS/mTLS support (gated behind `tls` feature)
/// - Envelope encryption (AES-GCM)
/// - Token binding and channel security
pub mod crypto;
pub mod origin;
pub mod tls;
pub mod token_binding; // Always include tls module (ClientCertificateFingerprint is always needed)
pub mod turn_credentials;

pub use crypto::EnvelopeEncryptor;
pub use origin::{OriginPolicy, OriginPolicyError};
pub use token_binding::{
    derive_server_nonce_secret, derive_session_secret, ActiveTokenBinding, TokenBindingChallenge,
    TokenBindingError, TokenBindingProof, TokenBoundBinaryFrame, TOKEN_BINDING_BINARY_DOMAIN,
    TOKEN_BINDING_JSON_DOMAIN, TOKEN_BINDING_MAX_SAFE_INTEGER, TOKEN_BINDING_VERSION,
};
pub use turn_credentials::{
    build_ice_servers, mint_turn_credentials, turn_expiry_unix, TurnCredentials,
};

// ClientCertificateFingerprint and CLIENT_FINGERPRINT_HEADER_CANDIDATES are always available
pub use tls::{ClientCertificateFingerprint, CLIENT_FINGERPRINT_HEADER_CANDIDATES};

#[cfg(feature = "tls")]
pub use tls::{build_rustls_config, VerifiedClientCertificate, VerifiedClientCertificateAcceptor};

/// Compare two secrets without leaking, via timing, how many leading bytes
/// matched.
///
/// This is the single source of truth for secret comparison across the crate —
/// the metrics bearer token (`websocket::metrics`), reconnection tokens
/// (`reconnection`), and token-binding proofs (`security::token_binding`) all
/// route through it, so the constant-time guarantee can never silently drift
/// between call sites.
///
/// Length is intentionally **not** treated as secret: an early length check
/// short-circuits. Every secret compared here is fixed-width for a given
/// deployment (UUID tokens, hex/base64 bearer tokens, HMAC signatures), so the
/// length is not attacker-controlled information, and revealing "wrong length"
/// in variable time is the standard, accepted trade-off (`subtle` itself
/// documents length as non-secret).
#[must_use]
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && a.as_bytes().ct_eq(b.as_bytes()).into()
}

#[cfg(test)]
mod constant_time_eq_tests {
    use super::constant_time_eq;

    #[test]
    fn equal_strings_match() {
        assert!(constant_time_eq("s3cr3t-token", "s3cr3t-token"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn different_content_same_length_does_not_match() {
        assert!(!constant_time_eq("abcdef", "abcdeg"));
        assert!(!constant_time_eq("abcdef", "zbcdef"));
    }

    #[test]
    fn different_length_does_not_match() {
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("abcd", "abc"));
        assert!(!constant_time_eq("token", ""));
    }
}
