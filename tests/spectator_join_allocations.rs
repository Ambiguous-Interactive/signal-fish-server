//! Allocation contract for the complete spectator admission path.

mod test_helpers;

use signal_fish_server::protocol::{ServerMessage, SpectatorInfo};
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// Bound roster-copy growth while preserving both spectator admission snapshots.
#[tokio::test]
async fn spectator_admission_reuses_its_owned_room_and_roster() {
    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 254] {
        let server = test_helpers::create_test_server().await;
        let host = Uuid::from_u128(1);
        let spectator = Uuid::from_u128(2);
        let (host_sender, mut host_receiver) = tokio::sync::mpsc::channel(16);
        let (spectator_sender, mut spectator_receiver) = tokio::sync::mpsc::channel(16);
        server.connect_client(host, host_sender).await;
        server.connect_client(spectator, spectator_sender).await;
        server
            .handle_join_room(
                &host,
                "spectator-profile".into(),
                Some("SPROF1".into()),
                "Host".into(),
                Some(4),
                Some(true),
                None,
                None,
                None,
            )
            .await;
        let room_joined = host_receiver.try_recv().unwrap();
        assert!(matches!(room_joined.as_ref(), ServerMessage::RoomJoined(_)));
        let lobby = host_receiver.try_recv().unwrap();
        assert!(matches!(
            lobby.as_ref(),
            ServerMessage::LobbyStateChanged { .. }
        ));
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
                    },
                )
                .await
                .unwrap());
        }
        server
            .handle_join_as_spectator(
                &spectator,
                "spectator-profile".into(),
                "SPROF1".into(),
                "Viewer".into(),
                None,
            )
            .await;
        let warm_baseline = spectator_receiver.try_recv().unwrap();
        assert!(matches!(
            warm_baseline.as_ref(),
            ServerMessage::SpectatorJoined(_)
        ));
        let warm_notification = host_receiver.try_recv().unwrap();
        assert!(matches!(
            warm_notification.as_ref(),
            ServerMessage::NewSpectatorJoined { .. }
        ));
        server.handle_leave_spectator(&spectator).await;
        let left = spectator_receiver.try_recv().unwrap();
        assert!(matches!(left.as_ref(), ServerMessage::SpectatorLeft { .. }));
        let disconnected = host_receiver.try_recv().unwrap();
        assert!(matches!(
            disconnected.as_ref(),
            ServerMessage::SpectatorDisconnected { .. }
        ));

        let game_name = "spectator-profile".into();
        let room_code = "SPROF1".into();
        let spectator_name = "Viewer".into();
        let region = stats_alloc::Region::new(ALLOCATOR);
        server
            .handle_join_as_spectator(&spectator, game_name, room_code, spectator_name, None)
            .await;
        let stats = region.change();
        println!("SPECTATOR_JOIN_PROFILE spectators={spectators} stats={stats:?}");
        profiles.push((spectators, stats.allocations));

        let baseline = spectator_receiver.try_recv().unwrap();
        let ServerMessage::SpectatorJoined(payload) = baseline.as_ref() else {
            panic!("expected spectator baseline, got {baseline:?}");
        };
        assert_eq!(payload.room_id, room_id);
        assert_eq!(payload.room_code, "SPROF1");
        assert_eq!(payload.game_name, "spectator-profile");
        assert_eq!(payload.current_players.len(), 1);
        assert_eq!(payload.current_players[0].id, host);
        assert_eq!(
            payload.current_spectators.len(),
            usize::try_from(spectators).unwrap() + 1
        );
        let notification = host_receiver.try_recv().unwrap();
        let ServerMessage::NewSpectatorJoined {
            spectator: joined,
            current_spectators,
            spectator_count,
            ..
        } = notification.as_ref()
        else {
            panic!("expected spectator notification, got {notification:?}");
        };
        assert_eq!(joined.id, spectator);
        assert_eq!(
            serde_json::to_value(current_spectators).unwrap(),
            serde_json::to_value(&payload.current_spectators).unwrap()
        );
        assert_eq!(
            *spectator_count,
            Some(u32::try_from(spectators).unwrap() + 1)
        );
    }
    println!("SPECTATOR_JOIN_ALLOCATIONS {profiles:?}");
    let empty_allocations = profiles[0].1;
    for (spectators, allocations) in profiles {
        assert!(
            allocations <= empty_allocations + 5 * usize::try_from(spectators).unwrap(),
            "spectator roster allocation growth exceeded five copies: spectators={spectators} allocations={allocations} empty={empty_allocations}"
        );
    }
}
