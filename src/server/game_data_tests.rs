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
        while rx.try_recv().is_ok() {}

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
