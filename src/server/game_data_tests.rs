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
    /// and the room recovers after the window resets — while a second room
    /// keeps its own ceiling.
    #[tokio::test]
    async fn room_relay_byte_budget_bounds_joint_senders_and_recovers() {
        // Each sender's own budget (1000 bytes) stays far above every frame
        // used here; only the room ceiling (1200 bytes) can reject.
        let server = server_with_config(|config| {
            config.rate_limit_config.max_relay_bytes = 1000;
            config.rate_limit_config.max_room_relay_bytes = 1200;
            config.rate_limit_config.time_window = Duration::from_millis(100);
        })
        .await;
        let (sender_a, mut sender_a_rx) = register_client(&server).await;
        let (sender_b, mut sender_b_rx) = register_client(&server).await;
        let (peer, mut peer_rx) = register_client(&server).await;
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
            match recv(rx).await.as_ref() {
                ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 600),
                other => panic!("expected the peer sender's relayed frame, got {other:?}"),
            }
        }
        for _ in 0..2 {
            match recv(&mut peer_rx).await.as_ref() {
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
        expect_error(recv(&mut sender_a_rx).await, ErrorCode::RateLimitExceeded);
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

        // After the window resets, the room's ceiling is restored.
        tokio::time::sleep(Duration::from_millis(150)).await;
        server
            .handle_game_data_binary(
                &sender_a,
                GameDataEncoding::MessagePack,
                Bytes::from(vec![0u8; 1]),
            )
            .await;
        for rx in [&mut sender_b_rx, &mut peer_rx] {
            match recv(rx).await.as_ref() {
                ServerMessage::GameDataBinary { payload, .. } => assert_eq!(payload.len(), 1),
                other => panic!("expected relayed frame after room window reset, got {other:?}"),
            }
        }
    }
}
