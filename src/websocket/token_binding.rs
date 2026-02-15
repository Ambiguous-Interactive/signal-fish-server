use crate::protocol::ClientMessage;
use crate::protocol::ErrorCode;
use crate::security::{
    derive_session_secret, ActiveTokenBinding, ClientCertificateFingerprint, TokenBindingError,
    TokenBindingProof,
};
use axum::http::header::{SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use std::fmt;

#[derive(Clone)]
pub(super) struct TokenBindingHandshake {
    pub(super) verifier: ActiveTokenBinding,
    pub(super) fingerprint: Option<ClientCertificateFingerprint>,
}

pub(super) fn parse_client_message(
    raw_text: &str,
    binding: Option<&TokenBindingHandshake>,
) -> Result<ClientMessage, TokenBindingViolation> {
    if let Some(binding) = binding {
        let mut value: Value =
            serde_json::from_str(raw_text).map_err(TokenBindingViolation::InvalidJson)?;
        let obj = value
            .as_object_mut()
            .ok_or(TokenBindingViolation::MalformedEnvelope)?;
        let proof_value = obj
            .remove("token_binding")
            .ok_or(TokenBindingViolation::MissingProof)?;
        let proof: TokenBindingProof =
            serde_json::from_value(proof_value).map_err(TokenBindingViolation::InvalidProof)?;
        let canonical_payload =
            serde_json::to_vec(&value).map_err(TokenBindingViolation::Canonicalization)?;
        binding
            .verifier
            .verify(
                &proof,
                &canonical_payload,
                binding
                    .fingerprint
                    .as_ref()
                    .map(|fp| fp.fingerprint.as_ref()),
            )
            .map_err(TokenBindingViolation::Verification)?;
        serde_json::from_value(value).map_err(TokenBindingViolation::InvalidJson)
    } else {
        serde_json::from_str(raw_text).map_err(TokenBindingViolation::InvalidJson)
    }
}

pub(super) fn client_requested_subprotocol(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .any(|token| token.eq_ignore_ascii_case(expected))
        })
        .unwrap_or(false)
}

#[allow(clippy::result_large_err)]
pub(super) fn negotiate_token_binding(
    cfg: &crate::config::TokenBindingConfig,
    client_offered: bool,
    headers: &HeaderMap,
    fingerprint: Option<&ClientCertificateFingerprint>,
) -> Result<Option<TokenBindingHandshake>, Response> {
    if !cfg.enabled {
        return Ok(None);
    }

    if cfg.require_client_fingerprint && fingerprint.is_none() {
        tracing::warn!("Token binding requires client fingerprint but none was provided");
        return Err((
            StatusCode::UNAUTHORIZED,
            "client certificate fingerprint required",
        )
            .into_response());
    }

    if cfg.required && !client_offered {
        tracing::warn!("Client did not request the token binding subprotocol, rejecting");
        return Err((
            StatusCode::BAD_REQUEST,
            "token binding subprotocol required",
        )
            .into_response());
    }

    if !cfg.required && !client_offered {
        return Ok(None);
    }

    let Some(raw_key) = headers
        .get(SEC_WEBSOCKET_KEY)
        .and_then(|value| value.to_str().ok())
    else {
        tracing::warn!("Missing Sec-WebSocket-Key header on token-bound connection");
        return Err((StatusCode::BAD_REQUEST, "Sec-WebSocket-Key header missing").into_response());
    };

    let secret = match derive_session_secret(raw_key) {
        Ok(secret) => secret,
        Err(err) => {
            tracing::warn!(error = %err, "Failed to derive token binding session key");
            return Err(
                (StatusCode::BAD_REQUEST, "invalid token binding handshake").into_response()
            );
        }
    };

    Ok(Some(TokenBindingHandshake {
        verifier: ActiveTokenBinding::new(secret, cfg.scheme, cfg.require_client_fingerprint),
        fingerprint: fingerprint.cloned(),
    }))
}

#[derive(Debug)]
pub(super) enum TokenBindingViolation {
    InvalidJson(serde_json::Error),
    MalformedEnvelope,
    MissingProof,
    InvalidProof(serde_json::Error),
    Canonicalization(serde_json::Error),
    Verification(TokenBindingError),
}

impl TokenBindingViolation {
    pub(super) fn user_message(&self) -> &'static str {
        match self {
            Self::InvalidJson(_) => "Invalid client message",
            Self::MalformedEnvelope => "Malformed client message",
            Self::MissingProof => "Token binding proof missing",
            Self::InvalidProof(_) => "Invalid token binding proof",
            Self::Canonicalization(_) => "Unable to normalize client message",
            Self::Verification(
                TokenBindingError::MissingClientFingerprint
                | TokenBindingError::MissingServerFingerprint,
            ) => "Client fingerprint required",
            Self::Verification(
                TokenBindingError::InvalidSignatureEncoding(_)
                | TokenBindingError::InvalidSignature,
            ) => "Invalid token binding signature",
            Self::Verification(TokenBindingError::FingerprintMismatch) => {
                "Client fingerprint mismatch"
            }
            Self::Verification(
                TokenBindingError::MissingHandshakeKey | TokenBindingError::InvalidHandshakeKey,
            ) => "Handshake metadata missing",
            Self::Verification(TokenBindingError::UnsupportedScheme(_)) => {
                "Unsupported token binding scheme"
            }
        }
    }

    pub(super) fn error_code(&self) -> ErrorCode {
        match self {
            Self::InvalidJson(_) | Self::MalformedEnvelope | Self::Canonicalization(_) => {
                ErrorCode::InvalidInput
            }
            _ => ErrorCode::Unauthorized,
        }
    }

    pub(super) fn should_disconnect(&self) -> bool {
        !matches!(self, Self::InvalidJson(_))
    }
}

impl fmt::Display for TokenBindingViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(err) => write!(f, "invalid json: {err}"),
            Self::MalformedEnvelope => write!(f, "message is not an object"),
            Self::MissingProof => write!(f, "missing token_binding section"),
            Self::InvalidProof(err) => {
                write!(f, "token_binding value is invalid: {err}")
            }
            Self::Canonicalization(err) => {
                write!(f, "failed to canonicalize payload: {err}")
            }
            Self::Verification(err) => {
                write!(f, "token binding verification failed: {err}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClientMessage;
    use crate::security::token_binding::TokenBindingScheme;
    use crate::security::CLIENT_FINGERPRINT_HEADER_CANDIDATES;
    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use hmac::{Hmac, Mac};
    use serde_json::json;
    use sha2::Sha256;
    use std::sync::Arc;

    fn signed_client_message(
        secret: &[u8],
        message: &ClientMessage,
        fingerprint: Option<&str>,
    ) -> String {
        let mut value = serde_json::to_value(message).expect("serialize test message");
        let canonical = serde_json::to_vec(&value).expect("serialize canonical payload");
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret).expect("create mac");
        mac.update(&canonical);
        if let Some(fp) = fingerprint {
            mac.update(fp.as_bytes());
        }
        let mut proof = json!({"scheme": "sec_websocket_key_sha256", "signature": BASE64_STANDARD.encode(mac.finalize().into_bytes())});
        if let (Some(fp), Some(obj)) = (fingerprint, proof.as_object_mut()) {
            obj.insert("fingerprint".to_string(), json!(fp));
        }
        if let Value::Object(ref mut map) = value {
            map.insert("token_binding".to_string(), proof);
        }
        value.to_string()
    }

    fn handshake_with_secret(
        secret: Arc<[u8]>,
        require_fp: bool,
        fingerprint: Option<&str>,
    ) -> TokenBindingHandshake {
        let fp_struct = fingerprint.map(|fp| ClientCertificateFingerprint {
            fingerprint: Arc::<str>::from(fp.to_owned()),
            source_header: CLIENT_FINGERPRINT_HEADER_CANDIDATES[0],
        });
        TokenBindingHandshake {
            verifier: ActiveTokenBinding::new(
                secret,
                TokenBindingScheme::SecWebsocketKeySha256,
                require_fp,
            ),
            fingerprint: fp_struct,
        }
    }

    #[test]
    fn token_binding_accepts_signed_payload() {
        let secret: Arc<[u8]> = Arc::from(b"0123456789abcdef".to_vec().into_boxed_slice());
        let handshake = handshake_with_secret(secret.clone(), false, None);
        let raw = signed_client_message(secret.as_ref(), &ClientMessage::Ping, None);
        let parsed = parse_client_message(&raw, Some(&handshake)).expect("valid token binding");
        assert!(matches!(parsed, ClientMessage::Ping));
    }

    #[test]
    fn token_binding_rejects_invalid_signature() {
        let secret: Arc<[u8]> = Arc::from(b"0123456789abcdef".to_vec().into_boxed_slice());
        let handshake = handshake_with_secret(secret, false, None);
        let mut value = serde_json::to_value(&ClientMessage::Ping).unwrap();
        if let Value::Object(ref mut map) = value {
            map.insert(
                "token_binding".to_string(),
                json!({"scheme":"sec_websocket_key_sha256","signature":"AAAA"}),
            );
        }
        let raw = value.to_string();
        assert!(matches!(
            parse_client_message(&raw, Some(&handshake)),
            Err(TokenBindingViolation::Verification(
                TokenBindingError::InvalidSignature
            ))
        ));
    }

    #[test]
    fn token_binding_enforces_fingerprint_when_required() {
        let secret: Arc<[u8]> = Arc::from(b"abcdef0123456789".to_vec().into_boxed_slice());
        let fingerprint = "sha256/abcdef";
        let handshake = handshake_with_secret(secret.clone(), true, Some(fingerprint));
        let raw = signed_client_message(secret.as_ref(), &ClientMessage::Ping, Some(fingerprint));
        assert!(parse_client_message(&raw, Some(&handshake)).is_ok());

        let handshake_missing = handshake_with_secret(secret.clone(), true, Some(fingerprint));
        let raw_missing = signed_client_message(secret.as_ref(), &ClientMessage::Ping, None);
        assert!(matches!(
            parse_client_message(&raw_missing, Some(&handshake_missing)),
            Err(TokenBindingViolation::Verification(
                TokenBindingError::MissingClientFingerprint
            ))
        ));
    }
}
