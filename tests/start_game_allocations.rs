//! Game start must not copy unrelated spectator records.

use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, InMemoryRoomOperationCoordinator,
    MessageCoordinator, RoomOperationCoordinatorTrait, StartGameOutcome,
};
use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::{LobbyState, ServerMessage, SpectatorInfo};
use signal_fish_server::server::InMemoryMessageCoordinator;
use std::sync::Arc;
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// Bound successful and rejected starts independently of the spectator roster.
#[tokio::test]
async fn start_game_does_not_copy_spectator_metadata() {
    let mut bounded = true;
    for spectators in [0_u128, 16, 255] {
        let database = Arc::new(InMemoryDatabase::new());
        let member = Uuid::from_u128(1);
        let room = database
            .create_room(
                "start-game".into(),
                None,
                4,
                true,
                member,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        for index in 0..spectators {
            let name = format!("Spectator{index:023}");
            signal_fish_server::protocol::validation::validate_player_name_with_config(
                &name,
                &signal_fish_server::config::ProtocolConfig::default(),
            )
            .unwrap();
            assert!(database
                .add_spectator_to_room(
                    &room.id,
                    SpectatorInfo {
                        id: Uuid::from_u128(index + 2),
                        name,
                        connected_at: None,
                    }
                )
                .await
                .unwrap());
        }
        let messages = Arc::new(InMemoryMessageCoordinator::new());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        messages
            .register_local_client(
                member,
                Some(room.id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .unwrap();
        let coordinator = InMemoryRoomOperationCoordinator::new(messages, database.clone(), None);
        for expected in ["forbidden", "not-ready", "started", "already-started"] {
            if expected == "started" {
                coordinator
                    .handle_player_ready(&room.id, &member, None)
                    .await
                    .unwrap();
                let readiness = receiver.try_recv().unwrap();
                assert!(matches!(
                    readiness.as_ref(),
                    ServerMessage::LobbyStateChanged {
                        all_ready: true,
                        ..
                    }
                ));
            }
            let region = stats_alloc::Region::new(ALLOCATOR);
            let requester = if expected == "forbidden" {
                Uuid::nil()
            } else {
                member
            };
            let result = coordinator
                .handle_start_game(&room.id, &requester)
                .await
                .unwrap();
            let stats = region.change();
            println!(
                "START_GAME_PROFILE spectators={spectators} outcome={expected} stats={stats:?}"
            );
            bounded &= stats.bytes_allocated < 20_000;
            match (expected, result) {
                ("forbidden", StartGameOutcome::Forbidden)
                | ("not-ready", StartGameOutcome::NotReady)
                | ("already-started", StartGameOutcome::AlreadyStarted) => {
                    let unexpected = receiver.try_recv();
                    assert!(
                        matches!(
                            unexpected,
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                        ),
                        "rejected start emitted an unexpected message: {unexpected:?}"
                    );
                }
                ("started", StartGameOutcome::Started(finalized)) => {
                    assert_eq!(finalized.game_name, "start-game");
                    assert_eq!(finalized.authority_player, Some(member));
                    assert_eq!(finalized.members.len(), 1);
                    assert_eq!(finalized.members[0].id, member);
                    let message = receiver.try_recv().unwrap();
                    let ServerMessage::GameStarting { peer_connections } = message.as_ref() else {
                        panic!("expected GameStarting, got {message:?}");
                    };
                    assert_eq!(peer_connections.len(), 1);
                    assert_eq!(peer_connections[0].player_id, member);
                    assert_eq!(peer_connections[0].relay_type, "relay");
                    assert_eq!(
                        database.get_room_lobby_state(&room.id).await.unwrap(),
                        Some(LobbyState::Finalized)
                    );
                }
                (_, result) => panic!("expected {expected}, got {result:?}"),
            }
        }
        assert_eq!(
            database.get_room_spectators(&room.id).await.unwrap().len(),
            usize::try_from(spectators).unwrap()
        );
    }
    assert!(bounded, "start copied unrelated spectator metadata");
}
