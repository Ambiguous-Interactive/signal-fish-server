use super::*;
use crate::protocol::{LobbyState, RoomOperationResult};

fn replay_event(index: u128, v3: bool, name: String) -> ServerMessage {
    ServerMessage::PlayerJoined {
        player: PlayerInfo {
            id: PlayerId::from_u128(10 + index),
            name,
            is_authority: false,
            is_ready: false,
            connected_at: None,
            connection_info: None,
            epoch: v3.then_some(1),
            seq: v3.then_some(7),
            region_id: "test".into(),
        },
    }
}

fn baseline(v3: bool, correlated: bool) -> ServerMessage {
    let message = ServerMessage::Reconnected(Box::new(ReconnectedPayload {
        room_id: RoomId::from_u128(1),
        room_code: "BUDGET".into(),
        player_id: PlayerId::from_u128(2),
        game_name: "game".into(),
        max_players: 4,
        supports_authority: false,
        current_players: Vec::new(),
        is_authority: false,
        lobby_state: LobbyState::Waiting,
        ready_players: Vec::new(),
        relay_type: "matchbox".into(),
        current_spectators: Vec::new(),
        ice_servers: Vec::new(),
        missed_events: (0..3)
            .map(|index| replay_event(index, v3, format!("{index}: 🐟\"\\\n")))
            .collect(),
        replay: v3.then_some(ReplayStatus::Complete),
        sender_watermarks: Vec::new(),
        reconnection_token: v3.then(|| "rotated-token".into()),
    }));
    if correlated {
        ServerMessage::RoomOperationResult {
            operation_id: crate::protocol::RoomOperationId::from_u128(3),
            result: Box::new(match message {
                ServerMessage::Reconnected(payload) => RoomOperationResult::Reconnected(payload),
                _ => unreachable!(),
            }),
        }
    } else {
        message
    }
}

fn wire_size(message: &ServerMessage) -> usize {
    serde_json::to_vec(message).expect("wire JSON").len()
}

#[test]
fn reconnect_replay_budget_preserves_exact_boundary_and_largest_suffix() {
    for (v3, correlated) in [(false, false), (true, false), (true, true)] {
        let original = baseline(v3, correlated);
        let full_size = wire_size(&original);
        assert_eq!(reconnect_json_size(&original).unwrap(), full_size);
        for cap in [full_size, full_size + 1] {
            let complete = bound_reconnect_baseline(original.clone(), v3, cap).unwrap();
            assert_eq!(
                serde_json::to_value(complete).unwrap(),
                serde_json::to_value(&original).unwrap()
            );
        }
        let mut suffix = original.clone();
        let payload = reconnect_payload_mut(&mut suffix).unwrap();
        payload.missed_events.remove(0);
        payload.replay = v3.then_some(ReplayStatus::Truncated);
        let suffix_size = wire_size(&suffix);
        for cap in [full_size - 1, suffix_size] {
            let fitted = bound_reconnect_baseline(original.clone(), v3, cap).unwrap();
            assert_eq!(
                serde_json::to_value(fitted).unwrap(),
                serde_json::to_value(&suffix).unwrap(),
                "v3={v3}, correlated={correlated}, cap={cap}"
            );
        }
        let fitted = bound_reconnect_baseline(original, v3, suffix_size - 1).unwrap();
        reconnect_payload_mut(&mut suffix)
            .unwrap()
            .missed_events
            .remove(0);
        assert_eq!(
            serde_json::to_value(fitted).unwrap(),
            serde_json::to_value(suffix).unwrap()
        );
    }
}

#[test]
fn reconnect_replay_budget_preserves_empty_baseline_and_refuses_oversized_snapshot() {
    for (v3, correlated) in [(false, false), (true, false), (true, true)] {
        let mut original = baseline(v3, correlated);
        reconnect_payload_mut(&mut original)
            .unwrap()
            .missed_events
            .clear();
        let size = wire_size(&original);
        assert!(bound_reconnect_baseline(original.clone(), v3, size - 1).is_err());
        let fitted = bound_reconnect_baseline(original.clone(), v3, size).unwrap();
        assert_eq!(
            serde_json::to_value(fitted).unwrap(),
            serde_json::to_value(original).unwrap()
        );
    }
    let mut unavailable = baseline(true, false);
    let payload = reconnect_payload_mut(&mut unavailable).unwrap();
    payload.missed_events.clear();
    payload.replay = Some(ReplayStatus::Unavailable);
    let size = wire_size(&unavailable);
    let fitted = bound_reconnect_baseline(unavailable.clone(), true, size).unwrap();
    assert_eq!(
        serde_json::to_value(fitted).unwrap(),
        serde_json::to_value(unavailable).unwrap()
    );
}

#[test]
fn reconnect_replay_budget_keeps_no_history_when_newest_event_cannot_fit() {
    for (v3, correlated) in [(false, false), (true, false), (true, true)] {
        let mut original = baseline(v3, correlated);
        let payload = reconnect_payload_mut(&mut original).unwrap();
        payload
            .missed_events
            .push(replay_event(50, v3, "large".repeat(1000)));
        let mut empty = original.clone();
        let payload = reconnect_payload_mut(&mut empty).unwrap();
        payload.missed_events.clear();
        payload.replay = v3.then_some(ReplayStatus::Truncated);
        let size = wire_size(&empty);
        for cap in [size, size + 200] {
            let fitted = bound_reconnect_baseline(original.clone(), v3, cap).unwrap();
            assert_eq!(
                serde_json::to_value(fitted).unwrap(),
                serde_json::to_value(&empty).unwrap(),
                "must retain a suffix, never skip the oversized newest event"
            );
        }
    }
}

#[test]
fn reconnect_replay_budget_sizes_the_v3_wire_projection_before_truncation() {
    for correlated in [false, true] {
        let mut original = baseline(true, correlated);
        let payload = reconnect_payload_mut(&mut original).unwrap();
        let ServerMessage::PlayerJoined { mut player } = replay_event(1, true, "Peer".into())
        else {
            unreachable!();
        };
        player.connected_at = Some(chrono::DateTime::UNIX_EPOCH);
        player.connection_info = Some(crate::protocol::ConnectionInfo::WebRTC {
            sdp: Some("sdp".repeat(1000)),
            ice_candidates: Vec::new(),
        });
        payload.current_players.push(player.clone());
        payload
            .missed_events
            .push(ServerMessage::PlayerJoined { player });
        let mut projected = original.clone();
        crate::websocket::project_reconnect_payload_for_v3(
            reconnect_payload_mut(&mut projected).unwrap(),
        );
        let cap = wire_size(&projected);
        assert!(wire_size(&original) > cap);
        let fitted = bound_reconnect_baseline(original, true, cap).unwrap();
        assert_eq!(
            serde_json::to_value(fitted).unwrap(),
            serde_json::to_value(projected).unwrap(),
            "v2-only metadata must not cause false truncation"
        );
    }
}
