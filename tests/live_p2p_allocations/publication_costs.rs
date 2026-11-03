//! Complete exact-membership publications with caller-owned frame batches.
use super::*;
use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, MessageCoordinator, RoomMessageTransactionOutcome,
    RoomRecipientMessages,
};
use signal_fish_server::server::InMemoryMessageCoordinator;
use std::sync::Arc;

pub(crate) fn transaction_reuses_owned_batches() {
    for count in [2_u128, 16, 255] {
        allocation_harness::block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let coordinator = InMemoryMessageCoordinator::new();
            let room = Uuid::from_u128(1000);
            let members: Vec<_> = (1..=count).map(Uuid::from_u128).collect();
            let first = Arc::new(ServerMessage::Pong);
            let second = Arc::new(ServerMessage::Error {
                message: "second phase".into(),
                error_code: None,
            });
            let mut receivers = Vec::new();
            for &id in &members {
                let (sender, receiver) = tokio::sync::mpsc::channel(4);
                receivers.push(receiver);
                coordinator
                    .register_local_client(
                        id,
                        Some(room),
                        ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                    )
                    .await
                    .unwrap();
            }
            for measured in [false, true] {
                let batches = members
                    .iter()
                    .rev()
                    .map(|&id| {
                        RoomRecipientMessages::in_order(id, vec![first.clone(), second.clone()])
                    })
                    .collect();
                let region = stats_alloc::Region::new(ALLOCATOR);
                let outcome = coordinator
                    .commit_room_messages_if_members_with_hook(
                        &room,
                        &members,
                        batches,
                        vec![],
                        Box::new(|| Box::pin(async { Ok(true) })),
                        Box::new(|failed| {
                            assert_eq!(failed, 0);
                            true
                        }),
                    )
                    .await
                    .unwrap();
                let stats = region.change();
                assert_eq!(outcome, RoomMessageTransactionOutcome::Committed);
                if measured {
                    println!("PUBLICATION_COST members={count} stats={stats:?}");
                    if count == 255 {
                        assert!(
                            stats.allocations <= 530,
                            "publication copied seated batches: {stats:?}"
                        );
                    }
                }
                for receiver in &mut receivers {
                    assert!(Arc::ptr_eq(&receiver.try_recv().unwrap(), &first));
                    assert!(Arc::ptr_eq(&receiver.try_recv().unwrap(), &second));
                    assert!(receiver.try_recv().is_err());
                }
            }
        });
    }
}

pub(crate) fn two_phase_batch_reserves_once() {
    use signal_fish_server::server::allocation_benchmark::publication_batch_for_measurement;
    for followup in [false, true] {
        let first = Arc::new(ServerMessage::Pong);
        let second = Arc::new(ServerMessage::Error {
            message: "plan frame".into(),
            error_code: None,
        });
        let region = stats_alloc::Region::new(ALLOCATOR);
        let batch = publication_batch_for_measurement(
            Uuid::nil(),
            first.clone(),
            followup.then(|| second.clone()),
        );
        let stats = region.change();
        println!("FRAME_BATCH followup={followup} stats={stats:?}");
        assert_eq!(batch.first_phase, 0);
        assert_eq!(batch.player_id, Uuid::nil());
        assert_eq!(batch.messages.len(), if followup { 2 } else { 1 });
        assert!(Arc::ptr_eq(&batch.messages[0], &first));
        if followup {
            assert!(Arc::ptr_eq(&batch.messages[1], &second));
        }
        assert_eq!(stats.allocations, 1);
        assert_eq!(stats.reallocations, 0, "frame batch grew: {stats:?}");
    }
}
