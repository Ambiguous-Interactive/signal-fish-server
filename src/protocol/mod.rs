// Protocol module: Message types, validation, and room state management

pub mod error_codes;
pub mod messages;
pub mod room_codes;
pub mod room_state;
pub mod types;
pub mod validation;

// Re-export everything for backward compatibility
// This allows external code to use `use crate::protocol::*`

// From error_codes
pub use error_codes::ErrorCode;

// From types
pub use types::{
    ConnectionInfo, GameDataEncoding, PeerConnectionInfo, PlayerId, PlayerInfo,
    PlayerNameRulesPayload, ProtocolInfoPayload, RateLimitInfo, RelayTransport, RoomId,
    SpectatorInfo, SpectatorStateChangeReason, DEFAULT_MAX_GAME_NAME_LENGTH,
    DEFAULT_MAX_PLAYERS_LIMIT, DEFAULT_MAX_PLAYER_NAME_LENGTH, DEFAULT_REGION_ID,
    DEFAULT_ROOM_CODE_LENGTH,
};

// From messages
pub use messages::{
    ClientMessage, ReconnectedPayload, RoomJoinedPayload, ServerMessage, SpectatorJoinedPayload,
};

// From room_state
pub use room_state::{LobbyState, Room};

#[cfg(test)]
mod tests {
    use super::room_codes;
    use super::validation::{
        validate_game_name_with_config, validate_player_name_with_config,
        validate_room_code_with_config,
    };
    use super::*;
    use crate::config::ProtocolConfig;
    use proptest::prelude::*;
    use uuid::Uuid;

    #[test]
    fn test_room_creation() {
        let room = Room::new(
            "test_game".to_string(),
            "ABC123".to_string(),
            4,
            true,
            "matchbox".to_string(),
        );
        assert_eq!(room.game_name, "test_game");
        assert_eq!(room.code, "ABC123");
        assert_eq!(room.max_players, 4);
        assert!(room.supports_authority);
        assert_eq!(room.relay_type, "matchbox");
        assert!(room.can_join());
    }

    #[test]
    fn test_player_management() {
        let mut room = Room::new(
            "test_game".to_string(),
            "ABC123".to_string(),
            2,
            true,
            "matchbox".to_string(),
        );

        let player1 = PlayerInfo {
            id: Uuid::new_v4(),
            name: "Player1".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        let player2 = PlayerInfo {
            id: Uuid::new_v4(),
            name: "Player2".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        assert!(room.add_player(player1));
        assert!(room.add_player(player2));
        assert!(!room.can_join());

        let player3 = PlayerInfo {
            id: Uuid::new_v4(),
            name: "Player3".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        assert!(!room.add_player(player3));
    }

    #[test]
    fn test_authority_management() {
        let mut room = Room::new(
            "test_game".to_string(),
            "ABC123".to_string(),
            4,
            true,
            "matchbox".to_string(),
        );

        let player_id = Uuid::new_v4();
        let player = PlayerInfo {
            id: player_id,
            name: "Authority Player".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        room.add_player(player);
        assert!(room.set_authority(Some(player_id)));
        assert_eq!(room.authority_player, Some(player_id));
        assert!(room.players[&player_id].is_authority);
    }

    #[test]
    fn test_authority_management_disabled() {
        let mut room = Room::new(
            "test_game".to_string(),
            "ABC123".to_string(),
            4,
            false,
            "matchbox".to_string(),
        );

        let player_id = Uuid::new_v4();
        let player = PlayerInfo {
            id: player_id,
            name: "Authority Player".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        room.add_player(player);

        // Authority operations should fail when not supported
        assert!(!room.set_authority(Some(player_id)));
        assert_eq!(room.authority_player, None);
        assert!(!room.players[&player_id].is_authority);
    }

    #[test]
    fn test_validation() {
        use validation::*;

        assert!(validate_game_name("valid_game").is_ok());
        assert!(validate_game_name("").is_err());
        assert!(validate_game_name("a".repeat(100).as_str()).is_err());

        assert!(validate_room_code("ABC123").is_ok());
        assert!(validate_room_code("").is_err());
        assert!(validate_room_code("abc!@#").is_err());

        assert!(validate_player_name("ValidPlayer").is_ok());
        assert!(validate_player_name("Player One").is_ok());
        assert!(validate_player_name("Player-One").is_ok());
        assert!(validate_player_name("玩家One").is_ok());
        assert!(validate_player_name("").is_err());
        assert!(validate_player_name("  ").is_err());
        assert!(validate_player_name(" spaced ").is_err());
        assert!(validate_player_name("Player\tOne").is_err()); // Contains tab
        assert!(validate_player_name("User@123").is_err()); // Contains disallowed symbol

        assert!(validate_max_players(4).is_ok());
        assert!(validate_max_players(0).is_err());
        assert!(validate_max_players(101).is_err());
    }

    #[test]
    fn player_name_validation_obeys_config_overrides() {
        use validation::validate_player_name_with_config;

        let mut config = ProtocolConfig::default();
        config.player_name_validation.allow_spaces = false;
        config.player_name_validation.allow_unicode_alphanumeric = false;

        assert!(validate_player_name_with_config("AsciiName", &config).is_ok());
        assert!(validate_player_name_with_config("Player Two", &config).is_err());
        assert!(validate_player_name_with_config("玩家", &config).is_err());

        config.player_name_validation.allowed_symbols.push('!');
        assert!(validate_player_name_with_config("Alert!", &config).is_ok());
    }

    #[test]
    fn test_player_name_uniqueness() {
        use std::collections::HashMap;
        use validation::*;

        let mut players = HashMap::new();

        // Add first player
        players.insert(
            Uuid::new_v4(),
            PlayerInfo {
                id: Uuid::new_v4(),
                name: "Player1".to_string(),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                region_id: types::DEFAULT_REGION_ID.to_string(),
            },
        );

        // Different name should be OK
        assert!(validate_player_name_uniqueness("Player2", &players).is_ok());

        // Exact same name should fail
        assert!(validate_player_name_uniqueness("Player1", &players).is_err());

        // Case insensitive check should fail
        assert!(validate_player_name_uniqueness("player1", &players).is_err());
        assert!(validate_player_name_uniqueness("PLAYER1", &players).is_err());
    }

    #[test]
    fn test_room_code_generation() {
        use room_codes::*;

        let code = generate_room_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_alphanumeric()));
        assert!(code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));

        let clean_code = generate_clean_room_code();
        assert_eq!(clean_code.len(), 6);
        // Should not contain confusing characters
        assert!(!clean_code.contains('0'));
        assert!(!clean_code.contains('O'));
        assert!(!clean_code.contains('I'));
        assert!(!clean_code.contains('1'));

        // Generate multiple codes to test uniqueness probability
        let mut codes = std::collections::HashSet::new();
        for _ in 0..100 {
            codes.insert(generate_clean_room_code());
        }
        // Should generate many unique codes (high probability)
        assert!(codes.len() > 90);
    }

    #[test]
    fn test_authority_protocol_basic_rules() {
        // Test first player gets authority rule
        let mut room = Room::new(
            "test_game".to_string(),
            "AUTH01".to_string(),
            4,
            true,
            "matchbox".to_string(),
        );

        let player1_id = Uuid::new_v4();
        let player1 = PlayerInfo {
            id: player1_id,
            name: "Player1".to_string(),
            is_authority: true, // First player should get authority
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        room.add_player(player1);
        room.authority_player = Some(player1_id);

        assert_eq!(room.authority_player, Some(player1_id));
        assert!(room.players[&player1_id].is_authority);

        // Add second player - should NOT get authority
        let player2_id = Uuid::new_v4();
        let player2 = PlayerInfo {
            id: player2_id,
            name: "Player2".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        room.add_player(player2);

        // Only first player should have authority
        assert_eq!(room.authority_player, Some(player1_id));
        assert!(room.players[&player1_id].is_authority);
        assert!(!room.players[&player2_id].is_authority);
    }

    #[test]
    fn test_authority_protocol_single_authority_rule() {
        let mut room = Room::new(
            "test_game".to_string(),
            "SINGLE".to_string(),
            4,
            true,
            "matchbox".to_string(),
        );

        let player1_id = Uuid::new_v4();
        let player2_id = Uuid::new_v4();

        let player1 = PlayerInfo {
            id: player1_id,
            name: "Player1".to_string(),
            is_authority: true,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };
        let player2 = PlayerInfo {
            id: player2_id,
            name: "Player2".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        room.add_player(player1);
        room.add_player(player2);
        room.authority_player = Some(player1_id);

        // Test: Can only have 0 or 1 players with authority
        let authority_count = room.players.values().filter(|p| p.is_authority).count();
        assert_eq!(authority_count, 1);

        // Test authority transfer (clear old, set new)
        room.clear_authority();
        let authority_count_after_clear = room.players.values().filter(|p| p.is_authority).count();
        assert_eq!(authority_count_after_clear, 0);
        assert_eq!(room.authority_player, None);

        // Set new authority
        room.set_authority(Some(player2_id));
        let authority_count_after_set = room.players.values().filter(|p| p.is_authority).count();
        assert_eq!(authority_count_after_set, 1);
        assert_eq!(room.authority_player, Some(player2_id));
        assert!(!room.players[&player1_id].is_authority);
        assert!(room.players[&player2_id].is_authority);
    }

    #[test]
    fn test_authority_protocol_no_auto_reassignment() {
        let mut room = Room::new(
            "test_game".to_string(),
            "NOAUTO".to_string(),
            4,
            true,
            "matchbox".to_string(),
        );

        let player1_id = Uuid::new_v4();
        let player2_id = Uuid::new_v4();

        let player1 = PlayerInfo {
            id: player1_id,
            name: "AuthorityPlayer".to_string(),
            is_authority: true,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };
        let player2 = PlayerInfo {
            id: player2_id,
            name: "RegularPlayer".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        room.add_player(player1);
        room.add_player(player2);
        room.authority_player = Some(player1_id);

        // Authority player leaves
        room.remove_player(&player1_id);

        // Per protocol: Authority should be cleared, NOT auto-assigned to remaining player
        assert_eq!(room.authority_player, None);
        if let Some(remaining_player) = room.players.get(&player2_id) {
            assert!(!remaining_player.is_authority);
        }
    }

    #[test]
    fn test_authority_protocol_room_support_validation() {
        // Room WITH authority support
        let mut auth_room = Room::new(
            "auth_game".to_string(),
            "WITHAUTH".to_string(),
            4,
            true,
            "matchbox".to_string(),
        );
        assert!(auth_room.supports_authority);

        let player_id = Uuid::new_v4();
        let player = PlayerInfo {
            id: player_id,
            name: "Player".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        auth_room.add_player(player);

        // Should be able to set authority
        assert!(auth_room.set_authority(Some(player_id)));
        assert_eq!(auth_room.authority_player, Some(player_id));

        // Room WITHOUT authority support
        let mut no_auth_room = Room::new(
            "noauth_game".to_string(),
            "NOAUTH".to_string(),
            4,
            false,
            "matchbox".to_string(),
        );
        assert!(!no_auth_room.supports_authority);

        let player2_id = Uuid::new_v4();
        let player2 = PlayerInfo {
            id: player2_id,
            name: "Player2".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };

        no_auth_room.add_player(player2);

        // Should NOT be able to set authority
        assert!(!no_auth_room.set_authority(Some(player2_id)));
        assert_eq!(no_auth_room.authority_player, None);
        assert!(!no_auth_room.players[&player2_id].is_authority);
    }

    #[test]
    fn test_lobby_state_transitions() {
        let mut room = Room::new(
            "lobby_game".to_string(),
            "LOBBY1".to_string(),
            2,
            true,
            "matchbox".to_string(),
        );

        // Initially in waiting state
        assert_eq!(room.lobby_state, LobbyState::Waiting);
        assert!(!room.should_enter_lobby());
        assert!(!room.all_players_ready());

        // Add first player
        let player1_id = Uuid::new_v4();
        let player1 = PlayerInfo {
            id: player1_id,
            name: "Player1".to_string(),
            is_authority: true,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };
        room.add_player(player1);

        // Still shouldn't enter lobby with only one player
        assert!(!room.should_enter_lobby());
        assert!(!room.enter_lobby());
        assert_eq!(room.lobby_state, LobbyState::Waiting);

        // Add second player (room is now full)
        let player2_id = Uuid::new_v4();
        let player2 = PlayerInfo {
            id: player2_id,
            name: "Player2".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        };
        room.add_player(player2);

        // Now should transition to lobby
        assert!(room.should_enter_lobby());
        assert!(room.enter_lobby());
        assert_eq!(room.lobby_state, LobbyState::Lobby);
        assert!(room.lobby_started_at.is_some());

        // Players should be able to mark themselves ready
        assert!(room.set_player_ready(&player1_id, true));
        assert_eq!(room.ready_players.len(), 1);
        assert!(room.players[&player1_id].is_ready);
        assert!(!room.all_players_ready());

        // Mark second player ready
        assert!(room.set_player_ready(&player2_id, true));
        assert_eq!(room.ready_players.len(), 2);
        assert!(room.all_players_ready());

        // Should be able to finalize game
        assert!(room.finalize_game());
        assert_eq!(room.lobby_state, LobbyState::Finalized);
        assert!(room.game_finalized_at.is_some());
        assert!(room.is_finalized());
    }

    #[test]
    fn test_lobby_ready_state_changes() {
        let mut room = Room::new(
            "ready_game".to_string(),
            "READY1".to_string(),
            3,
            true,
            "matchbox".to_string(),
        );

        // Add three players
        let player1_id = Uuid::new_v4();
        let player2_id = Uuid::new_v4();
        let player3_id = Uuid::new_v4();

        for (id, name) in [(player1_id, "P1"), (player2_id, "P2"), (player3_id, "P3")] {
            room.add_player(PlayerInfo {
                id,
                name: name.to_string(),
                is_authority: id == player1_id,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                region_id: types::DEFAULT_REGION_ID.to_string(),
            });
        }

        // Enter lobby
        room.enter_lobby();

        // Mark players ready one by one
        room.set_player_ready(&player1_id, true);
        assert!(!room.all_players_ready());

        room.set_player_ready(&player2_id, true);
        assert!(!room.all_players_ready());

        room.set_player_ready(&player3_id, true);
        assert!(room.all_players_ready());

        // Unready one player
        room.set_player_ready(&player2_id, false);
        assert!(!room.all_players_ready());
        assert!(!room.players[&player2_id].is_ready);
        assert_eq!(room.ready_players.len(), 2);
    }

    #[test]
    fn test_peer_connections() {
        let mut room = Room::new(
            "peer_game".to_string(),
            "PEER01".to_string(),
            2,
            true,
            "matchbox".to_string(),
        );

        let player1_id = Uuid::new_v4();
        let player2_id = Uuid::new_v4();

        room.add_player(PlayerInfo {
            id: player1_id,
            name: "Authority".to_string(),
            is_authority: true,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        });

        room.add_player(PlayerInfo {
            id: player2_id,
            name: "Player".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: chrono::Utc::now(),
            connection_info: None,
            region_id: types::DEFAULT_REGION_ID.to_string(),
        });

        let peer_connections = room.get_peer_connections();
        assert_eq!(peer_connections.len(), 2);

        // Verify authority and player info is correct
        let auth_peer = peer_connections.iter().find(|p| p.is_authority).unwrap();
        let player_peer = peer_connections.iter().find(|p| !p.is_authority).unwrap();

        assert_eq!(auth_peer.player_id, player1_id);
        assert_eq!(auth_peer.player_name, "Authority");
        assert_eq!(player_peer.player_id, player2_id);
        assert_eq!(player_peer.player_name, "Player");
    }

    #[test]
    fn test_lobby_edge_cases() {
        let mut room = Room::new(
            "edge_game".to_string(),
            "EDGE01".to_string(),
            2,
            false,
            "matchbox".to_string(),
        );

        // Add players
        let player1_id = Uuid::new_v4();
        let player2_id = Uuid::new_v4();

        for id in [player1_id, player2_id] {
            room.add_player(PlayerInfo {
                id,
                name: format!("Player{id}"),
                is_authority: false,
                is_ready: false,
                connected_at: chrono::Utc::now(),
                connection_info: None,
                region_id: types::DEFAULT_REGION_ID.to_string(),
            });
        }

        // Enter lobby
        room.enter_lobby();

        // Can't set ready for non-existent player
        let fake_id = Uuid::new_v4();
        assert!(!room.set_player_ready(&fake_id, true));

        // Can't finalize without all players ready
        room.set_player_ready(&player1_id, true);
        assert!(!room.finalize_game());

        // Can't set ready when not in lobby state
        room.lobby_state = LobbyState::Waiting;
        assert!(!room.set_player_ready(&player1_id, false));
    }

    fn expected_game_name_ok(name: &str, config: &ProtocolConfig) -> bool {
        !name.is_empty()
            && name.len() <= config.max_game_name_length
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ' ')
    }

    fn expected_room_code_ok(code: &str, config: &ProtocolConfig) -> bool {
        code.len() == config.room_code_length && code.chars().all(|c| c.is_ascii_alphanumeric())
    }

    fn expected_player_name_ok(name: &str, config: &ProtocolConfig) -> bool {
        if name.is_empty() || name.len() > config.max_player_name_length {
            return false;
        }

        let trimmed = name.trim();
        if trimmed.is_empty() {
            return false;
        }

        let rules = &config.player_name_validation;
        if !rules.allow_leading_trailing_whitespace && trimmed.len() != name.len() {
            return false;
        }

        for ch in name.chars() {
            if ch == ' ' {
                if rules.allow_spaces {
                    continue;
                }
                return false;
            }
            if ch.is_whitespace() {
                return false;
            }

            let is_alphanumeric = if rules.allow_unicode_alphanumeric {
                ch.is_alphanumeric()
            } else {
                ch.is_ascii_alphanumeric()
            };

            if is_alphanumeric || rules.is_allowed_symbol(ch) {
                continue;
            }

            return false;
        }

        true
    }

    proptest! {
        #[test]
        fn game_name_validation_matches_predicate(raw in proptest::collection::vec(any::<char>(), 0..=64)) {
            let candidate: String = raw.into_iter().collect();
            let config = ProtocolConfig::default();
            prop_assert_eq!(
                validate_game_name_with_config(&candidate, &config).is_ok(),
                expected_game_name_ok(&candidate, &config)
            );
        }

        #[test]
        fn room_code_validation_matches_predicate(raw in proptest::collection::vec(any::<char>(), 0..=10)) {
            let candidate: String = raw.into_iter().collect();
            let config = ProtocolConfig::default();
            prop_assert_eq!(
                validate_room_code_with_config(&candidate, &config).is_ok(),
                expected_room_code_ok(&candidate, &config)
            );
        }

        #[test]
        fn player_name_validation_matches_predicate(raw in proptest::collection::vec(any::<char>(), 0..=32)) {
            let candidate: String = raw.into_iter().collect();
            let config = ProtocolConfig::default();
            prop_assert_eq!(
                validate_player_name_with_config(&candidate, &config).is_ok(),
                expected_player_name_ok(&candidate, &config)
            );
        }

    }

    #[test]
    fn player_name_rules_payload_reflects_protocol_config() {
        let mut config = ProtocolConfig {
            max_player_name_length: 40,
            ..ProtocolConfig::default()
        };
        config.player_name_validation.allow_spaces = false;
        config.player_name_validation.allowed_symbols = vec!['*'];
        config.player_name_validation.additional_allowed_characters = Some("!?".to_string());

        let hint = PlayerNameRulesPayload::from_protocol_config(&config);
        assert_eq!(hint.max_length, 40);
        assert_eq!(hint.min_length, 1);
        assert!(!hint.allow_spaces);
        assert!(hint.allow_unicode_alphanumeric);
        assert!(hint.allowed_symbols.contains(&'*'));
        assert_eq!(hint.additional_allowed_characters.as_deref(), Some("!?"));
    }

    #[test]
    fn region_room_code_applies_prefix() {
        let config = ProtocolConfig {
            room_code_length: 6,
            ..ProtocolConfig::default()
        };
        let code = room_codes::generate_region_room_code(&config, Some("na"));
        assert!(code.starts_with("NA"));
        assert_eq!(code.len(), 6);
    }

    #[test]
    fn region_room_code_falls_back_when_prefix_too_long() {
        let config = ProtocolConfig {
            room_code_length: 4,
            ..ProtocolConfig::default()
        };
        let code = room_codes::generate_region_room_code(&config, Some("LONGPREFIX"));
        assert_eq!(code.len(), 4);
    }
}
