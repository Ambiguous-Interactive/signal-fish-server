use crate::config::ProtocolConfig;
use std::collections::HashMap;

use super::types::{ConnectionInfo, DirectEndpoint, PlayerId, PlayerInfo};

impl DirectEndpoint {
    /// Validate and project legacy direct metadata into the authoritative v3
    /// endpoint shape. Non-direct metadata and unusable addresses are ignored.
    #[must_use]
    pub fn from_connection_info(connection_info: &ConnectionInfo) -> Option<Self> {
        let ConnectionInfo::Direct { host, port } = connection_info else {
            return None;
        };

        if *port == 0 || !direct_host_is_usable(host) {
            return None;
        }

        Some(Self {
            host: host.clone(),
            port: *port,
        })
    }
}

/// Accept an IP address (except an unspecified address) or a conservative DNS
/// hostname. Resolution and reachability remain client responsibilities.
fn direct_host_is_usable(host: &str) -> bool {
    use std::net::IpAddr;

    if host.is_empty() || host.len() > 253 || host.trim() != host {
        return false;
    }

    let hostname = host.strip_suffix('.').unwrap_or(host);
    if let Ok(address) = hostname.parse::<IpAddr>() {
        // A trailing root dot is DNS syntax, not IP-literal syntax. Reject the
        // ambiguous form instead of letting an unspecified IP masquerade as a
        // syntactically valid absolute DNS name.
        return hostname == host && !address.is_unspecified();
    }

    !hostname.is_empty()
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

pub fn validate_game_name_with_config(name: &str, config: &ProtocolConfig) -> Result<(), String> {
    if name.is_empty() {
        return Err("Game name cannot be empty".to_string());
    }
    // Length is measured in UTF-8 bytes, not characters, so multi-byte
    // alphabets consume the budget faster than a character count suggests.
    if name.len() > config.max_game_name_length {
        return Err(format!(
            "Game name too long (max {} bytes)",
            config.max_game_name_length
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ' ')
    {
        return Err("Game name contains invalid characters".to_string());
    }
    Ok(())
}

pub fn validate_room_code_with_config(code: &str, config: &ProtocolConfig) -> Result<(), String> {
    if code.is_empty() {
        return Err("Room code cannot be empty".to_string());
    }
    if code.len() != config.room_code_length {
        return Err(format!(
            "Room code must be exactly {} characters",
            config.room_code_length
        ));
    }
    if !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Room code must be alphanumeric".to_string());
    }
    Ok(())
}

pub fn validate_player_name_with_config(name: &str, config: &ProtocolConfig) -> Result<(), String> {
    if name.is_empty() {
        return Err("Player name cannot be empty".to_string());
    }
    // Length is measured in UTF-8 bytes, not characters — the same unit
    // `PlayerNameRulesPayload::max_length` advertises to clients.
    if name.len() > config.max_player_name_length {
        return Err(format!(
            "Player name too long (max {} bytes)",
            config.max_player_name_length
        ));
    }

    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Player name cannot be blank".to_string());
    }

    let rules = &config.player_name_validation;
    if !rules.allow_leading_trailing_whitespace && trimmed.len() != name.len() {
        return Err("Player name cannot have leading or trailing whitespace".to_string());
    }

    for ch in name.chars() {
        if ch == ' ' {
            if rules.allow_spaces {
                continue;
            }
            return Err("Player name cannot contain spaces".to_string());
        }

        if ch.is_whitespace() {
            return Err("Player name cannot contain whitespace characters".to_string());
        }

        let is_alphanumeric = if rules.allow_unicode_alphanumeric {
            ch.is_alphanumeric()
        } else {
            ch.is_ascii_alphanumeric()
        };

        if is_alphanumeric || rules.is_allowed_symbol(ch) {
            continue;
        }

        return Err("Player name contains invalid characters".to_string());
    }

    Ok(())
}

/// Case-insensitive, canonically-composed identity key for player names.
///
/// Uniqueness must compare what players *see*, not raw bytes: the same visible
/// name can have byte-distinct spellings, and comparing raw (or
/// lowercased-raw) bytes let an impersonator join a room under a visually
/// identical name while it was "taken". Composing to NFC first collapses those
/// spellings to one byte string; the subsequent lowercase keeps the historical
/// ASCII case-insensitivity. The transform is deterministic, so byte-equal
/// inputs stay byte-equal after it.
///
/// Reachability note: under the default charset rules the live gap is
/// Hangul — precomposed syllables are alphanumeric while their decomposed
/// jamo sequences are too (e.g. U+AC01 vs U+1100 U+1161 U+11A8). Latin NFD
/// spellings (`cafe` + combining acute) are already rejected by the charset
/// validator because combining marks are not `char::is_alphanumeric`, but an
/// operator who allowlists such marks via `allowed_symbols` /
/// `additional_allowed_characters` reopens that spelling surface, so the
/// comparison must stay composition-aware for every configuration.
fn canonical_player_name_key(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().collect::<String>().to_lowercase()
}

pub fn validate_player_name_uniqueness(
    name: &str,
    existing_players: &HashMap<PlayerId, PlayerInfo>,
) -> Result<(), String> {
    let normalized_name = canonical_player_name_key(name);
    for player in existing_players.values() {
        if canonical_player_name_key(&player.name) == normalized_name {
            return Err("Player name already exists in this room".to_string());
        }
    }
    Ok(())
}

pub fn validate_max_players_with_config(
    max_players: u8,
    config: &ProtocolConfig,
) -> Result<(), String> {
    if max_players < 1 {
        return Err("Max players must be at least 1".to_string());
    }
    if max_players > config.max_players_limit {
        return Err(format!(
            "Max players cannot exceed {}",
            config.max_players_limit
        ));
    }
    Ok(())
}

// Legacy validation functions using default constants for backward compatibility
#[allow(dead_code)]
pub fn validate_game_name(name: &str) -> Result<(), &'static str> {
    // Delegate to config-aware validator using default protocol config
    let cfg = crate::config::ProtocolConfig::default();
    match validate_game_name_with_config(name, &cfg) {
        Ok(()) => Ok(()),
        Err(_) => Err("Invalid game name"),
    }
}

#[allow(dead_code)]
pub fn validate_room_code(code: &str) -> Result<(), &'static str> {
    let cfg = crate::config::ProtocolConfig::default();
    match validate_room_code_with_config(code, &cfg) {
        Ok(()) => Ok(()),
        Err(_) => Err("Invalid room code"),
    }
}

#[allow(dead_code)]
pub fn validate_player_name(name: &str) -> Result<(), &'static str> {
    let cfg = crate::config::ProtocolConfig::default();
    match validate_player_name_with_config(name, &cfg) {
        Ok(()) => Ok(()),
        Err(_) => Err("Invalid player name"),
    }
}

#[allow(dead_code)]
pub fn validate_max_players(max_players: u8) -> Result<(), &'static str> {
    let cfg = crate::config::ProtocolConfig::default();
    match validate_max_players_with_config(max_players, &cfg) {
        Ok(()) => Ok(()),
        Err(_) => Err("Invalid max players"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProtocolConfig;

    // -- Game name length boundary (validation.rs:10, `>` boundary) -----------

    #[test]
    fn game_name_at_exact_max_length_is_accepted_and_over_is_rejected() {
        let config = ProtocolConfig::default();
        let max = config.max_game_name_length;
        assert!(max > 0, "the default max game-name length is non-trivial");

        // EXACTLY max length: accepted. Kills `replace > with >=` (which would
        // reject this name).
        let at_max = "a".repeat(max);
        assert_eq!(at_max.len(), max);
        assert!(
            validate_game_name_with_config(&at_max, &config).is_ok(),
            "a game name of exactly the max length must be accepted"
        );

        // max + 1: rejected. Pins the upper side of the boundary.
        let over_max = "a".repeat(max + 1);
        assert!(
            validate_game_name_with_config(&over_max, &config).is_err(),
            "a game name longer than the max must be rejected"
        );
    }

    // -- Player name length boundary (validation.rs:45, `>` boundary) ---------

    #[test]
    fn player_name_length_boundary_is_byte_measured_max_inclusive() {
        let config = ProtocolConfig::default();
        let max = config.max_player_name_length;
        assert!(max > 1, "the default max player-name length is non-trivial");

        // A length strictly below max is accepted. Kills `replace > with ==`
        // (which would reject this shorter-than-max name too).
        let below_max = "a".repeat(max - 1);
        assert_eq!(below_max.len(), max - 1);
        assert!(
            validate_player_name_with_config(&below_max, &config).is_ok(),
            "a player name shorter than the max must be accepted"
        );

        // EXACTLY max bytes: accepted. Kills `replace > with >=` (which would
        // reject this name).
        let at_max = "a".repeat(max);
        assert_eq!(at_max.len(), max);
        assert!(
            validate_player_name_with_config(&at_max, &config).is_ok(),
            "a player name of exactly the max byte length must be accepted"
        );

        // max + 1 bytes: rejected. Pins the upper side of the boundary.
        let over_max = "a".repeat(max + 1);
        assert!(
            validate_player_name_with_config(&over_max, &config).is_err(),
            "a player name longer than the max must be rejected"
        );
    }

    // -- Max-players boundaries (validation.rs:107 `<`, :110 `>`) -------------

    /// Data-driven: the advertised name limits are UTF-8 **bytes**, not
    /// characters. A multi-byte name whose character count fits comfortably
    /// but whose byte length exceeds the limit must be rejected (this is the
    /// mismatch a client checking against a character reading of
    /// `max_length` would get wrong), and the same name truncated under the
    /// byte budget must be accepted.
    #[test]
    fn name_limits_are_measured_in_bytes_not_characters() {
        let config = ProtocolConfig::default();
        let cjk = "\u{6c34}"; // U+6C34 "water": one char, three UTF-8 bytes
        assert_eq!(cjk.len(), 3);
        assert_eq!(cjk.chars().count(), 1);

        // Player names.
        let max = config.max_player_name_length;
        let over_budget_chars = max / 3 + 1;
        let player_over = cjk.repeat(over_budget_chars);
        assert!(
            player_over.chars().count() <= max,
            "fixture must fit by character count"
        );
        assert!(player_over.len() > max, "fixture must exceed by bytes");
        assert!(validate_player_name_with_config(&player_over, &config).is_err());
        let player_under = cjk.repeat(max / 3);
        assert!(player_under.len() <= max);
        assert!(validate_player_name_with_config(&player_under, &config).is_ok());

        // Game names share the byte semantics (unicode alphanumerics allowed).
        let game_max = config.max_game_name_length;
        let game_over = cjk.repeat(game_max / 3 + 1);
        assert!(game_over.chars().count() <= game_max);
        assert!(game_over.len() > game_max);
        assert!(validate_game_name_with_config(&game_over, &config).is_err());
        assert!(
            validate_game_name_with_config(&cjk.repeat(game_max / 3), &config).is_ok(),
            "the same name within the byte budget is accepted"
        );
    }

    // -- Player-name uniqueness identity (validation.rs:145) ------------------

    /// Data-driven: every pair of byte-distinct spellings that render
    /// identically must collide under `validate_player_name_uniqueness`, and
    /// genuinely different names must not. The NFC/NFD rows pin the
    /// composition contract (the function is also the guard for operator
    /// configurations whose custom symbol allowlists admit combining marks
    /// that the default charset rules would reject); the ASCII rows pin
    /// case-insensitivity.
    #[test]
    fn visually_identical_name_spellings_cannot_coexist_in_a_room() {
        const NFC_CAFE: &str = "café"; // c a f é (U+00E9)
        const NFD_CAFE: &str = "cafe\u{0301}"; // c a f e + combining acute
        assert_ne!(NFC_CAFE.as_bytes(), NFD_CAFE.as_bytes());
        const NFC_HANGUL: &str = "\u{ac01}"; // U+AC01 (precomposed syllable)
        const NFD_HANGUL: &str = "\u{1100}\u{1161}\u{11a8}"; // same jamo sequence
        assert_ne!(NFC_HANGUL.as_bytes(), NFD_HANGUL.as_bytes());

        let collisions: &[(&str, &str)] = &[
            ("Player1", "player1"),
            ("Player1", "PLAYER1"),
            (NFC_CAFE, NFD_CAFE),
            (NFD_CAFE, NFC_CAFE),
            (NFC_HANGUL, NFD_HANGUL),
            (NFD_HANGUL, NFC_HANGUL),
        ];
        for (member, joiner) in collisions {
            let mut players = HashMap::new();
            players.insert(
                PlayerId::new_v4(),
                PlayerInfo {
                    id: PlayerId::new_v4(),
                    name: (*member).to_string(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: chrono::Utc::now(),
                    connection_info: None,
                    epoch: None,
                    seq: None,
                    region_id: crate::protocol::types::DEFAULT_REGION_ID.to_string(),
                },
            );
            assert!(
                validate_player_name_uniqueness(joiner, &players).is_err(),
                "joiner {joiner:?} must collide with member {member:?}"
            );
        }

        // A genuinely distinct name never collides.
        let mut players = HashMap::new();
        players.insert(
            PlayerId::new_v4(),
            PlayerInfo {
                id: PlayerId::new_v4(),
                name: NFC_CAFE.to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                epoch: None,
                seq: None,
                region_id: crate::protocol::types::DEFAULT_REGION_ID.to_string(),
            },
        );
        assert!(validate_player_name_uniqueness("cafe", &players).is_ok());
    }

    #[test]
    fn max_players_min_boundary_is_inclusive_of_one() {
        let config = ProtocolConfig::default();

        // 0 is below the minimum: rejected.
        assert!(
            validate_max_players_with_config(0, &config).is_err(),
            "max_players of 0 is below the minimum and must be rejected"
        );

        // EXACTLY the minimum (1) is accepted. Kills `replace < with <=` (which
        // would reject 1 as below the minimum).
        assert!(
            validate_max_players_with_config(1, &config).is_ok(),
            "max_players of exactly 1 (the minimum) must be accepted"
        );
    }

    #[test]
    fn max_players_max_boundary_is_inclusive_of_limit() {
        let config = ProtocolConfig::default();
        let limit = config.max_players_limit;
        assert!(limit > 1, "the default max-players limit is non-trivial");

        // EXACTLY the limit is accepted. Kills `replace > with >=` (which would
        // reject the limit itself).
        assert!(
            validate_max_players_with_config(limit, &config).is_ok(),
            "max_players of exactly the limit must be accepted"
        );

        // One above the limit is rejected (when the limit leaves room in u8).
        if let Some(over) = limit.checked_add(1) {
            assert!(
                validate_max_players_with_config(over, &config).is_err(),
                "max_players above the limit must be rejected"
            );
        }
    }
}
