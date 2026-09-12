//! Optional tenant `connect_token` verification (issue #517).
//!
//! The `Authenticate` handshake is accounting, not authentication: any client
//! can replay a public `app_id`. Hosted multi-tenant deployments can require a
//! second, signed field — `connect_token` — minted by the operator's control
//! plane with an Ed25519 private key. This server verifies the signature
//! against one configured public key; the private key never reaches this
//! process, so verification is stateless (no callouts, no store) and fits the
//! zero-dependency runtime contract.
//!
//! # Wire format
//!
//! ```text
//! token   = "sfct_v1" "." payload_b64 "." signature_b64
//! payload = UTF-8 JSON {"app_id": string, "exp": i64, "nonce": string}
//! ```
//!
//! Both base64 encodings are URL-safe without padding. The signature is a
//! 64-byte Ed25519 signature (RFC 8032) over the exact ASCII bytes of
//! `"sfct_v1." ++ payload_b64` — the encoded form is signed, so no JSON
//! canonicalization exists anywhere in the contract.
//!
//! # Verification order
//!
//! Encoding parse, signature, expiry (`exp > now`), TTL ceiling, then
//! `app_id` equality with the presented `app_id`. Every failure is reported
//! as the single `CONNECT_TOKEN_INVALID` error code on the wire; the reason
//! is only in the human-readable `error` string, which never quotes token
//! material.
//!
//! # Replay policy (accepted, issue #517)
//!
//! The server is stateless and does no single-use tracking. A token is valid
//! until `exp`, so a leaked token replays until expiry — accepted over TLS.
//! The server additionally refuses tokens whose remaining validity exceeds
//! `CONNECT_TOKEN_MAX_TTL_SECS` (plus a fixed clock-skew allowance), which
//! caps the replay window at mint time even for a misbehaving minter. The
//! signed `nonce` gives the control plane an anchor for optional single-use
//! enforcement at its edge.

use std::sync::{Arc, RwLock};

use base64::engine::general_purpose::{
    STANDARD as BASE64_STANDARD, URL_SAFE as BASE64_URL_SAFE,
    URL_SAFE_NO_PAD as BASE64_URL_SAFE_NO_PAD,
};
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use signature::Verifier;
use thiserror::Error;

/// Wire format version prefix. A future format change mints under a new
/// prefix; this server rejects unknown prefixes instead of guessing.
pub const CONNECT_TOKEN_PREFIX: &str = "sfct_v1";

/// Maximum token validity the server accepts: the ratified #517 replay
/// policy. A minter may set a shorter `exp`; a longer one is refused so the
/// replay window stays bounded by policy, not by minter discipline.
pub const CONNECT_TOKEN_MAX_TTL_SECS: i64 = 300;

/// Fixed clock-skew allowance applied to the TTL ceiling only (not to
/// expiry): a minter that sets `exp = issued + 300` is not failed by a few
/// seconds of server/minter clock divergence, while an absurd validity
/// window still fails.
pub const CONNECT_TOKEN_CLOCK_SKEW_SECS: i64 = 60;

/// Compile-time acceptance ceiling: max TTL plus skew (300 + 60 = 360).
/// Written as a literal so the addition is compile-time checkable and the
/// runtime check needs no arithmetic; the pairing is pinned by
/// `ttl_ceiling_constant_tracks_the_two_policy_constants`.
const CONNECT_TOKEN_MAX_REMAINING_SECS: i128 = 360;

/// Largest accepted encoded token. The payload is bounded by the token cap
/// itself (a 2048-byte token leaves ~1.9 KiB for the JSON), and acceptance
/// additionally requires byte-equality between the signed `app_id` and the
/// presented (allowlist-length-capped) `app_id`, so a handshake never
/// buffers an unbounded credential string just to reject it.
pub const CONNECT_TOKEN_MAX_ENCODED_LENGTH: usize = 2048;

/// Largest accepted `nonce` byte length. The nonce is opaque to this server
/// (single-use enforcement lives at the control plane); the cap only bounds
/// payload size.
const CONNECT_TOKEN_MAX_NONCE_LENGTH: usize = 128;

/// Signed token payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectTokenClaims {
    /// App identity the token was minted for; must equal the presented
    /// `app_id` byte-for-byte.
    pub app_id: String,
    /// Expiry as Unix seconds (UTC). Validity requires `exp > now`.
    pub exp: i64,
    /// Minter-chosen opaque value; single-use enforcement is the control
    /// plane's option, never this server's.
    pub nonce: String,
}

/// Failure to verify one presented connect token. The wire always maps every
/// variant to `ErrorCode::ConnectTokenInvalid`; the `Display` strings are the
/// per-connection `error` text and never quote token material.
#[derive(Debug, Error)]
pub enum ConnectTokenError {
    /// A token was presented but no verification key is configured. Fail
    /// closed: a client that expects credentials to matter must not be
    /// silently downgraded to public-label semantics.
    #[error("connect token rejected: the server has no verification key configured")]
    NoKeyConfigured,
    #[error("connect token rejected: malformed token encoding")]
    Malformed,
    #[error("connect token rejected: signature verification failed")]
    InvalidSignature,
    #[error("connect token rejected: token expired")]
    Expired,
    #[error(
        "connect token rejected: validity window exceeds the server maximum of \
         {CONNECT_TOKEN_MAX_TTL_SECS} seconds plus a {CONNECT_TOKEN_CLOCK_SKEW_SECS} second \
         clock-skew allowance"
    )]
    TtlTooLong,
    #[error("connect token rejected: token was minted for a different app id")]
    AppIdMismatch,
}

/// Failure to load a configured verification key (startup or SIGHUP reload).
#[derive(Debug, Error)]
pub enum ConnectTokenKeyError {
    #[error("security.connect_token.public_key must be base64 (standard or URL-safe) of a 32-byte Ed25519 public key: {0}")]
    InvalidEncoding(String),
    #[error(
        "security.connect_token.public_key must decode to exactly 32 bytes (an Ed25519 \
         public key); got {0} bytes"
    )]
    InvalidLength(usize),
}

/// The signed payload's on-disk (on-wire) JSON shape. Unknown keys are
/// tolerated: the signature binds the exact encoded bytes, so a minter may
/// add fields for its own edge enforcement without breaking this parse.
#[derive(Debug, Deserialize)]
struct ConnectTokenPayload {
    app_id: String,
    exp: i64,
    nonce: String,
}

/// One configured verification key. Immutable after construction, so it can
/// be shared freely through an [`Arc`] snapshot.
#[derive(Debug, Clone)]
pub struct ConnectTokenVerifier {
    key: VerifyingKey,
}

impl ConnectTokenVerifier {
    /// Build a verifier from the base64-encoded 32-byte public key that
    /// operators configure under `security.connect_token.public_key`.
    ///
    /// Accepts standard (padded) or URL-safe base64 so the same key can be
    /// pasted from either a cloud CLI or a PEM-less export; the decoded bytes
    /// must be exactly 32.
    pub fn from_encoded_key(encoded: &str) -> Result<Self, ConnectTokenKeyError> {
        let trimmed = encoded.trim();
        let decoded = BASE64_STANDARD
            .decode(trimmed)
            .or_else(|_| BASE64_URL_SAFE_NO_PAD.decode(trimmed))
            .or_else(|_| BASE64_URL_SAFE.decode(trimmed))
            .map_err(|error| ConnectTokenKeyError::InvalidEncoding(error.to_string()))?;
        if decoded.len() != 32 {
            return Err(ConnectTokenKeyError::InvalidLength(decoded.len()));
        }
        let key_len = decoded.len();
        let bytes: [u8; 32] = <[u8; 32]>::try_from(decoded)
            .map_err(|_| ConnectTokenKeyError::InvalidLength(key_len))?;
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|error| ConnectTokenKeyError::InvalidEncoding(error.to_string()))?;
        Ok(Self { key })
    }

    /// Verify one presented token against this key at `now_unix_secs`.
    ///
    /// Order follows the ratified #517 design: encoding parse, signature,
    /// expiry, TTL ceiling, `app_id` equality. `presented_app_id` is the
    /// already-allowlisted `Authenticate.app_id`.
    pub fn verify(
        &self,
        token: &str,
        presented_app_id: &str,
        now_unix_secs: i64,
    ) -> Result<ConnectTokenClaims, ConnectTokenError> {
        let (payload_bytes, signed_bytes, signature_bytes) = split_token(token)?;

        let signature =
            Signature::from_slice(&signature_bytes).map_err(|_| ConnectTokenError::Malformed)?;
        self.key
            .verify(signed_bytes, &signature)
            .map_err(|_| ConnectTokenError::InvalidSignature)?;

        let payload: ConnectTokenPayload =
            serde_json::from_slice(&payload_bytes).map_err(|_| ConnectTokenError::Malformed)?;
        // Field sanity before any policy check: a token minted with an empty
        // app id or an empty/oversized nonce is malformed regardless of what
        // the handshake presented.
        if payload.app_id.is_empty()
            || payload.nonce.is_empty()
            || payload.nonce.len() > CONNECT_TOKEN_MAX_NONCE_LENGTH
        {
            return Err(ConnectTokenError::Malformed);
        }

        // exp > now is the validity condition; the subtraction is total in
        // i128 and checked anyway, so an extreme minted `exp` cannot wrap
        // into a pass.
        let remaining = i128::from(payload.exp)
            .checked_sub(i128::from(now_unix_secs))
            .ok_or(ConnectTokenError::Expired)?;
        if remaining <= 0 {
            return Err(ConnectTokenError::Expired);
        }
        if remaining > CONNECT_TOKEN_MAX_REMAINING_SECS {
            return Err(ConnectTokenError::TtlTooLong);
        }

        // Constant-time comparison: cheap, and the payload's app id is
        // attacker-influenced material compared against an accounting key.
        if !super::constant_time_eq(&payload.app_id, presented_app_id) {
            return Err(ConnectTokenError::AppIdMismatch);
        }

        Ok(ConnectTokenClaims {
            app_id: payload.app_id,
            exp: payload.exp,
            nonce: payload.nonce,
        })
    }
}

/// Split an encoded token into (payload bytes, signed bytes, signature bytes).
fn split_token(token: &str) -> Result<(Vec<u8>, &[u8], Vec<u8>), ConnectTokenError> {
    if token.len() > CONNECT_TOKEN_MAX_ENCODED_LENGTH {
        return Err(ConnectTokenError::Malformed);
    }
    let mut parts = token.split('.');
    let prefix = parts.next().ok_or(ConnectTokenError::Malformed)?;
    let payload_b64 = parts.next().ok_or(ConnectTokenError::Malformed)?;
    let signature_b64 = parts.next().ok_or(ConnectTokenError::Malformed)?;
    if parts.next().is_some() || prefix != CONNECT_TOKEN_PREFIX {
        return Err(ConnectTokenError::Malformed);
    }
    if payload_b64.is_empty() || signature_b64.is_empty() {
        return Err(ConnectTokenError::Malformed);
    }
    let payload_bytes = BASE64_URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| ConnectTokenError::Malformed)?;
    // The signature covers the encoded payload segment exactly as presented:
    // "sfct_v1." ++ payload_b64, which is the token head through the second
    // dot. The split above validated exactly three parts, so the last dot is
    // the payload/signature boundary and everything before it is the signed
    // head.
    let head_end = token.rfind('.').ok_or(ConnectTokenError::Malformed)?;
    let signed_bytes = token
        .as_bytes()
        .get(..head_end)
        .ok_or(ConnectTokenError::Malformed)?;
    let signature_bytes = BASE64_URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|_| ConnectTokenError::Malformed)?;
    Ok((payload_bytes, signed_bytes, signature_bytes))
}

/// The server-side holder for the configured verification key.
///
/// The key swaps atomically under a write lock (SIGHUP reload, issue #517);
/// each verification call clones the current [`Arc`] snapshot, so one call
/// never sees a half-swapped key. A handshake that straddles a reload
/// verifies against whatever key is installed when it reaches verification:
/// a token minted under a replaced key fails closed (retryable
/// `CONNECT_TOKEN_INVALID`) until the client presents a token under the new
/// key — the same direction as the app-ID allowlist reload.
#[derive(Debug, Default)]
pub struct ConnectTokenKeyState {
    verifier: RwLock<Option<Arc<ConnectTokenVerifier>>>,
}

impl ConnectTokenKeyState {
    /// Current key snapshot, if one is configured.
    #[must_use]
    pub fn current(&self) -> Option<Arc<ConnectTokenVerifier>> {
        self.verifier
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Swap the configured key, returning the replaced snapshot. `None`
    /// removes verification (tokens are refused while absent).
    pub fn replace(
        &self,
        verifier: Option<Arc<ConnectTokenVerifier>>,
    ) -> Option<Arc<ConnectTokenVerifier>> {
        let mut guard = self
            .verifier
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::replace(&mut guard, verifier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use sha2::{Digest, Sha256};
    use signature::Signer;

    /// Deterministic 32-byte test seed from a label.
    fn seed(label: &[u8]) -> [u8; 32] {
        Sha256::digest(label).into()
    }

    /// RFC 8032 §7.1 TEST 1 constants. Pinning the reference vector keeps the
    /// dependency's byte semantics anchored to the standard: if a dependency
    /// upgrade ever changed signature or key encoding, this fails before any
    /// deployment does.
    const RFC8032_TEST1_SECRET: [u8; 32] = [
        0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c,
        0xc4, 0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae,
        0x7f, 0x60,
    ];
    const RFC8032_TEST1_PUBLIC: [u8; 32] = [
        0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07,
        0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07,
        0x51, 0x1a,
    ];

    fn mint(signing: &SigningKey, app_id: &str, exp: i64, nonce: &str) -> String {
        mint_with_payload(
            signing,
            &serde_json::json!({ "app_id": app_id, "exp": exp, "nonce": nonce }),
        )
    }

    fn mint_with_payload(signing: &SigningKey, payload: &serde_json::Value) -> String {
        let payload_json = serde_json::to_vec(payload).expect("payload serializes");
        let payload_b64 = BASE64_URL_SAFE_NO_PAD.encode(&payload_json);
        let signed = format!("{CONNECT_TOKEN_PREFIX}.{payload_b64}");
        let signature = signing.sign(signed.as_bytes());
        format!(
            "{signed}.{}",
            BASE64_URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

    fn test_verifier(signing: &SigningKey) -> ConnectTokenVerifier {
        ConnectTokenVerifier::from_encoded_key(&BASE64_STANDARD.encode(signing.verifying_key()))
            .expect("test key parses")
    }

    #[test]
    fn rfc8032_reference_keypair_verifies_through_the_public_config_path() {
        // The reference secret must derive the RFC public bytes ...
        let signing = SigningKey::from_bytes(&RFC8032_TEST1_SECRET);
        assert_eq!(signing.verifying_key().to_bytes(), RFC8032_TEST1_PUBLIC);
        // ... and the configured-key path must accept the reference public
        // bytes and verify a fresh signature over the exact signed head.
        let verifier =
            ConnectTokenVerifier::from_encoded_key(&BASE64_STANDARD.encode(RFC8032_TEST1_PUBLIC))
                .expect("RFC public key parses");
        let payload_b64 = BASE64_URL_SAFE_NO_PAD.encode(br#"{"app_id":"a","exp":1,"nonce":"n"}"#);
        let signed = format!("{CONNECT_TOKEN_PREFIX}.{payload_b64}");
        let signature = signing.sign(signed.as_bytes());
        let token = format!(
            "{signed}.{}",
            BASE64_URL_SAFE_NO_PAD.encode(signature.to_bytes())
        );
        let claims = verifier
            .verify(&token, "a", 0)
            .expect("RFC-signed token verifies");
        assert_eq!(claims.exp, 1);
        assert_eq!(claims.nonce, "n");
    }

    #[test]
    fn valid_token_within_max_ttl_verifies_and_returns_claims() {
        let signing = SigningKey::from_bytes(&seed(b"seed"));
        let verifier = test_verifier(&signing);
        let claims = verifier
            .verify(
                &mint(&signing, "mb_app_one", 1_700_000_300, "n-1"),
                "mb_app_one",
                1_700_000_000,
            )
            .expect("valid token verifies");
        assert_eq!(claims.app_id, "mb_app_one");
        assert_eq!(claims.exp, 1_700_000_300);
        assert_eq!(claims.nonce, "n-1");
    }

    #[test]
    fn ttl_ceiling_constant_tracks_the_two_policy_constants() {
        assert_eq!(
            CONNECT_TOKEN_MAX_REMAINING_SECS,
            i128::from(CONNECT_TOKEN_MAX_TTL_SECS) + i128::from(CONNECT_TOKEN_CLOCK_SKEW_SECS),
            "the acceptance ceiling must stay max TTL plus the skew allowance"
        );
    }

    #[test]
    fn ttl_ceiling_allows_max_ttl_plus_skew_and_refuses_beyond() {
        let signing = SigningKey::from_bytes(&seed(b"ttl"));
        let verifier = test_verifier(&signing);
        let now = 1_000_000;
        let at_limit = mint(
            &signing,
            "app",
            now + CONNECT_TOKEN_MAX_TTL_SECS + CONNECT_TOKEN_CLOCK_SKEW_SECS,
            "n",
        );
        assert!(
            verifier.verify(&at_limit, "app", now).is_ok(),
            "limit boundary passes"
        );
        let beyond = mint(
            &signing,
            "app",
            now + CONNECT_TOKEN_MAX_TTL_SECS + CONNECT_TOKEN_CLOCK_SKEW_SECS + 1,
            "n",
        );
        assert!(
            matches!(
                verifier.verify(&beyond, "app", now),
                Err(ConnectTokenError::TtlTooLong)
            ),
            "one second beyond the ceiling fails"
        );
    }

    #[test]
    fn expiry_boundary_fails_at_and_below_now() {
        let signing = SigningKey::from_bytes(&seed(b"exp"));
        let verifier = test_verifier(&signing);
        let now = 5_000_000;
        assert!(matches!(
            verifier.verify(&mint(&signing, "app", now, "n"), "app", now),
            Err(ConnectTokenError::Expired)
        ));
        assert!(matches!(
            verifier.verify(&mint(&signing, "app", now - 1, "n"), "app", now),
            Err(ConnectTokenError::Expired)
        ));
    }

    #[test]
    fn tampered_payload_or_wrong_key_fails_signature() {
        let signing = SigningKey::from_bytes(&seed(b"sig"));
        let other = SigningKey::from_bytes(&seed(b"other"));
        let verifier = test_verifier(&signing);
        // Wrong verifier key.
        assert!(matches!(
            verifier.verify(
                &mint(&other, "app", 1_700_000_300, "n"),
                "app",
                1_700_000_000
            ),
            Err(ConnectTokenError::InvalidSignature)
        ));
        // One flipped character inside the signed payload segment (still a
        // valid base64 character, so the failure is the signature, not the
        // encoding).
        let token = mint(&signing, "app", 1_700_000_300, "n");
        let payload_end = token.rfind('.').expect("token shape");
        let mut corrupted = token.clone();
        let last = corrupted.as_bytes()[payload_end - 1];
        let replacement = if last == b'A' { 'B' } else { 'A' };
        corrupted.replace_range(payload_end - 1..payload_end, &replacement.to_string());
        assert_ne!(corrupted, token, "the mutation must change the token");
        assert!(matches!(
            verifier.verify(&corrupted, "app", 1_700_000_000),
            Err(ConnectTokenError::InvalidSignature)
        ));
    }

    #[test]
    fn app_id_mismatch_and_missing_key_fail_closed() {
        let signing = SigningKey::from_bytes(&seed(b"bind"));
        let verifier = test_verifier(&signing);
        let token = mint(&signing, "app", 1_700_000_300, "n");
        assert!(matches!(
            verifier.verify(&token, "other-app", 1_700_000_000),
            Err(ConnectTokenError::AppIdMismatch)
        ));
        let empty_state = ConnectTokenKeyState::default();
        assert!(matches!(
            empty_state
                .current()
                .ok_or(ConnectTokenError::NoKeyConfigured)
                .and_then(|v| v.verify(&token, "app", 1_700_000_000).map(|_| ())),
            Err(ConnectTokenError::NoKeyConfigured)
        ));
    }

    #[test]
    fn malformed_shapes_are_refused_before_trusting_any_bytes() {
        let signing = SigningKey::from_bytes(&seed(b"mal"));
        let verifier = test_verifier(&signing);
        let good = mint(&signing, "app", 1_700_000_300, "n");
        // One byte short of / one byte past a 64-byte Ed25519 signature.
        let good_sig_b64 = &good[good.rfind('.').unwrap() + 1..];
        let sig_63 = format!(
            "{}.{}",
            &good[..good.rfind('.').unwrap()],
            &good_sig_b64[..84]
        );
        let sig_65 = format!("{}.{}x", &good[..good.rfind('.').unwrap()], good_sig_b64);
        for (name, bad) in [
            (
                "no prefix",
                good.replacen(CONNECT_TOKEN_PREFIX, "sfct_v2", 1),
            ),
            ("three parts in payload", format!("{good}.x")),
            ("empty payload", format!("{CONNECT_TOKEN_PREFIX}..abc")),
            (
                "not base64 payload",
                format!("{CONNECT_TOKEN_PREFIX}.!!!.abc"),
            ),
            (
                "short signature",
                format!("{}.abc", &good[..good.rfind('.').unwrap()]),
            ),
            ("63-byte signature", sig_63),
            ("65-byte signature", sig_65),
            (
                "oversized token",
                format!(
                    "{CONNECT_TOKEN_PREFIX}.{}.{}",
                    "A".repeat(2100),
                    "B".repeat(86)
                ),
            ),
            (
                "one byte past the size cap",
                format!(
                    "{CONNECT_TOKEN_PREFIX}.{}.{}",
                    "A".repeat(1954),
                    "B".repeat(86)
                ),
            ),
        ] {
            assert!(
                matches!(
                    verifier.verify(&bad, "app", 1_700_000_000),
                    Err(ConnectTokenError::Malformed)
                ),
                "{name} must be Malformed"
            );
        }
    }

    /// The size cap is inclusive (`>`): the largest reachable token length at
    /// or under [`CONNECT_TOKEN_MAX_ENCODED_LENGTH`] reaches signature
    /// verification and is accepted when it genuinely verifies, and anything
    /// past the cap is refused before any byte is trusted.
    #[test]
    fn token_at_the_size_cap_boundary_verifies() {
        let signing = SigningKey::from_bytes(&seed(b"cap"));
        let verifier = test_verifier(&signing);
        // Widen the payload with a tolerated filler field; keep the last
        // candidate that still fits under the cap (a handful of iterations
        // from the computed starting point).
        let base_len = serde_json::to_vec(&serde_json::json!({
            "app_id": "app", "exp": 1_700_000_300, "nonce": "n"
        }))
        .expect("payload serializes")
        .len();
        let build = |filler: usize| {
            mint_with_payload(
                &signing,
                &serde_json::json!({
                    "app_id": "app",
                    "exp": 1_700_000_300,
                    "nonce": "n",
                    "pad": "p".repeat(filler),
                }),
            )
        };
        // ~1.33 encoded bytes per filler byte lands just under the cap.
        let mut filler = (CONNECT_TOKEN_MAX_ENCODED_LENGTH - 95 - base_len) * 3 / 4;
        while build(filler).len() > CONNECT_TOKEN_MAX_ENCODED_LENGTH {
            filler -= 1;
        }
        let token = build(filler);
        assert!(
            token.len() >= CONNECT_TOKEN_MAX_ENCODED_LENGTH - 4,
            "boundary search must reach the cap: {}",
            token.len()
        );
        assert!(verifier.verify(&token, "app", 1_700_000_000).is_ok());
    }

    /// `exp` extremes cannot wrap into a pass: the arithmetic is total in
    /// `i128`, so an absurd remaining window fails the TTL ceiling and a
    /// far-past instant fails `Expired`. (The at-expiry boundary is pinned by
    /// `expiry_boundary_fails_at_and_below_now`.)
    #[test]
    fn exp_extremes_fail_closed() {
        let signing = SigningKey::from_bytes(&seed(b"ext"));
        let verifier = test_verifier(&signing);
        let now = 1_700_000_000i64;
        let max = mint(&signing, "app", i64::MAX, "n");
        assert!(matches!(
            verifier.verify(&max, "app", now),
            Err(ConnectTokenError::TtlTooLong)
        ));
        let min = mint(&signing, "app", i64::MIN, "n");
        assert!(matches!(
            verifier.verify(&min, "app", now),
            Err(ConnectTokenError::Expired)
        ));
    }

    /// The nonce cap is inclusive (accept at 128 bytes, refuse 129).
    #[test]
    fn nonce_boundary_is_inclusive() {
        let signing = SigningKey::from_bytes(&seed(b"nonce"));
        let verifier = test_verifier(&signing);
        let at_cap = mint(&signing, "app", 1_700_000_300, &"n".repeat(128));
        assert!(
            verifier.verify(&at_cap, "app", 1_700_000_000).is_ok(),
            "128-byte nonce is accepted"
        );
        let over_cap = mint(&signing, "app", 1_700_000_300, &"n".repeat(129));
        assert!(matches!(
            verifier.verify(&over_cap, "app", 1_700_000_000),
            Err(ConnectTokenError::Malformed)
        ));
    }

    #[test]
    fn payload_extra_fields_are_bound_but_tolerated_and_empty_fields_refused() {
        let signing = SigningKey::from_bytes(&seed(b"extra"));
        let verifier = test_verifier(&signing);
        let token = mint_with_payload(
            &signing,
            &serde_json::json!({
                "app_id": "app",
                "exp": 1_700_000_300,
                "nonce": "n",
                "sid": "edge-session-anchor"
            }),
        );
        let claims = verifier
            .verify(&token, "app", 1_700_000_000)
            .expect("unknown payload fields are tolerated");
        assert_eq!(claims.nonce, "n");

        for payload in [
            serde_json::json!({ "app_id": "", "exp": 1_700_000_300, "nonce": "n" }),
            serde_json::json!({ "app_id": "app", "exp": 1_700_000_300, "nonce": "" }),
            serde_json::json!({ "app_id": "app", "exp": 1_700_000_300 }),
            serde_json::json!({ "app_id": "app", "nonce": "n" }),
            serde_json::json!({ "exp": 1_700_000_300, "nonce": "n" }),
        ] {
            let token = mint_with_payload(&signing, &payload);
            assert!(
                matches!(
                    verifier.verify(&token, "app", 1_700_000_000),
                    Err(ConnectTokenError::Malformed)
                ),
                "payload {payload} must be refused"
            );
        }
    }

    #[test]
    fn key_state_swaps_atomically_and_removal_is_visible() {
        let signing = SigningKey::from_bytes(&seed(b"swap"));
        let state = ConnectTokenKeyState::default();
        assert!(state.current().is_none());
        let first = Arc::new(test_verifier(&signing));
        assert!(state.replace(Some(Arc::clone(&first))).is_none());
        assert!(Arc::ptr_eq(&state.current().expect("installed"), &first));
        assert!(state.replace(None).is_some());
        assert!(state.current().is_none());
    }

    #[test]
    fn key_parsing_accepts_documented_encodings_and_rejects_bad_bytes() {
        // A real public key: `VerifyingKey::from_bytes` rejects non-curve
        // point bytes, so arbitrary byte patterns are not valid keys.
        let public = RFC8032_TEST1_PUBLIC;
        for encoded in [
            BASE64_STANDARD.encode(public),
            BASE64_URL_SAFE_NO_PAD.encode(public),
            BASE64_URL_SAFE.encode(public),
            format!("  {}\n", BASE64_STANDARD.encode(public)),
        ] {
            assert!(
                ConnectTokenVerifier::from_encoded_key(&encoded).is_ok(),
                "encoding must be accepted: {encoded}"
            );
        }
        for bad in [
            "not base64!",
            BASE64_STANDARD.encode([7_u8; 31]).as_str(),
            BASE64_STANDARD.encode([7_u8; 33]).as_str(),
            "",
        ] {
            assert!(
                ConnectTokenVerifier::from_encoded_key(bad).is_err(),
                "must fail: {bad}"
            );
        }
    }
}
