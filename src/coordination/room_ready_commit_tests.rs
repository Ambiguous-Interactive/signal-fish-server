use super::*;

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
