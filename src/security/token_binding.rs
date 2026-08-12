use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::fmt;
use thiserror::Error;

/// Supported token binding signature schemes.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TokenBindingScheme {
    #[default]
    SecWebsocketKeySha256,
}

impl TokenBindingScheme {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::SecWebsocketKeySha256 => "sec_websocket_key_sha256",
        }
    }
}

impl<'de> Deserialize<'de> for TokenBindingScheme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        let token = raw.trim();

        if token.eq_ignore_ascii_case("sec_websocket_key_sha256")
            || token.eq_ignore_ascii_case("SecWebsocketKeySha256")
        {
            Ok(Self::SecWebsocketKeySha256)
        } else {
            Err(serde::de::Error::custom(format!(
                "invalid token binding scheme '{raw}', expected: sec_websocket_key_sha256"
            )))
        }
    }
}

impl fmt::Display for TokenBindingScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Proof object embedded in every token-bound JSON client message.
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
            if !super::constant_time_eq(expected, provided) {
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
