use super::*;

/// #1421: acquired permits remain accounted while sibling reservations wait.
#[tokio::test(start_paused = true)]
async fn canceled_broadcast_counts_reservations_before_all_peers_are_ready() {
    for count in [2_u128, 32, 64] {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(60),
            metrics.clone(),
        );
        let room = RoomId::from_u128(700);
        let filler = Arc::new(ServerMessage::Pong);
        let mut channels = Vec::new();
        for index in 1..=count {
            let id = PlayerId::from_u128(index);
            let (sender, receiver) = mpsc::channel(1);
            if index == count {
                sender.try_send(filler.clone()).unwrap();
            }
            coordinator
                .register_local_client(
                    id,
                    Some(room),
                    ClientDeliveryHandle::new(sender.clone(), ConnectionCloseSignal::detached()),
                )
                .await
                .unwrap();
            channels.push((sender, receiver));
        }
        let (_drain_tx, drain) = watch::channel(false);
        let called = AtomicBool::new(false);
        let excluded = PlayerId::nil();
        let mut broadcast = coordinator.broadcast_to_room_except_if_with_hook(
            &room,
            &excluded,
            filler.clone(),
            &|| true,
            drain,
            Box::new(|| {
                Box::pin(async {
                    called.store(true, Ordering::Relaxed);
                })
            }),
        );
        assert!(futures_util::poll!(tokio::task::unconstrained(broadcast.as_mut())).is_pending());
        assert!(channels.iter().all(|(sender, _)| sender.capacity() == 0));
        drop(broadcast);
        assert!(!called.load(Ordering::Relaxed));
        assert_eq!(
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
            u64::try_from(count).unwrap()
        );
        assert_eq!(
            metrics
                .websocket_deliveries_canceled
                .load(Ordering::Relaxed),
            u64::try_from(count - 1).unwrap(),
            "count={count}: all completed reservations need a terminal outcome"
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            1
        );
        for (index, (sender, mut receiver)) in channels.into_iter().enumerate() {
            if index + 1 == usize::try_from(count).unwrap() {
                let received = receiver.try_recv().unwrap();
                assert!(Arc::ptr_eq(&received, &filler));
            }
            let trailing = receiver.try_recv();
            assert!(matches!(trailing, Err(mpsc::error::TryRecvError::Empty)));
            assert_eq!(sender.capacity(), 1);
            sender.try_send(filler.clone()).unwrap();
            let received = receiver.try_recv().unwrap();
            assert!(Arc::ptr_eq(&received, &filler));
        }
    }
}

/// #1422: P2P status delivery retains accounting through route revalidation.
#[tokio::test(start_paused = true)]
async fn canceled_targeted_publication_counts_permit_while_revalidating_route() {
    canceled_targeted_publication(false).await;
}

#[tokio::test(start_paused = true)]
async fn canceled_exact_membership_publication_counts_permit_while_revalidating_route() {
    canceled_targeted_publication(true).await;
}

async fn canceled_targeted_publication(exact_members: bool) {
    for kind in [ControlQueueKind::Legacy, ControlQueueKind::Classified] {
        for blocked_lock in ["routing", "room_players", "clients"] {
            let metrics = Arc::new(ServerMetrics::new());
            let coordinator = InMemoryMessageCoordinator::with_delivery_policy(
                Duration::from_secs(60),
                metrics.clone(),
            );
            let room = RoomId::from_u128(700);
            let player = PlayerId::from_u128(701);
            let (mut handle, _close_listener, mut receiver) = full_control_queue(kind);
            if matches!(kind, ControlQueueKind::Classified) {
                receiver.pop_message("roomless prefix");
                handle.sender = handle.sender.next_generation();
                let transition = Arc::new(ServerMessage::SpectatorJoined(Box::new(
                    SpectatorJoinedPayload {
                        room_id: room,
                        room_code: "CANCEL".into(),
                        spectator_id: player,
                        game_name: "conditional-publication".into(),
                        current_players: Vec::new(),
                        current_spectators: Vec::new(),
                        lobby_state: LobbyState::Lobby,
                        reason: None,
                    },
                )));
                assert!(
                    handle
                        .sender
                        .try_send(transition, Some(room))
                        .unwrap()
                        .enqueued
                );
                receiver.pop_message("room entry");
                handle
                    .sender
                    .try_send(Arc::new(ServerMessage::Pong), Some(room))
                    .unwrap();
            }
            let sender = handle.sender.clone();
            coordinator
                .register_local_client(player, Some(room), handle)
                .await
                .unwrap();
            let message = Arc::new(ServerMessage::Pong);
            let members = [player];
            let should_send = || true;
            let mut delivery = if exact_members {
                coordinator.send_to_player_in_room_if_members(
                    &player,
                    &room,
                    &members,
                    message.clone(),
                )
            } else {
                coordinator.send_to_player_in_room_if(&player, &room, message.clone(), &should_send)
            };
            assert!(
                futures_util::poll!(tokio::task::unconstrained(delivery.as_mut())).is_pending()
            );
            let routing = if blocked_lock == "routing" {
                Some(coordinator.room_routing_gates.write(room).await)
            } else {
                None
            };
            let players = if blocked_lock == "room_players" {
                Some(coordinator.room_players.write().await)
            } else {
                None
            };
            let clients = if blocked_lock == "clients" {
                Some(coordinator.local_clients.write().await)
            } else {
                None
            };
            receiver.pop_message("retained prefix");
            assert!(
                futures_util::poll!(tokio::task::unconstrained(delivery.as_mut())).is_pending()
            );
            assert!(
                sender.try_reserve_control(Some(room)).is_err(),
                "publication owns the only permit"
            );
            drop(delivery);
            assert_eq!(
                metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
                1
            );
            assert_eq!(
                metrics
                    .websocket_deliveries_canceled
                    .load(Ordering::Relaxed),
                1,
                "exact_members={exact_members} kind={kind:?} lock={blocked_lock}"
            );
            assert_eq!(
                metrics.websocket_messages_dropped.load(Ordering::Relaxed),
                0
            );
            drop((routing, players, clients));
            receiver.assert_empty("canceled frame");
            sender.try_send(message.clone(), Some(room)).unwrap();
            assert!(Arc::ptr_eq(
                &receiver.pop_message("reused capacity"),
                &message
            ));
            receiver.assert_empty("reused queue");
        }
    }
}

/// #1423: conditional publications keep lookup work below a full roster scan.
#[test]
fn conditional_publication_validation_bounds_room_lookup_work() {
    for count in [0_u128, 1, 2, 16, 255] {
        let mut reservations = Vec::new();
        let mut recipients = Vec::new();
        let mut receivers = Vec::new();
        for index in 1..=count {
            let id = PlayerId::from_u128(index);
            let (sender, receiver) = mpsc::channel(1);
            receivers.push(receiver);
            let handle = ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached());
            reservations.push(ConditionalDeliveryReservation::ChannelClosed {
                player_id: id,
                sender: handle.sender.clone(),
            });
            recipients.push((id, handle));
        }
        reservations.reverse();
        let mut sort_reads = 0;
        reservations.sort_unstable_by_key(|reservation| {
            sort_reads += 1;
            reservation.routed_player()
        });
        let mut comparisons = 0;
        for (id, handle) in &recipients {
            assert!(
                InMemoryMessageCoordinator::conditional_reservation_covers_recipient(
                    &reservations,
                    *id,
                    handle,
                    |reservation| {
                        comparisons += 1;
                        reservation.routed_player()
                    }
                )
            );
        }
        let mut original_comparisons = 0;
        assert!(recipients
            .iter()
            .all(|(id, handle)| reservations.iter().any(|reservation| {
                original_comparisons += 1;
                match reservation {
                    ConditionalDeliveryReservation::Reserved {
                        player_id, sender, ..
                    }
                    | ConditionalDeliveryReservation::ChannelClosed { player_id, sender } => {
                        id == player_id && sender.same_channel(&handle.sender)
                    }
                    _ => false,
                }
            })));
        println!("CONDITIONAL_LOOKUP count={count} original={original_comparisons} comparisons={comparisons} sort_reads={sort_reads}");
        let bound =
            usize::try_from(count).unwrap() * (usize::try_from(count.max(1).ilog2()).unwrap() + 2);
        if count >= 16 {
            assert!(
                original_comparisons > bound,
                "count={count}: linear scan {original_comparisons} no longer exceeds {bound}"
            );
        }
        assert!(
            sort_reads <= 4 * bound,
            "sort_reads={sort_reads} bound={bound}"
        );
        assert!(
            comparisons <= bound,
            "count={count}: {comparisons} comparisons exceeds {bound}"
        );
        let mut production_reads = 0;
        assert!(InMemoryMessageCoordinator::reservations_cover_recipients(
            &reservations,
            recipients.iter().map(|(id, handle)| (*id, handle)),
            |reservation| {
                production_reads += 1;
                reservation.routed_player()
            },
        ));
        assert!(
            production_reads >= recipients.len(),
            "count={count}: production validation bypassed the counted lookup"
        );
        println!("CONDITIONAL_PRODUCTION_LOOKUP count={count} reads={production_reads}");
        assert!(
            production_reads <= bound,
            "count={count}: {production_reads} production reads exceeds {bound}"
        );
        if let Some((id, handle)) = recipients.first_mut() {
            let original = handle.clone();
            let (replacement, _replacement_receiver) = mpsc::channel(1);
            *handle = ClientDeliveryHandle::new(replacement, ConnectionCloseSignal::detached());
            assert!(
                !InMemoryMessageCoordinator::conditional_reservation_covers_recipient(
                    &reservations,
                    *id,
                    handle,
                    ConditionalDeliveryReservation::routed_player
                )
            );
            *handle = original;
            *id = PlayerId::from_u128(count + 1);
            assert!(
                !InMemoryMessageCoordinator::conditional_reservation_covers_recipient(
                    &reservations,
                    *id,
                    handle,
                    ConditionalDeliveryReservation::routed_player
                )
            );
        }
    }
}

#[tokio::test]
async fn conditional_publication_commits_exact_frames_after_reservation_validation() {
    for count in [2_u128, 16, 255] {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(60),
            metrics.clone(),
        );
        let room = RoomId::from_u128(700);
        let mut receivers = Vec::new();
        for index in (1..=count).rev() {
            let (sender, receiver) = mpsc::channel(1);
            coordinator
                .register_local_client(
                    PlayerId::from_u128(index),
                    Some(room),
                    ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                )
                .await
                .unwrap();
            receivers.push(receiver);
        }
        let message = Arc::new(ServerMessage::Pong);
        let excluded = PlayerId::nil();
        let (_drain_tx, drain) = watch::channel(false);
        let hooks = AtomicUsize::new(0);
        assert!(coordinator
            .broadcast_to_room_except_if_with_hook(
                &room,
                &excluded,
                message.clone(),
                &|| true,
                drain,
                Box::new(|| Box::pin(async {
                    assert_eq!(
                        metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
                        u64::try_from(count).unwrap()
                    );
                    assert_eq!(
                        metrics
                            .websocket_deliveries_enqueued
                            .load(Ordering::Relaxed),
                        0
                    );
                    hooks.fetch_add(1, Ordering::Relaxed);
                })),
            )
            .await
            .unwrap());
        assert_eq!(hooks.load(Ordering::Relaxed), 1);
        assert_eq!(
            metrics
                .websocket_deliveries_enqueued
                .load(Ordering::Relaxed),
            u64::try_from(count).unwrap()
        );
        assert_eq!(
            metrics
                .websocket_deliveries_canceled
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            0
        );
        for receiver in &mut receivers {
            let received = receiver.try_recv().unwrap();
            assert!(Arc::ptr_eq(&received, &message));
            let trailing = receiver.try_recv();
            assert!(matches!(trailing, Err(mpsc::error::TryRecvError::Empty)));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn targeted_room_delivery_resolves_acquired_permit_once() {
    for exact_members in [false, true] {
        for outcome in ["delivered", "refused", "closed"] {
            let metrics = Arc::new(ServerMetrics::new());
            let coordinator = InMemoryMessageCoordinator::with_delivery_policy(
                Duration::from_secs(60),
                metrics.clone(),
            );
            let room = RoomId::from_u128(700);
            let player = PlayerId::from_u128(701);
            let (sender, mut receiver) = mpsc::channel(1);
            let message = Arc::new(ServerMessage::Pong);
            sender.try_send(message.clone()).unwrap();
            coordinator
                .register_local_client(
                    player,
                    Some(room),
                    ClientDeliveryHandle::new(sender.clone(), ConnectionCloseSignal::detached()),
                )
                .await
                .unwrap();
            let allowed = AtomicBool::new(true);
            let should_send = || allowed.load(Ordering::Relaxed);
            let members = [player];
            let mut delivery = if exact_members {
                coordinator.send_to_player_in_room_if_members(
                    &player,
                    &room,
                    &members,
                    message.clone(),
                )
            } else {
                coordinator.send_to_player_in_room_if(&player, &room, message.clone(), &should_send)
            };
            assert!(
                futures_util::poll!(tokio::task::unconstrained(delivery.as_mut())).is_pending()
            );
            let routing = coordinator.room_routing_gates.write(room).await;
            let received = receiver.try_recv().unwrap();
            assert!(Arc::ptr_eq(&received, &message));
            assert!(
                futures_util::poll!(tokio::task::unconstrained(delivery.as_mut())).is_pending()
            );
            assert_eq!(sender.capacity(), 0);
            if outcome == "refused" {
                if exact_members {
                    coordinator
                        .room_players
                        .write()
                        .await
                        .get_mut(&room)
                        .unwrap()
                        .remove(&player);
                } else {
                    allowed.store(false, Ordering::Relaxed);
                }
            }
            if outcome == "closed" {
                receiver.close();
            }
            drop(routing);
            assert_eq!(delivery.await.unwrap(), outcome == "delivered");
            assert_eq!(
                metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
                1
            );
            assert_eq!(
                metrics
                    .websocket_deliveries_enqueued
                    .load(Ordering::Relaxed),
                u64::from(outcome == "delivered")
            );
            assert_eq!(
                metrics
                    .websocket_deliveries_canceled
                    .load(Ordering::Relaxed),
                u64::from(outcome == "refused")
            );
            assert_eq!(
                metrics
                    .websocket_deliveries_channel_closed
                    .load(Ordering::Relaxed),
                u64::from(outcome == "closed")
            );
            assert_eq!(
                metrics.websocket_messages_dropped.load(Ordering::Relaxed),
                0
            );
            if outcome == "delivered" {
                let received = receiver.try_recv().unwrap();
                assert!(Arc::ptr_eq(&received, &message));
            }
            let trailing = receiver.try_recv();
            assert!(matches!(
                trailing,
                Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected)
            ));
            if outcome != "closed" {
                assert_eq!(sender.capacity(), 1);
                sender.try_send(message.clone()).unwrap();
                let received = receiver.try_recv().unwrap();
                assert!(Arc::ptr_eq(&received, &message));
            }
        }
    }
}
