use super::game_data::one_shot_message_builder;
use crate::protocol::{DeliveryClass, ErrorCode, GameDataEncoding, PlayerId, ServerMessage};
use bytes::Bytes;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn one_shot_message_builder_consumes_builder_once_and_defends_against_repeat_calls() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_builder = Arc::clone(&calls);
    let mut builder = one_shot_message_builder(move || {
        calls_for_builder.fetch_add(1, Ordering::Relaxed);
        Some(ServerMessage::Pong)
    });

    assert!(matches!(builder(), Some(ServerMessage::Pong)));
    assert!(
        builder().is_none(),
        "a repeated call must cancel defensively"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn one_shot_message_builder_preserves_cancellation_and_drops_called_capture() {
    let drops = Arc::new(AtomicUsize::new(0));
    let probe = DropProbe(Arc::clone(&drops));
    let mut builder = one_shot_message_builder(move || {
        drop(probe);
        None
    });

    assert!(builder().is_none(), "a missing stamp must cancel the relay");
    assert!(builder().is_none(), "cancellation remains one-shot");
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn one_shot_message_builder_drops_uncalled_builder_capture() {
    let drops = Arc::new(AtomicUsize::new(0));
    let probe = DropProbe(Arc::clone(&drops));
    let builder = one_shot_message_builder(move || {
        let _probe = probe;
        Some(ServerMessage::Pong)
    });

    drop(builder);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

// ---------------------------------------------------------------------------
// Handler honesty pins (#396 sweep): every rejected game-data write must be
// observable to its sender instead of vanishing.
// ---------------------------------------------------------------------------

mod handler_honesty {
    use super::*;
    use crate::config::{
        CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
        TransportSecurityConfig, TurnConfig,
    };
    use crate::coordination::ConnectionCloseSignal;
    use crate::database::DatabaseConfig;
    use crate::protocol::ConnectionInfo;
    use crate::server::EnhancedGameServer;
    use crate::server::ServerConfig;
    use std::net::SocketAddr;

    static PORT: AtomicU64 = AtomicU64::new(59_400);

    fn next_addr() -> SocketAddr {
        let port = PORT.fetch_add(1, Ordering::Relaxed);
        format!("127.0.0.1:{port}").parse().expect("valid addr")
    }

    async fn create_test_server() -> Arc<EnhancedGameServer> {
        EnhancedGameServer::new(
            ServerConfig::default(),
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

    /// Register one client with the default (pre-v3) negotiated protocol.
    async fn register_client(
        server: &EnhancedGameServer,
    ) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
        let (sender, receiver) = mpsc::channel(16);
        let player_id = server
            .connection_manager
            .register_client(
                sender,
                ConnectionCloseSignal::detached(),
                next_addr(),
                server.instance_id,
            )
            .await
            .expect("client registration succeeds");
        (player_id, receiver)
    }

    async fn recv(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Arc<ServerMessage> {
        timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("channel still open")
            .expect("message present")
    }

    /// Receive nothing within a short window: deterministically proves no
    /// message was enqueued because every operation before this is awaited.
    async fn assert_silent(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) {
        match timeout(Duration::from_millis(100), receiver.recv()).await {
            Err(_) => {}
            Ok(Some(message)) => panic!("expected silence, got {message:?}"),
            Ok(None) => panic!("channel closed while checking for silence"),
        }
    }

    fn expect_error(message: Arc<ServerMessage>, expected: ErrorCode) {
        match &*message {
            ServerMessage::Error {
                message,
                error_code,
            } => {
                assert_eq!(
                    *error_code,
                    Some(expected),
                    "error code mismatch: {message}"
                );
            }
            other => panic!("expected an Error frame, got {other:?}"),
        }
    }

    /// A registered client that never joined a room previously had its JSON
    /// game data vanish without any response: impossible to distinguish from
    /// "relayed". Every sibling surface (ProvideConnectionInfo, Signal,
    /// Authority) replies NOT_IN_ROOM; the data lanes now match (#396 sweep).
    #[tokio::test]
    async fn roomless_text_and_binary_game_data_reply_not_in_room() {
        let server = create_test_server().await;
        let (player, mut rx) = register_client(&server).await;

        server
            .handle_game_data(&player, serde_json::json!("payload"), None, None)
            .await;
        expect_error(recv(&mut rx).await, ErrorCode::NotInRoom);

        server
            .handle_game_data_binary(
                &player,
                GameDataEncoding::MessagePack,
                Bytes::from_static(b"payload"),
            )
            .await;
        expect_error(recv(&mut rx).await, ErrorCode::NotInRoom);
    }

    /// A pre-v3 sender supplying delivery metadata trips the legacy-lane
    /// guard (`class.is_none() && key.is_none()`); before this pin nothing
    /// asserted it, so reordering or deleting the arm survived silently.
    #[tokio::test]
    async fn pre_v3_sender_with_delivery_metadata_rejects_invalid_delivery_class() {
        let server = create_test_server().await;
        let (player, mut rx) = register_client(&server).await;
        assert!(
            !server.client_supports_v3(&player),
            "default registration negotiates below v3"
        );

        // The gate for pre-v3 senders is total metadata omission; drive both
        // shapes through it. `(Some(Latest), Some(7))` is even the v3-legal
        // pairing — below v3 it must still be rejected outright.
        server
            .handle_game_data(
                &player,
                serde_json::json!(1),
                Some(DeliveryClass::Latest),
                Some(7),
            )
            .await;
        expect_error(recv(&mut rx).await, ErrorCode::InvalidDeliveryClass);
        server
            .handle_game_data(
                &player,
                serde_json::json!(1),
                Some(DeliveryClass::Reliable),
                None,
            )
            .await;
        expect_error(recv(&mut rx).await, ErrorCode::InvalidDeliveryClass);
    }

    /// The legacy-metadata store reports success only through absence of
    /// error. When the durable membership row vanished between room
    /// resolution and the write (teardown race), the update used to land as
    /// `Ok(false)` and pass silently — indistinguishable from stored, so the
    /// player's peers would boot with stale/missing handoff data. It must
    /// surface INTERNAL_ERROR like any other failed persistence (#396).
    #[tokio::test]
    async fn provide_connection_info_on_vanished_membership_surfaces_internal_error() {
        let server = create_test_server().await;
        let (player, mut rx) = register_client(&server).await;

        server
            .handle_join_room(
                &player,
                "honesty-sweep".to_string(),
                Some("HNS001".to_string()),
                "player".to_string(),
                None,
                None,
                None,
            )
            .await;
        let room_id = server
            .get_client_room(&player)
            .await
            .expect("join routes the client into a room");

        // Join traffic is fully enqueued once `handle_join_room` returns, so
        // draining what is present is deterministic; every popped frame must
        // be an expected join/roster bookkeeping variant.
        loop {
            match rx.try_recv() {
                Ok(message) => match message.as_ref() {
                    ServerMessage::RoomJoined(_)
                    | ServerMessage::PlayerJoined { .. }
                    | ServerMessage::LobbyStateChanged { .. } => {}
                    other => panic!("unexpected pre-scenario frame {other:?}"),
                },
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(err) => panic!("channel closed while draining join traffic: {err}"),
            }
        }

        let vanished_row = server
            .database()
            .remove_player_from_room(&room_id, &player)
            .await
            .expect("membership row exists before removal");
        assert!(vanished_row.is_some(), "joined player owns a roster row");

        let info = ConnectionInfo::Direct {
            host: "127.0.0.1".to_string(),
            port: 7777,
        };
        server
            .handle_provide_connection_info(&player, info.clone())
            .await;
        expect_error(recv(&mut rx).await, ErrorCode::InternalError);

        // Healthy sibling: intact membership stores silently, with the value
        // observable in the roster row (no false-positive from the fix).
        server
            .database()
            .add_player_to_room(
                &room_id,
                crate::protocol::PlayerInfo {
                    id: player,
                    name: "player".to_string(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: chrono::Utc::now(),
                    connection_info: None,
                    epoch: None,
                    seq: None,
                    region_id: "region-a".to_string(),
                },
            )
            .await
            .expect("row restore succeeds");
        server.handle_provide_connection_info(&player, info).await;
        assert_silent(&mut rx).await;
        let players = server
            .database()
            .get_room_players(&room_id)
            .await
            .expect("roster readable");
        let stored = players
            .iter()
            .find(|p| p.id == player)
            .and_then(|p| p.connection_info.as_ref());
        let matches_direct = match stored {
            Some(ConnectionInfo::Direct { host, port }) => host == "127.0.0.1" && *port == 7777,
            _ => false,
        };
        assert!(
            matches_direct,
            "healthy write lands and persists: {stored:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Peer-metadata admission cap (issue #524) and the sender-side relay byte
// budget (issue #519).
// ---------------------------------------------------------------------------

mod admission_and_budget {
    use super::*;
    use crate::config::{
        CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
        TransportSecurityConfig, TurnConfig,
    };
    use crate::coordination::ConnectionCloseSignal;
    use crate::database::DatabaseConfig;
    use crate::protocol::{ConnectionInfo, RoomId};
    use crate::server::{EnhancedGameServer, ServerConfig};
    use std::net::SocketAddr;

    static PORT: AtomicU64 = AtomicU64::new(59_600);

    fn next_addr() -> SocketAddr {
        let port = PORT.fetch_add(1, Ordering::Relaxed);
        format!("127.0.0.1:{port}").parse().expect("valid addr")
    }

    /// Build a test server whose `ServerConfig` is visible to the caller's
    /// mutation closure (e.g. `config.rate_limit_config.max_relay_bytes = …`).
    async fn server_with_config(mutate: impl FnOnce(&mut ServerConfig)) -> Arc<EnhancedGameServer> {
        let mut config = ServerConfig::default();
        mutate(&mut config);
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

    async fn register_client(
        server: &EnhancedGameServer,
    ) -> (PlayerId, mpsc::Receiver<Arc<ServerMessage>>) {
        let (sender, receiver) = mpsc::channel(16);
        let player_id = server
            .connection_manager
            .register_client(
                sender,
                ConnectionCloseSignal::detached(),
                next_addr(),
                server.instance_id,
            )
            .await
            .expect("client registration succeeds");
        (player_id, receiver)
    }

    async fn recv(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Arc<ServerMessage> {
        timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("channel still open")
            .expect("message present")
    }

    fn expect_error(message: Arc<ServerMessage>, expected: ErrorCode) {
        match &*message {
            ServerMessage::Error {
                message,
                error_code,
            } => {
                assert_eq!(
                    *error_code,
                    Some(expected),
                    "error code mismatch: {message}"
                );
            }
            other => panic!("expected an Error frame, got {other:?}"),
        }
    }

    /// Like [`recv`], with a generous ceiling for tests whose real-time
    /// behavior must stay deterministic under Miri's 10-50x interpretation
    /// slowdown (the shared 1 s ceiling is tuned for native latency).
    async fn recv_relaxed(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Arc<ServerMessage> {
        timeout(Duration::from_secs(60), receiver.recv())
            .await
            .expect("channel still open")
            .expect("message present")
    }

    /// Join `players` into one shared room, draining each player's join
    /// bookkeeping frames so later receive assertions start from a quiet
    /// channel.
    async fn join_shared_room(
        server: &Arc<EnhancedGameServer>,
        mut players: Vec<(&PlayerId, &mut mpsc::Receiver<Arc<ServerMessage>>)>,
    ) -> RoomId {
        // Six-character code (the default `protocol.room_code_length`).
        let room_code = format!("P{:05x}", next_addr().port());
        let mut room_id = None;
        for (index, (player, _)) in players.iter().enumerate() {
            server
                .handle_join_room(
                    player,
                    "admission-budget".to_string(),
                    Some(room_code.clone()),
                    format!("player-{index}"),
                    None,
                    None,
                    None,
                )
                .await;
            if room_id.is_none() {
                room_id = server.get_client_room(player).await;
            }
        }
        for (_, rx) in players.iter_mut() {
            loop {
                match rx.try_recv() {
                    Ok(message) => match message.as_ref() {
                        ServerMessage::RoomJoined(_)
                        | ServerMessage::PlayerJoined { .. }
                        | ServerMessage::LobbyStateChanged { .. }
                        | ServerMessage::SpectatorJoined { .. } => {}
                        other => panic!("unexpected join bookkeeping frame {other:?}"),
                    },
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(err) => panic!("channel closed while draining join traffic: {err}"),
                }
            }
        }
        room_id.expect("join routes clients into a room")
    }

    /// `ConnectionInfo` entries above `security.max_connection_info_bytes`
    /// (configured here to 16 bytes) are rejected with `MESSAGE_TOO_LARGE`
    /// and never stored, so a full roster of oversized entries can no longer
    /// push `GameStarting.peer_connections` past the outbound cap and close
    /// every recipient (#524 eviction primitive).
    #[tokio::test]
    async fn oversized_connection_info_is_rejected_and_not_stored() {
        let server = server_with_config(|config| {
            // 64 bytes: the test's oversized Direct entry (~74 bytes of
            // canonical JSON) must trip the cap while the healthy small
            // entry (~37 bytes) stays under it.
            config.max_connection_info_bytes = 64;
        })
        .await;
        let (player, mut rx) = register_client(&server).await;
        server
            .handle_join_room(
                &player,
                "admission-budget".to_string(),
                Some("OCI001".to_string()),
                "player".to_string(),
                None,
                None,
                None,
            )
            .await;
        let room_id = server
            .get_client_room(&player)
            .await
            .expect("join routes the client into a room");
        loop {
            match rx.try_recv() {
                Ok(message) => match message.as_ref() {
                    ServerMessage::RoomJoined(_)
                    | ServerMessage::PlayerJoined { .. }
                    | ServerMessage::LobbyStateChanged { .. } => {}
                    other => panic!("unexpected join bookkeeping frame {other:?}"),
                },
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(err) => panic!("channel closed while draining join traffic: {err}"),
            }
        }

        let oversized = ConnectionInfo::Direct {
            host: "host-name-longer-than-sixteen-bytes".to_string(),
            port: 7777,
        };
        server
            .handle_provide_connection_info(&player, oversized.clone())
            .await;
        expect_error(recv(&mut rx).await, ErrorCode::MessageTooLarge);

        let players = server
            .database()
            .get_room_players(&room_id)
            .await
            .expect("roster readable");
        let stored = players
            .iter()
            .find(|p| p.id == player)
            .and_then(|p| p.connection_info.as_ref());
        assert!(
            stored.is_none(),
            "an oversized entry must not be stored: {stored:?}"
        );

        // A small entry below the cap still stores silently (the healthy
        // path keeps working).
        server
            .handle_provide_connection_info(
                &player,
                ConnectionInfo::Direct {
                    host: "h".to_string(),
                    port: 1,
                },
            )
            .await;
        match timeout(Duration::from_millis(100), rx.recv()).await {
            Err(_) => {}
            Ok(Some(message)) => panic!("expected silence, got {message:?}"),
            Ok(None) => panic!("channel closed while checking for silence"),
        }
    }

    /// The relay byte budget (#519) rejects an over-budget binary frame with
    /// `RATE_LIMIT_EXCEEDED`, relays nothing to the room-mate, and attributes
    /// both the rejection and the accepted bytes. After the fixed window
    /// elapses the sender can relay again.
    #[tokio::test]
    async fn relay_byte_budget_rejects_over_budget_frames_and_recovers() {
        // 64 KiB default window is too big for a snappy test; use 1000 bytes
        // and a 100 ms window.
        let server = server_with_config(|config| {
            config.rate_limit_config.max_relay_bytes = 1000;
            config.rate_limit_config.time_window = Duration::from_millis(100);
        })
        .await;
        let (sender, mut sender_rx) = register_client(&server).await;
        let (peer, mut peer_rx) = register_client(&server).await;
        join_shared_room(
            &server,
            vec![(&sender, &mut sender_rx), (&peer, &mut peer_rx)],
        )
        .await;

        // First frame fits the budget and is relayed to the peer only.
        server
            .handle_game_data_binary(
                &sender,
                GameDataEncoding::MessagePack,
                Bytes::from_static(b"within-budget"),
            )
            .await;
        match recv(&mut peer_rx).await.as_ref() {
            ServerMessage::GameDataBinary { payload, .. } => {
                assert_eq!(payload.as_ref(), b"within-budget");
            }
            other => panic!("expected relayed binary game data, got {other:?}"),
        }

        // A frame that would exceed the remaining budget is rejected and
        // relays nothing.
        let fat_frame = vec![0u8; 2000];
        server
            .handle_game_data_binary(
                &sender,
                GameDataEncoding::MessagePack,
                Bytes::from(fat_frame.clone()),
            )
            .await;
        expect_error(recv(&mut sender_rx).await, ErrorCode::RateLimitExceeded);
        match timeout(Duration::from_millis(100), peer_rx.recv()).await {
            Err(_) => {}
            Ok(Some(message)) => panic!("over-budget frame must not relay, got {message:?}"),
            Ok(None) => panic!("peer channel closed while checking for silence"),
        }

        let snapshot = server.metrics().snapshot().await;
        assert_eq!(
            snapshot.rate_limiting.relay_bandwidth_rejections, 1,
            "the over-budget frame is attributed exactly once"
        );
        assert_eq!(
            snapshot.players.relay_bytes_total,
            bytes::Bytes::from_static(b"within-budget").len() as u64,
            "only admitted bytes are accounted"
        );

        // The rejected frame also did not consume budget: after the window
        // resets the sender's full budget (1000 bytes) is available again —
        // exactly enough for the recovery frame below.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let recovery_frame = vec![0u8; 1000];
        server
            .handle_game_data_binary(
                &sender,
                GameDataEncoding::MessagePack,
                Bytes::from(recovery_frame),
            )
            .await;
        match recv(&mut peer_rx).await.as_ref() {
            ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 1000),
            other => panic!("expected relayed binary game data after window reset, got {other:?}"),
        }
    }

    /// The text lane shares the single relay byte budget: over-budget JSON
    /// game data is rejected with `RATE_LIMIT_EXCEEDED` without relaying.
    #[tokio::test]
    async fn relay_byte_budget_covers_the_text_lane() {
        let server = server_with_config(|config| {
            config.rate_limit_config.max_relay_bytes = 1;
            config.rate_limit_config.time_window = Duration::from_millis(100);
        })
        .await;
        let (sender, mut sender_rx) = register_client(&server).await;
        let (peer, mut peer_rx) = register_client(&server).await;
        join_shared_room(
            &server,
            vec![(&sender, &mut sender_rx), (&peer, &mut peer_rx)],
        )
        .await;

        server
            .handle_game_data(
                &sender,
                serde_json::json!("any payload consumes the budget"),
                None,
                None,
            )
            .await;
        expect_error(recv(&mut sender_rx).await, ErrorCode::RateLimitExceeded);
        match timeout(Duration::from_millis(100), peer_rx.recv()).await {
            Err(_) => {}
            Ok(Some(message)) => panic!("over-budget frame must not relay, got {message:?}"),
            Ok(None) => panic!("peer channel closed while checking for silence"),
        }
    }

    /// The per-room aggregate ceiling (#530) bounds many individually
    /// under-budget senders: once their joint submit volume exhausts the
    /// room's window, further frames are rejected with `RATE_LIMIT_EXCEEDED`
    /// on their own lane (`relay_room_bandwidth_rejections`), relay nothing,
    /// and leave the sender-budget charge the frame committed. Window-reset
    /// recovery for the room ceiling is pinned deterministically (paused
    /// time) at the limiter level, so this handler test uses a 60 s window —
    /// effectively frozen for its duration — and generous receive ceilings:
    /// Miri interprets this suite 10-50x slower than native, and a real-time
    /// window plus short receive deadlines would be flaky under it.
    #[tokio::test]
    async fn room_relay_byte_budget_bounds_joint_senders() {
        // Each sender's own budget (1000 bytes) stays far above every frame
        // used here; only the room ceiling (1200 bytes) can reject. The
        // allowlisted tiered context pins the attribution ordering: admitted
        // bytes are attributed per app only AFTER the room ceiling admitted
        // the frame (issue #530).
        let server = server_with_config(|config| {
            config.app_id_allowlist_enabled = true;
            config.rate_limit_config.max_relay_bytes = 1000;
            config.rate_limit_config.max_room_relay_bytes = 1200;
            config.rate_limit_config.time_window = Duration::from_secs(60);
        })
        .await;
        let (sender_a, mut sender_a_rx) = register_client(&server).await;
        let (sender_b, mut sender_b_rx) = register_client(&server).await;
        let (peer, mut peer_rx) = register_client(&server).await;
        let app_id = uuid::Uuid::new_v4();
        let tiered_context = crate::auth::middleware::AppContext {
            id: app_id,
            name: "Tiered".to_string(),
            organization: None,
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
            max_relay_bytes: None,
            rate_limits: crate::auth::middleware::RateLimits {
                per_minute: 1000,
                per_hour: 60_000,
                per_day: 1_440_000,
            },
        };
        for player in [&sender_a, &sender_b, &peer] {
            server.set_client_app_context(player, tiered_context.clone());
        }
        join_shared_room(
            &server,
            vec![
                (&sender_a, &mut sender_a_rx),
                (&sender_b, &mut sender_b_rx),
                (&peer, &mut peer_rx),
            ],
        )
        .await;

        // Joint submits fill the room's 1200-byte window (600 + 600). The
        // fan-out excludes only the sending connection, so each member
        // receives the other senders' frames.
        for sender in [&sender_a, &sender_b] {
            server
                .handle_game_data_binary(
                    sender,
                    GameDataEncoding::MessagePack,
                    Bytes::from(vec![0u8; 600]),
                )
                .await;
        }
        for rx in [&mut sender_a_rx, &mut sender_b_rx] {
            match recv_relaxed(rx).await.as_ref() {
                ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 600),
                other => panic!("expected the peer sender's relayed frame, got {other:?}"),
            }
        }
        for _ in 0..2 {
            match recv_relaxed(&mut peer_rx).await.as_ref() {
                ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 600),
                other => panic!("expected the relayed frame, got {other:?}"),
            }
        }

        // The next frame fits every sender budget but exhausts the room
        // ceiling: rejected, unrelayed, and attributed on its own lane.
        server
            .handle_game_data_binary(
                &sender_a,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 1]),
            )
            .await;
        expect_error(
            recv_relaxed(&mut sender_a_rx).await,
            ErrorCode::RateLimitExceeded,
        );
        let snapshot = server.metrics().snapshot().await;
        assert_eq!(
            snapshot.rate_limiting.relay_room_bandwidth_rejections, 1,
            "the room-ceiling rejection is attributed on its own lane"
        );
        assert_eq!(
            snapshot.rate_limiting.relay_bandwidth_rejections, 0,
            "no sender budget rejected anything"
        );
        assert_eq!(
            snapshot.players.relay_bytes_total, 1200,
            "only admitted bytes are accounted"
        );
        assert_eq!(
            snapshot.players.app_relay_bytes.get(&app_id),
            Some(&1200),
            "per-app attribution runs after the room ceiling admitted each frame: \
             the rejected 1-byte submit is attributed nowhere"
        );
        // The room-ceiling rejection retains the sender charge the frame
        // already committed: sender A's window shows 600 admitted + the
        // 1-byte rejected submit.
        assert_eq!(
            server
                .rate_limiter
                .get_player_stats(&sender_a)
                .await
                .expect("sender A stats entry exists")
                .relay_bytes,
            601,
            "a room-ceiling rejection retains the sender-budget charge"
        );
    }

    /// Per-app relay budget override (#530, handler level): an allowlisted
    /// app's tighter override bounds each of that app's senders below the
    /// server-wide budget, admitted bytes are attributed to the app (and
    /// only admitted bytes), and a sender without an override keeps the
    /// global budget.
    #[tokio::test]
    async fn per_app_relay_budget_override_bounds_the_apps_senders_and_attracts_their_bytes() {
        use crate::auth::middleware::{AppContext, RateLimits};

        let app_id = uuid::Uuid::new_v4();
        // Global sender budget 1000; the app override tightens it to 300.
        let server = server_with_config(|config| {
            config.app_id_allowlist_enabled = true;
            config.rate_limit_config.max_relay_bytes = 1000;
            config.rate_limit_config.max_room_relay_bytes = 100_000;
            config.rate_limit_config.time_window = Duration::from_secs(60);
        })
        .await;
        let tiered_context = AppContext {
            id: app_id,
            name: "Trial Tier".to_string(),
            organization: None,
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
            max_relay_bytes: Some(300),
            rate_limits: RateLimits {
                per_minute: 1000,
                per_hour: 60_000,
                per_day: 1_440_000,
            },
        };
        let default_context = AppContext {
            id: uuid::Uuid::new_v4(),
            name: "Untiered".to_string(),
            organization: None,
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
            max_relay_bytes: None,
            rate_limits: RateLimits {
                per_minute: 1000,
                per_hour: 60_000,
                per_day: 1_440_000,
            },
        };

        let (sender_a, mut sender_a_rx) = register_client(&server).await;
        let (sender_b, mut sender_b_rx) = register_client(&server).await;
        let (untiered, mut untiered_rx) = register_client(&server).await;
        let (peer, mut peer_rx) = register_client(&server).await;
        for (player, context) in [
            (&sender_a, &tiered_context),
            (&sender_b, &tiered_context),
            (&peer, &tiered_context),
            (&untiered, &default_context),
        ] {
            server.set_client_app_context(player, context.clone());
        }
        // App-scoped room admission (#520) keeps same-app members together,
        // so the untiered sender exercises its global budget from its own
        // room; sender budgets are per-sender and room-independent.
        join_shared_room(
            &server,
            vec![
                (&sender_a, &mut sender_a_rx),
                (&sender_b, &mut sender_b_rx),
                (&peer, &mut peer_rx),
            ],
        )
        .await;
        join_shared_room(&server, vec![(&untiered, &mut untiered_rx)]).await;

        // The override bounds each of the app's senders individually at 300
        // bytes (it replaces the budget value, it is not a shared pool):
        // sender A fills his window exactly, his 1-byte follow-up is
        // rejected, and sender B gets his own 300-byte window under the same
        // app identity.
        server
            .handle_game_data_binary(
                &sender_a,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 300]),
            )
            .await;
        server
            .handle_game_data_binary(
                &sender_a,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 1]),
            )
            .await;
        server
            .handle_game_data_binary(
                &sender_b,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 300]),
            )
            .await;
        server
            .handle_game_data_binary(
                &sender_b,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 1]),
            )
            .await;
        // Sender A: B's relayed frame, then the error for his own rejected
        // follow-up. Sender B: A's relayed frame, then the error for his own
        // rejected follow-up. The peer sees both admitted frames.
        expect_error(
            recv_relaxed(&mut sender_a_rx).await,
            ErrorCode::RateLimitExceeded,
        );
        match recv_relaxed(&mut sender_a_rx).await.as_ref() {
            ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 300),
            other => panic!("expected sender B's relayed frame, got {other:?}"),
        }
        match recv_relaxed(&mut sender_b_rx).await.as_ref() {
            ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 300),
            other => panic!("expected sender A's relayed frame, got {other:?}"),
        }
        expect_error(
            recv_relaxed(&mut sender_b_rx).await,
            ErrorCode::RateLimitExceeded,
        );
        match recv_relaxed(&mut peer_rx).await.as_ref() {
            ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 300),
            other => panic!("expected a relayed frame, got {other:?}"),
        }
        match recv_relaxed(&mut peer_rx).await.as_ref() {
            ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 300),
            other => panic!("expected a relayed frame, got {other:?}"),
        }

        let snapshot = server.metrics().snapshot().await;
        assert_eq!(
            snapshot.rate_limiting.relay_bandwidth_rejections, 2,
            "each tiered sender's over-override frame lands on the sender-budget lane"
        );
        assert_eq!(
            snapshot.players.relay_bytes_total, 600,
            "only admitted bytes are accounted server-wide"
        );
        assert_eq!(
            snapshot.players.app_relay_bytes.get(&app_id),
            Some(&600),
            "admitted bytes are attributed to the sending application"
        );
        assert_eq!(
            snapshot.players.app_relay_bytes.len(),
            1,
            "only the tiered app carries attributed bytes"
        );

        // The untiered sender keeps the global 1000-byte budget: 1000
        // admitted, then rejected.
        server
            .handle_game_data_binary(
                &untiered,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 1000]),
            )
            .await;
        server
            .handle_game_data_binary(
                &untiered,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 1]),
            )
            .await;
        expect_error(
            recv_relaxed(&mut untiered_rx).await,
            ErrorCode::RateLimitExceeded,
        );
        let snapshot = server.metrics().snapshot().await;
        assert_eq!(
            snapshot.players.app_relay_bytes.get(&app_id),
            Some(&600),
            "a rejected frame is never attributed to an app"
        );
        assert_eq!(
            snapshot.players.app_relay_bytes.len(),
            2,
            "every allowlisted app is attributed (billing needs per-tenant totals, \
             override or not): the untiered sender's admitted 1000 bytes count \
             under its own app against the global budget"
        );
        assert_eq!(snapshot.rate_limiting.relay_bandwidth_rejections, 3);
        assert_eq!(snapshot.players.relay_bytes_total, 1600);
    }

    /// Open-mode application identity is a client-chosen label: relay
    /// budgets and attribution must ignore any context payload, so a
    /// spoofed override can neither raise the budget nor fabricate a
    /// per-app billing series (#530).
    #[tokio::test]
    async fn open_mode_ignores_app_relay_policy_for_enforcement_and_attribution() {
        use crate::auth::middleware::{AppContext, RateLimits};

        let spoofed = uuid::Uuid::new_v4();
        let server = server_with_config(|config| {
            config.app_id_allowlist_enabled = false;
            config.rate_limit_config.max_relay_bytes = 1000;
            config.rate_limit_config.max_room_relay_bytes = 100_000;
            config.rate_limit_config.time_window = Duration::from_secs(60);
        })
        .await;
        let (sender, mut sender_rx) = register_client(&server).await;
        let (peer, mut peer_rx) = register_client(&server).await;
        let spoofed_context = AppContext {
            id: spoofed,
            name: "spoofed-tier".to_string(),
            organization: None,
            max_rooms: None,
            max_players_per_room: None,
            rate_limit_per_minute: None,
            // A client-chosen context must not be able to raise the budget;
            // the wire path never sets one, and this test pins that even a
            // hand-built one is ignored in open mode.
            max_relay_bytes: Some(u64::MAX),
            rate_limits: RateLimits {
                per_minute: 1000,
                per_hour: 60_000,
                per_day: 1_440_000,
            },
        };
        // Both members carry the same spoofed identity: app-scoped room
        // admission (#520) otherwise refuses a cross-app join into an owned
        // room.
        server.set_client_app_context(&sender, spoofed_context.clone());
        server.set_client_app_context(&peer, spoofed_context);
        join_shared_room(
            &server,
            vec![(&sender, &mut sender_rx), (&peer, &mut peer_rx)],
        )
        .await;

        // 1000 admitted under the global budget; a spoofed u64::MAX override
        // would have admitted far more.
        server
            .handle_game_data_binary(
                &sender,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 1000]),
            )
            .await;
        server
            .handle_game_data_binary(
                &sender,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 1]),
            )
            .await;
        expect_error(
            recv_relaxed(&mut sender_rx).await,
            ErrorCode::RateLimitExceeded,
        );
        match recv_relaxed(&mut peer_rx).await.as_ref() {
            ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 1000),
            other => panic!("expected the first relayed frame, got {other:?}"),
        }

        let snapshot = server.metrics().snapshot().await;
        assert_eq!(snapshot.players.relay_bytes_total, 1000);
        assert!(
            snapshot.players.app_relay_bytes.is_empty(),
            "open-mode relays are never attributed per app: the label is spoofable"
        );
    }
}
