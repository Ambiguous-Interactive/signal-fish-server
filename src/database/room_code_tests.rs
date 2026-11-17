use super::*;

/// #636: room-cap reads do not compete with member and metadata storage writes.
#[tokio::test]
async fn game_room_count_does_not_wait_for_room_table() {
    let database = InMemoryDatabase::new();
    database
        .create_room(
            "count-game".into(),
            Some("COUNT1".into()),
            4,
            true,
            PlayerId::from_u128(1),
            "relay".into(),
            "test".into(),
            None,
        )
        .await
        .unwrap();
    let _room_table = database.rooms.write().await;
    for (game, expected) in [("count-game", 1), ("missing", 0)] {
        let mut read = Box::pin(database.get_game_room_count(game));
        let std::task::Poll::Ready(count) = futures_util::poll!(read.as_mut()) else {
            panic!("room-cap read must use the independent code index");
        };
        let count = count.unwrap();
        assert_eq!(count, expected, "game {game}");
    }
}

/// #636: cancellation before an index commit cannot change the room-cap result.
#[tokio::test]
async fn game_room_counts_preserve_canceled_storage_commits() {
    let database = InMemoryDatabase::new();
    let game = "canceled-count";
    let room = database
        .create_room(
            game.into(),
            Some("COUNT1".into()),
            4,
            true,
            PlayerId::from_u128(1),
            "relay".into(),
            "test".into(),
            None,
        )
        .await
        .unwrap();
    {
        let _index = database.room_codes.write().await;
        let mut create = Box::pin(database.create_room(
            game.into(),
            Some("COUNT2".into()),
            4,
            true,
            PlayerId::from_u128(2),
            "relay".into(),
            "test".into(),
            None,
        ));
        assert!(futures_util::poll!(create.as_mut()).is_pending());
    }
    assert_eq!(database.get_game_room_count(game).await.unwrap(), 1);
    {
        let _index = database.room_codes.write().await;
        let mut delete = Box::pin(database.delete_room(&room.id));
        assert!(futures_util::poll!(delete.as_mut()).is_pending());
    }
    assert_eq!(database.get_game_room_count(game).await.unwrap(), 1);
    assert!(database.get_room_by_id(&room.id).await.unwrap().is_some());
    database.delete_room(&room.id).await.unwrap();
    assert_eq!(database.get_game_room_count(game).await.unwrap(), 0);
}

/// #636: compact writes retain the full rotation's registry and failure semantics.
#[tokio::test(start_paused = true)]
async fn compact_room_code_rotation_preserves_registry_and_clocks() {
    let database = InMemoryDatabase::new();
    let actor = PlayerId::from_u128(1);
    let mut rooms = Vec::new();
    for code in ["ORIGIN", "TAKEN1"] {
        rooms.push(
            database
                .create_room(
                    "compact-rotation".into(),
                    Some(code.into()),
                    4,
                    true,
                    actor,
                    "relay".into(),
                    "test".into(),
                    None,
                )
                .await
                .unwrap(),
        );
    }
    let room = &rooms[0];
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    let rotated = database
        .update_room_code_snapshot(&room.id, "FRESH1".into())
        .await
        .unwrap();
    assert_eq!(
        rotated,
        RoomCodeSnapshot {
            game_name: room.game_name.clone(),
            code: "FRESH1".into(),
            authority_player: Some(actor),
        }
    );
    assert!(database
        .get_room(&room.game_name, "ORIGIN")
        .await
        .unwrap()
        .is_none());
    let stored = database
        .get_room(&room.game_name, "FRESH1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.id, room.id);
    assert_eq!(
        serde_json::to_value(&stored.players).unwrap(),
        serde_json::to_value(&room.players).unwrap()
    );
    assert_eq!(stored.authority_player, room.authority_player);
    assert!(stored.last_activity >= room.last_activity);
    assert_eq!(
        database
            .room_liveness_monotonic
            .read()
            .await
            .get(&room.id)
            .unwrap()
            .idle_for(),
        chrono::Duration::zero()
    );
    let collision = database
        .update_room_code_snapshot(&room.id, "TAKEN1".into())
        .await
        .unwrap_err();
    assert!(
        matches!(collision, UpdateRoomCodeError::RoomCodeCollision { game_name, room_code } if game_name == room.game_name && room_code == "TAKEN1")
    );
    let unchanged = database.get_room_by_id(&room.id).await.unwrap().unwrap();
    assert_eq!(unchanged.code, stored.code);
    assert_eq!(unchanged.last_activity, stored.last_activity);
    assert_eq!(
        database
            .get_room(&room.game_name, "TAKEN1")
            .await
            .unwrap()
            .unwrap()
            .id,
        rooms[1].id
    );
    assert!(matches!(
        database
            .update_room_code_snapshot(&RoomId::nil(), "ABSENT".into())
            .await,
        Err(UpdateRoomCodeError::Storage(_))
    ));
    database
        .update_room_code(&room.id, "LEGACY".into())
        .await
        .unwrap();
    assert_eq!(
        database
            .get_room_code_snapshot(&room.id)
            .await
            .unwrap()
            .unwrap()
            .code,
        "LEGACY"
    );
    assert!(database
        .get_room(&room.game_name, "FRESH1")
        .await
        .unwrap()
        .is_none());
}
