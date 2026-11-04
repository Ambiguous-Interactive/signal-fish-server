//! Complete exact-membership publications with caller-owned frame batches.
use super::*;
use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, MessageCoordinator, RoomMessageTransactionOutcome,
    RoomRecipientMessages,
};
use signal_fish_server::server::InMemoryMessageCoordinator;
use std::sync::Arc;

/// #1416/#1417: measure permits and routing validation inside complete publications.
pub(crate) fn transaction_reuses_owned_batches() {
    watch_channel_lifecycle_cost();
    let mut profiles = Vec::new();
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
                    if count == 255 {
                        let trace_storage = if cfg!(feature = "trace-validation") {
                            13_000
                        } else {
                            0
                        };
                        assert!(
                            stats.bytes_allocated <= 540_000 + trace_storage,
                            "publication rebuilt owned batch storage: {stats:?}"
                        );
                    }
                    println!("PUBLICATION_COST members={count} stats={stats:?}");
                    profiles.push((count, stats));
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
    assert!(
        profiles.iter().all(|(count, stats)| stats.reallocations
            <= match count {
                0..=4 => 0,
                5..=32 => 2,
                _ => 12,
            }),
        "publication validation grew temporary rosters: {profiles:?}"
    );
    assert!(
        profiles
            .iter()
            .all(|(count, stats)| *count != 255 || stats.allocations <= 269),
        "publication allocated per-recipient permit storage: {profiles:?}"
    );
}

fn watch_channel_lifecycle_cost() {
    let region = stats_alloc::Region::new(ALLOCATOR);
    let (sender, receiver) = tokio::sync::watch::channel(false);
    assert!(!*receiver.borrow());
    drop(receiver);
    drop(sender);
    let stats = region.change();
    println!("PUBLICATION_WATCH_LIFECYCLE stats={stats:?}");
    // A new watch channel owns one allocation on Linux/Windows. macOS also
    // initializes nine native notification mutexes when its halves are dropped.
    #[cfg(target_os = "macos")]
    assert_eq!(stats.allocations, 10, "watch lifecycle: {stats:?}");
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    assert_eq!(stats.allocations, 1, "watch lifecycle: {stats:?}");
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
            stats.allocations <= 600,
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
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    assert_eq!(
        cold.allocations, 0,
        "permit initialization allocated: {cold:?}"
    );
    (cold.allocations, cold.bytes_allocated)
}

/// Measure complete uniform room events with both membership validation modes.
pub(crate) fn uniform_publication_borrows_commit_routes() {
    for count in [2_u128, 16, 255] {
        for exact in [false, true] {
            allocation_harness::block_on(async {
                allocation_harness::warm_current_thread_runtime().await;
                let coordinator = InMemoryMessageCoordinator::new();
                let room = Uuid::from_u128(1000);
                let members: Vec<_> = (1..=count).map(Uuid::from_u128).collect();
                let message = Arc::new(ServerMessage::LobbyStateChanged {
                    lobby_state: signal_fish_server::protocol::LobbyState::Lobby,
                    ready_players: members.clone(),
                    all_ready: true,
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
                    let hooks = std::sync::atomic::AtomicUsize::new(0);
                    let hook = Box::new(|| {
                        Box::pin(async {
                            hooks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        })
                            as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                    });
                    let region = stats_alloc::Region::new(ALLOCATOR);
                    let delivered = if exact {
                        coordinator
                            .broadcast_to_room_if_members_with_hook(
                                &room,
                                &members,
                                message.clone(),
                                hook,
                            )
                            .await
                            .unwrap()
                    } else {
                        coordinator
                            .broadcast_to_room_with_hook(&room, message.clone(), hook)
                            .await
                            .unwrap()
                    };
                    let stats = region.change();
                    assert!(delivered);
                    assert_eq!(hooks.load(std::sync::atomic::Ordering::Relaxed), 1);
                    if measured {
                        println!(
                            "UNIFORM_PUBLICATION members={count} exact={exact} stats={stats:?}"
                        );
                        if count == 255 {
                            let trace_storage = if cfg!(feature = "trace-validation") {
                                13_000
                            } else {
                                0
                            };
                            assert!(stats.bytes_allocated <= 425_000 + usize::from(exact) * 4_096 + trace_storage,
                                "uniform publication copied commit routes: exact={exact} stats={stats:?}");
                        }
                    }
                    for receiver in &mut receivers {
                        assert!(Arc::ptr_eq(&receiver.try_recv().unwrap(), &message));
                        assert_membership_queue_empty(receiver);
                    }
                }
            });
        }
    }
}
