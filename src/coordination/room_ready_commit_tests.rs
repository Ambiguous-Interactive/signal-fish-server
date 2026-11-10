use super::*;

/// #1332: durable rows alone cannot promote a room with no effective seats.
#[tokio::test]
async fn lobby_promotion_requires_an_effective_routed_member() {
    for route in ["absent", "disjoint", "spectator"] {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(crate::server::InMemoryMessageCoordinator::new());
        let member = PlayerId::from_u128(1);
        let other = PlayerId::from_u128(2);
        let room = database
            .create_room(
                "routed-lobby".into(),
                None,
                4,
                false,
                member,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        let (other_sender, mut other_receiver) = mpsc::channel(4);
        if route == "disjoint" {
            messages
                .register_local_client(
                    other,
                    Some(room.id),
                    ClientDeliveryHandle::new(other_sender, ConnectionCloseSignal::detached()),
                )
                .await
                .unwrap();
        } else if route == "spectator" {
            database
                .add_spectator_to_room(
                    &room.id,
                    crate::protocol::SpectatorInfo {
                        id: other,
                        name: "Observer".into(),
                        connected_at: None,
                    },
                )
                .await
                .unwrap();
            messages
                .register_local_spectator_with_initial_message_async(
                    other,
                    room.id,
                    ClientDeliveryHandle::new(other_sender, ConnectionCloseSignal::detached()),
                    &|| true,
                    None,
                    None,
                    Box::new(|_| {
                        Box::pin(async {
                            Ok(Arc::new(ServerMessage::LobbyStateChanged {
                                lobby_state: LobbyState::Waiting,
                                ready_players: vec![],
                                all_ready: false,
                            }))
                        })
                    }),
                )
                .await
                .unwrap();
            let baseline = other_receiver.try_recv().unwrap();
            assert!(
                matches!(baseline.as_ref(), ServerMessage::LobbyStateChanged {
                lobby_state: LobbyState::Waiting, ready_players, all_ready: false,
            } if ready_players.is_empty())
            );
        } else {
            drop(other_sender);
        }
        let coordinator =
            InMemoryRoomOperationCoordinator::new(messages.clone(), database.clone(), None);
        coordinator
            .ready_players
            .write()
            .await
            .insert(room.id, HashSet::from([other]));
        let before = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert!(
            !coordinator
                .transition_room_to_lobby(&room.id)
                .await
                .unwrap(),
            "route={route}"
        );
        let after = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert_eq!(after.lobby_state, LobbyState::Waiting);
        assert_eq!(after.last_activity, before.last_activity);
        assert!(after.lobby_started_at.is_none());
        assert_eq!(after.players.len(), 1);
        let other_pending = other_receiver.try_recv();
        if route == "absent" {
            assert!(matches!(
                other_pending,
                Err(mpsc::error::TryRecvError::Disconnected)
            ));
        } else {
            assert!(matches!(
                other_pending,
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
        let (sender, mut receiver) = mpsc::channel(4);
        messages
            .register_local_client(
                member,
                Some(room.id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .unwrap();
        assert!(coordinator
            .transition_room_to_lobby(&room.id)
            .await
            .unwrap());
        let event = receiver.try_recv().unwrap();
        assert!(matches!(event.as_ref(), ServerMessage::LobbyStateChanged {
            lobby_state: LobbyState::Lobby, ready_players, all_ready: false,
        } if ready_players.is_empty()));
        if route != "absent" {
            let observed = other_receiver.try_recv().unwrap();
            assert_eq!(
                serde_json::to_value(observed.as_ref()).unwrap(),
                serde_json::to_value(event.as_ref()).unwrap()
            );
        }
        assert!(!coordinator
            .transition_room_to_lobby(&room.id)
            .await
            .unwrap());
        let trailing = receiver.try_recv();
        assert!(matches!(trailing, Err(mpsc::error::TryRecvError::Empty)));
        let other_pending = other_receiver.try_recv();
        if route == "absent" {
            assert!(matches!(
                other_pending,
                Err(mpsc::error::TryRecvError::Disconnected)
            ));
        } else {
            assert!(matches!(
                other_pending,
                Err(mpsc::error::TryRecvError::Empty)
            ));
        }
    }
}

#[tokio::test]
async fn ready_toggle_promotes_waiting_storage_before_its_lobby_event() {
    for state in [LobbyState::Waiting, LobbyState::Lobby] {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(RecordingMessageCoordinator::default());
        let coordinator =
            InMemoryRoomOperationCoordinator::new(messages.clone(), database.clone(), None);
        let member = PlayerId::from_u128(1);
        let room = database
            .create_room(
                "ready-state".into(),
                None,
                4,
                false,
                member,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        if state == LobbyState::Lobby {
            database.transition_room_to_lobby(&room.id).await.unwrap();
        }
        for ready in [true, false] {
            coordinator
                .handle_player_ready(&room.id, &member, None)
                .await
                .unwrap();
            let broadcasts = messages.broadcasts().await;
            let ServerMessage::LobbyStateChanged {
                lobby_state,
                ready_players,
                all_ready,
            } = &broadcasts.last().unwrap().message
            else {
                panic!("ready toggle must publish its state");
            };
            assert_eq!(
                *lobby_state,
                LobbyState::Lobby,
                "ready events retain the documented Lobby state"
            );
            assert_eq!(*all_ready, ready);
            assert_eq!(
                ready_players.as_slice(),
                if ready {
                    std::slice::from_ref(&member)
                } else {
                    &[]
                }
            );
            assert_eq!(
                database.get_room_lobby_state(&room.id).await.unwrap(),
                Some(LobbyState::Lobby),
                "the announced Lobby state must be committed before delivery"
            );
        }
    }
}

#[tokio::test]
async fn canceling_early_ready_after_promotion_preserves_readiness_and_publication() {
    let database = Arc::new(InMemoryDatabase::new());
    let messages = Arc::new(RecordingMessageCoordinator::default());
    let coordinator = Arc::new(InMemoryRoomOperationCoordinator::new(
        messages.clone(),
        database.clone(),
        None,
    ));
    let member = PlayerId::from_u128(1);
    let room = database
        .create_room(
            "owned-ready".into(),
            None,
            4,
            false,
            member,
            "relay".into(),
            "test".into(),
            None,
        )
        .await
        .unwrap();
    let ready_read = coordinator.ready_players.read().await;
    let caller = {
        let coordinator = coordinator.clone();
        tokio::spawn(async move {
            coordinator
                .handle_player_ready(&room.id, &member, None)
                .await
        })
    };
    timeout(Duration::from_secs(1), async {
        loop {
            if database.get_room_lobby_state(&room.id).await.unwrap() == Some(LobbyState::Lobby) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("ready toggle must promote before waiting for the ready-map write lock");
    assert!(messages.broadcasts().await.is_empty());
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    drop(ready_read);
    timeout(Duration::from_secs(1), async {
        loop {
            if !messages.broadcasts().await.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned toggle must publish after its caller is canceled");
    assert_eq!(
        coordinator.current_ready_players(&room.id).await,
        vec![member]
    );
    let broadcasts = messages.broadcasts().await;
    assert_eq!(broadcasts.len(), 1);
    assert!(
        matches!(&broadcasts[0].message, ServerMessage::LobbyStateChanged { lobby_state: LobbyState::Lobby, ready_players, all_ready: true } if ready_players == &[member])
    );
    assert!(!coordinator
        .transition_room_to_lobby(&room.id)
        .await
        .unwrap());
}

#[tokio::test]
async fn lobby_entry_borrows_ready_members_and_sorts_the_exact_intersection() {
    let first = PlayerId::from_u128(1);
    let second = PlayerId::from_u128(2);
    let ghost = PlayerId::from_u128(3);
    for (name, ready, expected, all_ready) in [
        ("empty", vec![], vec![], false),
        ("partial with ghost", vec![first, ghost], vec![first], false),
        ("all with ghost", vec![second, ghost, first], vec![first, second], true),
        ("ghost only", vec![ghost], vec![], false),
    ] {
        let database = Arc::new(InMemoryDatabase::new());
        let room = database.create_room("ready-intersection".into(), None, 4, false, first,
            "relay".into(), "test".into(), None).await.unwrap();
        let mut peer = room.players[&first].clone();
        peer.id = second;
        peer.name = "Peer".into();
        assert!(database.add_player_to_room(&room.id, peer).await.unwrap());
        let messages = Arc::new(RecordingMessageCoordinator::default());
        let coordinator = InMemoryRoomOperationCoordinator::new(messages.clone(), database, None);
        let ready: HashSet<_> = ready.into_iter().collect();
        coordinator.ready_players.write().await.insert(room.id, ready.clone());
        assert!(coordinator.transition_room_to_lobby(&room.id).await.unwrap(), "{name}");
        let events = messages.broadcasts().await;
        assert_eq!(events.len(), 1, "{name}");
        assert!(matches!(&events[0].message,
            ServerMessage::LobbyStateChanged { lobby_state: LobbyState::Lobby, ready_players, all_ready: actual }
            if *ready_players == expected && *actual == all_ready), "{name}: {:?}", events[0].message);
        assert_eq!(coordinator.ready_players.read().await[&room.id], ready, "{name}");
    }
}

/// #1549: A delayed start must still belong to the exact effective starting roster.
#[tokio::test]
async fn start_game_rechecks_requester_membership_after_room_gate() {
    for retained_seat in [false, true] {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(crate::server::InMemoryMessageCoordinator::new());
        let member = PlayerId::from_u128(1);
        let departed = PlayerId::from_u128(2);
        let room = database
            .create_room(
                "stale-start".into(),
                None,
                4,
                false,
                member,
                "relay".into(),
                "test".into(),
                None,
            )
            .await
            .unwrap();
        database
            .add_player_to_room(&room.id, player_fixture(departed, "departed", false, None))
            .await
            .unwrap();
        let mut receivers = Vec::new();
        for player in [member, departed] {
            let (sender, receiver) = mpsc::channel(8);
            messages
                .register_local_client(
                    player,
                    Some(room.id),
                    ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
                )
                .await
                .unwrap();
            receivers.push(receiver);
        }
        let coordinator =
            InMemoryRoomOperationCoordinator::new(messages.clone(), database.clone(), None);
        coordinator
            .ready_players
            .write()
            .await
            .insert(room.id, HashSet::from([member]));
        let guard = messages.lock_room_event_mutation(&room.id).await;
        let mut request = Box::pin(coordinator.handle_start_game(&room.id, &departed));
        assert!(futures_util::poll!(&mut request).is_pending());
        messages.unregister_local_client(&departed).await.unwrap();
        if !retained_seat {
            database
                .remove_player_from_room(&room.id, &departed)
                .await
                .unwrap();
        }
        let before = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        drop(guard);
        assert!(matches!(
            request.await.unwrap(),
            StartGameOutcome::Forbidden
        ));
        let after = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert_eq!(after.lobby_state, before.lobby_state);
        assert_eq!(after.last_activity, before.last_activity);
        assert_eq!(
            coordinator.current_ready_players(&room.id).await,
            vec![member]
        );
        let member_pending = receivers[0].try_recv();
        let departed_pending = receivers[1].try_recv();
        assert!(matches!(member_pending, Err(mpsc::error::TryRecvError::Empty)));
        assert!(matches!(departed_pending, Err(mpsc::error::TryRecvError::Disconnected)));
        database
            .remove_player_from_room(&room.id, &departed)
            .await
            .unwrap();
        assert!(matches!(
            coordinator
                .handle_start_game(&room.id, &member)
                .await
                .unwrap(),
            StartGameOutcome::Started(_)
        ));
    }
}

/// #1553: a member cannot disappear between the roster read and readiness mutation.
#[tokio::test]
async fn ready_commit_keeps_its_roster_while_waiting_for_ready_state() {
    for remove_actor in [false, true] {
        let database = Arc::new(InMemoryDatabase::new());
        let messages = Arc::new(crate::server::InMemoryMessageCoordinator::new());
        let replay = Arc::new(crate::reconnection::ReconnectionManager::new(30, 16, Arc::new(crate::metrics::ServerMetrics::new())));
        let member = PlayerId::from_u128(1);
        let peer = PlayerId::from_u128(2);
        let room = database.create_room("ready-route-fence".into(), None, 4, false, member, "relay".into(), "test".into(), None).await.unwrap();
        database.add_player_to_room(&room.id, player_fixture(peer, "peer", false, None)).await.unwrap();
        replay.register_disconnection(PlayerId::from_u128(3), room.id, false, None, 0).await;
        let mut receivers = Vec::new();
        for player in [member, peer] {
            let (sender, receiver) = mpsc::channel(8);
            receivers.push(receiver);
            messages.register_local_client(player, Some(room.id), ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached())).await.unwrap();
        }
        let coordinator = Arc::new(InMemoryRoomOperationCoordinator::new(messages.clone(), database.clone(), Some(replay.clone())));
        coordinator.ready_players.write().await.insert(room.id, HashSet::from([peer]));
        let ready = coordinator.ready_players.read().await;
        let request = {
            let coordinator = coordinator.clone();
            tokio::spawn(async move { coordinator.handle_player_ready(&room.id, &member, None).await })
        };
        timeout(Duration::from_secs(1), async {
            while database.get_room_lobby_state(&room.id).await.unwrap() != Some(LobbyState::Lobby) {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let removed = if remove_actor { member } else { peer };
        let mut removal = Box::pin(messages.unregister_local_client(&removed));
        assert!(futures_util::poll!(&mut removal).is_pending(), "route removal must wait for the ready commit");
        drop(ready);
        let (result, removed) = tokio::join!(request, removal);
        result.unwrap().unwrap();
        removed.unwrap();
        let mut ready = coordinator.current_ready_players(&room.id).await;
        ready.sort_unstable();
        assert_eq!(ready, vec![member, peer]);
        let events = replay.get_missed_events(&room.id, 0).await.events;
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], ServerMessage::LobbyStateChanged { all_ready: true, ready_players, .. } if ready_players.len() == 2 && ready_players.contains(&member) && ready_players.contains(&peer)));
        let survivor = if remove_actor { &mut receivers[1] } else { &mut receivers[0] };
        let message = survivor.try_recv().unwrap();
        assert_eq!(serde_json::to_value(message.as_ref()).unwrap(), serde_json::to_value(&events[0]).unwrap());
        let trailing = survivor.try_recv();
        assert!(matches!(trailing, Err(mpsc::error::TryRecvError::Empty)));
    }
}
