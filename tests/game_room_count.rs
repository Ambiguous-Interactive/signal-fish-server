use signal_fish_server::database::{GameDatabase, InMemoryDatabase};
use signal_fish_server::protocol::{PlayerId, Room};
use std::collections::HashSet;

async fn create(database: &InMemoryDatabase, game: &str, code: &str) -> Room {
    database
        .create_pending_room_classified(
            game.into(),
            Some(code.into()),
            4,
            true,
            PlayerId::from_u128(1),
            "relay".into(),
            "test".into(),
            None,
            None,
        )
        .await
        .expect("create room")
}

async fn assert_count(database: &InMemoryDatabase, game: &str, expected: usize) {
    assert_eq!(database.get_game_room_count(game).await.unwrap(), expected);
    assert_eq!(
        database
            .get_rooms_by_game()
            .await
            .unwrap()
            .get(game)
            .copied()
            .unwrap_or_default(),
        expected,
        "independent room-table count for {game}"
    );
}

/// #636: indexed room caps include pending rooms and track every removal path.
#[tokio::test(start_paused = true)]
async fn game_room_counts_preserve_lifecycle_and_game_isolation() {
    let database = InMemoryDatabase::new();
    let game = "ゲーム:alpha";
    assert_count(&database, game, 0).await;
    let pending = create(&database, game, "SHARED").await;
    assert!(!database.is_room_published(&pending.id).await.unwrap());
    assert_count(&database, game, 1).await;
    let other = create(&database, "other", "SHARED").await;
    assert_count(&database, "other", 1).await;
    database.publish_room(&other.id).await.unwrap();
    let second = create(&database, game, "SECOND").await;
    database.publish_room(&second.id).await.unwrap();
    assert_count(&database, game, 2).await;
    assert!(database
        .create_room(
            game.into(),
            Some("SHARED".into()),
            4,
            true,
            PlayerId::from_u128(2),
            "relay".into(),
            "test".into(),
            None,
        )
        .await
        .is_err());
    assert!(database
        .update_room_code(&second.id, "SHARED".into())
        .await
        .is_err());
    assert_count(&database, game, 2).await;
    database
        .update_room_code(&second.id, "ROTATE".into())
        .await
        .unwrap();
    assert_count(&database, game, 2).await;
    database
        .abandon_room(&pending.id, &PlayerId::from_u128(1))
        .await
        .unwrap();
    assert!(database.delete_room_if_pending(&pending.id).await.unwrap());
    assert_count(&database, game, 1).await;
    assert!(database.delete_room(&second.id).await.unwrap());
    assert!(!database.delete_room(&second.id).await.unwrap());
    assert_count(&database, game, 0).await;
    let reused = create(&database, game, "SHARED").await;
    database.publish_room(&reused.id).await.unwrap();
    database
        .remove_player_from_room(&reused.id, &PlayerId::from_u128(1))
        .await
        .unwrap();
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    let removed = database
        .cleanup_empty_rooms(jiff::SignedDuration::from_secs(1), &HashSet::new())
        .await
        .unwrap();
    assert_eq!(removed, vec![reused.id]);
    assert_count(&database, game, 0).await;
    let retained_pending = create(&database, game, "PENDING").await;
    tokio::time::advance(std::time::Duration::from_secs(10)).await;
    let protected = HashSet::from([other.id]);
    let retained = database
        .cleanup_expired_rooms(
            jiff::SignedDuration::from_secs(1),
            jiff::SignedDuration::from_secs(1),
            &protected,
        )
        .await
        .unwrap();
    assert_eq!(retained.inactive_rooms_cleaned, 0);
    assert_count(&database, game, 1).await;
    assert_count(&database, "other", 1).await;
    let removed = database
        .cleanup_expired_rooms(
            jiff::SignedDuration::from_secs(1),
            jiff::SignedDuration::from_secs(1),
            &HashSet::new(),
        )
        .await
        .unwrap();
    assert_eq!(removed.inactive_rooms_cleaned, 1);
    assert_count(&database, game, 1).await;
    database.delete_room(&retained_pending.id).await.unwrap();
    assert_count(&database, game, 0).await;
    assert_count(&database, "other", 0).await;
    assert_count(&database, "missing", 0).await;
}
