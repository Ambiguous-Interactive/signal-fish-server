//! Allocation contract for complete player admission with stored spectators.

mod test_helpers;

use signal_fish_server::protocol::{ServerMessage, SpectatorInfo};
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// Bound roster copies while preserving the joining player's complete baseline.
#[tokio::test]
async fn player_admission_does_not_copy_the_room_to_find_its_lane() {
    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 255] {
        let server = test_helpers::create_test_server().await;
        let host = Uuid::from_u128(1);
        let player = Uuid::from_u128(2);
        let (host_sender, mut host_receiver) = tokio::sync::mpsc::channel(16);
        let (player_sender, mut player_receiver) = tokio::sync::mpsc::channel(16);
        server.connect_client(host, host_sender).await;
        server.connect_client(player, player_sender).await;
        server
            .handle_join_room(
                &host,
                "player-profile".into(),
                Some("PPROF1".into()),
                "Host".into(),
                Some(4),
                Some(true),
                None,
                None,
                None,
            )
            .await;
        {
            let created = host_receiver.try_recv().unwrap();
            assert!(
                matches!(created.as_ref(), ServerMessage::RoomJoined(payload) if payload.player_id == host && payload.room_code == "PPROF1")
            );
            let lobby = host_receiver.try_recv().unwrap();
            assert!(matches!(
                lobby.as_ref(),
                ServerMessage::LobbyStateChanged { .. }
            ));
            let remaining = host_receiver.try_recv();
            assert!(matches!(
                remaining,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
        }
        let room_id = server.get_client_room(&host).await.unwrap();
        server
            .database()
            .set_room_max_spectators(&room_id, Some(255))
            .await
            .unwrap();
        for index in 0..spectators {
            assert!(server
                .database()
                .add_spectator_to_room(
                    &room_id,
                    SpectatorInfo {
                        id: Uuid::from_u128(index + 3),
                        name: format!("Viewer{index}"),
                        connected_at: None,
                    }
                )
                .await
                .unwrap());
        }
        let game = "player-profile".into();
        let code = "PPROF1".into();
        let name = "Player".into();
        let region = stats_alloc::Region::new(ALLOCATOR);
        server
            .handle_join_room(
                &player,
                game,
                Some(code),
                name,
                Some(4),
                Some(true),
                None,
                None,
                None,
            )
            .await;
        let stats = region.change();
        println!("PLAYER_JOIN_PROFILE spectators={spectators} stats={stats:?}");
        profiles.push((spectators, stats.allocations, stats.bytes_allocated));
        let baseline = player_receiver.try_recv().unwrap();
        let ServerMessage::RoomJoined(payload) = baseline.as_ref() else {
            panic!("expected player baseline, got {baseline:?}");
        };
        assert_eq!(payload.room_id, room_id);
        assert_eq!(payload.room_code, "PPROF1");
        assert_eq!(payload.player_id, player);
        assert_eq!(payload.current_players.len(), 2);
        assert!(payload
            .current_players
            .iter()
            .any(|entry| entry.id == host && entry.name == "Host"));
        assert!(payload
            .current_players
            .iter()
            .any(|entry| entry.id == player && entry.name == "Player"));
        assert_eq!(
            payload.current_spectators.len(),
            usize::try_from(spectators).unwrap()
        );
        for index in 0..spectators {
            assert!(payload
                .current_spectators
                .iter()
                .any(|entry| entry.id == Uuid::from_u128(index + 3)
                    && entry.name == format!("Viewer{index}")
                    && entry.connected_at.is_none()));
        }
        let joined = host_receiver.try_recv().unwrap();
        assert!(
            matches!(joined.as_ref(), ServerMessage::PlayerJoined { player: joined } if joined.id == player && joined.name == "Player")
        );
        assert_eq!(server.get_client_room(&player).await, Some(room_id));
    }
    println!("PLAYER_JOIN_ALLOCATIONS {profiles:?}");
    let empty_allocations = profiles[0].1;
    for (spectators, allocations, _) in profiles {
        assert!(allocations <= empty_allocations + 4 * usize::try_from(spectators).unwrap() + 8,
            "player admission exceeds four spectator copies: spectators={spectators} allocations={allocations} empty={empty_allocations}");
    }
}
