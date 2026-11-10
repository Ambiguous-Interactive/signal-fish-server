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

/// Exercise bounded observations and eventual recovery across every cleanup queue.
async fn busy_cleanup_batch_shares_one_wait(lane: &str) {
    let server = server().await;
    let mut room_guards = Vec::new();
    let mut lifecycle_guards = Vec::new();
    let mut receivers = Vec::new();
    for id in 1..=32 {
        let player = PlayerId::from_u128(id);
        let room_id = room(&server, player).await;
        match lane {
            "detach" => {
                server
                    .pending_durable_player_detaches
                    .insert((room_id, player), None);
            }
            "reconnect" => {
                server
                    .reconnection_manager()
                    .unwrap()
                    .register_disconnection(player, room_id, false, None, 0)
                    .await;
            }
            "deleted" => {
                let (sender, receiver) = tokio::sync::mpsc::channel(8);
                server.connect_client(player, sender).await;
                receivers.push(receiver);
                server
                    .connection_manager
                    .assign_client_to_room(&player, room_id)
                    .await;
                let lifecycle = server.connection_manager.client_lifecycle(&player).unwrap();
                lifecycle_guards.push(lifecycle.lock_owned().await);
                server.database.delete_room(&room_id).await.unwrap();
            }
            _ => unreachable!(),
        }
        if lane != "deleted" {
            room_guards.push(
                server
                    .message_coordinator
                    .lock_room_event_mutation(&room_id)
                    .await,
            );
        }
    }
    tokio::time::advance(Duration::from_secs(2)).await;
    let sweep = async {
        match lane {
            "detach" => server.cleanup_pending_durable_player_detaches().await,
            "reconnect" => server.cleanup_expired_reconnections().await,
            "deleted" => server.cleanup_clients_in_missing_rooms().await,
            _ => unreachable!(),
        }
    };
    let result = timeout(Duration::from_millis(500), sweep).await;
    assert!(
        result.is_ok(),
        "{lane}: one sweep must share its wait across all busy candidates"
    );
    assert_eq!(result.unwrap(), 0, "held candidates must remain intact");
    drop(room_guards);
    drop(lifecycle_guards);
    timeout(Duration::from_secs(2), async {
        loop {
            let remains = match lane {
                "detach" => {
                    server.cleanup_pending_durable_player_detaches().await;
                    !server.pending_durable_player_detaches.is_empty()
                }
                "reconnect" => {
                    server.cleanup_expired_reconnections().await;
                    !server
                        .reconnection_manager()
                        .unwrap()
                        .expired_cleanup_candidates()
                        .await
                        .is_empty()
                }
                "deleted" => {
                    server.cleanup_clients_in_missing_rooms().await;
                    !server.connection_manager.client_ids().is_empty()
                }
                _ => unreachable!(),
            };
            if !remains {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("every retained candidate must finish after its gate releases");
    drop(receivers);
}

/// #1561: busy candidates share one observation wait and remain retryable.
#[tokio::test(start_paused = true)]
async fn durable_detach_batch_shares_one_wait() {
    busy_cleanup_batch_shares_one_wait("detach").await;
}

/// #1562: busy candidates share one observation wait and remain retryable.
#[tokio::test(start_paused = true)]
async fn expired_reconnect_batch_shares_one_wait() {
    busy_cleanup_batch_shares_one_wait("reconnect").await;
}

/// #1563: busy candidates share one observation wait and remain retryable.
#[tokio::test(start_paused = true)]
async fn deleted_room_batch_shares_one_wait() {
    busy_cleanup_batch_shares_one_wait("deleted").await;
}

/// #1561/#1562: storage completion remains owned after the shared observation wait.
#[tokio::test(start_paused = true)]
async fn admitted_seat_cleanup_outlives_repeated_observers_while_storage_waits() {
    for expired in [false, true] {
        let server = server().await;
        let player = PlayerId::from_u128(1);
        let room_id = room(&server, player).await;
        if expired {
            server
                .reconnection_manager()
                .unwrap()
                .register_disconnection(player, room_id, false, None, 0)
                .await;
            tokio::time::advance(Duration::from_secs(2)).await;
        } else {
            server
                .pending_durable_player_detaches
                .insert((room_id, player), None);
        }
        let database = server
            .database
            .as_any()
            .downcast_ref::<crate::database::InMemoryDatabase>()
            .unwrap();
        database.pause_next_removed_player_for_test();
        let observer = {
            let server = server.clone();
            tokio::spawn(async move {
                if expired {
                    server.cleanup_expired_reconnections().await
                } else {
                    server.cleanup_pending_durable_player_detaches().await
                }
            })
        };
        timeout(
            Duration::from_secs(1),
            database.wait_for_removed_player_for_test(),
        )
        .await
        .unwrap();
        assert_eq!(
            timeout(Duration::from_millis(500), observer)
                .await
                .expect("a committed removal cannot hold its observer behind storage")
                .unwrap(),
            0
        );
        for _ in 0..2 {
            let observed = timeout(Duration::from_millis(500), async {
                if expired {
                    server.cleanup_expired_reconnections().await
                } else {
                    server.cleanup_pending_durable_player_detaches().await
                }
            })
            .await
            .expect("a retained transaction must not block the next sweep or candidate scan");
            assert_eq!(observed, 0);
        }
        database.release_removed_player_for_test();
        timeout(Duration::from_secs(1), async {
            loop {
                if expired {
                    server.cleanup_expired_reconnections().await;
                } else {
                    server.cleanup_pending_durable_player_detaches().await;
                }
                let pending = if expired {
                    server
                        .reconnection_manager()
                        .unwrap()
                        .has_pending_reconnection(&player)
                        .await
                } else {
                    server
                        .pending_durable_player_detaches
                        .contains_key(&(room_id, player))
                };
                if !pending {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the admitted removal must finish after storage resumes");
        let members = server.database.get_room_players(&room_id).await.unwrap();
        assert!(members.is_empty());
    }
}

/// #1563: admitted teardown must survive caller cancellation after the close is pinned.
#[tokio::test(start_paused = true)]
async fn canceled_deleted_room_observer_keeps_the_admitted_terminal_teardown() {
    let server = server().await;
    let player = PlayerId::from_u128(1);
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    server.connect_client(player, sender).await;
    let room_id = room(&server, player).await;
    server
        .connection_manager
        .assign_client_to_room(&player, room_id)
        .await;
    server.database.delete_room(&room_id).await.unwrap();
    let gate = server
        .message_coordinator
        .lock_room_event_mutation(&room_id)
        .await;
    let close = server.connection_manager.close_signal_for(&player).unwrap();
    let observer = {
        let server = server.clone();
        tokio::spawn(async move { server.cleanup_clients_in_missing_rooms().await })
    };
    timeout(Duration::from_secs(1), async {
        while close.requested_reason().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(close.requested_reason(), Some(CloseReason::RoomInactive));
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert_eq!(
        timeout(
            Duration::from_millis(500),
            server.cleanup_clients_in_missing_rooms()
        )
        .await
        .unwrap(),
        0
    );
    assert!(server.connection_manager.has_client(&player));
    let farewell = receiver.try_recv().unwrap();
    assert!(matches!(
        farewell.as_ref(),
        ServerMessage::Error {
            error_code: Some(ErrorCode::RoomNotFound),
            ..
        }
    ));
    let pending = receiver.try_recv();
    assert!(matches!(
        pending,
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    drop(gate);
    timeout(Duration::from_secs(1), async {
        while server.connection_manager.has_client(&player) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the original worker must finish without another cleanup invocation");
    assert_eq!(close.requested_reason(), Some(CloseReason::RoomInactive));
    assert!(
        !server
            .reconnection_manager()
            .unwrap()
            .has_pending_reconnection(&player)
            .await
    );
}

/// #1562: one admitted storage wait must not block a healthy expired seat.
#[tokio::test(start_paused = true)]
async fn expired_seat_storage_wait_preserves_healthy_neighbor_progress() {
    let server = server().await;
    let blocked = PlayerId::from_u128(1);
    let healthy = PlayerId::from_u128(2);
    let blocked_room = room(&server, blocked).await;
    let healthy_room = room(&server, healthy).await;
    let manager = server.reconnection_manager().unwrap();
    for (player, room) in [(blocked, blocked_room), (healthy, healthy_room)] {
        manager
            .register_disconnection(player, room, false, None, 0)
            .await;
    }
    tokio::time::advance(Duration::from_secs(2)).await;
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    database.pause_next_removed_player_for_test();
    let blocked_observer = {
        let server = server.clone();
        tokio::spawn(async move {
            server
                .cleanup_expired_reconnection_candidates(vec![(blocked, blocked_room)])
                .await
        })
    };
    timeout(
        Duration::from_secs(1),
        database.wait_for_removed_player_for_test(),
    )
    .await
    .unwrap();
    blocked_observer.abort();
    assert!(blocked_observer.await.unwrap_err().is_cancelled());
    let observed = timeout(
        Duration::from_millis(500),
        server.cleanup_expired_reconnections(),
    )
    .await;
    let healthy_members = server
        .database
        .get_room_players(&healthy_room)
        .await
        .unwrap();
    database.release_removed_player_for_test();
    assert_eq!(
        observed.unwrap(),
        1,
        "the healthy expired neighbor must finish while the first removal waits"
    );
    assert!(
        healthy_members.is_empty(),
        "the healthy room must recover its seat without releasing the blocked removal"
    );
    timeout(
        Duration::from_secs(1),
        server.cleanup_expired_reconnections(),
    )
    .await
    .unwrap();
    assert!(!manager.has_pending_reconnection(&blocked).await);
    assert!(!manager.has_pending_reconnection(&healthy).await);
}

/// #1563: room discovery must remain owned after its observer reaches the deadline.
#[tokio::test(start_paused = true)]
async fn deleted_room_discovery_outlives_the_observation_deadline() {
    let server = server().await;
    let player = PlayerId::from_u128(1);
    let (sender, _receiver) = tokio::sync::mpsc::channel(8);
    server.connect_client(player, sender).await;
    let room_id = room(&server, player).await;
    server
        .connection_manager
        .assign_client_to_room(&player, room_id)
        .await;
    server.database.delete_room(&room_id).await.unwrap();
    let database = server
        .database
        .as_any()
        .downcast_ref::<crate::database::InMemoryDatabase>()
        .unwrap();
    let storage = database.hold_room_reads_for_test().await;
    assert_eq!(
        timeout(
            Duration::from_millis(500),
            server.cleanup_clients_in_missing_rooms()
        )
        .await
        .expect("a storage read must not hold the sweep observer"),
        0
    );
    assert!(server.connection_manager.has_client(&player));
    drop(storage);
    timeout(Duration::from_secs(1), async {
        while server.connection_manager.has_client(&player) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the admitted room check must finish teardown without another sweep");
}

/// #1568: slow room storage must leave reconnect registration and replay writable.
#[tokio::test(start_paused = true)]
async fn room_gc_storage_wait_does_not_block_reconnect_or_replay() {
    for (expired, occupied) in [(false, false), (true, false), (true, true)] {
        let server = server().await;
        let manager = server.reconnection_manager().unwrap();
        let player = PlayerId::new_v4();
        let room_id = room(&server, player).await;
        let database = server
            .database
            .as_any()
            .downcast_ref::<crate::database::InMemoryDatabase>()
            .unwrap();
        if !occupied {
            server
                .database
                .remove_player_from_room(&room_id, &player)
                .await
                .unwrap();
        }
        database
            .backdate_room_activity_for_test(&room_id, chrono::Duration::hours(1))
            .await;
        let storage = database.hold_room_writes_for_test().await;
        let mut cleanup = Box::pin(async {
            if expired {
                server
                    .cleanup_expired_rooms_protecting_reconnections(
                        chrono::Duration::zero(),
                        chrono::Duration::zero(),
                    )
                    .await
                    .unwrap();
            } else {
                server
                    .cleanup_empty_rooms_protecting_reconnections(chrono::Duration::zero())
                    .await
                    .unwrap();
            }
        });
        assert!(futures_util::poll!(&mut cleanup).is_pending());
        let mut registration =
            Box::pin(manager.register_disconnection(player, room_id, false, None, 0));
        assert!(
            futures_util::poll!(&mut registration).is_ready(),
            "room GC waiting for storage must not block reconnect registration: expired={expired}"
        );
        let message = ServerMessage::AuthorityChanged {
            authority_player: None,
            you_are_authority: false,
        };
        let mut replay = Box::pin(manager.record_room_event(&room_id, &message));
        assert!(
            futures_util::poll!(&mut replay).is_ready(),
            "room GC waiting for storage must not block live room publication"
        );
        drop(storage);
        cleanup.await;
        assert!(server.database.room_exists(&room_id).await.unwrap());
        assert!(manager.has_pending_reconnection(&player).await);
        assert_eq!(manager.replay_sample().await.replay_events, 1);
        manager.complete_reconnection(&player).await;
        if expired {
            let outcome = server
                .cleanup_expired_rooms_protecting_reconnections(
                    chrono::Duration::zero(),
                    chrono::Duration::zero(),
                )
                .await
                .unwrap();
            assert_eq!(outcome.empty_rooms_cleaned, usize::from(!occupied));
            assert_eq!(outcome.inactive_rooms_cleaned, usize::from(occupied));
        } else {
            assert_eq!(
                server
                    .cleanup_empty_rooms_protecting_reconnections(chrono::Duration::zero())
                    .await
                    .unwrap(),
                vec![room_id]
            );
        }
        assert!(!server.database.room_exists(&room_id).await.unwrap());
    }
}
