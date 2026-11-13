use super::*;

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
