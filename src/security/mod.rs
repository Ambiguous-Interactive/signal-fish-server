/// Security and cryptography utilities
///
/// This module provides security-related functionality including:
/// - TLS/mTLS support (gated behind `tls` feature)
/// - Envelope encryption (AES-GCM)
/// - Token binding and channel security
pub mod crypto;
pub mod tls;
pub mod token_binding; // Always include tls module (ClientCertificateFingerprint is always needed)

pub use crypto::EnvelopeEncryptor;
pub use token_binding::{
    derive_session_secret, ActiveTokenBinding, TokenBindingError, TokenBindingProof,
};

// ClientCertificateFingerprint and CLIENT_FINGERPRINT_HEADER_CANDIDATES are always available
pub use tls::{ClientCertificateFingerprint, CLIENT_FINGERPRINT_HEADER_CANDIDATES};

#[cfg(feature = "tls")]
pub use tls::build_rustls_config;
