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
