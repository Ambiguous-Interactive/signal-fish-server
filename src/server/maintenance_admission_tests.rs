//! Admission contention, retry preservation and room-assignment controls.

use super::*;
use crate::config::{
    CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
    TransportSecurityConfig, TurnConfig,
};
use crate::database::DatabaseConfig;
use crate::server::ServerConfig;
use tokio::time::{timeout, Duration};

/// Use a short reconnect window for paused-clock expiry histories.
async fn server() -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        ServerConfig {
            reconnection_window: Duration::from_secs(1),
            ..ServerConfig::default()
        },
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        TurnConfig::default(),
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .unwrap()
}

/// Create an occupied durable room without registering a live connection.
async fn room(server: &EnhancedGameServer, player: PlayerId) -> RoomId {
    server
        .database
        .create_room(
            "maintenance-admission".into(),
            None,
            4,
            false,
            player,
            "relay".into(),
            "test".into(),
            None,
        )
        .await
        .unwrap()
        .id
}

/// Repair available rooms while retaining a busy room's durable detach for retry.
#[tokio::test(start_paused = true)]
async fn durable_detach_admission_skips_busy_room_and_retries_without_losing_seats() {
    let server = server().await;
    let blocked = PlayerId::from_u128(1);
    let healthy = PlayerId::from_u128(2);
    let blocked_room = room(&server, blocked).await;
    let healthy_room = room(&server, healthy).await;
    for pair in [(blocked_room, blocked), (healthy_room, healthy)] {
        server.pending_durable_player_detaches.insert(pair, None);
    }
    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&blocked_room)
        .await;
    assert_eq!(
        timeout(
            Duration::from_secs(1),
            server.cleanup_pending_durable_player_detaches()
        )
        .await
        .expect("one busy room must not stop other durable detach repairs"),
        1
    );
    assert!(server
        .pending_durable_player_detaches
        .contains_key(&(blocked_room, blocked)));
    assert!(server
        .database
        .get_room_players(&healthy_room)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        server
            .database
            .get_room_players(&blocked_room)
            .await
            .unwrap()
            .len(),
        1
    );
    drop(guard);
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 1);
    assert!(server
        .database
        .get_room_players(&blocked_room)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(server.cleanup_pending_durable_player_detaches().await, 0);
}

/// Retain a busy expired seat and its record until a later admitted cleanup.
#[tokio::test(start_paused = true)]
async fn expired_reconnect_admission_skips_busy_room_and_preserves_retry_record() {
    let server = server().await;
    let manager = server.reconnection_manager().unwrap();
    let blocked = PlayerId::from_u128(1);
    let healthy = PlayerId::from_u128(2);
    let blocked_room = room(&server, blocked).await;
    let healthy_room = room(&server, healthy).await;
    for (player, room) in [(blocked, blocked_room), (healthy, healthy_room)] {
        manager
            .register_disconnection(player, room, false, None, 0)
            .await;
    }
    tokio::time::advance(Duration::from_secs(2)).await;
    let guard = server
        .message_coordinator
        .lock_room_event_mutation(&blocked_room)
        .await;
    assert_eq!(
        timeout(
            Duration::from_secs(1),
            server.cleanup_expired_reconnections()
        )
        .await
        .expect("one busy room must not stop other expired seat repairs"),
        1
    );
    assert!(manager.has_pending_reconnection(&blocked).await);
    assert!(!manager.has_pending_reconnection(&healthy).await);
    assert_eq!(
        server
            .database
            .get_room_players(&blocked_room)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(server
        .database
        .get_room_players(&healthy_room)
        .await
        .unwrap()
        .is_empty());
    drop(guard);
    assert_eq!(server.cleanup_expired_reconnections().await, 1);
    assert!(!manager.has_pending_reconnection(&blocked).await);
    assert!(server
        .database
        .get_room_players(&blocked_room)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(server.cleanup_expired_reconnections().await, 0);
}

/// Skip a busy lifecycle and preserve a client that moves to a live room.
#[tokio::test(start_paused = true)]
async fn deleted_room_admission_skips_busy_lifecycle_and_rechecks_room_assignment() {
    let server = server().await;
    let blocked = PlayerId::from_u128(1);
    let healthy = PlayerId::from_u128(2);
    let mut receivers = Vec::new();
    for player in [blocked, healthy] {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        server.connect_client(player, sender).await;
        receivers.push(receiver);
        let room_id = room(&server, player).await;
        server
            .connection_manager
            .assign_client_to_room(&player, room_id)
            .await;
        server.database.delete_room(&room_id).await.unwrap();
    }
    let lifecycle = server
        .connection_manager
        .client_lifecycle(&blocked)
        .unwrap();
    let guard = lifecycle.lock().await;
    assert_eq!(
        timeout(
            Duration::from_secs(1),
            server.cleanup_clients_in_missing_rooms()
        )
        .await
        .expect("one busy connection must not stop other deleted-room cleanup"),
        1
    );
    assert!(server.connection_manager.has_client(&blocked));
    assert!(!server.connection_manager.has_client(&healthy));
    let live_room = room(&server, blocked).await;
    server
        .connection_manager
        .assign_client_to_room(&blocked, live_room)
        .await;
    drop(guard);
    assert_eq!(server.cleanup_clients_in_missing_rooms().await, 0);
    assert!(server.connection_manager.has_client(&blocked));
    server.database.delete_room(&live_room).await.unwrap();
    assert_eq!(server.cleanup_clients_in_missing_rooms().await, 1);
    assert!(!server.connection_manager.has_client(&blocked));
    drop(receivers);
}

/// Admit a contended room when its gate releases before the wait expires.
#[tokio::test(start_paused = true)]
async fn room_cleanup_admission_accepts_gate_release_within_the_wait_budget() {
    for expired in [false, true] {
        let server = server().await;
        let player = PlayerId::from_u128(1);
        let room = room(&server, player).await;
        let manager = server.reconnection_manager().unwrap();
        if expired {
            manager
                .register_disconnection(player, room, false, None, 0)
                .await;
            tokio::time::advance(Duration::from_secs(2)).await;
        } else {
            server
                .pending_durable_player_detaches
                .insert((room, player), None);
        }
        let guard = server
            .message_coordinator
            .lock_room_event_mutation(&room)
            .await;
        let release = tokio::spawn(async move {
            tokio::time::sleep(MAINTENANCE_ADMISSION_WAIT / 2).await;
            drop(guard);
        });
        let cleaned = if expired {
            server.cleanup_expired_reconnections().await
        } else {
            server.cleanup_pending_durable_player_detaches().await
        };
        assert_eq!(cleaned, 1, "admit the released gate: expired={expired}");
        release.await.unwrap();
        assert!(server
            .database
            .get_room_players(&room)
            .await
            .unwrap()
            .is_empty());
    }
}

/// #1547: An activity timeout must not keep the maintenance sweep behind a busy lifecycle.
#[tokio::test(start_paused = true)]
async fn activity_eviction_allows_maintenance_to_finish_while_unregister_waits() {
    for blocked_gate in ["lifecycle", "room"] {
        let server = server().await;
        let player = PlayerId::from_u128(1);
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        server.connect_client(player, sender).await;
        let lifecycle = server.connection_manager.client_lifecycle(&player).unwrap();
        let room_id = room(&server, player).await;
        server
            .connection_manager
            .assign_client_to_room(&player, room_id)
            .await;
        let lifecycle_guard = if blocked_gate == "lifecycle" {
            Some(lifecycle.lock().await)
        } else {
            None
        };
        let room_guard = if blocked_gate == "room" {
            Some(
                server
                    .message_coordinator
                    .lock_room_event_mutation(&room_id)
                    .await,
            )
        } else {
            None
        };
        tokio::time::advance(server.config.ping_timeout + Duration::from_secs(1)).await;
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let maintenance_server = Arc::clone(&server);
        let maintenance = tokio::spawn(async move {
            maintenance_server
                .cleanup_task_until(async {
                    let _ = stopped.await;
                })
                .await;
        });
        let swept = timeout(Duration::from_millis(500), async {
            while server
                .metrics
                .maintenance_sweeps_total
                .load(Ordering::Relaxed)
                == 0
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            swept.is_ok(),
            "a busy unregister must not stop the maintenance sweep"
        );
        assert!(server.connection_manager.has_client(&player));
        let farewell = receiver.try_recv().unwrap();
        assert!(matches!(
            farewell.as_ref(),
            ServerMessage::Error {
                error_code: Some(ErrorCode::ActivityTimeout),
                ..
            }
        ));
        drop(lifecycle_guard);
        drop(room_guard);
        stop.send(()).unwrap();
        timeout(Duration::from_secs(1), maintenance)
            .await
            .unwrap()
            .unwrap();
        timeout(Duration::from_secs(1), async {
            while server.connection_manager.has_client(&player) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the owned unregister must finish after admission resumes");
    }
}

/// #1547: A batch shares one observation budget and never repeats pinned evictions.
#[tokio::test(start_paused = true)]
async fn activity_eviction_batch_shares_a_wait_budget_and_keeps_owned_teardowns() {
    let server = server().await;
    let mut guards = Vec::new();
    let mut receivers = Vec::new();
    let mut players = Vec::new();
    for id in 1..=8 {
        let player = PlayerId::from_u128(id);
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        server.connect_client(player, sender).await;
        let lifecycle = server.connection_manager.client_lifecycle(&player).unwrap();
        guards.push(lifecycle.lock_owned().await);
        receivers.push(receiver);
        players.push(player);
    }
    tokio::time::advance(server.config.ping_timeout + Duration::from_secs(1)).await;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let maintenance_server = Arc::clone(&server);
    let maintenance = tokio::spawn(async move {
        maintenance_server
            .cleanup_task_until(async {
                let _ = stopped.await;
            })
            .await;
    });
    timeout(Duration::from_millis(500), async {
        while server
            .metrics
            .maintenance_sweeps_total
            .load(Ordering::Relaxed)
            == 0
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("one eviction batch must share its teardown observation budget");
    assert_eq!(
        server
            .metrics
            .expired_players_cleaned
            .load(Ordering::Relaxed),
        8
    );
    let first_sweeps = server
        .metrics
        .maintenance_sweeps_total
        .load(Ordering::Relaxed);
    tokio::time::advance(
        server
            .config
            .room_cleanup_interval
            .max(Duration::from_secs(1)),
    )
    .await;
    timeout(Duration::from_millis(500), async {
        while server
            .metrics
            .maintenance_sweeps_total
            .load(Ordering::Relaxed)
            <= first_sweeps
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a later sweep must not wait for already pinned activity evictions");
    assert_eq!(
        server
            .metrics
            .expired_players_cleaned
            .load(Ordering::Relaxed),
        8
    );
    for (player, receiver) in players.iter().zip(&mut receivers) {
        assert!(server.connection_manager.has_client(player));
        let farewell = receiver.try_recv().unwrap();
        assert!(matches!(
            farewell.as_ref(),
            ServerMessage::Error {
                error_code: Some(ErrorCode::ActivityTimeout),
                ..
            }
        ));
        let pending = receiver.try_recv();
        assert!(matches!(
            pending,
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }
    stop.send(()).unwrap();
    timeout(Duration::from_secs(1), maintenance)
        .await
        .unwrap()
        .unwrap();
    drop(guards);
    timeout(Duration::from_secs(1), async {
        while players
            .iter()
            .any(|player| server.connection_manager.has_client(player))
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("every teardown must outlive its observer and finish after gate release");
}
