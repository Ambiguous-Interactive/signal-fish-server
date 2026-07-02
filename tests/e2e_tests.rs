mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use signal_fish_server::protocol::*;
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;
use std::sync::Arc;
use test_helpers::{
    create_test_server, create_test_server_with_config, test_protocol_config, test_server_config,
};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{expect_no_server_message_within, next_server_message_within};

const AUTHORITY_RELEASE_RESPONSE_TIMEOUT: tokio::time::Duration =
    tokio::time::Duration::from_secs(11);
const AUTHORITY_NOTIFICATION_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(6);
const NO_NOTIFICATION_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_millis(250);
const AUTHORITY_SUCCESS_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(10);
const TWO_PLAYER_AUTHORITY_RELEASE_TIMEOUT: tokio::time::Duration =
    tokio::time::Duration::from_secs(15);

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
type WsReceiver = futures_util::stream::SplitStream<WsStream>;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct BinaryGameDataFrame {
    from_player: PlayerId,
    encoding: GameDataEncoding,
    #[serde(with = "serde_bytes")]
    payload: Vec<u8>,
}

/// Helper to create a test server and return its address
async fn start_test_server() -> std::net::SocketAddr {
    let game_server = create_test_server().await;
    start_server_with_instance(game_server).await
}

async fn start_test_server_with_config(server_config: ServerConfig) -> std::net::SocketAddr {
    start_test_server_with_config_and_protocol(server_config, test_protocol_config()).await
}

async fn start_test_server_with_config_and_protocol(
    server_config: ServerConfig,
    protocol_config: signal_fish_server::config::ProtocolConfig,
) -> std::net::SocketAddr {
    let game_server = create_test_server_with_config(server_config, protocol_config).await;
    start_server_with_instance(game_server).await
}

async fn start_server_with_instance(game_server: Arc<EnhancedGameServer>) -> std::net::SocketAddr {
    // Initialize tracing for debugging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Create enhanced protocol router
    let enhanced_router = create_router("http://localhost:3000").with_state(game_server);

    // Combine routers like in main.rs
    let combined_router = axum::Router::new()
        .nest("/v2", enhanced_router) // New protocol under /v2
        .fallback(|| async { "Use /v2/ws for enhanced protocol" });

    tokio::spawn(async move {
        axum::serve(
            listener,
            combined_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    // No startup sleep: the listener is already bound above before the serve
    // task spawns, so the kernel queues the client's connection until axum
    // begins accepting — `connect_client` then connects under its own timeout.
    // A fixed sleep here only added 2s of dead wall-clock to every consumer and
    // could not actually guarantee readiness; the bound-listener handshake does.
    addr
}

/// Wall-clock scaling for the timing-sensitive idle-timeout tests below.
///
/// Defaults to 1 (local runs and the `Nextest (*)` lanes, where the suite is
/// fast and the multiprocess group is CPU-reserved). The two CI lanes that run
/// the suite WITHOUT nextest's isolation — `MSRV Verification` (plain
/// `cargo test`) and `Coverage (llvm-cov)` (instrumented, ~2-4x slower) — export
/// `SIGNAL_FISH_TEST_TIMEOUT_MULTIPLIER` to widen the idle window (and the
/// heartbeat budget that must outlast it) proportionally. A test task that is
/// briefly CPU-starved on a 48-binary oversubscribed runner then still resets
/// the idle timer in time instead of flaking. Scaling the window and the
/// heartbeat duration together preserves the invariant under test
/// (heartbeat interval << idle window) at any multiplier.
fn idle_timeout_test_multiplier() -> u64 {
    std::env::var("SIGNAL_FISH_TEST_TIMEOUT_MULTIPLIER")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|&m| m >= 1)
        .unwrap_or(1)
}

/// Helper to connect a WebSocket client
async fn connect_client(addr: std::net::SocketAddr, path: &str) -> (WsSink, WsReceiver) {
    let url = format!("ws://{addr}{path}");
    println!("Connecting to WebSocket URL: {url}");

    // Add timeout to WebSocket connection to prevent infinite hanging
    let (ws_stream, _) =
        tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
            .await
            .expect("WebSocket connection timed out after 10 seconds")
            .expect("Failed to connect");

    println!("WebSocket connection established");
    ws_stream.split()
}

/// Helper to send a message and return the response
async fn send_and_receive(
    sender: &mut WsSink,
    receiver: &mut WsReceiver,
    message: ClientMessage,
) -> Result<ServerMessage, Box<dyn std::error::Error>> {
    let json = serde_json::to_string(&message)?;
    sender.send(Message::Text(json.into())).await?;

    // Add timeout to prevent infinite waiting
    match tokio::time::timeout(tokio::time::Duration::from_secs(5), receiver.next()).await {
        Ok(Some(msg)) => {
            let text = msg?.into_text()?;
            let server_msg: ServerMessage = serde_json::from_str(&text)?;
            Ok(server_msg)
        }
        Ok(None) => Err("Connection closed".into()),
        Err(_) => Err("Timeout waiting for response".into()),
    }
}

async fn receive_server_message(receiver: &mut WsReceiver) -> ServerMessage {
    match tokio::time::timeout(tokio::time::Duration::from_secs(5), receiver.next()).await {
        Ok(Some(msg)) => {
            let text = msg
                .expect("websocket message")
                .into_text()
                .expect("text frame");
            serde_json::from_str(&text).expect("valid ServerMessage")
        }
        Ok(None) => panic!("Connection closed while waiting for server message"),
        Err(_) => panic!("Timeout waiting for server message"),
    }
}

async fn expect_player_joined_notification(receiver: &mut WsReceiver, context: &str) {
    match next_server_message_within(receiver, AUTHORITY_NOTIFICATION_TIMEOUT, context).await {
        ServerMessage::PlayerJoined { .. } => {}
        other => panic!("{context}: expected PlayerJoined notification, got {other:?}"),
    }
}

async fn expect_lobby_state_changed_notification(receiver: &mut WsReceiver, context: &str) {
    match next_server_message_within(receiver, AUTHORITY_NOTIFICATION_TIMEOUT, context).await {
        ServerMessage::LobbyStateChanged { .. } => {}
        other => panic!("{context}: expected LobbyStateChanged notification, got {other:?}"),
    }
}

async fn expect_authority_changed_notification(
    receiver: &mut WsReceiver,
    context: &str,
    expected_authority_player: Option<PlayerId>,
    expected_you_are_authority: bool,
) {
    match next_server_message_within(receiver, AUTHORITY_NOTIFICATION_TIMEOUT, context).await {
        ServerMessage::AuthorityChanged {
            you_are_authority,
            authority_player,
        } => {
            assert_eq!(
                authority_player, expected_authority_player,
                "{context}: authority player"
            );
            assert_eq!(
                you_are_authority, expected_you_are_authority,
                "{context}: you_are_authority"
            );
        }
        other => panic!("{context}: expected AuthorityChanged notification, got {other:?}"),
    }
}

async fn expect_authority_response(
    receiver: &mut WsReceiver,
    timeout: tokio::time::Duration,
    context: &str,
    expected_granted: bool,
    expected_reason_contains: Option<&str>,
) {
    match next_server_message_within(receiver, timeout, context).await {
        ServerMessage::AuthorityResponse {
            granted, reason, ..
        } => {
            assert_eq!(granted, expected_granted, "{context}: granted");
            match expected_reason_contains {
                Some(expected) => {
                    let reason = reason.unwrap_or_else(|| {
                        panic!("{context}: expected denial reason containing {expected:?}")
                    });
                    assert!(
                        reason.contains(expected),
                        "{context}: expected denial reason containing {expected:?}, got {reason:?}"
                    );
                }
                None => assert!(reason.is_none(), "{context}: unexpected reason {reason:?}"),
            }
        }
        other => panic!("{context}: expected AuthorityResponse, got {other:?}"),
    }
}

async fn authenticate_for_message_pack(sender: &mut WsSink, receiver: &mut WsReceiver) {
    let auth = ClientMessage::Authenticate {
        app_id: "binary-e2e-app".to_string(),
        sdk_version: Some("1.0.0".to_string()),
        platform: Some("test".to_string()),
        game_data_format: Some(GameDataEncoding::MessagePack),
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };

    let json = serde_json::to_string(&auth).expect("auth serializes");
    sender.send(Message::Text(json.into())).await.unwrap();

    let first = receive_server_message(receiver).await;
    assert!(
        matches!(first, ServerMessage::Authenticated { .. }),
        "expected Authenticated first, got {first:?}"
    );

    match receive_server_message(receiver).await {
        ServerMessage::ProtocolInfo(info) => assert!(
            info.game_data_formats
                .contains(&GameDataEncoding::MessagePack),
            "server must advertise message_pack support: {info:?}"
        ),
        other => panic!("expected ProtocolInfo after auth, got {other:?}"),
    }
}

#[tokio::test]
async fn test_rkyv_game_data_format_request_falls_back_to_json_and_is_not_advertised() {
    let addr = start_test_server().await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    let auth = ClientMessage::Authenticate {
        app_id: "rkyv-negotiation-app".to_string(),
        sdk_version: Some("1.0.0".to_string()),
        platform: Some("test".to_string()),
        game_data_format: Some(GameDataEncoding::Rkyv),
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };

    sender
        .send(Message::Text(
            serde_json::to_string(&auth)
                .expect("auth serializes")
                .into(),
        ))
        .await
        .expect("send auth");

    match receive_server_message(&mut receiver).await {
        ServerMessage::Error {
            message,
            error_code,
        } => {
            assert_eq!(error_code, Some(ErrorCode::UnsupportedGameDataFormat));
            assert!(
                message.contains("rkyv"),
                "unsupported-format diagnostic should use the requested wire token: {message}"
            );
            assert!(
                message.contains("json, message_pack"),
                "supported-format diagnostic should use advertised wire tokens: {message}"
            );
            assert!(
                !message.contains("Rkyv") && !message.contains("MessagePack"),
                "diagnostic must not leak Rust variant names: {message}"
            );
        }
        other => panic!("expected UnsupportedGameDataFormat error first, got {other:?}"),
    }

    match receive_server_message(&mut receiver).await {
        ServerMessage::Authenticated { .. } => {}
        other => panic!("expected Authenticated after unsupported-format warning, got {other:?}"),
    }

    match receive_server_message(&mut receiver).await {
        ServerMessage::ProtocolInfo(info) => {
            assert_eq!(
                info.game_data_formats,
                vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
                "ProtocolInfo advertises only negotiable game-data encodings"
            );
            assert!(
                !info.game_data_formats.contains(&GameDataEncoding::Rkyv),
                "Rkyv is reserved/internal and must not be advertised"
            );
        }
        other => panic!("expected ProtocolInfo after auth, got {other:?}"),
    }
}

async fn receive_binary_game_data_wire_frame(receiver: &mut WsReceiver) -> Vec<u8> {
    tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        loop {
            match receiver.next().await {
                Some(Ok(Message::Binary(bytes))) => return bytes.to_vec(),
                Some(Ok(Message::Text(text))) => {
                    if text.contains(r#""type":"GameDataBinary""#) {
                        panic!(
                            "GameDataBinary is an in-memory carrier and must not be sent as a text envelope"
                        );
                    }

                    let server_msg: ServerMessage = serde_json::from_str(&text)
                        .expect("valid ServerMessage while waiting for binary game data");
                    match server_msg {
                        ServerMessage::PlayerJoined { .. }
                        | ServerMessage::LobbyStateChanged { .. }
                        | ServerMessage::AuthorityChanged { .. }
                        | ServerMessage::AuthorityResponse { .. }
                        | ServerMessage::GameStarting { .. } => {}
                        other => panic!(
                            "unexpected text message while waiting for binary game data: {other:?}"
                        ),
                    }
                }
                Some(Ok(other)) => {
                    panic!("unexpected websocket frame while waiting for binary game data: {other:?}")
                }
                Some(Err(err)) => {
                    panic!("websocket error while waiting for binary game data: {err}")
                }
                None => panic!("connection closed while waiting for binary game data"),
            }
        }
    })
    .await
    .expect("timeout waiting for binary game data")
}

#[tokio::test]
async fn test_websocket_connection_limit_enforced() {
    let mut server_config = test_server_config();
    server_config.max_connections_per_ip = 1;

    let addr = start_test_server_with_config(server_config).await;

    let (mut sender1, receiver1) = connect_client(addr, "/v2/ws").await;

    let (_sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;
    let frame = tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver2.next())
        .await
        .expect("expected response frame")
        .expect("WebSocket frame result");

    match frame {
        Ok(Message::Text(text)) => {
            let server_msg: ServerMessage =
                serde_json::from_str(&text).expect("valid ServerMessage");
            match server_msg {
                ServerMessage::Error { error_code, .. } => {
                    assert_eq!(
                        error_code,
                        Some(ErrorCode::TooManyConnections),
                        "second connection should be rejected"
                    );
                }
                other => panic!("expected error message, got {other:?}"),
            }
        }
        Ok(other) => panic!("expected text error frame, got {other:?}"),
        Err(err) => panic!("WebSocket error received: {err:?}"),
    }

    // First connection should remain active; close it cleanly
    let _ = sender1.close().await;
    drop(receiver1);
}

#[tokio::test]
async fn test_health_check() {
    let addr = start_test_server().await;

    // Test health endpoint first
    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{addr}/v2/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    assert_eq!(text, "OK");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_websocket_connection() {
    // Add overall test timeout to prevent infinite hanging
    let test_timeout = tokio::time::Duration::from_secs(30);
    tokio::time::timeout(test_timeout, async {
        let addr = start_test_server().await;

        // First test if we can connect to the health endpoint
        let client = reqwest::Client::new();
        let response = client
            .get(format!("http://{addr}/v2/health"))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "Health check failed: {}",
            response.status()
        );

        let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

        // Test ping/pong
        let pong = send_and_receive(&mut sender, &mut receiver, ClientMessage::Ping)
            .await
            .unwrap();
        match pong {
            ServerMessage::Pong => {
                // Success
            }
            _ => panic!("Expected Pong, got {pong:?}"),
        }
    })
    .await
    .expect("Test timed out after 30 seconds");
}

#[tokio::test]
async fn test_room_creation_and_joining() {
    let addr = start_test_server().await;

    // Connect two clients
    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;

    // Player 1 creates a room
    let join_msg1 = ClientMessage::JoinRoom {
        game_name: "test_game".to_string(),
        room_code: Some("E2E123".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response1 = send_and_receive(&mut sender1, &mut receiver1, join_msg1)
        .await
        .unwrap();
    match response1 {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "E2E123");
            assert_eq!(payload.game_name, "test_game");
            assert_eq!(payload.max_players, 2);
            assert_eq!(payload.current_players.len(), 1);
            assert!(payload.is_authority);
        }
        _ => panic!("Expected RoomJoined, got {response1:?}"),
    }

    // Player 2 joins the same room
    let join_msg2 = ClientMessage::JoinRoom {
        game_name: "test_game".to_string(),
        room_code: Some("E2E123".to_string()),
        player_name: "Player2".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response2 = send_and_receive(&mut sender2, &mut receiver2, join_msg2)
        .await
        .unwrap();
    match response2 {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "E2E123");
            assert_eq!(payload.game_name, "test_game");
            assert_eq!(payload.current_players.len(), 2);
            assert!(!payload.is_authority);
        }
        _ => panic!("Expected RoomJoined for player 2, got {response2:?}"),
    }

    // The room enters the lobby as soon as player 1 joins (`max_players` is a
    // ceiling, not a required count), so player 1 first receives an own-join
    // LobbyStateChanged before the PlayerJoined for player 2.
    expect_lobby_state_changed_notification(
        &mut receiver1,
        "player 1 room-creation own-join update",
    )
    .await;

    // Player 1 should receive a PlayerJoined notification
    match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver1.next()).await {
        Ok(Some(msg)) => {
            let text = msg.unwrap().into_text().unwrap();
            let notification: ServerMessage = serde_json::from_str(&text).unwrap();
            match notification {
                ServerMessage::PlayerJoined { player } => {
                    assert_eq!(player.name, "Player2");
                    assert!(!player.is_authority);
                }
                _ => panic!("Expected PlayerJoined notification, got {notification:?}"),
            }
        }
        Ok(None) => panic!("Connection closed while waiting for PlayerJoined notification"),
        Err(_) => panic!("Timeout waiting for PlayerJoined notification for player 1"),
    }
}

#[tokio::test]
async fn test_game_data_broadcasting() {
    let addr = start_test_server().await;

    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;

    // Both players join the same room
    let join_msg = ClientMessage::JoinRoom {
        game_name: "data_test".to_string(),
        room_code: Some("DTA123".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let _ = send_and_receive(&mut sender1, &mut receiver1, join_msg)
        .await
        .unwrap();

    let join_msg2 = ClientMessage::JoinRoom {
        game_name: "data_test".to_string(),
        room_code: Some("DTA123".to_string()),
        player_name: "Player2".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let _ = send_and_receive(&mut sender2, &mut receiver2, join_msg2)
        .await
        .unwrap();

    // The room enters the lobby EXACTLY ONCE, as soon as player 1 joins
    // (`max_players` is a ceiling, not a required count). Player 1 sees its
    // own-join lobby update, then a PlayerJoined when player 2 joins. Player 2
    // joins an already-Lobby room, so its `RoomJoined` already carries
    // `lobby_state: Lobby` and no further LobbyStateChanged is broadcast.
    expect_lobby_state_changed_notification(&mut receiver1, "player 1 game-data own-join update")
        .await;
    expect_player_joined_notification(&mut receiver1, "player 1 game-data join notification").await;

    // Player 1 sends game data
    let game_data = serde_json::json!({"action": "move", "x": 100, "y": 200});
    let data_msg = ClientMessage::GameData {
        data: game_data.clone(),
    };

    let json = serde_json::to_string(&data_msg).unwrap();
    sender1.send(Message::Text(json.into())).await.unwrap();

    // Player 2 should receive the game data
    match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver2.next()).await {
        Ok(Some(msg)) => {
            let text = msg.unwrap().into_text().unwrap();
            let received: ServerMessage = serde_json::from_str(&text).unwrap();
            match received {
                ServerMessage::GameData { data, .. } => {
                    assert_eq!(data, game_data);
                }
                _ => panic!("Expected GameData, got {received:?}"),
            }
        }
        Ok(None) => panic!("Connection closed while waiting for game data"),
        Err(_) => panic!("Timeout waiting for game data"),
    }
}

#[tokio::test]
async fn test_binary_game_data_broadcasting_uses_bare_message_pack_frame() {
    let addr =
        start_test_server_with_config_and_protocol(test_server_config(), test_protocol_config())
            .await;

    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;

    authenticate_for_message_pack(&mut sender1, &mut receiver1).await;
    authenticate_for_message_pack(&mut sender2, &mut receiver2).await;

    let join_msg = ClientMessage::JoinRoom {
        game_name: "binary_data_test".to_string(),
        room_code: Some("BIN1".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let player1_id = match send_and_receive(&mut sender1, &mut receiver1, join_msg)
        .await
        .unwrap()
    {
        ServerMessage::RoomJoined(payload) => payload.player_id,
        other => panic!("Expected RoomJoined for player 1, got {other:?}"),
    };

    let join_msg2 = ClientMessage::JoinRoom {
        game_name: "binary_data_test".to_string(),
        room_code: Some("BIN1".to_string()),
        player_name: "Player2".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let _ = send_and_receive(&mut sender2, &mut receiver2, join_msg2)
        .await
        .unwrap();

    let payload = vec![0x01, 0x02, 0x03, 0x04];
    sender1
        .send(Message::Binary(payload.clone().into()))
        .await
        .unwrap();

    let wire = receive_binary_game_data_wire_frame(&mut receiver2).await;
    let frame: BinaryGameDataFrame =
        rmp_serde::from_slice(&wire).expect("bare MessagePack game-data frame");

    assert_eq!(
        frame,
        BinaryGameDataFrame {
            from_player: player1_id,
            encoding: GameDataEncoding::MessagePack,
            payload,
        }
    );
}

#[tokio::test]
async fn test_room_capacity_limit() {
    let addr = start_test_server().await;

    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;
    let (mut sender3, mut receiver3) = connect_client(addr, "/v2/ws").await;

    // Create room with max 2 players
    let join_msg1 = ClientMessage::JoinRoom {
        game_name: "limited_game".to_string(),
        room_code: Some("LMT123".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let _ = send_and_receive(&mut sender1, &mut receiver1, join_msg1)
        .await
        .unwrap();

    // Second player joins
    let join_msg2 = ClientMessage::JoinRoom {
        game_name: "limited_game".to_string(),
        room_code: Some("LMT123".to_string()),
        player_name: "Player2".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };
    let _ = send_and_receive(&mut sender2, &mut receiver2, join_msg2)
        .await
        .unwrap();

    // Third player should be rejected
    let join_msg3 = ClientMessage::JoinRoom {
        game_name: "limited_game".to_string(),
        room_code: Some("LMT123".to_string()),
        player_name: "Player3".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response3 = send_and_receive(&mut sender3, &mut receiver3, join_msg3)
        .await
        .unwrap();
    match response3 {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("full") || reason.contains("capacity"));
        }
        _ => panic!("Expected RoomJoinFailed, got {response3:?}"),
    }
}

#[tokio::test]
async fn test_validation_errors() {
    let addr = start_test_server().await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Test invalid room code
    let invalid_join = ClientMessage::JoinRoom {
        game_name: "valid_game".to_string(),
        room_code: Some("INVALID!@#".to_string()), // Contains special characters
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, invalid_join)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Room code") || reason.contains("alphanumeric"));
        }
        _ => panic!("Expected RoomJoinFailed for invalid room code, got {response:?}"),
    }
}

#[tokio::test]
async fn test_e2e_custom_protocol_limits() {
    // Test end-to-end behavior with custom protocol configuration
    let server_config = ServerConfig::default();
    let protocol_config = signal_fish_server::config::ProtocolConfig {
        max_game_name_length: 20,
        room_code_length: 4,
        max_player_name_length: 10,
        max_players_limit: 8,
        ..signal_fish_server::config::ProtocolConfig::default()
    };

    let addr = start_test_server_with_config_and_protocol(server_config, protocol_config).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Test 1: Game name exceeding custom limit should fail
    let long_name_msg = ClientMessage::JoinRoom {
        game_name: "this_game_name_is_too_long_for_our_custom_limit".to_string(), // 49 chars > 20
        room_code: Some("ABCD".to_string()),
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, long_name_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Game name too long") && reason.contains("20"));
        }
        _ => panic!("Expected game name validation failure, got {response:?}"),
    }

    // Test 2: Room code with wrong length should fail
    let wrong_length_msg = ClientMessage::JoinRoom {
        game_name: "shortgame".to_string(),
        room_code: Some("ABCDEF".to_string()), // 6 chars but we configured 4
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, wrong_length_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Room code must be exactly 4 characters"));
        }
        _ => panic!("Expected room code validation failure, got {response:?}"),
    }

    // Test 3: Player name exceeding custom limit should fail
    let long_player_msg = ClientMessage::JoinRoom {
        game_name: "testgame".to_string(),
        room_code: Some("TEST".to_string()),
        player_name: "VeryLongPlayerName".to_string(), // 18 chars > 10
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, long_player_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Player name too long") && reason.contains("10"));
        }
        _ => panic!("Expected player name validation failure, got {response:?}"),
    }

    // Test 4: Max players exceeding limit should fail
    let too_many_players_msg = ClientMessage::JoinRoom {
        game_name: "testgame".to_string(),
        room_code: Some("TEST".to_string()),
        player_name: "Player".to_string(),
        max_players: Some(16), // Exceeds our custom limit of 8
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, too_many_players_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Max players cannot exceed 8"));
        }
        _ => panic!("Expected max players validation failure, got {response:?}"),
    }

    // Test 5: Valid configuration should work
    let valid_msg = ClientMessage::JoinRoom {
        game_name: "game".to_string(),       // Short
        room_code: Some("GOOD".to_string()), // 4 chars
        player_name: "Bob".to_string(),      // Short
        max_players: Some(6),                // Within limit
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, valid_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "GOOD");
            assert_eq!(payload.game_name, "game");
            assert_eq!(payload.max_players, 6);
        }
        _ => panic!("Expected successful room join with valid parameters, got {response:?}"),
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_e2e_config_with_file_and_env() {
    use std::env;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    // Create temporary config file
    let dir = tempdir().unwrap();
    let config_file = dir.path().join("e2e_test_config.json");

    let config_json = r#"{
        "port": 0,
        "server": {
            "default_max_players": 16
        },
        "protocol": {
            "max_game_name_length": 50,
            "room_code_length": 5
        }
    }"#;

    let mut file = File::create(&config_file).unwrap();
    file.write_all(config_json.as_bytes()).unwrap();
    file.flush().unwrap();

    // Give file system time to complete write operations
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Set environment variables to test precedence
    let config_path = config_file.to_str().unwrap();
    println!("Using config file path: {config_path}");
    env::set_var("SIGNAL_FISH_CONFIG_PATH", config_path);
    env::set_var("SIGNAL_FISH__PROTOCOL__ROOM_CODE_LENGTH", "3"); // Override file config

    let loaded_config = signal_fish_server::config::load();

    // Verify precedence: env var should override file value
    assert_eq!(loaded_config.protocol.room_code_length, 3); // From env override
    assert_eq!(loaded_config.protocol.max_game_name_length, 50); // From file
    assert_eq!(loaded_config.server.default_max_players, 16); // From file
    assert_eq!(loaded_config.server.ping_timeout, 30); // Default value

    // Test that the loaded config affects server behavior
    let addr = start_test_server_with_config_and_protocol(
        ServerConfig {
            default_max_players: loaded_config.server.default_max_players,
            ..ServerConfig::default()
        },
        loaded_config.protocol,
    )
    .await;

    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Test that room code length validation uses our custom 3-character limit
    let test_msg = ClientMessage::JoinRoom {
        game_name: "configtest".to_string(),
        room_code: Some("ABCD".to_string()), // 4 chars but we configured 3
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, test_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoinFailed { reason, .. } => {
            assert!(reason.contains("Room code must be exactly 3 characters"));
        }
        _ => panic!("Expected room code validation failure for 3-char limit, got {response:?}"),
    }

    // Test valid 3-character code works
    let valid_msg = ClientMessage::JoinRoom {
        game_name: "configtest".to_string(),
        room_code: Some("ABC".to_string()), // 3 chars - should work
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, valid_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(payload.room_code, "ABC");
        }
        _ => panic!("Expected successful room join with 3-char code, got {response:?}"),
    }

    // Clean up
    env::remove_var("SIGNAL_FISH_CONFIG_PATH");
    env::remove_var("SIGNAL_FISH__PROTOCOL__ROOM_CODE_LENGTH");
}

#[tokio::test]
async fn test_e2e_authority_protocol_enforcement() {
    let addr = start_test_server().await;

    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;

    // Player 1 creates room with authority support
    let create_msg = ClientMessage::JoinRoom {
        game_name: "authority_game".to_string(),
        room_code: Some("AUTH01".to_string()),
        player_name: "AuthPlayer1".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response1 = send_and_receive(&mut sender1, &mut receiver1, create_msg)
        .await
        .unwrap();
    let player1_id = match response1 {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(payload.is_authority); // First player should have authority
            assert!(payload.supports_authority); // Room supports authority
            assert_eq!(payload.current_players.len(), 1);
            assert!(payload.current_players[0].is_authority);
            payload.player_id
        }
        _ => panic!("Expected RoomJoined for player 1, got {response1:?}"),
    };

    // Player 2 joins the same room
    let join_msg = ClientMessage::JoinRoom {
        game_name: "authority_game".to_string(),
        room_code: Some("AUTH01".to_string()),
        player_name: "AuthPlayer2".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response2 = send_and_receive(&mut sender2, &mut receiver2, join_msg)
        .await
        .unwrap();
    let player2_id = match response2 {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(!payload.is_authority); // Second player should NOT have authority
            assert_eq!(payload.current_players.len(), 2);
            // First player should still have authority
            let authority_count = payload
                .current_players
                .iter()
                .filter(|p| p.is_authority)
                .count();
            assert_eq!(authority_count, 1);
            payload.player_id
        }
        _ => panic!("Expected RoomJoined for player 2, got {response2:?}"),
    };
    assert_ne!(player1_id, player2_id, "test must use distinct players");

    // The room enters the lobby EXACTLY ONCE, on player 1's join (`max_players`
    // is a ceiling, not a required count): player 1 sees its own-join lobby
    // update, then a PlayerJoined for player 2. Player 2 joins an already-Lobby
    // room, so no further LobbyStateChanged is broadcast.
    expect_lobby_state_changed_notification(
        &mut receiver1,
        "player 1 authority-protocol own-join lobby update",
    )
    .await;
    expect_player_joined_notification(
        &mut receiver1,
        "player 1 authority-protocol join notification",
    )
    .await;
    expect_no_server_message_within(
        &mut receiver1,
        NO_NOTIFICATION_TIMEOUT,
        "player 1 authority-protocol post-join silence",
    )
    .await;
    expect_no_server_message_within(
        &mut receiver2,
        NO_NOTIFICATION_TIMEOUT,
        "player 2 authority-protocol post-join silence",
    )
    .await;

    // Player 2 tries to request authority while Player 1 has it - should be DENIED
    let authority_request = ClientMessage::AuthorityRequest {
        become_authority: true,
    };

    let json = serde_json::to_string(&authority_request).unwrap();
    sender2.send(Message::Text(json.into())).await.unwrap();

    // The coordinator and server wrapper currently both emit the request result.
    for context in [
        "player 2 denied authority response from coordinator",
        "player 2 denied authority response from server",
    ] {
        expect_authority_response(
            &mut receiver2,
            AUTHORITY_NOTIFICATION_TIMEOUT,
            context,
            false,
            Some("Another player already has authority"),
        )
        .await;
    }
    expect_no_server_message_within(
        &mut receiver1,
        NO_NOTIFICATION_TIMEOUT,
        "player 1 after denied authority request",
    )
    .await;
    expect_no_server_message_within(
        &mut receiver2,
        NO_NOTIFICATION_TIMEOUT,
        "player 2 after denied authority request",
    )
    .await;

    // Player 1 releases authority
    let release_request = ClientMessage::AuthorityRequest {
        become_authority: false,
    };

    // Add a longer delay to ensure the previous authority request is fully processed
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    println!("Sending authority release request from player 1");
    let json = serde_json::to_string(&release_request).unwrap();
    sender1.send(Message::Text(json.into())).await.unwrap();
    println!("Authority release request sent");

    // Give server time to process the request
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    expect_authority_response(
        &mut receiver1,
        AUTHORITY_RELEASE_RESPONSE_TIMEOUT,
        "player 1 authority release response",
        true,
        None,
    )
    .await;
    expect_authority_changed_notification(
        &mut receiver1,
        "player 1 authority release state change",
        None,
        false,
    )
    .await;
    expect_authority_response(
        &mut receiver1,
        AUTHORITY_RELEASE_RESPONSE_TIMEOUT,
        "player 1 authority release server response",
        true,
        None,
    )
    .await;
    expect_authority_changed_notification(
        &mut receiver2,
        "player 2 authority release notification",
        None,
        false,
    )
    .await;
    expect_no_server_message_within(
        &mut receiver1,
        NO_NOTIFICATION_TIMEOUT,
        "player 1 after authority release",
    )
    .await;
    expect_no_server_message_within(
        &mut receiver2,
        NO_NOTIFICATION_TIMEOUT,
        "player 2 after authority release",
    )
    .await;

    // NOW Player 2 can successfully request authority
    let authority_request2 = ClientMessage::AuthorityRequest {
        become_authority: true,
    };

    // Add a delay before next request
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    println!("Sending authority request from player 2");
    let json = serde_json::to_string(&authority_request2).unwrap();
    sender2.send(Message::Text(json.into())).await.unwrap();
    println!("Authority request sent from player 2");

    // Give server time to process the request
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    expect_authority_response(
        &mut receiver2,
        AUTHORITY_SUCCESS_TIMEOUT,
        "player 2 successful authority response from coordinator",
        true,
        None,
    )
    .await;
    expect_authority_changed_notification(
        &mut receiver1,
        "player 1 sees player 2 become authority",
        Some(player2_id),
        false,
    )
    .await;
    expect_authority_changed_notification(
        &mut receiver2,
        "player 2 sees self become authority",
        Some(player2_id),
        true,
    )
    .await;
    expect_authority_response(
        &mut receiver2,
        AUTHORITY_SUCCESS_TIMEOUT,
        "player 2 successful authority response from server",
        true,
        None,
    )
    .await;
    expect_no_server_message_within(
        &mut receiver1,
        NO_NOTIFICATION_TIMEOUT,
        "player 1 after player 2 authority success",
    )
    .await;
    expect_no_server_message_within(
        &mut receiver2,
        NO_NOTIFICATION_TIMEOUT,
        "player 2 after player 2 authority success",
    )
    .await;
}

#[tokio::test]
async fn test_simple_authority_release() {
    let addr = start_test_server().await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Join room as first player (should get authority)
    let join_msg = ClientMessage::JoinRoom {
        game_name: "simple_test".to_string(),
        room_code: Some("SIMPLE".to_string()),
        player_name: "TestPlayer".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, join_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(payload.is_authority, "First player should have authority");
        }
        _ => panic!("Expected RoomJoined, got {response:?}"),
    }

    // The room enters the lobby as soon as the creator joins (`max_players` is a
    // ceiling, not a required count), so an own-join LobbyStateChanged follows
    // RoomJoined. Drain it before exercising the authority release flow.
    expect_lobby_state_changed_notification(&mut receiver, "single-player lobby entry").await;

    // Now try to release authority
    println!("Attempting to release authority");
    let release_request = ClientMessage::AuthorityRequest {
        become_authority: false,
    };

    let json = serde_json::to_string(&release_request).unwrap();
    sender.send(Message::Text(json.into())).await.unwrap();

    expect_authority_response(
        &mut receiver,
        AUTHORITY_SUCCESS_TIMEOUT,
        "single-player authority release response",
        true,
        None,
    )
    .await;
    expect_authority_changed_notification(
        &mut receiver,
        "single-player authority release state change",
        None,
        false,
    )
    .await;
    expect_authority_response(
        &mut receiver,
        AUTHORITY_SUCCESS_TIMEOUT,
        "single-player authority release server response",
        true,
        None,
    )
    .await;
    expect_no_server_message_within(
        &mut receiver,
        NO_NOTIFICATION_TIMEOUT,
        "single-player authority release completion",
    )
    .await;

    println!("Authority release test completed successfully");
}

#[tokio::test]
async fn test_two_player_authority_release() {
    let addr = start_test_server().await;
    let (mut sender1, mut receiver1) = connect_client(addr, "/v2/ws").await;
    let (mut sender2, mut receiver2) = connect_client(addr, "/v2/ws").await;

    // Player 1 creates room (should get authority)
    let join_msg1 = ClientMessage::JoinRoom {
        game_name: "two_player_test".to_string(),
        room_code: Some("TWO123".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response1 = send_and_receive(&mut sender1, &mut receiver1, join_msg1)
        .await
        .unwrap();
    match response1 {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(payload.is_authority, "First player should have authority");
        }
        _ => panic!("Expected RoomJoined, got {response1:?}"),
    }

    // Player 2 joins room (should NOT get authority)
    let join_msg2 = ClientMessage::JoinRoom {
        game_name: "two_player_test".to_string(),
        room_code: Some("TWO123".to_string()),
        player_name: "Player2".to_string(),
        max_players: Some(2),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response2 = send_and_receive(&mut sender2, &mut receiver2, join_msg2)
        .await
        .unwrap();
    match response2 {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(
                !payload.is_authority,
                "Second player should NOT have authority"
            );
        }
        _ => panic!("Expected RoomJoined, got {response2:?}"),
    }

    // The room enters the lobby EXACTLY ONCE, on player 1's join: player 1 sees
    // its own-join lobby update, then a PlayerJoined for player 2. Player 2 joins
    // an already-Lobby room, so no further LobbyStateChanged is broadcast.
    expect_lobby_state_changed_notification(&mut receiver1, "player 1 two-player own-join update")
        .await;
    expect_player_joined_notification(&mut receiver1, "player 1 two-player join notification")
        .await;

    // Now Player 1 releases authority
    let release_request = ClientMessage::AuthorityRequest {
        become_authority: false,
    };

    let json = serde_json::to_string(&release_request).unwrap();
    sender1.send(Message::Text(json.into())).await.unwrap();

    expect_authority_response(
        &mut receiver1,
        TWO_PLAYER_AUTHORITY_RELEASE_TIMEOUT,
        "two-player authority release response",
        true,
        None,
    )
    .await;
    expect_authority_changed_notification(
        &mut receiver1,
        "player 1 two-player authority release state change",
        None,
        false,
    )
    .await;
    expect_authority_response(
        &mut receiver1,
        TWO_PLAYER_AUTHORITY_RELEASE_TIMEOUT,
        "two-player authority release server response",
        true,
        None,
    )
    .await;
    expect_authority_changed_notification(
        &mut receiver2,
        "player 2 two-player authority release notification",
        None,
        false,
    )
    .await;
    expect_no_server_message_within(
        &mut receiver1,
        NO_NOTIFICATION_TIMEOUT,
        "player 1 after two-player authority release",
    )
    .await;
    expect_no_server_message_within(
        &mut receiver2,
        NO_NOTIFICATION_TIMEOUT,
        "player 2 after two-player authority release",
    )
    .await;
    println!("Two-player authority release test completed successfully");
}

#[tokio::test]
async fn test_e2e_authority_disabled_rooms() {
    let addr = start_test_server().await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Create room without authority support
    let create_msg = ClientMessage::JoinRoom {
        game_name: "no_auth_game".to_string(),
        room_code: Some("NOAUTH".to_string()),
        player_name: "Player1".to_string(),
        max_players: Some(4),
        supports_authority: Some(false), // Authority disabled
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, create_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert!(!payload.is_authority); // No authority in non-authority room
            assert!(!payload.supports_authority); // Room doesn't support authority
        }
        _ => panic!("Expected RoomJoined, got {response:?}"),
    }

    // The room enters the lobby as soon as the creator joins (`max_players` is a
    // ceiling, not a required count), so an own-join LobbyStateChanged follows
    // RoomJoined. Drain it before asserting the authority-request denial.
    expect_lobby_state_changed_notification(&mut receiver, "no-auth room lobby entry").await;

    // Try to request authority - should be denied
    let authority_request = ClientMessage::AuthorityRequest {
        become_authority: true,
    };

    let json = serde_json::to_string(&authority_request).unwrap();
    sender.send(Message::Text(json.into())).await.unwrap();

    // Should receive denial
    match tokio::time::timeout(tokio::time::Duration::from_secs(2), receiver.next()).await {
        Ok(Some(msg)) => {
            let text = msg.unwrap().into_text().unwrap();
            let response: ServerMessage = serde_json::from_str(&text).unwrap();
            match response {
                ServerMessage::AuthorityResponse {
                    granted, reason, ..
                } => {
                    assert!(!granted);
                    assert!(reason.is_some());
                    assert!(reason.unwrap().contains("Room does not support authority"));
                }
                _ => panic!("Expected AuthorityResponse denial, got {response:?}"),
            }
        }
        Ok(None) => panic!("Connection closed while waiting for authority response"),
        Err(_) => panic!("Timeout waiting for authority response"),
    }
}

#[tokio::test]
async fn test_e2e_room_code_generation_with_custom_length() {
    // Test that room code generation respects custom configuration
    let protocol_config = signal_fish_server::config::ProtocolConfig {
        room_code_length: 8, // Custom length
        ..Default::default()
    };

    let addr =
        start_test_server_with_config_and_protocol(ServerConfig::default(), protocol_config).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Create room without specifying code (should auto-generate with custom length)
    let auto_room_msg = ClientMessage::JoinRoom {
        game_name: "autogame".to_string(),
        room_code: None, // Auto-generate
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, auto_room_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(ref payload) => {
            assert_eq!(
                payload.room_code.len(),
                8,
                "Generated room code should have custom length of 8"
            );
            // Should not contain confusing characters
            assert!(!payload.room_code.contains('0'));
            assert!(!payload.room_code.contains('O'));
            assert!(!payload.room_code.contains('I'));
            assert!(!payload.room_code.contains('1'));
        }
        _ => panic!("Expected successful room creation with auto-generated code, got {response:?}"),
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_e2e_fallback_to_defaults_on_invalid_config() {
    use std::env;

    // Set invalid JSON config - should fallback to defaults
    env::set_var("SIGNAL_FISH_CONFIG_JSON", "{invalid json content}");

    let config = signal_fish_server::config::load();

    // Should use default values despite invalid JSON
    assert_eq!(config.port, 3536); // Default port
    assert_eq!(config.protocol.room_code_length, 6); // Default room code length
    assert_eq!(config.server.default_max_players, 8); // Default max players

    // Clean up
    env::remove_var("SIGNAL_FISH_CONFIG_JSON");

    // Test that server still works with these defaults
    let addr =
        start_test_server_with_config_and_protocol(ServerConfig::default(), config.protocol).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Should work with default 6-character room codes
    let test_msg = ClientMessage::JoinRoom {
        game_name: "defaulttest".to_string(),
        room_code: Some("DEF123".to_string()), // 6 chars - default length
        player_name: "Player".to_string(),
        max_players: Some(4),
        supports_authority: Some(true),
        relay_transport: None,
    };

    let response = send_and_receive(&mut sender, &mut receiver, test_msg)
        .await
        .unwrap();
    match response {
        ServerMessage::RoomJoined(_) => {
            // Should work with defaults
        }
        _ => panic!("Expected room join to work with default config, got {response:?}"),
    }
}

// ===========================================================================
// Post-auth idle timeout (`websocket.idle_timeout_secs`)
// ===========================================================================

/// Build a test server config with the given post-auth idle timeout.
fn idle_timeout_server_config(idle_timeout_secs: u64) -> ServerConfig {
    let mut config = test_server_config();
    config.websocket_config = signal_fish_server::config::WebSocketConfig {
        idle_timeout_secs,
        ..signal_fish_server::config::WebSocketConfig::default()
    };
    config
}

/// Read frames until the connection terminates (Close frame, clean end-of-stream,
/// or transport error), panicking on timeout. Returns the non-close frames seen.
async fn collect_frames_until_closed(
    receiver: &mut WsReceiver,
    timeout: tokio::time::Duration,
    context: &str,
) -> Vec<Message> {
    let mut frames = Vec::new();
    let deadline_result = tokio::time::timeout(timeout, async {
        loop {
            match receiver.next().await {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(frame)) => frames.push(frame),
                // A reset without a close handshake still proves termination.
                Some(Err(_)) => break,
            }
        }
    })
    .await;
    assert!(
        deadline_result.is_ok(),
        "{context}: connection did not close within {timeout:?}"
    );
    frames
}

/// An authenticated connection that sends no frames at all is closed once the
/// socket-level idle timeout elapses, after receiving a
/// `CONNECTION_IDLE_TIMEOUT` error. (Auth is disabled in `test_server_config`,
/// so the connection counts as authenticated from the first frame — the idle
/// window applies immediately.)
///
/// The maintenance reaper is deliberately NOT running here (the e2e harness
/// does not spawn it), so the socket-level idle mechanism — the backstop for
/// reaper-less deployments and library embedders — is what closes the
/// connection. The reaper-driven eviction path is covered separately by
/// `test_silent_client_is_reaped_with_activity_timeout`.
#[tokio::test]
async fn test_idle_client_is_disconnected_after_idle_timeout() {
    let mult = idle_timeout_test_multiplier();
    let config = idle_timeout_server_config(2 * mult);
    let game_server = create_test_server_with_config(config, test_protocol_config()).await;
    let addr = start_server_with_instance(game_server).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Prove the connection works, then go silent.
    let response = send_and_receive(
        &mut sender,
        &mut receiver,
        ClientMessage::JoinRoom {
            game_name: "idle-timeout-game".to_string(),
            room_code: None,
            player_name: "Idler".to_string(),
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await
    .expect("join before idling");
    assert!(
        matches!(response, ServerMessage::RoomJoined(_)),
        "expected RoomJoined, got {response:?}"
    );

    // Stay idle: the server must deliver the idle-timeout error and close the
    // socket well within this window (timeout is 2s; allow CI slack).
    let frames = collect_frames_until_closed(
        &mut receiver,
        tokio::time::Duration::from_secs(10 * mult),
        "idle client",
    )
    .await;

    let idle_error_seen = frames.iter().any(|frame| match frame {
        Message::Text(text) => matches!(
            serde_json::from_str::<ServerMessage>(text),
            Ok(ServerMessage::Error {
                error_code: Some(ErrorCode::ConnectionIdleTimeout),
                ..
            })
        ),
        _ => false,
    });
    assert!(
        idle_error_seen,
        "expected an Error frame with CONNECTION_IDLE_TIMEOUT before close, got frames: {frames:?}"
    );
}

/// A completely silent client is evicted by the maintenance reaper
/// (`server.ping_timeout`) with a loud `ACTIVITY_TIMEOUT` farewell, and its
/// socket is closed promptly — long before the (much larger) socket-level
/// idle window. This is the production-default ordering (30s reaper vs 300s
/// idle) scaled down, and it pins two behaviors at once: the reaper's
/// distinct disconnect reason (not `CONNECTION_IDLE_TIMEOUT`, which is the
/// socket-level close), and the positive socket teardown on unregistration
/// (reaped connections must not linger half-alive until the idle window).
#[tokio::test]
async fn test_silent_client_is_reaped_with_activity_timeout() {
    let mult = idle_timeout_test_multiplier();
    // Idle window far beyond the test deadline: only the reaper can close
    // this connection.
    let mut config = idle_timeout_server_config(300);
    config.ping_timeout = tokio::time::Duration::from_millis(500);
    let game_server = create_test_server_with_config(config, test_protocol_config()).await;
    // Run the maintenance reaper (main.rs spawns it the same way); the e2e
    // harness does not start it by default.
    let cleanup_server = Arc::clone(&game_server);
    tokio::spawn(async move {
        cleanup_server.cleanup_task().await;
    });
    let addr = start_server_with_instance(game_server).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Prove the connection works, then go silent.
    let response = send_and_receive(
        &mut sender,
        &mut receiver,
        ClientMessage::JoinRoom {
            game_name: "reaper-eviction-game".to_string(),
            room_code: None,
            player_name: "Silent".to_string(),
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await
    .expect("join before going silent");
    assert!(
        matches!(response, ServerMessage::RoomJoined(_)),
        "expected RoomJoined, got {response:?}"
    );

    // Reaper window (500ms) + cleanup interval (1s, from `test_server_config`)
    // elapse well within this deadline; the idle window (300s) never can.
    let frames = collect_frames_until_closed(
        &mut receiver,
        tokio::time::Duration::from_secs(15 * mult),
        "reaped silent client",
    )
    .await;

    let activity_timeout_seen = frames.iter().any(|frame| match frame {
        Message::Text(text) => matches!(
            serde_json::from_str::<ServerMessage>(text),
            Ok(ServerMessage::Error {
                error_code: Some(ErrorCode::ActivityTimeout),
                ..
            })
        ),
        _ => false,
    });
    assert!(
        activity_timeout_seen,
        "expected an Error frame with ACTIVITY_TIMEOUT before close, got frames: {frames:?}"
    );
}

/// A client that keeps sending frames (heartbeat pings) survives well past the
/// idle window: every inbound frame resets the timeout.
#[tokio::test]
async fn test_active_client_survives_past_idle_timeout_window() {
    let mult = idle_timeout_test_multiplier();
    let addr = start_test_server_with_config(idle_timeout_server_config(2 * mult)).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Heartbeat every 500ms for (2.5 * mult)s total — strictly longer than the
    // (2 * mult)s idle window — asserting the Pong response each round (no silent
    // drains). Scaling the round count with the window keeps "total heartbeat
    // span > idle window" true at any multiplier, while the per-round 500ms
    // interval stays far below the window so a briefly-starved scheduler still
    // resets the idle timer in time.
    for round in 0..(5 * mult) {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let response = send_and_receive(&mut sender, &mut receiver, ClientMessage::Ping)
            .await
            .unwrap_or_else(|err| panic!("heartbeat round {round} failed: {err}"));
        assert!(
            matches!(response, ServerMessage::Pong),
            "heartbeat round {round}: expected Pong, got {response:?}"
        );
    }

    // The connection outlived the idle window and still serves requests.
    let response = send_and_receive(
        &mut sender,
        &mut receiver,
        ClientMessage::JoinRoom {
            game_name: "active-survivor-game".to_string(),
            room_code: None,
            player_name: "Heartbeater".to_string(),
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await
    .expect("join after surviving idle window");
    assert!(
        matches!(response, ServerMessage::RoomJoined(_)),
        "expected RoomJoined after surviving idle window, got {response:?}"
    );
}

/// `idle_timeout_secs = 0` disables idle enforcement entirely: a completely
/// silent authenticated client stays connected for comfortably longer than the
/// small nonzero windows that close the sibling tests' connections.
/// `ping_timeout` stays at the test default (10s), so the state reaper cannot
/// reap the client within the silent window and confound the assertion — the
/// only mechanism under test is the idle timeout.
#[tokio::test]
async fn test_idle_timeout_zero_disables_idle_enforcement() {
    let addr = start_test_server_with_config(idle_timeout_server_config(0)).await;
    let (mut sender, mut receiver) = connect_client(addr, "/v2/ws").await;

    // Prove the connection works, then go silent.
    let response = send_and_receive(
        &mut sender,
        &mut receiver,
        ClientMessage::JoinRoom {
            game_name: "idle-disabled-game".to_string(),
            room_code: None,
            player_name: "Lurker".to_string(),
            max_players: Some(4),
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await
    .expect("join before idling");
    assert!(
        matches!(response, ServerMessage::RoomJoined(_)),
        "expected RoomJoined, got {response:?}"
    );

    // The room enters the lobby as soon as the creator joins (`max_players` is a
    // ceiling, not a required count), so an own-join LobbyStateChanged follows
    // RoomJoined. Drain it before asserting the connection then goes silent.
    expect_lobby_state_changed_notification(&mut receiver, "idle-disabled lobby entry").await;

    // Stay silent for 3s — long enough to surface any spurious idle close. With
    // the idle timeout disabled the server must send nothing at all: no
    // `CONNECTION_IDLE_TIMEOUT` error, no Close frame. This test does not race
    // the idle window (there is none), so it needs no timeout multiplier, and
    // ping_timeout's 10s reaper stays well clear of this 3s window.
    let silence = tokio::time::timeout(tokio::time::Duration::from_secs(3), receiver.next()).await;
    assert!(
        silence.is_err(),
        "expected no frames while idle with idle_timeout_secs = 0, got {silence:?}"
    );

    // The connection is still alive and still serves requests.
    let response = send_and_receive(&mut sender, &mut receiver, ClientMessage::Ping)
        .await
        .expect("ping after silent window");
    assert!(
        matches!(response, ServerMessage::Pong),
        "expected Pong after silent window, got {response:?}"
    );
}
