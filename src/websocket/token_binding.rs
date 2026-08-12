use crate::protocol::ClientMessage;
use crate::protocol::ErrorCode;
use crate::security::{
    derive_server_nonce_secret, ActiveTokenBinding, ClientCertificateFingerprint,
    TokenBindingChallenge, TokenBindingError, TokenBindingProof, TokenBoundBinaryFrame,
    TOKEN_BINDING_BINARY_DOMAIN, TOKEN_BINDING_JSON_DOMAIN, TOKEN_BINDING_MAX_SAFE_INTEGER,
    TOKEN_BINDING_VERSION,
};
use axum::http::header::{SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::Value;
use std::fmt;

fn canonical_json(value: &Value) -> serde_json::Result<Vec<u8>> {
    fn write(value: &Value, output: &mut Vec<u8>) -> serde_json::Result<()> {
        match value {
            Value::Null => output.extend_from_slice(b"null"),
            Value::Bool(true) => output.extend_from_slice(b"true"),
            Value::Bool(false) => output.extend_from_slice(b"false"),
            Value::Number(number) => {
                let rendered = number
                    .as_i64()
                    .map(|value| value.to_string())
                    .or_else(|| number.as_u64().map(|value| value.to_string()))
                    .ok_or_else(|| {
                        <serde_json::Error as serde::ser::Error>::custom(
                            "non-integer JSON number reached token-binding canonicalization",
                        )
                    })?;
                output.extend_from_slice(rendered.as_bytes());
            }
            Value::String(string) => serde_json::to_writer(output, string)?,
            Value::Array(values) => {
                output.push(b'[');
                for (index, item) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    write(item, output)?;
                }
                output.push(b']');
            }
            Value::Object(values) => {
                let mut properties: Vec<_> = values.iter().collect();
                properties.sort_by_cached_key(|(key, _)| key.encode_utf16().collect::<Vec<_>>());
                output.push(b'{');
                for (index, (key, item)) in properties.into_iter().enumerate() {
                    if index > 0 {
                        output.push(b',');
                    }
                    serde_json::to_writer(&mut *output, key)?;
                    output.push(b':');
                    write(item, output)?;
                }
                output.push(b'}');
            }
        }
        Ok(())
    }

    let mut output = Vec::with_capacity(128);
    write(value, &mut output)?;
    Ok(output)
}

#[derive(Clone)]
pub(super) struct TokenBindingHandshake {
    pub(super) verifier: ActiveTokenBinding,
    pub(super) fingerprint: Option<ClientCertificateFingerprint>,
    pub(super) challenge: TokenBindingChallenge,
}

struct UniqueJsonValue(Value);

impl<'de> Deserialize<'de> for UniqueJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

struct UniqueJsonVisitor;

impl<'de> Visitor<'de> for UniqueJsonVisitor {
    type Value = UniqueJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object members")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueJsonValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.unsigned_abs() > TOKEN_BINDING_MAX_SAFE_INTEGER {
            return Err(E::custom(
                "JSON integer exceeds the interoperable safe range",
            ));
        }
        Ok(UniqueJsonValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value > TOKEN_BINDING_MAX_SAFE_INTEGER {
            return Err(E::custom(
                "JSON integer exceeds the interoperable safe range",
            ));
        }
        Ok(UniqueJsonValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let _ = value;
        Err(E::custom(
            "negative-zero, fractional, or exponent-form JSON numbers are not supported on token-bound connections",
        ))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueJsonValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJsonValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJsonValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(UniqueJsonValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(UniqueJsonValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = serde_json::Map::new();
        while let Some((key, UniqueJsonValue(value))) = entries.next_entry::<String, _>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON object member: {key}"
                )));
            }
            values.insert(key, value);
        }
        Ok(UniqueJsonValue(Value::Object(values)))
    }
}

pub(super) fn parse_client_message(
    raw_text: &str,
    binding: Option<&TokenBindingHandshake>,
) -> Result<ClientMessage, TokenBindingViolation> {
    if let Some(binding) = binding {
        let UniqueJsonValue(mut value) =
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
            canonical_json(&value).map_err(TokenBindingViolation::Canonicalization)?;
        binding
            .verifier
            .verify(
                &proof,
                TOKEN_BINDING_JSON_DOMAIN,
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

pub(super) fn parse_binary_message(
    raw: &[u8],
    binding: &TokenBindingHandshake,
) -> Result<Vec<u8>, TokenBindingViolation> {
    let frame: TokenBoundBinaryFrame =
        rmp_serde::from_slice(raw).map_err(TokenBindingViolation::InvalidBinaryEnvelope)?;
    binding
        .verifier
        .verify(
            &frame.token_binding,
            TOKEN_BINDING_BINARY_DOMAIN,
            &frame.payload,
            binding
                .fingerprint
                .as_ref()
                .map(|fp| fp.fingerprint.as_ref()),
        )
        .map_err(TokenBindingViolation::Verification)?;
    Ok(frame.payload)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenBindingProtocolOffer {
    None,
    Supported,
    Unsupported,
}

pub(super) fn client_token_binding_offer(
    headers: &HeaderMap,
    expected: &str,
) -> TokenBindingProtocolOffer {
    let mut reserved_unsupported = false;
    for value in headers.get_all(SEC_WEBSOCKET_PROTOCOL) {
        let Some(raw) = value.to_str().ok() else {
            continue;
        };
        for token in raw
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            // RFC 6455 subprotocol tokens are case-sensitive. Prefer the
            // supported token if it appears in any repeated header field.
            if token == expected {
                return TokenBindingProtocolOffer::Supported;
            }
            if token
                .to_ascii_lowercase()
                .starts_with("signalfish.tokenbinding.")
            {
                reserved_unsupported = true;
            }
        }
    }
    if reserved_unsupported {
        TokenBindingProtocolOffer::Unsupported
    } else {
        TokenBindingProtocolOffer::None
    }
}

#[allow(clippy::result_large_err)]
pub(super) fn negotiate_token_binding(
    cfg: &crate::config::TokenBindingConfig,
    client_offered: bool,
    headers: &HeaderMap,
    fingerprint: Option<&ClientCertificateFingerprint>,
) -> Result<Option<TokenBindingHandshake>, Response> {
    if !cfg.enabled && (cfg.required || cfg.require_client_fingerprint) {
        tracing::error!(
            required = cfg.required,
            require_client_fingerprint = cfg.require_client_fingerprint,
            "Invalid token-binding configuration reached WebSocket negotiation"
        );
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid token binding configuration",
        )
            .into_response());
    }
    if !cfg.enabled {
        return Ok(None);
    }

    if !crate::security::token_binding::token_binding_subprotocol_is_v2_compatible(&cfg.subprotocol)
    {
        tracing::error!(
            subprotocol = %cfg.subprotocol,
            "Reserved token-binding subprotocol does not match the v2 wire contract"
        );
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid token binding subprotocol",
        )
            .into_response());
    }

    if cfg.require_client_fingerprint && fingerprint.is_none() {
        tracing::warn!("Token binding requires client fingerprint but none was provided");
        return Err((
            StatusCode::UNAUTHORIZED,
            "client certificate fingerprint required",
        )
            .into_response());
    }

    // Fingerprint binding is meaningless if a client can omit the subprotocol.
    // Keep this fail-closed even when a library embedder constructs an invalid
    // config directly and bypasses the process-level validation.
    if (cfg.required || cfg.require_client_fingerprint) && !client_offered {
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

    if cfg.scheme == crate::security::token_binding::TokenBindingScheme::SecWebsocketKeySha256 {
        tracing::error!("Insecure protocol-v1 token-binding scheme reached negotiation");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid token binding scheme",
        )
            .into_response());
    }

    let mut nonce = [0_u8; 32];
    if let Err(err) = getrandom::fill(&mut nonce) {
        tracing::error!(error = %err, "Failed to generate token-binding challenge");
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "token binding unavailable",
        )
            .into_response());
    }
    let secret = match derive_server_nonce_secret(raw_key, &nonce) {
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
        challenge: TokenBindingChallenge {
            version: TOKEN_BINDING_VERSION,
            scheme: cfg.scheme,
            nonce: base64::engine::general_purpose::STANDARD.encode(nonce),
            first_sequence: 1,
        },
    }))
}

#[derive(Debug)]
pub(super) enum TokenBindingViolation {
    InvalidJson(serde_json::Error),
    MalformedEnvelope,
    MissingProof,
    InvalidProof(serde_json::Error),
    InvalidBinaryEnvelope(rmp_serde::decode::Error),
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
            Self::InvalidBinaryEnvelope(_) => "Invalid token-bound binary frame",
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
            Self::Verification(TokenBindingError::KeyDerivation) => {
                "Token binding key derivation failed"
            }
            Self::Verification(
                TokenBindingError::UnsupportedVersion(_)
                | TokenBindingError::InvalidSequence { .. }
                | TokenBindingError::SequenceExhausted,
            ) => "Invalid token binding version or sequence",
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
            Self::InvalidBinaryEnvelope(err) => write!(f, "binary envelope is invalid: {err}"),
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
    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine};
    use hmac::{Hmac, KeyInit, Mac};
    use serde_json::json;
    use sha2::Sha256;
    use std::sync::Arc;

    fn proof(
        secret: &[u8],
        domain: &[u8],
        payload: &[u8],
        sequence: u64,
        fingerprint: Option<&str>,
    ) -> TokenBindingProof {
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret).expect("create mac");
        mac.update(domain);
        mac.update(&sequence.to_be_bytes());
        mac.update(payload);
        if let Some(fingerprint) = fingerprint {
            mac.update(fingerprint.as_bytes());
        }
        TokenBindingProof {
            version: TOKEN_BINDING_VERSION,
            scheme: TokenBindingScheme::ServerNonceHkdfSha256,
            sequence,
            signature: BASE64_STANDARD.encode(mac.finalize().into_bytes()),
            fingerprint: fingerprint.map(str::to_string),
        }
    }

    fn signed_client_message(
        secret: &[u8],
        message: &ClientMessage,
        sequence: u64,
        fingerprint: Option<&str>,
    ) -> String {
        let mut value = serde_json::to_value(message).expect("serialize test message");
        let canonical = canonical_json(&value).expect("canonicalize test message");
        let proof = serde_json::to_value(proof(
            secret,
            TOKEN_BINDING_JSON_DOMAIN,
            &canonical,
            sequence,
            fingerprint,
        ))
        .expect("serialize proof");
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
                TokenBindingScheme::ServerNonceHkdfSha256,
                require_fp,
            ),
            fingerprint: fp_struct,
            challenge: TokenBindingChallenge {
                version: TOKEN_BINDING_VERSION,
                scheme: TokenBindingScheme::ServerNonceHkdfSha256,
                nonce: BASE64_STANDARD.encode([7_u8; 32]),
                first_sequence: 1,
            },
        }
    }

    #[test]
    fn token_binding_accepts_signed_payload() {
        let secret: Arc<[u8]> = Arc::from(b"0123456789abcdef".to_vec().into_boxed_slice());
        let handshake = handshake_with_secret(secret.clone(), false, None);
        let raw = signed_client_message(secret.as_ref(), &ClientMessage::Ping, 1, None);
        let parsed = parse_client_message(&raw, Some(&handshake)).expect("valid token binding");
        assert!(matches!(parsed, ClientMessage::Ping));
    }

    #[test]
    fn token_binding_rfc8785_safe_integer_goldens_are_feature_independent() {
        for (input, expected) in [
            ("1", "1"),
            ("9007199254740991", "9007199254740991"),
            ("-9007199254740991", "-9007199254740991"),
        ] {
            let value: Value = serde_json::from_str(input).expect("numeric test vector parses");
            let canonical = canonical_json(&value).expect("numeric test vector canonicalizes");
            assert_eq!(
                std::str::from_utf8(&canonical).expect("canonical JSON is UTF-8"),
                expected,
                "RFC 8785 vector {input}"
            );
        }
    }

    #[test]
    fn token_binding_v2_cross_language_golden_is_stable() {
        let nonce: Vec<u8> = (0_u8..32).collect();
        let secret = derive_server_nonce_secret("MDEyMzQ1Njc4OWFiY2RlZg==", &nonce)
            .expect("derive golden connection key");
        let secret_hex: String = secret.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            secret_hex,
            "abb5860d3be9f16fad2763e718d0e9a038b7196fd136f24d62cb3ab6fc631da7"
        );

        let value: Value =
            serde_json::from_str(r#"{"zzz":{"y":2,"x":"é"},"type":"Ping","aaa":[3,1]}"#)
                .expect("parse golden payload");
        let canonical = canonical_json(&value).expect("canonicalize golden");
        assert_eq!(
            std::str::from_utf8(&canonical).expect("canonical JSON is UTF-8"),
            r#"{"aaa":[3,1],"type":"Ping","zzz":{"x":"é","y":2}}"#
        );
        let proof = proof(
            secret.as_ref(),
            TOKEN_BINDING_JSON_DOMAIN,
            &canonical,
            1,
            None,
        );
        assert_eq!(
            proof.signature,
            "HobFBjbmzHgNF/QoXXFpqNy5s4/InE7+tCYO56+Dqig="
        );
    }

    #[test]
    fn token_binding_rejects_invalid_signature() {
        let secret: Arc<[u8]> = Arc::from(b"0123456789abcdef".to_vec().into_boxed_slice());
        let handshake = handshake_with_secret(secret, false, None);
        let mut value = serde_json::to_value(&ClientMessage::Ping).unwrap();
        if let Value::Object(ref mut map) = value {
            map.insert(
                "token_binding".to_string(),
                json!({"version":2,"scheme":"server_nonce_hkdf_sha256","sequence":1,"signature":"AAAA"}),
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
    fn token_binding_rejects_duplicate_json_members_before_verification() {
        let secret: Arc<[u8]> = Arc::from([4_u8; 32]);
        let handshake = handshake_with_secret(Arc::clone(&secret), false, None);
        let signed = signed_client_message(secret.as_ref(), &ClientMessage::Ping, 1, None);
        let duplicated = signed.replacen(r#""type":"Ping""#, r#""type":"Ping","type":"Ping""#, 1);
        assert!(matches!(
            parse_client_message(&duplicated, Some(&handshake)),
            Err(TokenBindingViolation::InvalidJson(_))
        ));
    }

    #[test]
    fn token_binding_rejects_integers_outside_the_portable_json_range() {
        let secret: Arc<[u8]> = Arc::from([5_u8; 32]);
        for number in ["9007199254740992", "-9007199254740992"] {
            let handshake = handshake_with_secret(Arc::clone(&secret), false, None);
            let raw = format!(
                r#"{{"type":"GameData","data":{{"number":{number}}},"token_binding":{{"version":2,"scheme":"server_nonce_hkdf_sha256","sequence":1,"signature":"AAAA"}}}}"#
            );
            assert!(matches!(
                parse_client_message(&raw, Some(&handshake)),
                Err(TokenBindingViolation::InvalidJson(_))
            ));
        }
    }

    #[test]
    fn token_binding_rejects_non_integer_numeric_forms() {
        let secret: Arc<[u8]> = Arc::from([6_u8; 32]);
        for number in [
            "1.0",
            "1e0",
            "-0",
            "-0.0",
            "0.000001",
            "333333333.33333329",
            "1e30",
        ] {
            let handshake = handshake_with_secret(Arc::clone(&secret), false, None);
            let raw = format!(
                r#"{{"type":"GameData","data":{{"number":{number}}},"token_binding":{{"version":2,"scheme":"server_nonce_hkdf_sha256","sequence":1,"signature":"AAAA"}}}}"#
            );
            assert!(
                matches!(
                    parse_client_message(&raw, Some(&handshake)),
                    Err(TokenBindingViolation::InvalidJson(_))
                ),
                "token-bound numeric syntax must be portable: {number}"
            );
        }
    }

    #[test]
    fn token_binding_enforces_fingerprint_when_required() {
        let secret: Arc<[u8]> = Arc::from(b"abcdef0123456789".to_vec().into_boxed_slice());
        let fingerprint = "sha256/abcdef";
        let handshake = handshake_with_secret(secret.clone(), true, Some(fingerprint));
        let raw =
            signed_client_message(secret.as_ref(), &ClientMessage::Ping, 1, Some(fingerprint));
        assert!(parse_client_message(&raw, Some(&handshake)).is_ok());

        let handshake_missing = handshake_with_secret(secret.clone(), true, Some(fingerprint));
        let raw_missing = signed_client_message(secret.as_ref(), &ClientMessage::Ping, 1, None);
        assert!(matches!(
            parse_client_message(&raw_missing, Some(&handshake_missing)),
            Err(TokenBindingViolation::Verification(
                TokenBindingError::MissingClientFingerprint
            ))
        ));
    }

    #[test]
    fn token_binding_rejects_replay_and_cross_connection_reuse() {
        let handshake_key = "MDEyMzQ1Njc4OWFiY2RlZg==";
        let secret_a = derive_server_nonce_secret(handshake_key, &[1_u8; 32])
            .expect("derive first connection key");
        let secret_b = derive_server_nonce_secret(handshake_key, &[2_u8; 32])
            .expect("derive fresh connection key from the same client key");
        let handshake_a = handshake_with_secret(Arc::clone(&secret_a), false, None);
        let handshake_b = handshake_with_secret(secret_b, false, None);
        let raw = signed_client_message(secret_a.as_ref(), &ClientMessage::Ping, 1, None);
        assert!(parse_client_message(&raw, Some(&handshake_a)).is_ok());
        assert!(matches!(
            parse_client_message(&raw, Some(&handshake_a)),
            Err(TokenBindingViolation::Verification(
                TokenBindingError::InvalidSequence { .. }
            ))
        ));
        assert!(matches!(
            parse_client_message(&raw, Some(&handshake_b)),
            Err(TokenBindingViolation::Verification(
                TokenBindingError::InvalidSignature
            ))
        ));
    }

    #[test]
    fn token_binding_authenticates_binary_envelope() {
        let secret: Arc<[u8]> = Arc::from([3_u8; 32]);
        let handshake = handshake_with_secret(Arc::clone(&secret), false, None);
        let payload = vec![0x81, 0xa4, b'd', b'a', b't', b'a', 0x01];
        let frame = TokenBoundBinaryFrame {
            token_binding: proof(
                secret.as_ref(),
                TOKEN_BINDING_BINARY_DOMAIN,
                &payload,
                1,
                None,
            ),
            payload: payload.clone(),
        };
        let encoded = rmp_serde::to_vec_named(&frame).expect("encode binary envelope");
        assert_eq!(
            parse_binary_message(&encoded, &handshake).expect("verify binary envelope"),
            payload
        );
    }

    #[test]
    fn token_binding_binary_cross_language_golden_is_stable() {
        fn decode_hex(raw: &str) -> Vec<u8> {
            raw.as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    let digit = |byte: u8| match byte {
                        b'0'..=b'9' => byte - b'0',
                        b'a'..=b'f' => byte - b'a' + 10,
                        _ => 0xff,
                    };
                    let high = digit(pair[0]);
                    let low = digit(pair[1]);
                    assert!(high < 16 && low < 16, "golden hex must be valid");
                    high * 16 + low
                })
                .collect()
        }

        let nonce: Vec<u8> = (0_u8..32).collect();
        let secret = derive_server_nonce_secret("MDEyMzQ1Njc4OWFiY2RlZg==", &nonce)
            .expect("derive golden binary connection key");
        let payload = decode_hex("81a46461746101");
        let frame = TokenBoundBinaryFrame {
            token_binding: proof(
                secret.as_ref(),
                TOKEN_BINDING_BINARY_DOMAIN,
                &payload,
                2,
                None,
            ),
            payload,
        };
        assert_eq!(
            frame.token_binding.signature,
            "7/4RSNk/Euc4JnPJqlrMVDqp1l8oLXl+A8jAqDZIt1A="
        );
        let encoded = rmp_serde::to_vec_named(&frame).expect("encode binary golden envelope");
        let expected = concat!(
            "82ad746f6b656e5f62696e64696e6785a776657273696f6e02a6736368656d65",
            "b87365727665725f6e6f6e63655f686b64665f736861323536a873657175656e",
            "636502a97369676e6174757265d92c372f3452534e6b2f457563344a6e504a71",
            "6c724d56447170316c386f4c586c2b41386a4171445a497431413dab66696e67",
            "65727072696e74c0a77061796c6f6164c40781a46461746101"
        );
        assert_eq!(encoded, decode_hex(expected));
    }

    #[test]
    fn fingerprint_binding_cannot_be_bypassed_by_omitting_subprotocol() {
        let config = crate::config::TokenBindingConfig {
            enabled: true,
            required: false,
            require_client_fingerprint: true,
            ..crate::config::TokenBindingConfig::default()
        };
        let fingerprint = ClientCertificateFingerprint {
            fingerprint: Arc::from("verified-fingerprint"),
            source_header: "rustls-peer-certificate",
        };

        let rejection_status =
            negotiate_token_binding(&config, false, &HeaderMap::new(), Some(&fingerprint))
                .err()
                .map(|response| response.status());
        assert_eq!(rejection_status, Some(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn disabled_binding_cannot_bypass_required_runtime_policy() {
        for config in [
            crate::config::TokenBindingConfig {
                enabled: false,
                required: true,
                ..crate::config::TokenBindingConfig::default()
            },
            crate::config::TokenBindingConfig {
                enabled: false,
                require_client_fingerprint: true,
                ..crate::config::TokenBindingConfig::default()
            },
        ] {
            let rejection_status = negotiate_token_binding(&config, false, &HeaderMap::new(), None)
                .err()
                .map(|response| response.status());
            assert_eq!(rejection_status, Some(StatusCode::INTERNAL_SERVER_ERROR));
        }
    }

    #[test]
    fn token_binding_subprotocol_offers_are_case_sensitive_and_fail_closed() {
        let expected = "signalfish.tokenbinding.v2";
        let mut headers = HeaderMap::new();
        assert_eq!(
            client_token_binding_offer(&headers, expected),
            TokenBindingProtocolOffer::None
        );

        headers.append(
            SEC_WEBSOCKET_PROTOCOL,
            "ordinary.protocol"
                .parse()
                .expect("ordinary protocol header"),
        );
        headers.append(
            SEC_WEBSOCKET_PROTOCOL,
            "signalfish.tokenbinding.v1, signalfish.tokenbinding.v2"
                .parse()
                .expect("repeated protocol header"),
        );
        assert_eq!(
            client_token_binding_offer(&headers, expected),
            TokenBindingProtocolOffer::Supported
        );

        for unsupported in [
            "signalfish.tokenbinding.v1",
            "Signalfish.TokenBinding.V2",
            "signalfish.tokenbinding.v3",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                SEC_WEBSOCKET_PROTOCOL,
                unsupported.parse().expect("unsupported protocol header"),
            );
            assert_eq!(
                client_token_binding_offer(&headers, expected),
                TokenBindingProtocolOffer::Unsupported
            );
        }
    }

    #[test]
    fn negotiation_rejects_reserved_non_v2_config_when_validation_is_bypassed() {
        let config = crate::config::TokenBindingConfig {
            enabled: true,
            subprotocol: "signalfish.tokenbinding.v1".to_string(),
            ..crate::config::TokenBindingConfig::default()
        };
        let rejection_status = negotiate_token_binding(&config, true, &HeaderMap::new(), None)
            .err()
            .map(|response| response.status());
        assert_eq!(rejection_status, Some(StatusCode::INTERNAL_SERVER_ERROR));
    }
}
