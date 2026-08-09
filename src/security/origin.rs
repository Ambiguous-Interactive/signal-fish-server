//! Shared HTTP CORS and WebSocket upgrade Origin policy.

use axum::http::header::ORIGIN;
use axum::http::{HeaderMap, HeaderValue};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use url::Url;

/// A configuration error in `security.cors_origins`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid security.cors_origins: {0}")]
pub struct OriginPolicyError(String);

/// One parsed policy shared by HTTP CORS responses and WebSocket upgrades.
///
/// Missing `Origin` is accepted deliberately for native/non-browser clients.
/// Browser WebSocket implementations send `Origin`, so configured allowlists
/// still reject cross-site browser upgrades. This is a browser-origin control,
/// not client authentication: non-browser clients can choose or omit headers.
#[derive(Clone, Debug)]
pub struct OriginPolicy {
    allow_any: bool,
    allow_originless: bool,
    allowed_origins: Arc<[HeaderValue]>,
}

impl OriginPolicy {
    /// Parse `*`, `null`, or a comma-separated list of serialized HTTP(S)
    /// origins.
    pub fn parse(configured: &str) -> Result<Self, OriginPolicyError> {
        let configured = configured.trim();
        if configured == "*" {
            return Ok(Self {
                allow_any: true,
                allow_originless: true,
                allowed_origins: Arc::from([]),
            });
        }
        if configured.is_empty() {
            return Err(OriginPolicyError("value must not be blank".to_string()));
        }

        let mut allowed_origins = Vec::new();
        for entry in configured.split(',') {
            let origin = entry.trim();
            if origin.is_empty() {
                return Err(OriginPolicyError(
                    "comma-separated entries must not be blank".to_string(),
                ));
            }
            if origin == "*" {
                return Err(OriginPolicyError(
                    "wildcard `*` must be the entire value".to_string(),
                ));
            }
            validate_serialized_origin(origin)?;
            let header = origin.parse::<HeaderValue>().map_err(|error| {
                OriginPolicyError(format!(
                    "{origin:?} is not a valid HTTP header value: {error}"
                ))
            })?;
            if !allowed_origins.contains(&header) {
                allowed_origins.push(header);
            }
        }

        Ok(Self {
            allow_any: false,
            allow_originless: true,
            allowed_origins: allowed_origins.into(),
        })
    }

    /// A fail-closed policy for compatibility constructors given invalid
    /// configuration. Unlike a valid explicit allowlist, this rejects native
    /// clients that omit `Origin` as well as browser-originated upgrades.
    pub(crate) fn deny_all_upgrades() -> Self {
        Self {
            allow_any: false,
            allow_originless: false,
            allowed_origins: Arc::from([]),
        }
    }

    /// Whether this upgrade's browser Origin is allowed.
    #[must_use]
    pub fn allows_upgrade(&self, headers: &HeaderMap) -> bool {
        if self.allow_any {
            return true;
        }

        let mut origins = headers.get_all(ORIGIN).iter();
        let Some(origin) = origins.next() else {
            // Native clients do not send Origin. This is intentional
            // compatibility, not authentication of the origin-less caller.
            return self.allow_originless;
        };
        if origins.next().is_some() {
            return false;
        }
        self.allowed_origins.contains(origin)
    }

    /// Build HTTP response CORS behavior from the same parsed allowlist used
    /// by [`Self::allows_upgrade`].
    pub fn cors_layer(&self) -> CorsLayer {
        if self.allow_any {
            CorsLayer::permissive()
        } else {
            CorsLayer::new()
                .allow_origin(self.allowed_origins.iter().cloned().collect::<Vec<_>>())
                .allow_methods(Any)
                .allow_headers(Any)
        }
    }
}

fn validate_serialized_origin(origin: &str) -> Result<(), OriginPolicyError> {
    if origin == "null" {
        return Ok(());
    }

    let url = Url::parse(origin).map_err(|error| {
        OriginPolicyError(format!("{origin:?} is not a valid origin URL: {error}"))
    })?;
    let scheme = url.scheme();
    if !matches!(scheme, "http" | "https") || !origin.starts_with(&format!("{scheme}://")) {
        return Err(OriginPolicyError(format!(
            "{origin:?} must use a lowercase http or https scheme"
        )));
    }
    if url.host().is_none() {
        return Err(OriginPolicyError(format!(
            "{origin:?} must include a host authority"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(OriginPolicyError(format!(
            "{origin:?} must not include user information"
        )));
    }
    let serialized_origin = url.origin().ascii_serialization();
    if origin != serialized_origin {
        return Err(OriginPolicyError(format!(
            "{origin:?} is not a canonical serialized browser origin; use {serialized_origin:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_policy_matches_exact_configured_browser_origins() {
        let policy = OriginPolicy::parse("https://game.example, http://localhost:3000")
            .expect("parse explicit origin allowlist");

        for (origin, expected) in [
            (Some("https://game.example"), true),
            (Some("http://localhost:3000"), true),
            (Some("https://evil.example"), false),
            (None, true),
        ] {
            let mut headers = HeaderMap::new();
            if let Some(origin) = origin {
                headers.insert(ORIGIN, HeaderValue::from_static(origin));
            }
            assert_eq!(
                policy.allows_upgrade(&headers),
                expected,
                "origin={origin:?}"
            );
        }
    }

    #[test]
    fn wildcard_origin_policy_accepts_every_upgrade_origin() {
        let policy = OriginPolicy::parse("*").expect("parse wildcard policy");
        let mut headers = HeaderMap::new();
        headers.append(ORIGIN, HeaderValue::from_static("https://one.example"));
        headers.append(ORIGIN, HeaderValue::from_static("https://two.example"));

        assert!(policy.allows_upgrade(&headers));
    }

    #[test]
    fn explicit_origin_policy_rejects_ambiguous_duplicate_headers() {
        let policy = OriginPolicy::parse("https://game.example").expect("parse policy");
        let mut headers = HeaderMap::new();
        headers.append(ORIGIN, HeaderValue::from_static("https://game.example"));
        headers.append(ORIGIN, HeaderValue::from_static("https://game.example"));

        assert!(!policy.allows_upgrade(&headers));
    }

    #[test]
    fn opaque_null_origin_must_be_configured_explicitly() {
        let null_policy = OriginPolicy::parse("null").expect("parse opaque Origin policy");
        let http_policy = OriginPolicy::parse("https://game.example").expect("parse HTTP policy");
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static("null"));

        assert!(null_policy.allows_upgrade(&headers));
        assert!(!http_policy.allows_upgrade(&headers));
    }

    #[test]
    fn invalid_origin_configuration_fails_closed() {
        let out_of_range_port = format!("https://game.example:{}", u32::from(u16::MAX) + 1);
        assert!(
            OriginPolicy::parse(&out_of_range_port).is_err(),
            "out-of-range port: {out_of_range_port:?} must be rejected"
        );

        for (configured, reason) in [
            ("", "blank value"),
            ("https://game.example,", "blank list entry"),
            ("https://game.example,*", "mixed wildcard"),
            ("ws://game.example", "WebSocket rather than page scheme"),
            (
                "HTTPS://game.example",
                "uppercase scheme is not browser-canonical",
            ),
            ("https://game.example/path", "path"),
            ("https://game.example?query", "query"),
            ("https://user@game.example", "user information"),
            (
                "https://GAME.example",
                "uppercase host is not browser-canonical",
            ),
            (
                "https://game.example:443",
                "default port is omitted by browsers",
            ),
            (
                "http://game.example:80",
                "default port is omitted by browsers",
            ),
            ("https://game.example:", "empty trailing port"),
            ("http://127.1", "IPv4 shorthand"),
            ("http://0x7f000001", "hexadecimal IPv4"),
            ("http://[0:0:0:0:0:0:0:1]:3000", "uncompressed IPv6"),
        ] {
            assert!(
                OriginPolicy::parse(configured).is_err(),
                "{reason}: {configured:?} must be rejected"
            );
        }
    }

    #[test]
    fn canonical_non_default_ports_and_ipv6_origins_are_accepted() {
        for configured in [
            "https://game.example:8443",
            "http://localhost:3000",
            "http://[::1]:3000",
        ] {
            assert!(
                OriginPolicy::parse(configured).is_ok(),
                "canonical serialized origin must be accepted: {configured:?}"
            );
        }
    }

    #[test]
    fn invalid_configuration_fallback_rejects_originless_upgrades() {
        assert!(!OriginPolicy::deny_all_upgrades().allows_upgrade(&HeaderMap::new()));
    }
}
