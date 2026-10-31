use super::*;

/// Accepted transfers keep storage, recipient flags and replay in agreement.
#[tokio::test]
async fn authority_transfer_commits_storage_before_live_and_replay_events() {
    let database = Arc::new(InMemoryDatabase::new());
    let messages = Arc::new(crate::server::InMemoryMessageCoordinator::new());
    let replay = Arc::new(crate::reconnection::ReconnectionManager::new(
        30,
        16,
        Arc::new(crate::metrics::ServerMetrics::new()),
    ));
    let authority = PlayerId::from_u128(1);
    let successor = PlayerId::from_u128(2);
    let room = database
        .create_room(
            "authority-transfer".into(),
            None,
            4,
            true,
            authority,
            "relay".into(),
            "test".into(),
            None,
        )
        .await
        .unwrap();
    database
        .add_player_to_room(
            &room.id,
            player_fixture(successor, "successor", false, None),
        )
        .await
        .unwrap();
    let (authority_sender, mut authority_receiver) = mpsc::channel(8);
    let (successor_sender, mut successor_receiver) = mpsc::channel(8);
    for (player, sender) in [(authority, authority_sender), (successor, successor_sender)] {
        messages
            .register_local_client(
                player,
                Some(room.id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .unwrap();
    }
    replay
        .register_disconnection(PlayerId::from_u128(3), room.id, false, None, 0)
        .await;
    let coordinator =
        InMemoryRoomOperationCoordinator::new(messages, database.clone(), Some(replay.clone()));

    for target in [successor, authority] {
        assert!(coordinator
            .coordinate_authority_transfer(&room.id, &target)
            .await
            .unwrap());
        let stored = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert_eq!(stored.authority_player, Some(target));
        for player in stored.players.values() {
            assert_eq!(player.is_authority, player.id == target);
        }
        for (recipient, receiver) in [
            (authority, &mut authority_receiver),
            (successor, &mut successor_receiver),
        ] {
            let received = receiver.try_recv().unwrap();
            assert!(matches!(
                received.as_ref(),
                ServerMessage::AuthorityChanged { authority_player: Some(id), you_are_authority }
                    if *id == target && *you_are_authority == (recipient == target)
            ));
            let unexpected = receiver.try_recv();
            assert!(matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)));
        }
    }
    let replayed = replay.get_missed_events(&room.id, 0).await;
    let authorities: Vec<_> = replayed
        .events
        .iter()
        .filter_map(|event| match event {
            ServerMessage::AuthorityChanged {
                authority_player, ..
            } => Some(*authority_player),
            _ => None,
        })
        .collect();
    assert_eq!(authorities, vec![Some(successor), Some(authority)]);
}

/// Refused transfers preserve room state without publishing a role change.
#[tokio::test]
async fn refused_authority_transfer_preserves_storage_and_emits_nothing() {
    for supports_authority in [false, true] {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(RecordingMessageCoordinator::default());
        let member = PlayerId::from_u128(1);
        let room = database
            .create_room(
                "refused-transfer".into(),
                None,
                4,
                supports_authority,
                member,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        let departed = PlayerId::from_u128(3);
        database
            .add_player_to_room(&room.id, player_fixture(departed, "departed", false, None))
            .await
            .unwrap();
        database
            .remove_player_from_room(&room.id, &departed)
            .await
            .unwrap();
        let spectator = PlayerId::from_u128(4);
        database
            .add_spectator_to_room(
                &room.id,
                crate::protocol::SpectatorInfo {
                    id: spectator,
                    name: "observer".into(),
                    connected_at: None,
                },
            )
            .await
            .unwrap();
        let before = format!("{:?}", database.get_room_by_id(&room.id).await.unwrap());
        let coordinator =
            InMemoryRoomOperationCoordinator::new(messages.clone(), database.clone(), None);
        for (room_id, target) in [
            (room.id, PlayerId::from_u128(2)),
            (room.id, departed),
            (room.id, spectator),
            (RoomId::from_u128(999), member),
        ] {
            assert!(!coordinator
                .coordinate_authority_transfer(&room_id, &target)
                .await
                .unwrap());
        }
        if !supports_authority {
            assert!(!coordinator
                .coordinate_authority_transfer(&room.id, &member)
                .await
                .unwrap());
        }
        assert_eq!(
            format!("{:?}", database.get_room_by_id(&room.id).await.unwrap()),
            before
        );
        assert!(messages.broadcasts().await.is_empty());
    }
}

/// Caller cancellation preserves the committed role and completes live and replay events.
#[tokio::test]
async fn canceling_committed_authority_transfer_preserves_storage_and_replay() {
    let database = Arc::new(InMemoryDatabase::new());
    let messages = Arc::new(BlockingBroadcastMessageCoordinator::new());
    let replay = Arc::new(crate::reconnection::ReconnectionManager::new(
        30,
        16,
        Arc::new(crate::metrics::ServerMetrics::new()),
    ));
    let previous = PlayerId::from_u128(1);
    let target = PlayerId::from_u128(2);
    let room = database
        .create_room(
            "canceled-transfer".into(),
            None,
            4,
            true,
            previous,
            "relay".into(),
            "test".into(),
            None,
        )
        .await
        .unwrap();
    database
        .add_player_to_room(&room.id, player_fixture(target, "target", false, None))
        .await
        .unwrap();
    replay
        .register_disconnection(PlayerId::from_u128(3), room.id, false, None, 0)
        .await;
    let coordinator = InMemoryRoomOperationCoordinator::new(
        messages.clone(),
        database.clone(),
        Some(replay.clone()),
    );
    let transfer = tokio::spawn(async move {
        coordinator
            .coordinate_authority_transfer(&room.id, &target)
            .await
    });
    timeout(Duration::from_secs(1), messages.wait_for_broadcast_start())
        .await
        .unwrap();
    assert_eq!(
        database
            .get_room_by_id(&room.id)
            .await
            .unwrap()
            .unwrap()
            .authority_player,
        Some(target)
    );
    transfer.abort();
    transfer.await.expect_err("caller is canceled");
    messages.release_broadcast();
    let guard = timeout(
        Duration::from_secs(1),
        messages.lock_room_event_mutation(&room.id),
    )
    .await
    .unwrap();
    drop(guard);
    let broadcasts = messages.broadcasts().await;
    assert!(matches!(
        broadcasts.as_slice(),
        [BroadcastEvent {
            room_id,
            message: ServerMessage::AuthorityChanged {
                authority_player: Some(id),
                you_are_authority: false,
            },
        }] if *room_id == room.id && *id == target
    ));
    let replayed = replay.get_missed_events(&room.id, 0).await;
    assert!(
        matches!(replayed.events.as_slice(), [ServerMessage::AuthorityChanged { authority_player: Some(id), .. }] if *id == target)
    );
    assert_eq!(
        database
            .get_room_by_id(&room.id)
            .await
            .unwrap()
            .unwrap()
            .authority_player,
        Some(target)
    );
}

/// A delivery failure preserves the committed role and its replay record.
#[tokio::test]
async fn failed_authority_transfer_broadcast_preserves_committed_storage_and_replay() {
    let database = Arc::new(InMemoryDatabase::new());
    let messages = Arc::new(RecordingMessageCoordinator::default());
    let replay = Arc::new(crate::reconnection::ReconnectionManager::new(
        30,
        16,
        Arc::new(crate::metrics::ServerMetrics::new()),
    ));
    let previous = PlayerId::from_u128(1);
    let target = PlayerId::from_u128(2);
    let room = database
        .create_room(
            "failed-transfer".into(),
            None,
            4,
            true,
            previous,
            "relay".into(),
            "test".into(),
            None,
        )
        .await
        .unwrap();
    database
        .add_player_to_room(&room.id, player_fixture(target, "target", false, None))
        .await
        .unwrap();
    replay
        .register_disconnection(PlayerId::from_u128(3), room.id, false, None, 0)
        .await;
    messages.fail_broadcast_on(1).await;
    let coordinator = InMemoryRoomOperationCoordinator::new(
        messages.clone(),
        database.clone(),
        Some(replay.clone()),
    );
    assert!(coordinator
        .coordinate_authority_transfer(&room.id, &target)
        .await
        .is_err());
    assert_eq!(
        database
            .get_room_by_id(&room.id)
            .await
            .unwrap()
            .unwrap()
            .authority_player,
        Some(target)
    );
    assert!(messages.broadcasts().await.is_empty());
    let replayed = replay.get_missed_events(&room.id, 0).await;
    assert!(
        matches!(replayed.events.as_slice(), [ServerMessage::AuthorityChanged { authority_player: Some(id), .. }] if *id == target)
    );
    assert!(coordinator
        .coordinate_authority_transfer(&room.id, &previous)
        .await
        .unwrap());
    assert_eq!(messages.broadcasts().await.len(), 1);
}

#[tokio::test]
async fn failed_authority_response_still_publishes_committed_claim_and_release() {
    for failed_broadcast in [None, Some(3)] {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(RecordingMessageCoordinator::default());
        let replay = Arc::new(crate::reconnection::ReconnectionManager::new(
            30,
            16,
            Arc::new(crate::metrics::ServerMetrics::new()),
        ));
        let member = PlayerId::from_u128(1);
        let peer = PlayerId::from_u128(2);
        let room = database
            .create_room(
                "authority-response-failure".into(),
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
        database
            .add_player_to_room(&room.id, player_fixture(peer, "Peer", false, None))
            .await
            .unwrap();
        replay
            .register_disconnection(PlayerId::from_u128(3), room.id, false, None, 0)
            .await;
        let coordinator = InMemoryRoomOperationCoordinator::new(
            messages.clone(),
            database.clone(),
            Some(replay.clone()),
        );
        messages.failed_direct_messages.store(1, Ordering::Release);
        if let Some(call) = failed_broadcast {
            messages.fail_broadcast_on(call).await;
        }
        for (requester, become_authority, expected_authority, granted) in [
            (member, false, None, true),
            (peer, true, Some(peer), true),
            (member, true, Some(peer), false),
            (peer, false, None, true),
        ] {
            let before = messages.broadcasts().await.len();
            let outcome = coordinator
                .handle_authority_request(&room.id, &requester, become_authority)
                .await
                .unwrap();
            assert_eq!(outcome.granted(), granted);
            let stored = database.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert_eq!(stored.authority_player, expected_authority);
            assert!(stored
                .players
                .values()
                .all(|player| player.is_authority == (Some(player.id) == expected_authority)));
            let broadcasts = messages.broadcasts().await;
            assert_eq!(
                broadcasts.len(),
                before
                    + usize::from(
                        granted
                            && Some(messages.broadcast_attempts.load(Ordering::Acquire))
                                != failed_broadcast
                    ),
                "committed authority changes must survive a failed direct response"
            );
            if granted
                && Some(messages.broadcast_attempts.load(Ordering::Acquire)) != failed_broadcast
            {
                assert!(matches!(&broadcasts.last().unwrap().message,
                ServerMessage::AuthorityChanged { authority_player, .. } if *authority_player == expected_authority));
            }
        }
        let replayed = replay.get_missed_events(&room.id, 0).await;
        let roles: Vec<_> = replayed
            .events
            .iter()
            .filter_map(|event| match event {
                ServerMessage::AuthorityChanged {
                    authority_player, ..
                } => Some(*authority_player),
                _ => None,
            })
            .collect();
        assert_eq!(roles, vec![None, Some(peer), None]);
    }
}
