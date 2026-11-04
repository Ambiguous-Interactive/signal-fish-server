//! Allocation profiles for complete ready toggles and lobby entry.
use super::*;
use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, InMemoryRoomOperationCoordinator,
    MessageCoordinator, RoomOperationCoordinatorTrait,
};
use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::PlayerInfo;
use signal_fish_server::server::InMemoryMessageCoordinator;
use std::sync::Arc;

pub(crate) fn readiness_reuses_membership(lobby: bool) {
    for members in [2_u128, 16, 255] {
        allocation_harness::block_on(async {
            tokio::task::yield_now().await;
            let database = Arc::new(InMemoryDatabase::new());
            let actor = Uuid::from_u128(1);
            let room = database
                .create_room(
                    "ready-cost".into(),
                    None,
                    255,
                    false,
                    actor,
                    "relay".into(),
                    "test".into(),
                    None,
                )
                .await
                .unwrap();
            let messages = Arc::new(InMemoryMessageCoordinator::new());
            let mut receivers = Vec::new();
            for index in 1..=members {
                let id = Uuid::from_u128(index);
                if index > 1 {
                    assert!(database
                        .add_player_to_room(
                            &room.id,
                            PlayerInfo {
                                id,
                                name: format!("Player{index}"),
                                is_authority: false,
                                is_ready: false,
                                connected_at: None,
                                connection_info: None,
                                epoch: None,
                                seq: None,
                                region_id: "test".into(),
                            }
                        )
                        .await
                        .unwrap());
                }
                let (sender, receiver) = tokio::sync::mpsc::channel(8);
                receivers.push(receiver);
                messages
                    .register_local_client(
                        id,
                        Some(room.id),
                        ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                    )
                    .await
                    .unwrap();
            }
            let coordinator =
                InMemoryRoomOperationCoordinator::new(messages, database.clone(), None);
            let region = stats_alloc::Region::new(ALLOCATOR);
            if lobby {
                assert!(coordinator
                    .transition_room_to_lobby(&room.id)
                    .await
                    .unwrap());
            } else {
                coordinator
                    .handle_player_ready(&room.id, &actor, None)
                    .await
                    .unwrap();
            }
            let stats = region.change();
            println!("READINESS_COST lobby={lobby} members={members} stats={stats:?}");
            if members == 255 {
                let trace_storage = if cfg!(feature = "trace-validation") {
                    9_000
                } else {
                    0
                };
                let budget = 447_000 + trace_storage;
                assert!(stats.bytes_allocated <= budget,
                    "readiness rebuilt effective membership: lobby={lobby} budget={budget} stats={stats:?}");
            }
            for receiver in &mut receivers {
                let message = receiver.try_recv().unwrap();
                let ServerMessage::LobbyStateChanged {
                    lobby_state,
                    ready_players,
                    all_ready,
                } = message.as_ref()
                else {
                    panic!("{message:?}");
                };
                assert_eq!(
                    *lobby_state,
                    signal_fish_server::protocol::LobbyState::Lobby
                );
                assert_eq!(ready_players, &if lobby { vec![] } else { vec![actor] });
                assert!(!all_ready);
                assert_membership_queue_empty(receiver);
            }
            assert_eq!(
                coordinator.current_ready_players(&room.id).await,
                if lobby { vec![] } else { vec![actor] }
            );
        });
    }
}
