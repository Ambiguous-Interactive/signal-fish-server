use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;

/// Supported token binding signature schemes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TokenBindingScheme {
    #[default]
    SecWebsocketKeySha256,
}

/// Proof object embedded in every token-bound client frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBindingProof {
    pub scheme: TokenBindingScheme,
    pub signature: String,
    #[serde(default)]
    pub fingerprint: Option<String>,
}

/// Errors encountered when deriving or validating token binding proofs.
#[derive(Debug, Error)]
pub enum TokenBindingError {
    #[error("missing Sec-WebSocket-Key header")]
    MissingHandshakeKey,
    #[error("invalid Sec-WebSocket-Key header")]
    InvalidHandshakeKey,
    #[error("unsupported token binding scheme: {0:?}")]
    UnsupportedScheme(TokenBindingScheme),
    #[error("invalid token binding signature encoding: {0}")]
    InvalidSignatureEncoding(String),
    #[error("token binding signature mismatch")]
    InvalidSignature,
    #[error("client fingerprint required but missing")]
    MissingClientFingerprint,
    #[error("client fingerprint mismatch")]
    FingerprintMismatch,
    #[error("client fingerprint metadata missing on server")]
    MissingServerFingerprint,
}

/// Per-connection token binding state (derived from the handshake).
#[derive(Debug, Clone)]
pub struct ActiveTokenBinding {
    secret: Arc<[u8]>,
    pub scheme: TokenBindingScheme,
    pub require_fingerprint: bool,
}

impl ActiveTokenBinding {
    pub fn new(secret: Arc<[u8]>, scheme: TokenBindingScheme, require_fingerprint: bool) -> Self {
        Self {
            secret,
            scheme,
            require_fingerprint,
        }
    }

    pub fn secret(&self) -> &[u8] {
        self.secret.as_ref()
    }

    /// Verify an incoming proof against the canonical payload bytes.
    pub fn verify(
        &self,
        proof: &TokenBindingProof,
        canonical_payload: &[u8],
        fingerprint: Option<&str>,
    ) -> Result<(), TokenBindingError> {
        if self.require_fingerprint {
            let expected = fingerprint.ok_or(TokenBindingError::MissingServerFingerprint)?;
            let provided = proof
                .fingerprint
                .as_deref()
                .ok_or(TokenBindingError::MissingClientFingerprint)?;
            if !constant_time_eq(expected, provided) {
                return Err(TokenBindingError::FingerprintMismatch);
            }
        }

        if proof.scheme != self.scheme {
            return Err(TokenBindingError::UnsupportedScheme(proof.scheme));
        }

        match proof.scheme {
            TokenBindingScheme::SecWebsocketKeySha256 => {
                verify_hmac(self.secret(), canonical_payload, proof)
            }
        }
    }
}

fn verify_hmac(
    secret: &[u8],
    payload: &[u8],
    proof: &TokenBindingProof,
) -> Result<(), TokenBindingError> {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| TokenBindingError::InvalidHandshakeKey)?;
    mac.update(payload);
    if let Some(fingerprint) = proof.fingerprint.as_deref() {
        mac.update(fingerprint.as_bytes());
    }

    let signature = BASE64_STANDARD
        .decode(proof.signature.as_bytes())
        .map_err(|err| TokenBindingError::InvalidSignatureEncoding(err.to_string()))?;
    mac.verify_slice(&signature)
        .map_err(|_| TokenBindingError::InvalidSignature)
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    // Simple constant-time comparison over ASCII hex strings.
    let mut diff = 0u8;
    for (a, b) in left.bytes().zip(right.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Derive the per-connection secret from the WebSocket handshake key.
pub fn derive_session_secret(raw_key: &str) -> Result<Arc<[u8]>, TokenBindingError> {
    if raw_key.trim().is_empty() {
        return Err(TokenBindingError::MissingHandshakeKey);
    }
    let decoded = BASE64_STANDARD
        .decode(raw_key.as_bytes())
        .map_err(|_| TokenBindingError::InvalidHandshakeKey)?;
    if decoded.len() != 16 {
        return Err(TokenBindingError::InvalidHandshakeKey);
    }
    Ok(Arc::from(decoded.into_boxed_slice()))
}
