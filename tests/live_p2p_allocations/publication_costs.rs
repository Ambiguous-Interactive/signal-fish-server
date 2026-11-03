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
                    let trailing = receiver.try_recv();
                    assert!(matches!(
                        trailing,
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
                    ));
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

pub(crate) fn observer_retry_reuses_owned_seated_batches() {
    use signal_fish_server::metrics::ServerMetrics;
    use signal_fish_server::protocol::{LobbyState, SpectatorJoinedPayload};
    use std::sync::atomic::{AtomicUsize, Ordering};
    allocation_harness::block_on(async {
        allocation_harness::warm_current_thread_runtime().await;
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = InMemoryMessageCoordinator::with_delivery_policy(
            std::time::Duration::from_secs(60),
            metrics.clone(),
        );
        let room = Uuid::from_u128(1000);
        let members: Vec<_> = (1..=255).map(Uuid::from_u128).collect();
        let first = Arc::new(ServerMessage::Pong);
        let second = Arc::new(ServerMessage::Error {
            message: "second phase".into(),
            error_code: None,
        });
        let filler = Arc::new(ServerMessage::Error {
            message: "queue barrier".into(),
            error_code: None,
        });
        let mut receivers = Vec::new();
        let mut lock_allocations = 0;
        let mut lock_bytes = 0;
        for (index, &id) in members.iter().enumerate() {
            let (sender, receiver) = tokio::sync::mpsc::channel(if index == 0 { 2 } else { 4 });
            if index == 0 {
                sender.try_send(filler.clone()).unwrap();
            }
            let initialized = warm_control_permit(&sender);
            lock_allocations += initialized.0;
            lock_bytes += initialized.1;
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
        let observer = Uuid::from_u128(999);
        let (observer_sender, mut observer_receiver) = tokio::sync::mpsc::channel(4);
        let initialized = warm_control_permit(&observer_sender);
        lock_allocations += initialized.0;
        lock_bytes += initialized.1;
        println!("PERMIT_LOCK_INIT channels=256 allocations={lock_allocations} bytes={lock_bytes}");
        let observer_handle =
            ClientDeliveryHandle::new(observer_sender, ConnectionCloseSignal::detached());
        let baseline = Arc::new(ServerMessage::SpectatorJoined(Box::new(
            SpectatorJoinedPayload {
                room_id: room,
                room_code: "RETRY1".into(),
                spectator_id: observer,
                game_name: "publication-retry".into(),
                current_players: vec![],
                current_spectators: vec![],
                lobby_state: LobbyState::Lobby,
                reason: None,
            },
        )));
        let batches = members
            .iter()
            .map(|&id| RoomRecipientMessages::in_order(id, vec![first.clone(), second.clone()]))
            .collect();
        let hook_calls = AtomicUsize::new(0);
        let region = stats_alloc::Region::new(ALLOCATOR);
        let mut transaction = coordinator.commit_room_messages_if_members_with_hook(
            &room,
            &members,
            batches,
            vec![first.clone()],
            Box::new(|| {
                Box::pin(async {
                    assert_eq!(
                        metrics
                            .websocket_deliveries_canceled
                            .load(Ordering::Relaxed),
                        510,
                        "the observer mismatch must release the complete first reservation set"
                    );
                    hook_calls.fetch_add(1, Ordering::Relaxed);
                    Ok(true)
                })
            }),
            Box::new(|failed| {
                assert_eq!(failed, 0);
                true
            }),
        );
        assert!(futures_util::poll!(tokio::task::unconstrained(transaction.as_mut())).is_pending());
        assert_eq!(
            coordinator
                .register_local_spectator_with_initial_message_async(
                    observer,
                    room,
                    observer_handle,
                    &|| true,
                    None,
                    None,
                    Box::new(move |_| Box::pin(async move { Ok(baseline) })),
                )
                .await
                .unwrap(),
            signal_fish_server::coordination::DeliveryOutcome::Delivered
        );
        let queued = receivers[0].try_recv().unwrap();
        assert!(Arc::ptr_eq(&queued, &filler));
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), transaction)
            .await
            .unwrap()
            .unwrap();
        let stats = region.change();
        println!("PUBLICATION_RETRY members=255 stats={stats:?}");
        assert!(
            stats.allocations <= 1120,
            "retry copied seated batches: {stats:?}"
        );
        assert_eq!(outcome, RoomMessageTransactionOutcome::Committed);
        assert_eq!(hook_calls.load(Ordering::Relaxed), 1);
        for receiver in &mut receivers {
            assert!(Arc::ptr_eq(&receiver.try_recv().unwrap(), &first));
            assert!(Arc::ptr_eq(&receiver.try_recv().unwrap(), &second));
            let trailing = receiver.try_recv();
            assert!(matches!(
                trailing,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
        }
        let received_baseline = observer_receiver.try_recv().unwrap();
        assert!(matches!(
            received_baseline.as_ref(),
            ServerMessage::SpectatorJoined(_)
        ));
        assert!(Arc::ptr_eq(&observer_receiver.try_recv().unwrap(), &first));
        let trailing = observer_receiver.try_recv();
        assert!(matches!(
            trailing,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    });
}

fn warm_control_permit(sender: &tokio::sync::mpsc::Sender<Arc<ServerMessage>>) -> (usize, usize) {
    let cold_region = stats_alloc::Region::new(ALLOCATOR);
    drop(sender.clone().try_reserve_owned().unwrap());
    let cold = cold_region.change();
    let warm_region = stats_alloc::Region::new(ALLOCATOR);
    drop(sender.clone().try_reserve_owned().unwrap());
    let warm = warm_region.change();
    assert_eq!(
        warm.allocations, 0,
        "repeated permit release allocated: {warm:?}"
    );
    #[cfg(target_os = "macos")]
    {
        assert_eq!(cold.allocations, 1, "native semaphore lock: {cold:?}");
        assert_eq!(cold.bytes_allocated, 64, "native semaphore lock: {cold:?}");
    }
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        cold.allocations, 0,
        "permit initialization allocated: {cold:?}"
    );
    (cold.allocations, cold.bytes_allocated)
}
