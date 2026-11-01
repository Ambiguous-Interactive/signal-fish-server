use crate::protocol::{PlayerId, ServerMessage};
use std::sync::Arc;

use super::{ClientLifecycle, EnhancedGameServer};

impl EnhancedGameServer {
    /// A transport control frame may resume after its socket loses this ID.
    /// Keep its liveness writes on the source connection's lifecycle gate.
    pub(crate) async fn record_transport_activity_from_lifecycle(
        &self,
        player_id: &PlayerId,
        source_lifecycle: Arc<ClientLifecycle>,
    ) {
        let _guard = source_lifecycle.lock().await;
        if source_lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &source_lifecycle)
        {
            return;
        }
        self.record_transport_activity(player_id).await;
    }

    /// Refresh liveness from transport-level WebSocket traffic (a client
    /// Ping, or a Pong answering our probe) without generating an
    /// application-level `ServerMessage::Pong` response.
    pub(crate) async fn record_transport_activity(&self, player_id: &PlayerId) {
        self.record_client_activity(player_id);
        self.maybe_update_last_seen(player_id).await;
    }

    /// Handle ping with coordination.
    ///
    /// Records the in-memory ping timestamp (always) for disconnect detection and
    /// replies `Pong`. The throttled `last_seen` + room-activity refresh is done
    /// once per inbound message by the router (`handle_client_message` →
    /// `maybe_update_last_seen`), so `Ping` needs no separate refresh here.
    ///
    /// The `Pong` reply charges the per-connection error-reply budget
    /// (issue #518): a Ping flood is the same one-write-one-reply
    /// amplification channel as malformed-frame refusals. An exhausted budget
    /// closes the connection with `4006 inbound_rate_limited` instead of
    /// answering again; liveness is still recorded for the frame that trips
    /// it, and the close itself follows.
    pub async fn handle_ping(&self, player_id: &PlayerId) {
        self.handle_ping_from_lifecycle(player_id, None).await;
    }

    pub(super) async fn handle_ping_from_lifecycle(
        &self,
        player_id: &PlayerId,
        source_lifecycle: Option<Arc<ClientLifecycle>>,
    ) {
        let Some(lifecycle) =
            source_lifecycle.or_else(|| self.connection_manager.client_lifecycle(player_id))
        else {
            return;
        };
        let _lifecycle_guard = lifecycle.lock().await;
        if lifecycle.player_id() != *player_id
            || !self
                .connection_manager
                .lifecycle_matches(player_id, &lifecycle)
        {
            return;
        }

        // Always record the ping in memory for disconnect detection
        self.connection_manager.record_ping(player_id);

        if !self.connection_manager.charge_error_reply(player_id).await {
            return;
        }

        let _ = self
            .message_coordinator
            .send_to_player(player_id, Arc::new(ServerMessage::Pong))
            .await;
    }

    /// Conditionally updates `last_seen` if the throttle threshold has elapsed.
    /// This reduces database writes while keeping local liveness timestamps current.
    /// A canceled or failed refresh permits the next message to retry.
    pub(super) async fn maybe_update_last_seen(&self, player_id: &PlayerId) {
        let threshold = self.config.heartbeat_throttle;

        let reservation = self
            .connection_manager
            .reserve_last_seen_update(player_id, threshold);

        if let Some(reservation) = reservation {
            self.metrics.increment_heartbeat_updates();
            let player_updated = match self.database.update_player_last_seen(player_id).await {
                Err(e) => {
                    tracing::warn!(%player_id, "Failed to update player last_seen: {}", e);
                    false
                }
                Ok(()) => true,
            };
            let occupied_room = self
                .connection_manager
                .get_client_room(player_id)
                .or_else(|| self.spectator_service.spectator_room(player_id));
            let room_updated = if let Some(room_id) = occupied_room {
                match self.database.update_room_activity(&room_id).await {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(%player_id, %room_id, "Failed to update room activity: {}", e);
                        false
                    }
                }
            } else {
                true
            };
            if player_updated && room_updated {
                reservation.complete();
            }
        } else {
            self.metrics.increment_heartbeat_skipped();
            tracing::trace!(%player_id, "Skipped last_seen update (throttled)");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::config::{
        CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
        TransportSecurityConfig, TurnConfig,
    };
    use crate::database::{DatabaseConfig, InMemoryDatabase};
    use crate::protocol::{ClientMessage, GameDataEncoding, ServerMessage};
    use crate::server::{EnhancedGameServer, ServerConfig};
    use bytes::Bytes;
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration as StdDuration;
    use tokio::sync::mpsc;
    use tokio::time::{advance, timeout, Duration};

    async fn create_test_server() -> Arc<EnhancedGameServer> {
        create_test_server_with_config(ServerConfig {
            max_connections_per_ip: 32,
            ..ServerConfig::default()
        })
        .await
    }

    async fn create_test_server_with_config(config: ServerConfig) -> Arc<EnhancedGameServer> {
        EnhancedGameServer::new(
            config,
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
        .expect("failed to construct test server")
    }

    #[tokio::test(start_paused = true)]
    async fn failed_heartbeat_persistence_retries_before_the_throttle_deadline() {
        for failed_update in ["player", "room"] {
            let server = create_test_server().await;
            let player = uuid::Uuid::new_v4();
            let (sender, _receiver) = mpsc::channel(16);
            server.connect_client(player, sender).await;
            server
                .handle_join_room(
                    &player,
                    "heartbeat-error".into(),
                    Some("ERROR1".into()),
                    "Host".into(),
                    Some(2),
                    Some(true),
                    None,
                    None,
                    None,
                )
                .await;
            let room_id = server.get_client_room(&player).await.unwrap();
            let database = server
                .database
                .as_any()
                .downcast_ref::<InMemoryDatabase>()
                .unwrap();
            database
                .backdate_room_activity_for_test(&room_id, chrono::Duration::hours(2))
                .await;
            match failed_update {
                "player" => database.fail_next_player_heartbeat_for_test(),
                "room" => database.fail_next_room_heartbeat_for_test(),
                _ => unreachable!(),
            }
            server.maybe_update_last_seen(&player).await;
            assert_eq!(server.metrics.heartbeat_updates.load(Ordering::Relaxed), 1);
            server.maybe_update_last_seen(&player).await;
            assert_eq!(
                server.metrics.heartbeat_updates.load(Ordering::Relaxed),
                2,
                "retry {failed_update} persistence immediately"
            );
            server.maybe_update_last_seen(&player).await;
            assert_eq!(server.metrics.heartbeat_updates.load(Ordering::Relaxed), 2);
            assert!(server
                .database
                .cleanup_expired_rooms(
                    chrono::Duration::minutes(5),
                    chrono::Duration::hours(1),
                    &HashSet::new(),
                )
                .await
                .unwrap()
                .is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn canceled_room_refresh_does_not_consume_the_heartbeat_window() {
        for previously_updated in [false, true] {
            let server = create_test_server().await;
            let player = uuid::Uuid::new_v4();
            let (sender, _receiver) = mpsc::channel(16);
            server.connect_client(player, sender).await;
            server
                .handle_join_room(
                    &player,
                    "heartbeat-retry".into(),
                    Some("RETRY1".into()),
                    "Host".into(),
                    Some(2),
                    Some(true),
                    None,
                    None,
                    None,
                )
                .await;
            let room_id = server.get_client_room(&player).await.unwrap();
            if previously_updated {
                server.maybe_update_last_seen(&player).await;
                advance(server.config.heartbeat_throttle).await;
            }
            let database = server
                .database
                .as_any()
                .downcast_ref::<InMemoryDatabase>()
                .unwrap();
            database
                .backdate_room_activity_for_test(&room_id, chrono::Duration::hours(2))
                .await;
            let before = server
                .database
                .get_room_by_id(&room_id)
                .await
                .unwrap()
                .unwrap()
                .last_activity;
            let updates_before = server.metrics.heartbeat_updates.load(Ordering::Relaxed);
            let held_rooms = database.hold_room_writes_for_test().await;
            let mut refresh = Box::pin(server.maybe_update_last_seen(&player));
            assert!(futures_util::poll!(&mut refresh).is_pending());
            assert_eq!(
                server.metrics.heartbeat_updates.load(Ordering::Relaxed),
                updates_before + 1
            );
            drop(refresh);
            drop(held_rooms);
            server.maybe_update_last_seen(&player).await;
            let after = server
                .database
                .get_room_by_id(&room_id)
                .await
                .unwrap()
                .unwrap();
            assert!(
                after.last_activity > before,
                "the next frame must retry canceled room activity: previous={previously_updated}"
            );
            assert_eq!(
                server.metrics.heartbeat_updates.load(Ordering::Relaxed),
                updates_before + 2
            );
            server.maybe_update_last_seen(&player).await;
            assert_eq!(
                server.metrics.heartbeat_updates.load(Ordering::Relaxed),
                updates_before + 2,
                "successful refresh starts the throttle window"
            );
            assert!(server
                .database
                .cleanup_expired_rooms(
                    chrono::Duration::minutes(5),
                    chrono::Duration::hours(1),
                    &HashSet::new(),
                )
                .await
                .unwrap()
                .is_empty());
        }
    }

    #[tokio::test(start_paused = true)]
    #[cfg_attr(miri, ignore)]
    async fn oversized_binary_payload_does_not_refresh_client_or_room_liveness() {
        let server = create_test_server_with_config(ServerConfig {
            heartbeat_throttle: StdDuration::ZERO,
            max_connections_per_ip: 32,
            max_message_size: 128,
            max_signal_bytes: 4,
            max_connection_info_bytes: 4,
            ..ServerConfig::default()
        })
        .await;
        let (sender, mut receiver) = mpsc::channel(4);
        let player_id = server
            .connection_manager
            .register_client(
                sender,
                crate::coordination::ConnectionCloseSignal::detached(),
                "127.0.0.1:45002".parse().unwrap(),
                server.instance_id,
            )
            .await
            .expect("client registration");
        let room = server
            .database
            .create_room(
                "invalid-binary-liveness".to_string(),
                Some("BINBAD".to_string()),
                2,
                true,
                player_id,
                "relay".to_string(),
                "test".to_string(),
                None,
            )
            .await
            .expect("room creation");
        server
            .connection_manager
            .assign_client_to_room(&player_id, room.id)
            .await;
        let database = server
            .database
            .as_any()
            .downcast_ref::<InMemoryDatabase>()
            .expect("test server uses in-memory storage");
        database
            .backdate_room_activity_for_test(&room.id, chrono::Duration::minutes(5))
            .await;
        let room_activity_before = server
            .database
            .get_room_by_id(&room.id)
            .await
            .expect("room lookup")
            .expect("room exists")
            .last_activity;

        advance(Duration::from_millis(25)).await;
        assert_eq!(
            server
                .connection_manager
                .collect_expired_clients(StdDuration::from_millis(5)),
            vec![player_id],
            "client should be stale before the rejected payload"
        );

        server
            .handle_game_data_binary(
                &player_id,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![b'1'; 129]),
            )
            .await;

        let response = receiver.recv().await.expect("oversize error is delivered");
        assert!(matches!(
            response.as_ref(),
            ServerMessage::Error {
                error_code: Some(crate::protocol::ErrorCode::MessageTooLarge),
                ..
            }
        ));
        assert_eq!(
            server
                .connection_manager
                .collect_expired_clients(StdDuration::from_millis(5)),
            vec![player_id],
            "a rejected payload must not keep an inactive client alive"
        );
        let room_activity_after = server
            .database
            .get_room_by_id(&room.id)
            .await
            .expect("room lookup")
            .expect("room exists")
            .last_activity;
        assert_eq!(
            room_activity_after, room_activity_before,
            "a rejected payload must not keep an inactive room alive"
        );
    }

    #[tokio::test(start_paused = true)]
    #[cfg_attr(miri, ignore)]
    async fn zero_throttle_still_suppresses_unknown_player_last_seen() {
        let server = create_test_server_with_config(ServerConfig {
            heartbeat_throttle: StdDuration::ZERO,
            max_connections_per_ip: 32,
            ..ServerConfig::default()
        })
        .await;
        let unknown_player_id = crate::protocol::PlayerId::new_v4();

        let updates_before = server.metrics.heartbeat_updates.load(Ordering::Relaxed);
        server.maybe_update_last_seen(&unknown_player_id).await;

        assert_eq!(
            updates_before,
            server.metrics.heartbeat_updates.load(Ordering::Relaxed),
            "a teardown-racing frame from an unregistered player must not fire the heartbeat update even with the throttle disabled"
        );
        assert_eq!(
            1,
            server.metrics.heartbeat_skipped.load(Ordering::Relaxed),
            "the suppressed update must count as skipped"
        );
    }

    // Deterministic under the paused-clock runtime: the activity reaper reads
    // `tokio::time::Instant`, so `advance(..)` drives `last_ping` staleness with
    // no wall-clock dependence (no `sleep`, so nothing to overshoot under load).
    #[tokio::test(start_paused = true)]
    #[cfg_attr(miri, ignore)]
    async fn handle_ping_resets_timeout_and_replies() {
        let server = create_test_server().await;
        let (sender, mut receiver) = mpsc::channel(4);
        let addr: SocketAddr = "127.0.0.1:45000".parse().unwrap();

        let player_id = server
            .connection_manager
            .register_client(
                sender,
                crate::coordination::ConnectionCloseSignal::detached(),
                addr,
                server.instance_id,
            )
            .await
            .expect("client registration");

        advance(Duration::from_millis(25)).await;
        let expired_before = server
            .connection_manager
            .collect_expired_clients(StdDuration::from_millis(5));
        assert_eq!(
            expired_before,
            vec![player_id],
            "player should look expired before ping"
        );

        server.handle_ping(&player_id).await;

        let msg = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("channel still open")
            .expect("message present");
        assert!(
            matches!(*msg, ServerMessage::Pong),
            "server responds with Pong"
        );

        let expired_after = server
            .connection_manager
            .collect_expired_clients(StdDuration::from_millis(5));
        assert!(
            expired_after.is_empty(),
            "ping refresh should remove player from expired set"
        );
    }

    #[tokio::test(start_paused = true)]
    #[cfg_attr(miri, ignore)]
    async fn transport_pong_resets_timeout_without_application_pong() {
        let server = create_test_server().await;
        let (sender, mut receiver) = mpsc::channel(4);
        let addr: SocketAddr = "127.0.0.1:45001".parse().unwrap();
        let player_id = server
            .connection_manager
            .register_client(
                sender,
                crate::coordination::ConnectionCloseSignal::detached(),
                addr,
                server.instance_id,
            )
            .await
            .expect("client registration");

        advance(Duration::from_millis(25)).await;
        assert_eq!(
            server
                .connection_manager
                .collect_expired_clients(StdDuration::from_millis(5)),
            vec![player_id],
            "player should look expired before transport Pong"
        );

        server.record_transport_activity(&player_id).await;

        assert!(
            server
                .connection_manager
                .collect_expired_clients(StdDuration::from_millis(5))
                .is_empty(),
            "transport Pong must refresh activity-reaper state"
        );
        match receiver.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("transport activity test channel disconnected unexpectedly")
            }
            Ok(message) => {
                panic!("transport Pong must not generate an application response, got {message:?}")
            }
        }
    }

    #[tokio::test]
    async fn test_spectator_activity_routes_refresh_room_through_throttle_issue_241() {
        #[derive(Clone, Copy, Debug)]
        enum ActivityRoute {
            Text,
            Binary,
            ApplicationPing,
            TransportPong,
        }

        for (index, route) in [
            ActivityRoute::Text,
            ActivityRoute::Binary,
            ActivityRoute::ApplicationPing,
            ActivityRoute::TransportPong,
        ]
        .into_iter()
        .enumerate()
        {
            let server = EnhancedGameServer::new(
                ServerConfig {
                    heartbeat_throttle: StdDuration::from_secs(60),
                    max_connections_per_ip: 32,
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
            .expect("failed to construct test server");
            let creator_id = crate::protocol::PlayerId::new_v4();
            let room = server
                .database
                .create_room(
                    format!("spectator-heartbeat-{index}"),
                    Some(format!("SPH{index:03}")),
                    2,
                    true,
                    creator_id,
                    "relay".to_string(),
                    "us-east-1".to_string(),
                    None,
                )
                .await
                .expect("create spectator room");
            let (sender, _receiver) = mpsc::channel(8);
            let spectator_id = server
                .connection_manager
                .register_client(
                    sender,
                    crate::coordination::ConnectionCloseSignal::detached(),
                    format!("127.0.0.1:{}", 45_100 + index)
                        .parse()
                        .expect("test address"),
                    server.instance_id,
                )
                .await
                .expect("client registration");
            server
                .spectator_service
                .join(
                    &spectator_id,
                    room.game_name.clone(),
                    room.code.clone(),
                    "Watcher".to_string(),
                )
                .await
                .expect("spectator joins");
            let before = server
                .database
                .get_room_by_id(&room.id)
                .await
                .expect("room lookup")
                .expect("room exists")
                .last_activity;

            tokio::time::sleep(Duration::from_millis(2)).await;
            match route {
                ActivityRoute::Text => {
                    server
                        .handle_client_message(&spectator_id, ClientMessage::PlayerReady)
                        .await;
                }
                ActivityRoute::Binary => {
                    server
                        .handle_game_data_binary(
                            &spectator_id,
                            GameDataEncoding::MessagePack,
                            Bytes::from_static(b"binary-liveness"),
                        )
                        .await;
                }
                ActivityRoute::ApplicationPing => {
                    server
                        .handle_client_message(&spectator_id, ClientMessage::Ping)
                        .await;
                }
                ActivityRoute::TransportPong => {
                    server.record_transport_activity(&spectator_id).await;
                }
            }

            let after = server
                .database
                .get_room_by_id(&room.id)
                .await
                .expect("room lookup")
                .expect("room exists")
                .last_activity;
            assert!(
                after > before,
                "{route:?} must refresh spectator room activity"
            );

            server.record_transport_activity(&spectator_id).await;
            let throttled = server
                .database
                .get_room_by_id(&room.id)
                .await
                .expect("room lookup")
                .expect("room exists")
                .last_activity;
            assert_eq!(
                throttled, after,
                "{route:?} must establish the nonzero throttle baseline"
            );
        }
    }

    #[tokio::test]
    async fn test_spectator_only_active_traffic_survives_inactive_gc_issue_241() {
        let server = EnhancedGameServer::new(
            ServerConfig {
                heartbeat_throttle: StdDuration::ZERO,
                max_connections_per_ip: 32,
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
        .expect("failed to construct test server");

        let mut room_ids = Vec::new();
        let mut spectator_ids = Vec::new();
        for index in 0..2 {
            let creator_id = crate::protocol::PlayerId::new_v4();
            let room = server
                .database
                .create_room(
                    format!("spectator-only-gc-{index}"),
                    Some(format!("SGC{index:03}")),
                    2,
                    true,
                    creator_id,
                    "relay".to_string(),
                    "us-east-1".to_string(),
                    None,
                )
                .await
                .expect("create spectator-only room");
            let (sender, _receiver) = mpsc::channel(8);
            let spectator_id = server
                .connection_manager
                .register_client(
                    sender,
                    crate::coordination::ConnectionCloseSignal::detached(),
                    format!("127.0.0.1:{}", 45_200 + index)
                        .parse()
                        .expect("test address"),
                    server.instance_id,
                )
                .await
                .expect("client registration");
            server
                .spectator_service
                .join(
                    &spectator_id,
                    room.game_name.clone(),
                    room.code.clone(),
                    "Watcher".to_string(),
                )
                .await
                .expect("spectator joins");
            server
                .database
                .remove_player_from_room(&room.id, &creator_id)
                .await
                .expect("creator removal succeeds");
            room_ids.push(room.id);
            spectator_ids.push(spectator_id);
        }

        let database = server
            .database
            .as_any()
            .downcast_ref::<InMemoryDatabase>()
            .expect("test server uses the in-memory database");
        for room_id in &room_ids {
            database
                .backdate_room_activity_for_test(room_id, chrono::Duration::hours(2))
                .await;
        }

        server
            .handle_client_message(&spectator_ids[0], ClientMessage::Ping)
            .await;

        let outcome = server
            .database
            .cleanup_expired_rooms(
                chrono::Duration::zero(),
                chrono::Duration::hours(1),
                &HashSet::new(),
            )
            .await
            .expect("inactive cleanup succeeds");

        assert_eq!(outcome.inactive_rooms_cleaned, 1);
        assert!(server
            .database
            .get_room_by_id(&room_ids[0])
            .await
            .expect("active room lookup succeeds")
            .is_some());
        assert!(server
            .database
            .get_room_by_id(&room_ids[1])
            .await
            .expect("control room lookup succeeds")
            .is_none());
    }
}
