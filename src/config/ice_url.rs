//! Shared ICE URL hygiene for the `[session]` and `[turn]` config blocks.
//!
//! Closes the scheme/duplicate URL checks formerly deferred to P4 (now that P4
//! wired in real STUN/TURN): every ICE URL a config block propagates verbatim to
//! clients must carry one of the four ICE schemes — checked **case-insensitively**
//! because URI schemes are case-insensitive (RFC 3986 §3.1) — followed by a
//! remainder that contains no whitespace or control characters at all (a URL the
//! browser-side grammar can never parse must fail at startup, not at client
//! gather time). Scheme violations are hard errors
//! (fail fast at startup, before a client ever receives an `RTCIceServer` it
//! cannot parse);
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
/// remainder free of whitespace and control characters.
///
/// Returns the human-readable violation on `Err`; the caller prefixes its own
/// indexed config path (e.g. `session.ice_servers[0].urls[1]`) so the two config
/// blocks keep their existing message voice. Deliberately **no trimming**: a URL
/// like `"stun :host"` splits to the scheme `"stun "` (with a space), which is
/// not a legal scheme token and is rejected, never repaired. Whitespace or
/// control characters anywhere in the URL are likewise rejected rather than
/// repaired: browsers parse `RTCIceServer.urls` with the RFC 3986/7065 grammar,
/// where such characters can only produce an invalid URL that throws at
/// `RTCPeerConnection` construction or dies silently at gather time.
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

    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "must not contain whitespace or control characters (got {url:?})"
        ));
    }

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
    if remainder.trim().is_empty() {
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
            "stun: ",
            "stun:\t",
            "turn: ",
            "stun :host",
        ] {
            assert!(
                check_url_scheme(url, ICE_SCHEMES).is_err(),
                "{url:?} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_whitespace_and_control_characters_inside_the_remainder() {
        // An embedded space, tab, newline, NUL, or other control character makes
        // the URL unparsable for every browser-side `RTCIceServer.urls` grammar
        // (RFC 3986/7065), so admitting it at startup would broadcast an entry
        // that can only fail at client gather time — exactly what this module
        // exists to prevent. Only whitespace-*only* remainders were rejected
        // before; embedded ones must be rejected too.
        for url in [
            "turn: host",
            "turn:host ",
            "turn:host\t",
            "turn:ho st",
            "turn:ho\nst",
            "turn:ho\u{0}st",
            "stun:ho\u{a0}st",
            "turns:\u{1f}host",
        ] {
            assert!(
                check_url_scheme(url, TURN_SCHEMES).is_err(),
                "{url:?} must be rejected by the TURN scheme list"
            );
            assert!(
                check_url_scheme(url, ICE_SCHEMES).is_err(),
                "{url:?} must be rejected by the ICE scheme list"
            );
        }
        // The documented happy paths stay green: query strings and IPv6
        // brackets carry no whitespace or control characters.
        assert!(check_url_scheme("turn:host:3478?transport=udp", TURN_SCHEMES).is_ok());
        assert!(check_url_scheme("turn:[2001:db8::1]:3478", TURN_SCHEMES).is_ok());
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
