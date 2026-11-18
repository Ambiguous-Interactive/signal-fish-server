use signal_fish_server::database::{GameDatabase, InMemoryDatabase, PendingRoomCreation};
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::{PlayerId, Room, RoomId};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

const CREATOR: PlayerId = PlayerId::from_u128(1);
const FIRST: Uuid = Uuid::from_u128(100);
const SECOND: Uuid = Uuid::from_u128(200);
const MISSING: Uuid = Uuid::from_u128(300);

async fn create(database: &InMemoryDatabase, code: &str, application: Option<Uuid>) -> Room {
    database
        .create_pending_room_classified(
            "game".into(),
            Some(code.into()),
            4,
            true,
            CREATOR,
            "relay".into(),
            "test".into(),
            application,
            None,
        )
        .await
        .expect("create room")
}

async fn assert_counts(database: &InMemoryDatabase, ids: &[RoomId], expected: [usize; 3]) {
    for (application, expected) in [FIRST, SECOND, MISSING].into_iter().zip(expected) {
        let mut rows = 0;
        for id in ids {
            if database
                .get_room_reconnect_snapshot(id, &CREATOR)
                .await
                .unwrap()
                .is_some_and(|row| row.application_id == Some(application))
            {
                rows += 1;
            }
        }
        assert_eq!(rows, expected, "independent room rows for {application}");
        assert_eq!(
            database
                .get_application_room_count(&application)
                .await
                .unwrap(),
            rows
        );
    }
}

/// #636: pending rows, claims, replacement, and rollback retain exact counts.
#[tokio::test(start_paused = true)]
async fn application_room_counts_preserve_ownership_and_deletion() {
    let database = InMemoryDatabase::new();
    let unowned = create(&database, "UNOWNED", None).await;
    let first = create(&database, "FIRST", Some(FIRST)).await;
    let second = create(&database, "SECOND", Some(SECOND)).await;
    let ids = [unowned.id, first.id, second.id];
    assert_counts(&database, &ids, [1, 1, 0]).await;
    assert!(!database.is_room_published(&first.id).await.unwrap());
    database.publish_room(&first.id).await.unwrap();
    database
        .update_room_code(&first.id, "ROTATE".into())
        .await
        .unwrap();
    database
        .set_room_application_id(&first.id, FIRST)
        .await
        .unwrap();
    assert_counts(&database, &ids, [1, 1, 0]).await;
    database
        .set_room_application_id(&unowned.id, FIRST)
        .await
        .unwrap();
    database
        .set_room_application_id(&first.id, SECOND)
        .await
        .unwrap();
    assert_counts(&database, &ids, [1, 2, 0]).await;
    assert!(!database
        .clear_room_application_id_if_matches(&first.id, FIRST)
        .await
        .unwrap());
    assert_counts(&database, &ids, [1, 2, 0]).await;
    assert!(database
        .clear_room_application_id_if_matches(&first.id, SECOND)
        .await
        .unwrap());
    assert!(!database
        .clear_room_application_id_if_matches(&first.id, SECOND)
        .await
        .unwrap());
    database.clear_room_application_id(&first.id).await.unwrap();
    assert_counts(&database, &ids, [1, 1, 0]).await;
    database
        .clear_room_application_id(&unowned.id)
        .await
        .unwrap();
    database
        .clear_room_application_id(&unowned.id)
        .await
        .unwrap();
    assert_counts(&database, &ids, [0, 1, 0]).await;
    assert!(database
        .set_room_application_id(&MISSING, FIRST)
        .await
        .is_err());
    database.clear_room_application_id(&MISSING).await.unwrap();
    assert!(!database
        .clear_room_application_id_if_matches(&MISSING, FIRST)
        .await
        .unwrap());
    assert!(!database.delete_room(&MISSING).await.unwrap());
    assert!(create_collision(&database, Some(FIRST)).await);
    assert_counts(&database, &ids, [0, 1, 0]).await;
    database.abandon_room(&second.id, &CREATOR).await.unwrap();
    assert!(database.delete_room_if_pending(&second.id).await.unwrap());
    assert!(!database.delete_room_if_pending(&second.id).await.unwrap());
    database
        .set_room_application_id(&first.id, FIRST)
        .await
        .unwrap();
    assert!(database.delete_room(&first.id).await.unwrap());
    assert!(!database.delete_room(&first.id).await.unwrap());
    assert_counts(&database, &ids, [0, 0, 0]).await;
}

async fn create_collision(database: &InMemoryDatabase, application: Option<Uuid>) -> bool {
    database
        .create_room(
            "game".into(),
            Some("ROTATE".into()),
            4,
            true,
            CREATOR,
            "relay".into(),
            "test".into(),
            application,
        )
        .await
        .is_err()
}

/// #636: both GC modes retain protected and pending rows and remove expired owners.
#[tokio::test(start_paused = true)]
async fn application_room_counts_follow_cleanup_and_abandoned_creation() {
    let database = InMemoryDatabase::new();
    let empty = create(&database, "EMPTY", Some(FIRST)).await;
    let occupied = create(&database, "LIVE", Some(FIRST)).await;
    let protected = create(&database, "SAFE", Some(SECOND)).await;
    let pending = create(&database, "PENDING", Some(SECOND)).await;
    let owner = PendingRoomCreation::new(Arc::new(ServerMetrics::new()));
    let abandoned = database
        .create_pending_room_owned_classified(
            "game".into(),
            Some("ABANDON".into()),
            4,
            true,
            CREATOR,
            "relay".into(),
            "test".into(),
            Some(FIRST),
            None,
            owner.clone(),
        )
        .await
        .unwrap();
    let ids = [
        empty.id,
        occupied.id,
        protected.id,
        pending.id,
        abandoned.id,
    ];
    for id in [empty.id, occupied.id, protected.id] {
        database.publish_room(&id).await.unwrap();
    }
    assert!(!database
        .delete_room_if_pending(&abandoned.id)
        .await
        .unwrap());
    assert_counts(&database, &ids, [3, 2, 0]).await;
    drop(owner);
    assert!(database
        .delete_room_if_pending(&abandoned.id)
        .await
        .unwrap());
    database
        .remove_player_from_room(&empty.id, &CREATOR)
        .await
        .unwrap();
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    let protected_ids = HashSet::from([protected.id]);
    assert_eq!(
        database
            .cleanup_empty_rooms(jiff::SignedDuration::from_secs(1), &protected_ids)
            .await
            .unwrap(),
        vec![empty.id]
    );
    assert_counts(&database, &ids, [1, 2, 0]).await;
    let cleanup = database
        .cleanup_expired_rooms(
            jiff::SignedDuration::from_secs(1),
            jiff::SignedDuration::from_secs(1),
            &protected_ids,
        )
        .await
        .unwrap();
    assert_eq!(cleanup.inactive_rooms_cleaned, 1);
    assert_counts(&database, &ids, [0, 2, 0]).await;
    database.publish_room(&pending.id).await.unwrap();
    let lease = database.lease_room_cleanup().await.unwrap().unwrap();
    assert_eq!(
        lease
            .cleanup_expired(
                jiff::SignedDuration::from_secs(1),
                jiff::SignedDuration::from_secs(1),
                &HashSet::new()
            )
            .inactive_rooms_cleaned,
        2
    );
    assert_counts(&database, &ids, [0, 0, 0]).await;
}

/// #636: concurrent ownership churn leaves the index equal to stored rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn application_room_counts_preserve_concurrent_ownership_changes() {
    let database = Arc::new(InMemoryDatabase::new());
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..32 {
        let database = Arc::clone(&database);
        tasks.spawn(async move {
            let room = create(&database, &format!("{index:06}"), Some(FIRST)).await;
            for _ in 0..4 {
                database
                    .set_room_application_id(&room.id, SECOND)
                    .await
                    .unwrap();
                database
                    .clear_room_application_id_if_matches(&room.id, SECOND)
                    .await
                    .unwrap();
                database
                    .set_room_application_id(&room.id, FIRST)
                    .await
                    .unwrap();
            }
            if index % 3 == 0 {
                database.delete_room(&room.id).await.unwrap();
            }
            room.id
        });
    }
    let mut ids = Vec::with_capacity(32);
    while let Some(result) = tasks.join_next().await {
        ids.push(result.expect("ownership worker"));
    }
    assert_counts(&database, &ids, [21, 0, 0]).await;
}
