use super::tests::{age_room, create_test_room};
use super::*;

/// #1317: repairing flags renews both clocks even when the selected ID stays the same.
#[tokio::test(start_paused = true)]
async fn authority_flag_repair_renews_both_room_clocks_once() {
    for selected_member in [false, true] {
        let db = InMemoryDatabase::new();
        let room = create_test_room(&db, "authority", "REPAIR").await.unwrap();
        let member = *room.players.keys().next().unwrap();
        let selected = selected_member.then_some(member);
        {
            let mut rooms = db.rooms.write().await;
            let stored = rooms.get_mut(&room.id).unwrap();
            stored.authority_player = selected;
            stored.players.get_mut(&member).unwrap().is_authority = !selected_member;
        }
        age_room(&db, &room.id, chrono::Duration::hours(2)).await;
        let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert!(db.update_room_authority(&room.id, selected).await.unwrap());
        let after = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert_eq!(after.authority_player, selected);
        assert_eq!(after.players[&member].is_authority, selected_member);
        assert!(after.last_activity > before.last_activity);
        let cleanup = db
            .cleanup_expired_rooms(
                chrono::Duration::seconds(300),
                chrono::Duration::hours(1),
                &HashSet::new(),
            )
            .await
            .unwrap();
        assert!(
            cleanup.is_empty(),
            "repaired flags must renew monotonic liveness"
        );
        age_room(&db, &room.id, chrono::Duration::hours(2)).await;
        let before = db.get_room_by_id(&room.id).await.unwrap().unwrap();
        assert!(db.update_room_authority(&room.id, selected).await.unwrap());
        assert_eq!(
            db.get_room_by_id(&room.id)
                .await
                .unwrap()
                .unwrap()
                .last_activity,
            before.last_activity
        );
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
            "consistent repetition must expire"
        );
    }
}

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

/// #636: existence reads include pending rows and follow deletion without snapshots.
#[tokio::test]
async fn room_existence_tracks_pending_published_and_deleted_rows() {
    let db = InMemoryDatabase::new();
    assert!(!db.room_exists(&Uuid::nil()).await.unwrap());
    let room = db
        .create_pending_room_classified(
            "existence-雪".into(),
            Some("EXISTS".into()),
            4,
            true,
            Uuid::new_v4(),
            "relay".into(),
            "default".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(db.room_exists(&room.id).await.unwrap());
    assert!(!db.is_room_published(&room.id).await.unwrap());
    db.publish_room(&room.id).await.unwrap();
    assert!(db.room_exists(&room.id).await.unwrap());
    assert!(db.is_room_published(&room.id).await.unwrap());
    db.fail_get_room_by_id_for_test(true);
    assert!(db.room_exists(&room.id).await.is_err());
    db.fail_get_room_by_id_for_test(false);
    assert!(db.delete_room(&room.id).await.unwrap());
    assert!(!db.room_exists(&room.id).await.unwrap());
    assert!(!db.room_exists(&Uuid::nil()).await.unwrap());
}

/// ID-only lookup retains code scope, publication visibility and stale-index handling.
#[tokio::test]
async fn room_id_lookup_matches_full_snapshots_through_publication_rotation_and_deletion() {
    let db = InMemoryDatabase::new();
    let room = db
        .create_pending_room_classified(
            "lookup".into(),
            Some("BEFORE".into()),
            4,
            true,
            Uuid::new_v4(),
            "relay".into(),
            "test".into(),
            None,
            None,
        )
        .await
        .unwrap();
    for stage in ["pending", "published", "rotated", "deleted", "reused"] {
        match stage {
            "published" => db.publish_room(&room.id).await.unwrap(),
            "rotated" => {
                db.update_room_code(&room.id, "AFTER1".into())
                    .await
                    .unwrap();
            }
            "deleted" => {
                assert!(db.delete_room(&room.id).await.unwrap());
            }
            "reused" => {
                create_test_room(&db, "lookup", "BEFORE").await.unwrap();
            }
            _ => {}
        }
        for (game, code) in [
            ("lookup", "BEFORE"),
            ("lookup", "AFTER1"),
            ("other", "BEFORE"),
            ("lookup", "before"),
        ] {
            assert_eq!(
                db.get_room_id(game, code).await.unwrap(),
                db.get_room(game, code).await.unwrap().map(|room| room.id),
                "stage={stage}, game={game}, code={code}"
            );
        }
    }
    let stale = Uuid::new_v4();
    db.room_codes
        .write()
        .await
        .insert(("lookup".into(), "STALE1".into()), stale);
    assert_eq!(db.get_room_id("lookup", "STALE1").await.unwrap(), None);
}

/// The projection must wait for every lock that protects a full code lookup.
#[tokio::test]
async fn room_id_lookup_serializes_with_membership_codes_and_publication() {
    let db = InMemoryDatabase::new();
    let room = create_test_room(&db, "lookup", "LOCKED").await.unwrap();
    {
        let guard = db.rooms.write().await;
        let lookup = db.get_room_id("lookup", "LOCKED");
        tokio::pin!(lookup);
        assert!(futures_util::poll!(&mut lookup).is_pending());
        drop(guard);
        assert_eq!(lookup.await.unwrap(), Some(room.id));
    }
    {
        let guard = db.room_codes.write().await;
        let lookup = db.get_room_id("lookup", "LOCKED");
        tokio::pin!(lookup);
        assert!(futures_util::poll!(&mut lookup).is_pending());
        drop(guard);
        assert_eq!(lookup.await.unwrap(), Some(room.id));
    }
    {
        let guard = db.room_publication.write().await;
        let lookup = db.get_room_id("lookup", "LOCKED");
        tokio::pin!(lookup);
        assert!(futures_util::poll!(&mut lookup).is_pending());
        drop(guard);
        assert_eq!(lookup.await.unwrap(), Some(room.id));
    }
}

/// #826: session members, authority and state share one atomic room read.
#[tokio::test]
async fn session_snapshot_preserves_pending_members_policy_and_read_failures() {
    let database = InMemoryDatabase::new();
    let room = database
        .create_pending_room_classified(
            "session-snapshot".into(),
            Some("SNAP01".into()),
            4,
            true,
            PlayerId::from_u128(1),
            "relay".into(),
            "region".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(!database.is_room_published(&room.id).await.unwrap());
    for state in [
        crate::protocol::LobbyState::Waiting,
        crate::protocol::LobbyState::Lobby,
        crate::protocol::LobbyState::Finalized,
    ] {
        {
            let mut rooms = database.rooms.write().await;
            let stored = rooms.get_mut(&room.id).unwrap();
            stored.lobby_state = state.clone();
            let player = stored.players.values_mut().next().unwrap();
            player.name = "名称\0with metadata".into();
            player.connection_info = Some(ConnectionInfo::Custom {
                data: serde_json::json!({"nested": ["exact", 17]}),
            });
            player.epoch = Some(7);
            player.seq = Some(19);
        }
        let expected = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        let snapshot = database
            .get_room_session_snapshot(&room.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.lobby_state, state);
        assert_eq!(snapshot.authority_player, expected.authority_player);
        assert_eq!(snapshot.players.len(), expected.players.len());
        for player in snapshot.players {
            assert_eq!(
                serde_json::to_value(&player).unwrap(),
                serde_json::to_value(&expected.players[&player.id]).unwrap()
            );
            assert_eq!(player.region_id, expected.players[&player.id].region_id);
        }
    }
    let held = database.rooms.write().await;
    let mut read = Box::pin(database.get_room_session_snapshot(&room.id));
    assert!(futures_util::poll!(&mut read).is_pending());
    drop(read);
    drop(held);
    database.publish_room(&room.id).await.unwrap();
    database
        .update_room_authority(&room.id, None)
        .await
        .unwrap();
    assert_eq!(
        database
            .get_room_session_snapshot(&room.id)
            .await
            .unwrap()
            .unwrap()
            .authority_player,
        None
    );
    database
        .fail_get_room_by_id
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(database.get_room_session_snapshot(&room.id).await.is_err());
    database
        .fail_get_room_by_id
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(database.delete_room(&room.id).await.unwrap());
    assert!(database
        .get_room_session_snapshot(&room.id)
        .await
        .unwrap()
        .is_none());
}

/// #826: code and roster stay paired across pending visibility, cancellation and errors.
#[tokio::test]
async fn spectator_snapshot_preserves_atomic_code_roster_and_read_failures() {
    let database = InMemoryDatabase::new();
    let room = database
        .create_pending_room_classified(
            "spectator-snapshot".into(),
            Some("SPEC01".into()),
            4,
            true,
            PlayerId::from_u128(1),
            "relay".into(),
            "region".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(!database.is_room_published(&room.id).await.unwrap());
    for count in [0_u128, 1, 255] {
        {
            let mut rooms = database.rooms.write().await;
            let stored = rooms.get_mut(&room.id).unwrap();
            stored.code = format!("SPEC{count:03}");
            stored.spectators.clear();
            for index in 0..count {
                let id = PlayerId::from_u128(index + 2);
                stored.spectators.insert(
                    id,
                    SpectatorInfo {
                        id,
                        name: format!("名称\0-{index}"),
                        connected_at: chrono::DateTime::from_timestamp(1234, 0),
                    },
                );
            }
        }
        let expected = database.get_room_by_id(&room.id).await.unwrap().unwrap();
        let actual = database
            .get_room_spectator_snapshot(&room.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actual.room_code, expected.code);
        assert_eq!(actual.spectators.len(), expected.spectators.len());
        for spectator in actual.spectators {
            assert_eq!(
                serde_json::to_value(&spectator).unwrap(),
                serde_json::to_value(&expected.spectators[&spectator.id]).unwrap()
            );
        }
    }
    let mut held = database.rooms.write().await;
    let mut read = Box::pin(database.get_room_spectator_snapshot(&room.id));
    assert!(futures_util::poll!(&mut read).is_pending());
    drop(read);
    let stored = held.get_mut(&room.id).unwrap();
    stored.code = "PAIRED".into();
    stored.spectators.clear();
    drop(held);
    let actual = database
        .get_room_spectator_snapshot(&room.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(actual.room_code, "PAIRED");
    assert!(actual.spectators.is_empty());
    database
        .fail_get_room_by_id
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(database
        .get_room_spectator_snapshot(&room.id)
        .await
        .is_err());
    database
        .fail_get_room_by_id
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(database.delete_room(&room.id).await.unwrap());
    assert!(database
        .get_room_spectator_snapshot(&room.id)
        .await
        .unwrap()
        .is_none());
}
