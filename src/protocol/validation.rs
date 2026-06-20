use crate::config::ProtocolConfig;
use std::collections::HashMap;

use super::types::{PlayerId, PlayerInfo};

pub fn validate_game_name_with_config(name: &str, config: &ProtocolConfig) -> Result<(), String> {
    if name.is_empty() {
        return Err("Game name cannot be empty".to_string());
    }
    if name.len() > config.max_game_name_length {
        return Err(format!(
            "Game name too long (max {} characters)",
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
    if name.len() > config.max_player_name_length {
        return Err(format!(
            "Player name too long (max {} characters)",
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

pub fn validate_player_name_uniqueness(
    name: &str,
    existing_players: &HashMap<PlayerId, PlayerInfo>,
) -> Result<(), String> {
    let normalized_name = name.to_lowercase();
    for player in existing_players.values() {
        if player.name.to_lowercase() == normalized_name {
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
