use super::tests::{age_room, create_test_room};
use super::*;

/// #636: compare exact membership without a second current-member vector.
#[tokio::test]
async fn finalization_membership_requires_a_permutation_without_duplicates() {
    for count in [1, 4, 255] {
        let db = InMemoryDatabase::new();
        let room = db
            .create_room(
                "finalize".into(),
                Some(format!("FINAL{count}")),
                count,
                true,
                Uuid::new_v4(),
                "relay".into(),
                "default".into(),
                None,
            )
            .await
            .unwrap();
        let template = room.players.values().next().unwrap();
        for _ in 1..count {
            let mut player = template.clone();
            player.id = Uuid::new_v4();
            assert!(db.add_player_to_room(&room.id, player).await.unwrap());
        }
        age_room(&db, &room.id, chrono::Duration::hours(2)).await;
        let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        let expected = FinalizeRoomGameExpectation::from_room(&before);
        for invalid in ["absent", "short", "long", "duplicate", "authority", "state"] {
            let mut changed = expected.clone();
            match invalid {
                "absent" => changed.members[0] = Uuid::nil(),
                "short" => {
                    changed.members.pop();
                }
                "long" => changed.members.push(Uuid::nil()),
                "duplicate" => {
                    if changed.members.len() == 1 {
                        continue;
                    }
                    changed.members[1] = changed.members[0];
                }
                "authority" => changed.authority_player = None,
                "state" => changed.lobby_state = crate::protocol::LobbyState::Lobby,
                _ => unreachable!(),
            }
            assert_eq!(
                db.finalize_room_game(&room.id, &changed).await.unwrap(),
                FinalizeRoomGameOutcome::SnapshotChanged,
                "{count}: {invalid}"
            );
            let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
            assert_eq!(after.last_activity, before.last_activity);
            assert_eq!(after.lobby_state, before.lobby_state);
            assert_eq!(after.ready_players, before.ready_players);
            assert!(after.players.values().all(|player| !player.is_ready));
        }
        let mut reversed = expected.clone();
        reversed.members.reverse();
        let original = reversed.clone();
        assert_eq!(
            db.finalize_room_game(&room.id, &reversed).await.unwrap(),
            FinalizeRoomGameOutcome::Finalized
        );
        assert_eq!(reversed, original, "caller snapshot stays unchanged");
        let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert_eq!(
            after.ready_players, expected.members,
            "stored member order stays unchanged"
        );
        assert!(after.players.values().all(|player| player.is_ready));
        assert_eq!(
            db.finalize_room_game(&room.id, &reversed).await.unwrap(),
            FinalizeRoomGameOutcome::AlreadyFinalized
        );
    }
}

/// Apply one metadata write through the public storage trait.
async fn apply_metadata_change(
    db: &InMemoryDatabase,
    room: &Room,
    operation: &str,
    member: PlayerId,
) -> bool {
    match operation {
        "name" => db
            .update_player_name(&room.id, &member, "new name")
            .await
            .unwrap(),
        "connection" => db
            .update_player_connection_info(
                &room.id,
                &member,
                ConnectionInfo::Direct {
                    host: "localhost".into(),
                    port: 1234,
                },
            )
            .await
            .unwrap(),
        "clear" => db
            .clear_player_connection_info(&room.id, &member)
            .await
            .unwrap(),
        _ => unreachable!(),
    }
}

/// #1309: each changed field renews both clocks for a full expiry window.
#[tokio::test(start_paused = true)]
async fn player_metadata_changes_renew_room_expiry() {
    for operation in ["name", "connection", "clear"] {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "metadata", "METADATA").await.unwrap();
        let member = *room.players.keys().next().unwrap();
        if operation == "clear" {
            assert!(apply_metadata_change(&db, &room, "connection", member).await);
        }
        age_room(&db, &room.id, chrono::Duration::hours(2)).await;
        let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert!(apply_metadata_change(&db, &room, operation, member).await);
        let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert!(
            after.last_activity > before.last_activity,
            "{operation}: wall clock"
        );
        match operation {
            "name" => assert_eq!(after.players[&member].name, "new name"),
            "connection" => assert!(matches!(
                after.players[&member].connection_info,
                Some(ConnectionInfo::Direct { port: 1234, .. })
            )),
            "clear" => assert!(after.players[&member].connection_info.is_none()),
            _ => unreachable!(),
        }
        let cleanup = db
            .cleanup_expired_rooms(
                chrono::Duration::seconds(300),
                chrono::Duration::hours(1),
                &HashSet::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            cleanup.inactive_rooms_cleaned, 0,
            "{operation}: monotonic clock"
        );
        tokio::time::advance(std::time::Duration::from_secs(3601)).await;
        let cleanup = db
            .cleanup_expired_rooms(
                chrono::Duration::seconds(300),
                chrono::Duration::hours(1),
                &HashSet::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            cleanup.inactive_rooms_cleaned, 1,
            "{operation}: eventually expires"
        );
    }
}

/// #1309: repeated values and absent rows cannot extend room lifetime.
#[tokio::test(start_paused = true)]
async fn unchanged_and_absent_player_metadata_preserves_expiry() {
    for operation in ["name", "connection", "clear"] {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "metadata", "NOCHANGE").await.unwrap();
        let member = *room.players.keys().next().unwrap();
        assert!(apply_metadata_change(&db, &room, operation, member).await);
        age_room(&db, &room.id, chrono::Duration::hours(2)).await;
        let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert!(apply_metadata_change(&db, &room, operation, member).await);
        assert!(!apply_metadata_change(&db, &room, operation, Uuid::nil()).await);
        let mut absent = room.clone();
        absent.id = Uuid::nil();
        assert!(!apply_metadata_change(&db, &absent, operation, member).await);
        let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert_eq!(after.last_activity, before.last_activity, "{operation}");
        let cleanup = db
            .cleanup_expired_rooms(
                chrono::Duration::seconds(300),
                chrono::Duration::hours(1),
                &HashSet::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            cleanup.inactive_rooms_cleaned, 1,
            "{operation}: no extension"
        );
    }
}

/// #1309: a cancelled metadata write cannot commit before its expiry lock.
#[tokio::test]
async fn cancelled_player_metadata_preserves_room_and_clock() {
    for operation in ["name", "connection", "clear"] {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "metadata", "CANCELMETA")
            .await
            .unwrap();
        let member = *room.players.keys().next().unwrap();
        if operation == "clear" {
            assert!(apply_metadata_change(&db, &room, "connection", member).await);
        }
        age_room(&db, &room.id, chrono::Duration::hours(2)).await;
        let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        let liveness = db.room_liveness_monotonic.write().await;
        {
            let change = apply_metadata_change(&db, &room, operation, member);
            let mut change = std::pin::pin!(change);
            assert!(
                futures_util::poll!(change.as_mut()).is_pending(),
                "{operation}"
            );
            assert!(db.rooms.try_read().is_err());
        }
        let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert_eq!(after.last_activity, before.last_activity);
        assert_eq!(
            serde_json::to_value(&after.players[&member]).unwrap(),
            serde_json::to_value(&before.players[&member]).unwrap()
        );
        assert!(matches!(liveness[&room.id], RoomLiveness::AgedFor(_)));
    }
}
