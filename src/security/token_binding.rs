use std::fmt;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;

pub const TOKEN_BINDING_VERSION: u8 = 2;
pub(crate) const TOKEN_BINDING_SUBPROTOCOL_V2: &str = "signalfish.tokenbinding.v2";
/// Largest exact integer shared by JSON/ECMAScript implementations.
pub const TOKEN_BINDING_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const TOKEN_BINDING_JSON_DOMAIN: &[u8] = b"signalfish.tokenbinding.v2\0json\0";
pub const TOKEN_BINDING_BINARY_DOMAIN: &[u8] = b"signalfish.tokenbinding.v2\0binary\0";
const TOKEN_BINDING_HKDF_INFO: &[u8] = b"signalfish.tokenbinding.v2/session-key";

/// Whether a configured subprotocol name can carry the v2 wire contract.
///
/// Deployments may use an application-specific alias, but names in Signal
/// Fish's reserved namespace identify a concrete protocol version and must not
/// relabel the v2 challenge/proof exchange as v1 or a future version.
#[must_use]
pub(crate) fn token_binding_subprotocol_is_v2_compatible(subprotocol: &str) -> bool {
    let token = subprotocol.trim();
    !token
        .to_ascii_lowercase()
        .starts_with("signalfish.tokenbinding.")
        || token == TOKEN_BINDING_SUBPROTOCOL_V2
}

/// Supported token-binding signature schemes.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TokenBindingScheme {
    /// HKDF over the WebSocket handshake key and a server-generated nonce.
    #[default]
    ServerNonceHkdfSha256,
    /// Protocol-v1 compatibility token. It remains deserializable so old
    /// configuration receives a precise validation error, but cannot be
    /// enabled because a client can replay all of its inputs on a new socket.
    SecWebsocketKeySha256,
}

impl TokenBindingScheme {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ServerNonceHkdfSha256 => "server_nonce_hkdf_sha256",
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
        if token.eq_ignore_ascii_case("server_nonce_hkdf_sha256")
            || token.eq_ignore_ascii_case("ServerNonceHkdfSha256")
        {
            Ok(Self::ServerNonceHkdfSha256)
        } else if token.eq_ignore_ascii_case("sec_websocket_key_sha256")
            || token.eq_ignore_ascii_case("SecWebsocketKeySha256")
        {
            Ok(Self::SecWebsocketKeySha256)
        } else {
            Err(serde::de::Error::custom(format!(
                "invalid token binding scheme '{raw}', expected: server_nonce_hkdf_sha256"
            )))
        }
    }
}

impl fmt::Display for TokenBindingScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Server-fresh material sent immediately after a token-bound upgrade.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenBindingChallenge {
    pub version: u8,
    pub scheme: TokenBindingScheme,
    pub nonce: String,
    pub first_sequence: u64,
}

/// Proof object embedded in every token-bound client message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBindingProof {
    pub version: u8,
    pub scheme: TokenBindingScheme,
    pub sequence: u64,
    pub signature: String,
    #[serde(default)]
    pub fingerprint: Option<String>,
}

/// Versioned binary client-frame envelope. The payload is the exact legacy
/// binary game-data bytes; only token-bound connections use this outer frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBoundBinaryFrame {
    pub token_binding: TokenBindingProof,
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum TokenBindingError {
    #[error("missing Sec-WebSocket-Key header")]
    MissingHandshakeKey,
    #[error("invalid Sec-WebSocket-Key header")]
    InvalidHandshakeKey,
    #[error("unable to derive token binding key")]
    KeyDerivation,
    #[error("unsupported token binding scheme: {0:?}")]
    UnsupportedScheme(TokenBindingScheme),
    #[error("unsupported token binding proof version: {0}")]
    UnsupportedVersion(u8),
    #[error("invalid token binding sequence: expected {expected}, received {received}")]
    InvalidSequence { expected: u64, received: u64 },
    #[error("token binding sequence space exhausted")]
    SequenceExhausted,
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

/// Per-connection token-binding state derived from the client handshake key and
/// the server's fresh challenge. The sequence frontier is shared by JSON and
/// binary frames, making replay and reordering fail before application logic.
#[derive(Debug, Clone)]
pub struct ActiveTokenBinding {
    secret: Arc<[u8]>,
    expected_sequence: Arc<Mutex<u128>>,
    pub scheme: TokenBindingScheme,
    pub require_fingerprint: bool,
}

impl ActiveTokenBinding {
    pub fn new(secret: Arc<[u8]>, scheme: TokenBindingScheme, require_fingerprint: bool) -> Self {
        Self {
            secret,
            expected_sequence: Arc::new(Mutex::new(1)),
            scheme,
            require_fingerprint,
        }
    }

    pub fn secret(&self) -> &[u8] {
        self.secret.as_ref()
    }

    pub fn verify(
        &self,
        proof: &TokenBindingProof,
        domain: &[u8],
        payload: &[u8],
        fingerprint: Option<&str>,
    ) -> Result<(), TokenBindingError> {
        if proof.version != TOKEN_BINDING_VERSION {
            return Err(TokenBindingError::UnsupportedVersion(proof.version));
        }
        if proof.scheme != self.scheme {
            return Err(TokenBindingError::UnsupportedScheme(proof.scheme));
        }
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

        let mut expected_sequence = self
            .expected_sequence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let expected = u64::try_from(*expected_sequence)
            .ok()
            .filter(|expected| *expected <= TOKEN_BINDING_MAX_SAFE_INTEGER)
            .ok_or(TokenBindingError::SequenceExhausted)?;
        if proof.sequence != expected {
            return Err(TokenBindingError::InvalidSequence {
                expected,
                received: proof.sequence,
            });
        }

        verify_hmac(self.secret(), domain, payload, proof)?;
        *expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or(TokenBindingError::SequenceExhausted)?;
        Ok(())
    }
}

fn verify_hmac(
    secret: &[u8],
    domain: &[u8],
    payload: &[u8],
    proof: &TokenBindingProof,
) -> Result<(), TokenBindingError> {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| TokenBindingError::InvalidHandshakeKey)?;
    mac.update(domain);
    mac.update(&proof.sequence.to_be_bytes());
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

fn decode_handshake_key(raw_key: &str) -> Result<Vec<u8>, TokenBindingError> {
    if raw_key.trim().is_empty() {
        return Err(TokenBindingError::MissingHandshakeKey);
    }
    let decoded = BASE64_STANDARD
        .decode(raw_key.as_bytes())
        .map_err(|_| TokenBindingError::InvalidHandshakeKey)?;
    if decoded.len() != 16 {
        return Err(TokenBindingError::InvalidHandshakeKey);
    }
    Ok(decoded)
}

/// Legacy derivation retained for source compatibility. Negotiation rejects
/// this scheme because it contains no server freshness.
pub fn derive_session_secret(raw_key: &str) -> Result<Arc<[u8]>, TokenBindingError> {
    Ok(Arc::from(decode_handshake_key(raw_key)?.into_boxed_slice()))
}

/// Derive a 256-bit connection key from both endpoints' handshake material.
pub fn derive_server_nonce_secret(
    raw_key: &str,
    server_nonce: &[u8],
) -> Result<Arc<[u8]>, TokenBindingError> {
    let client_key = decode_handshake_key(raw_key)?;
    if server_nonce.len() != 32 {
        return Err(TokenBindingError::KeyDerivation);
    }
    let hkdf = Hkdf::<Sha256>::new(Some(server_nonce), &client_key);
    let mut output = [0_u8; 32];
    hkdf.expand(TOKEN_BINDING_HKDF_INFO, &mut output)
        .map_err(|_| TokenBindingError::KeyDerivation)?;
    Ok(Arc::from(output))
}
