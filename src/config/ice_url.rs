//! Shared ICE URL hygiene for the `[session]` and `[turn]` config blocks.
//!
//! Closes the scheme/duplicate URL checks formerly deferred to P4 (now that P4
//! wired in real STUN/TURN): every ICE URL a config block propagates verbatim to
//! clients must carry one of the four ICE schemes — checked **case-insensitively**
//! because URI schemes are case-insensitive (RFC 3986 §3.1) — with a non-empty
//! remainder after the colon. Scheme violations are hard errors (fail fast at
//! startup, before a client ever receives an `RTCIceServer` it cannot parse);
//! exact-duplicate URLs only warn (clients tolerate repeated entries, mirroring
//! the warn-but-succeed stance of the disabled-P2P topology warning).

use std::collections::HashSet;

/// Schemes legal in `session.ice_servers[].urls` (a static list may mix STUN and
/// TURN entries).
pub(crate) const ICE_SCHEMES: &[&str] = &["stun", "stuns", "turn", "turns"];

/// Schemes legal in `turn.urls` (the list feeding the credentialed TURN entry).
pub(crate) const TURN_SCHEMES: &[&str] = &["turn", "turns"];

/// Schemes legal in `turn.stun_urls` (credential-less public STUN).
pub(crate) const STUN_SCHEMES: &[&str] = &["stun", "stuns"];

/// Check that `url` starts with one of `allowed_schemes` (ASCII
/// case-insensitively — schemes are ASCII by grammar) followed by `:` and a
/// non-empty remainder.
///
/// Returns the human-readable violation on `Err`; the caller prefixes its own
/// indexed config path (e.g. `session.ice_servers[0].urls[1]`) so the two config
/// blocks keep their existing message voice. Deliberately **no trimming**: a URL
/// like `"stun :host"` splits to the scheme `"stun "` (with a space), which is
/// not a legal scheme token and is rejected, never repaired.
pub(crate) fn check_url_scheme(url: &str, allowed_schemes: &[&str]) -> Result<(), String> {
    // Built lazily: the "stun:, stuns:, ..." display string is only needed on
    // the two scheme-violation arms, so the (common) success path allocates
    // nothing.
    let allowed_list = || {
        allowed_schemes
            .iter()
            .map(|scheme| format!("{scheme}:"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let Some((scheme, remainder)) = url.split_once(':') else {
        return Err(format!(
            "must start with one of the {} URL schemes (got {url:?})",
            allowed_list()
        ));
    };
    if !allowed_schemes
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    {
        return Err(format!(
            "must start with one of the {} URL schemes (got {url:?})",
            allowed_list()
        ));
    }
    if remainder.is_empty() {
        return Err(format!(
            "must have a host after the \"{scheme}:\" scheme (got {url:?})"
        ));
    }
    Ok(())
}

/// Warn (never error) about exact-duplicate URL strings within one config
/// block's full URL set.
///
/// `urls` is the block's URLs flattened in declaration order (covering
/// duplicates both within one server's list and across the block); each
/// duplicated string is reported once, in first-duplicate order, so the warning
/// is deterministic for a given config.
pub(crate) fn warn_on_duplicate_urls<'a, I>(config_path: &str, urls: I)
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen: HashSet<&str> = HashSet::new();
    let mut duplicates: Vec<&str> = Vec::new();
    for url in urls {
        if !seen.insert(url) && !duplicates.contains(&url) {
            duplicates.push(url);
        }
    }
    if !duplicates.is_empty() {
        tracing::warn!(
            config_path,
            ?duplicates,
            "duplicate ICE URLs configured; clients tolerate repeated entries, \
             but this is likely a misconfiguration"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_each_allowed_scheme_case_insensitively() {
        for url in [
            "stun:host",
            "STUN:host",
            "stuns:host",
            "turn:host:3478?transport=udp",
            "TURNS:host",
            "turn:[2001:db8::1]:3478",
        ] {
            assert!(
                check_url_scheme(url, ICE_SCHEMES).is_ok(),
                "{url:?} must be accepted"
            );
        }
    }

    #[test]
    fn rejects_wrong_scheme_missing_colon_empty_remainder_and_space_in_scheme() {
        for url in [
            "http://example.com",
            "relay:foo",
            "no-colon",
            "stun:",
            "stun :host",
        ] {
            assert!(
                check_url_scheme(url, ICE_SCHEMES).is_err(),
                "{url:?} must be rejected"
            );
        }
    }

    #[test]
    fn scheme_lists_are_scoped_per_config_field() {
        assert!(check_url_scheme("stun:host", TURN_SCHEMES).is_err());
        assert!(check_url_scheme("turn:host", STUN_SCHEMES).is_err());
        assert!(check_url_scheme("turns:host", TURN_SCHEMES).is_ok());
        assert!(check_url_scheme("stuns:host", STUN_SCHEMES).is_ok());
    }

    #[test]
    fn error_names_the_offending_url_and_the_allowed_schemes() {
        let err = check_url_scheme("relay:foo", TURN_SCHEMES).expect_err("rejected");
        assert!(err.contains("turn:, turns:"), "allowed list present: {err}");
        assert!(err.contains("relay:foo"), "offending URL present: {err}");
    }

    #[test]
    fn duplicate_warning_is_a_warning_only() {
        // No panic, no error value — the helper only logs. Exercise both the
        // duplicate and the all-unique path for coverage of the gating branch.
        warn_on_duplicate_urls("test.block", ["a:1", "a:1", "b:2"]);
        warn_on_duplicate_urls("test.block", ["a:1", "b:2"]);
    }
}
