//! Ephemeral TURN credential minting (Protocol v3).
//!
//! Implements the coturn REST API (a.k.a. "long-term credential") scheme: the
//! static authentication secret (`turnserver --use-auth-secret
//! --static-auth-secret=<secret>`) lives **only** on the server. Clients never
//! receive it; instead they receive a short-lived, per-player username/credential
//! pair derived from it:
//!
//! ```text
//! expiry_unix = now_unix + credential_ttl_secs
//! username    = "{expiry_unix}:{player_id}"
//! credential  = base64_standard( HMAC_SHA1( static_auth_secret, username ) )
//! ```
//!
//! The core [`mint_turn_credentials`] function is **pure**: the caller supplies
//! `expiry_unix`, so minting is deterministic and vector-testable (the time source
//! is held at the emit site so all members of one finalize share an expiry). The
//! HMAC-SHA1 building block is pinned against RFC 2202 Test Case 2 in the tests.
//!
//! This module also hosts [`build_ice_servers`], the pure helper that turns a
//! [`TurnConfig`] into the per-recipient TURN-derived ICE entries, keeping
//! `session_policy.rs` focused on selection.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hmac::{Hmac, KeyInit, Mac};

use crate::config::TurnConfig;
use crate::protocol::{IceServer, PlayerId};

/// HMAC-SHA1 over a server secret, as used by the coturn REST API scheme.
type HmacSha1 = Hmac<sha1::Sha1>;

/// A minted, short-lived TURN credential pair handed to a single client.
///
/// `username` embeds the expiry and the player id (`"{expiry}:{player_id}"`);
/// `credential` is the base64 of `HMAC-SHA1(static_auth_secret, username)`. coturn
/// recomputes the same HMAC server-side to authenticate the client, and rejects the
/// pair once `expiry` has passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnCredentials {
    /// `"{expiry_unix}:{player_id}"` — the coturn REST username.
    pub username: String,
    /// `base64( HMAC-SHA1( static_auth_secret, username ) )`.
    pub credential: String,
}

/// Compute `base64_standard( HMAC-SHA1( key, msg ) )`.
///
/// The building block of the coturn REST scheme, factored out so it can be pinned
/// against the RFC 2202 HMAC-SHA1 test vectors. HMAC accepts a key of any length
/// (`new_from_slice` is infallible for `Hmac`), so an empty key still produces
/// output — that degenerate case is guarded at the config layer
/// ([`TurnConfig::validate`](crate::config::TurnConfig::validate) rejects an empty
/// secret when TURN is enabled), never here.
#[must_use]
fn hmac_sha1_base64(key: &[u8], msg: &[u8]) -> Option<String> {
    let mut mac = HmacSha1::new_from_slice(key).ok()?;
    mac.update(msg);
    Some(BASE64.encode(mac.finalize().into_bytes()))
}

/// Mint a coturn REST TURN credential pair for `player_id`, valid until
/// `expiry_unix` (the coturn REST credential contract in `docs/deployment-turn.md`).
///
/// Pure and deterministic: the same `(static_auth_secret, player_id, expiry_unix)`
/// always yields the same pair. The time source is intentionally **not** read here
/// — the caller captures `now` once and passes `expiry_unix = now + ttl` — so all
/// members finalized together share one expiry and the function is unit-testable
/// against fixed vectors.
#[must_use]
pub fn mint_turn_credentials(
    static_auth_secret: &str,
    player_id: PlayerId,
    expiry_unix: i64,
) -> TurnCredentials {
    let username = format!("{expiry_unix}:{player_id}");
    let credential =
        hmac_sha1_base64(static_auth_secret.as_bytes(), username.as_bytes()).unwrap_or_default();
    TurnCredentials {
        username,
        credential,
    }
}

/// Compute the expiry timestamp `now_unix + ttl_secs`, saturating on overflow.
///
/// A thin convenience for the emit site, kept separate from the pure
/// [`mint_turn_credentials`] so the latter stays deterministic. `ttl_secs` is a
/// `u64` (config type); the addition saturates at [`i64::MAX`] rather than wrapping
/// into the past, so a pathological TTL can never mint an already-expired (or
/// negative-expiry) credential.
#[must_use]
pub fn turn_expiry_unix(now_unix: i64, ttl_secs: u64) -> i64 {
    i64::try_from(ttl_secs)
        .ok()
        .and_then(|ttl| now_unix.checked_add(ttl))
        .unwrap_or(i64::MAX)
}

/// Build the TURN-derived ICE entries contributed by `turn` for `player_id`
/// (the session-selection `build_ice_servers` path).
///
/// Returns **only** the entries the `[turn]` block contributes — the caller
/// prepends the operator's static `session.ice_servers` list (preserved verbatim
/// for back-compat). The result is, in order:
///
/// 1. one credential-less STUN [`IceServer`] carrying every `turn.stun_urls`
///    entry, when that list is non-empty (public STUN needs no auth); then
/// 2. when `turn.enabled` and `turn.urls` is non-empty: one TURN [`IceServer`]
///    whose `username`/`credential` are freshly minted for `player_id` (so each
///    recipient gets a distinct, time-limited pair embedding their own id).
///
/// TURN is fully self-hosted — the credential is computed locally from the
/// operator's `static_auth_secret`; no third-party cloud is ever contacted. A
/// disabled block, or an enabled block with no `urls`, contributes no TURN entry
/// — yielding only public STUN, which is the TURN contract's "disabled ⇒ STUN only"
/// acceptance.
///
/// `now_unix` is the single timestamp captured once per `emit_session_plan` call,
/// so every member of one finalize shares the same expiry.
#[must_use]
pub fn build_ice_servers(turn: &TurnConfig, player_id: PlayerId, now_unix: i64) -> Vec<IceServer> {
    let mut servers = Vec::new();

    // Public STUN is always credential-less and is advertised whenever configured,
    // independent of `enabled` (it costs nothing and needs no secret).
    if !turn.stun_urls.is_empty() {
        servers.push(IceServer {
            urls: turn.stun_urls.clone(),
            username: None,
            credential: None,
        });
    }

    if turn.enabled && !turn.urls.is_empty() {
        let expiry_unix = turn_expiry_unix(now_unix, turn.credential_ttl_secs);
        let minted = mint_turn_credentials(&turn.static_auth_secret, player_id, expiry_unix);
        servers.push(IceServer {
            urls: turn.urls.clone(),
            username: Some(minted.username),
            credential: Some(minted.credential),
        });
    }

    servers
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// RFC 2202, HMAC-SHA1 Test Case 2: key = "Jefe", data = "what do ya want for
    /// nothing?" ⇒ HMAC-SHA1 hex `effcdf6ae5eb2fa2d27416d5f184df9c259a7c79`. The
    /// base64 of those 20 bytes is `7/zfauXrL6LSdBbV8YTfnCWafHk=` — this anchors
    /// our HMAC building block to a published vector so a crate swap can't silently
    /// change the crypto.
    #[test]
    fn hmac_sha1_base64_matches_rfc2202_case2() {
        let got = hmac_sha1_base64(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(got.as_deref(), Some("7/zfauXrL6LSdBbV8YTfnCWafHk="));

        // Cross-check the base64 against the canonical hex digest, so the literal
        // above is provably the same 20 bytes RFC 2202 publishes.
        let raw = BASE64
            .decode(got.unwrap_or_default())
            .expect("our own base64 decodes");
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    #[test]
    fn username_is_expiry_colon_player_id() {
        let player_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let creds = mint_turn_credentials("secret", player_id, 1_700_000_000);
        assert_eq!(
            creds.username,
            "1700000000:00000000-0000-0000-0000-000000000001"
        );
    }

    #[test]
    fn minting_is_deterministic() {
        let player_id = Uuid::new_v4();
        let a = mint_turn_credentials("secret", player_id, 1_700_000_000);
        let b = mint_turn_credentials("secret", player_id, 1_700_000_000);
        assert_eq!(a, b);
    }

    #[test]
    fn different_player_ids_differ_in_username_and_credential() {
        let p1 = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let p2 = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let a = mint_turn_credentials("secret", p1, 1_700_000_000);
        let b = mint_turn_credentials("secret", p2, 1_700_000_000);
        assert_ne!(a.username, b.username);
        assert_ne!(a.credential, b.credential);
    }

    #[test]
    fn different_expiry_differs_in_username_and_credential() {
        let player_id = Uuid::new_v4();
        let a = mint_turn_credentials("secret", player_id, 1_700_000_000);
        let b = mint_turn_credentials("secret", player_id, 1_700_000_001);
        assert_ne!(a.username, b.username);
        assert_ne!(a.credential, b.credential);
    }

    #[test]
    fn different_secret_differs_in_credential_only() {
        // The username does not embed the secret, so it is unchanged; only the
        // HMAC-derived credential differs.
        let player_id = Uuid::new_v4();
        let a = mint_turn_credentials("secret-a", player_id, 1_700_000_000);
        let b = mint_turn_credentials("secret-b", player_id, 1_700_000_000);
        assert_eq!(a.username, b.username);
        assert_ne!(a.credential, b.credential);
    }

    #[test]
    fn empty_secret_still_produces_output() {
        // HMAC allows an empty key, so minting does not panic. The empty-secret
        // case is rejected at the config layer (TurnConfig::validate), not here.
        let player_id = Uuid::new_v4();
        let creds = mint_turn_credentials("", player_id, 1_700_000_000);
        assert!(!creds.credential.is_empty());
    }

    #[test]
    fn turn_expiry_saturates_instead_of_wrapping() {
        // A pathological TTL must never mint a past-dated credential.
        assert_eq!(turn_expiry_unix(1_000, 3_600), 4_600);
        assert_eq!(turn_expiry_unix(i64::MAX, 1), i64::MAX);
        assert_eq!(turn_expiry_unix(0, u64::MAX), i64::MAX);
    }

    fn enabled_static_turn() -> TurnConfig {
        TurnConfig {
            enabled: true,
            static_auth_secret: "super-secret".to_string(),
            urls: vec!["turn:turn.example.com:3478".to_string()],
            stun_urls: vec!["stun:stun.l.google.com:19302".to_string()],
            credential_ttl_secs: 3600,
        }
    }

    #[test]
    fn build_ice_disabled_yields_only_stun() {
        let turn = TurnConfig {
            enabled: false,
            ..enabled_static_turn()
        };
        let servers = build_ice_servers(&turn, Uuid::new_v4(), 1_000);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].urls, vec!["stun:stun.l.google.com:19302"]);
        assert!(servers[0].username.is_none());
        assert!(servers[0].credential.is_none());
    }

    #[test]
    fn build_ice_disabled_and_no_stun_yields_empty() {
        let turn = TurnConfig {
            enabled: false,
            stun_urls: Vec::new(),
            ..enabled_static_turn()
        };
        assert!(build_ice_servers(&turn, Uuid::new_v4(), 1_000).is_empty());
    }

    #[test]
    fn build_ice_static_secret_yields_stun_then_turn() {
        let turn = enabled_static_turn();
        let player_id = Uuid::new_v4();
        let now = 1_700_000_000;
        let servers = build_ice_servers(&turn, player_id, now);

        assert_eq!(servers.len(), 2, "STUN entry followed by TURN entry");
        // STUN first, credential-less.
        assert_eq!(servers[0].urls, vec!["stun:stun.l.google.com:19302"]);
        assert!(servers[0].username.is_none());
        // TURN second, with minted creds for this expiry.
        assert_eq!(servers[1].urls, vec!["turn:turn.example.com:3478"]);
        let expected_expiry = now + 3600;
        let username = servers[1].username.as_ref().expect("turn has a username");
        assert!(username.starts_with(&format!("{expected_expiry}:")));
        assert_eq!(username, &format!("{expected_expiry}:{player_id}"));
        let credential = servers[1]
            .credential
            .as_ref()
            .expect("turn has a credential");
        assert!(!credential.is_empty());
        // Credential must be valid standard base64.
        assert!(BASE64.decode(credential).is_ok());
    }

    #[test]
    fn build_ice_distinct_players_share_expiry_but_differ_in_creds() {
        let turn = enabled_static_turn();
        let now = 1_700_000_000;
        let p1 = Uuid::new_v4();
        let p2 = Uuid::new_v4();
        let a = build_ice_servers(&turn, p1, now);
        let b = build_ice_servers(&turn, p2, now);

        let a_user = a[1].username.as_ref().unwrap();
        let b_user = b[1].username.as_ref().unwrap();
        let expected_expiry = now + 3600;
        // Same expiry prefix (one `now` per finalize), distinct ids ⇒ distinct creds.
        assert!(a_user.starts_with(&format!("{expected_expiry}:")));
        assert!(b_user.starts_with(&format!("{expected_expiry}:")));
        assert_ne!(a_user, b_user);
        assert_ne!(a[1].credential, b[1].credential);
    }

    #[test]
    fn build_ice_static_secret_without_turn_urls_yields_only_stun() {
        // Enabled static_secret but no TURN urls: contributes STUN only (the TURN
        // entry needs at least one url). `validate` rejects this at the config
        // layer, but the builder is defensive.
        let turn = TurnConfig {
            urls: Vec::new(),
            ..enabled_static_turn()
        };
        let servers = build_ice_servers(&turn, Uuid::new_v4(), 1_000);
        assert_eq!(servers.len(), 1);
        assert!(servers[0].username.is_none());
    }

    #[test]
    fn build_ice_carries_all_stun_and_turn_urls() {
        let turn = TurnConfig {
            urls: vec![
                "turn:turn.example.com:3478".to_string(),
                "turns:turn.example.com:5349".to_string(),
            ],
            stun_urls: vec![
                "stun:stun.l.google.com:19302".to_string(),
                "stun:stun1.l.google.com:19302".to_string(),
            ],
            ..enabled_static_turn()
        };
        let servers = build_ice_servers(&turn, Uuid::new_v4(), 1_000);
        assert_eq!(servers[0].urls.len(), 2);
        assert_eq!(servers[1].urls.len(), 2);
    }
}
