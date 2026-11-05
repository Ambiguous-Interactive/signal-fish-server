//! Readiness and exact-membership reply allocation controls.
use super::*;
use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, MessageCoordinator,
};
use signal_fish_server::protocol::PlayerInfo;
use signal_fish_server::server::{
    allocation_benchmark::apply_snapshot_readiness, InMemoryMessageCoordinator,
};
use std::sync::Arc;

/// Bound readiness heap use while preserving order, metadata, and oversized custom inputs.
pub(crate) fn readiness_sort_avoids_heap_storage() {
    for count in [127_u128, 128, 129, 255, 256, 512] {
        let mut players: Vec<_> = (1..=count)
            .map(|id| PlayerInfo {
                id: Uuid::from_u128(id),
                name: format!("Player{id}"),
                is_authority: id == 1,
                is_ready: false,
                connected_at: None,
                connection_info: None,
                epoch: Some(7),
                seq: Some(9),
                region_id: "test".into(),
            })
            .collect();
        players.reverse();
        let ready: Vec<_> = (1..=count)
            .rev()
            .map(|id| Uuid::from_u128(if id == 2 { 1 } else { id }))
            .collect();
        let unchanged = ready.clone();
        let region = stats_alloc::Region::new(ALLOCATOR);
        apply_snapshot_readiness(&mut players, &ready);
        let stats = region.change();
        println!("READINESS_SORT_COST count={count} stats={stats:?}");
        for player in &players {
            assert_eq!(player.is_ready, ready.contains(&player.id));
            assert_eq!(player.name, format!("Player{}", player.id.as_u128()));
            assert_eq!(player.epoch, Some(7));
            assert_eq!(player.seq, Some(9));
        }
        assert_eq!(ready, unchanged);
        if count <= 255 {
            assert_eq!(
                stats.allocations, 0,
                "bounded readiness copied the ready roster: {stats:?}"
            );
        }
    }
}

/// Preserve exact replies and reject changed membership without copying current routes.
pub(crate) fn exact_membership_reply_avoids_current_roster_copy() {
    for count in [2_u128, 16, 255] {
        allocation_harness::block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let coordinator = InMemoryMessageCoordinator::new();
            let room = Uuid::from_u128(999);
            let mut receivers = Vec::new();
            let mut members: Vec<_> = (1..=count).map(Uuid::from_u128).collect();
            for &player in &members {
                let (sender, receiver) = tokio::sync::mpsc::channel(4);
                receivers.push(receiver);
                coordinator
                    .register_local_client(
                        player,
                        Some(room),
                        ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                    )
                    .await
                    .unwrap();
            }
            let message = Arc::new(ServerMessage::Pong);
            members.reverse();
            let target = Uuid::from_u128(1);
            let construction_region = stats_alloc::Region::new(ALLOCATOR);
            let unpolled = coordinator.send_to_player_in_room_if_members(
                &target,
                &room,
                &members,
                message.clone(),
            );
            drop(unpolled);
            let construction = construction_region.change();
            let watch_region = stats_alloc::Region::new(ALLOCATOR);
            let (sender, receiver) = tokio::sync::watch::channel(false);
            assert!(!*receiver.borrow());
            drop(receiver);
            drop(sender);
            let watch = watch_region.change();
            println!(
                "EXACT_REPLY_CONTROLS count={count} construction={construction:?} watch={watch:?}"
            );
            for measured in [false, true] {
                let region = stats_alloc::Region::new(ALLOCATOR);
                assert!(coordinator
                    .send_to_player_in_room_if_members(&target, &room, &members, message.clone())
                    .await
                    .unwrap());
                let stats = region.change();
                println!("EXACT_REPLY_COST count={count} measured={measured} stats={stats:?}");
                assert!(Arc::ptr_eq(&receivers[0].try_recv().unwrap(), &message));
                if measured {
                    assert!(
                        stats.allocations <= construction.allocations + watch.allocations + 1
                            && stats.bytes_allocated
                                <= construction.bytes_allocated
                                    + watch.bytes_allocated
                                    + std::mem::size_of_val(members.as_slice()),
                        "reply copied both expected and current rosters: {stats:?}"
                    );
                }
            }
            for invalid in [
                vec![],
                vec![target; usize::try_from(count).unwrap()],
                members.iter().copied().skip(1).collect(),
                {
                    let mut wrong = members.clone();
                    wrong[0] = Uuid::nil();
                    wrong
                },
            ] {
                assert!(!coordinator
                    .send_to_player_in_room_if_members(&target, &room, &invalid, message.clone())
                    .await
                    .unwrap());
                for receiver in &mut receivers {
                    assert!(matches!(
                        receiver.try_recv(),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                    ));
                }
                assert!(coordinator
                    .send_to_player_in_room_if_members(&target, &room, &members, message.clone(),)
                    .await
                    .unwrap());
                assert!(Arc::ptr_eq(&receivers[0].try_recv().unwrap(), &message));
            }
        });
    }
}
