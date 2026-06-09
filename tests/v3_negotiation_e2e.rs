//! End-to-end protocol v3 (P1) negotiation tests through the real WebSocket
//! stack: a client advertising `protocol_version: 3` is negotiated to v3 and
//! sees `ProtocolInfo.protocol_version == Some(3)`, while a client omitting the
//! fields is recorded as v2 / relay-only. Also exercises the `/v3/ws` alias
//! default.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::SinkExt;
use signal_fish_server::config::AppAuthEntry;
use signal_fish_server::protocol::{ClientMessage, ServerMessage, Topology, Transport};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use std::sync::Arc;
use test_helpers::{test_protocol_config, test_server_config};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{next_server_message_within, WsStream};

const APP_ID: &str = "v3-test-app";
const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(5);

fn app_entry() -> AppAuthEntry {
    AppAuthEntry {
        app_id: APP_ID.to_string(),
        app_secret: "secret".to_string(),
        app_name: "V3 Test App".to_string(),
        max_rooms: Some(10),
        max_players_per_room: Some(8),
        rate_limit_per_minute: Some(600),
    }
}

async fn start_auth_server() -> std::net::SocketAddr {
    let mut server_config: ServerConfig = test_server_config();
    server_config.auth_enabled = true;

    // Disable SDK platform/version enforcement so an `Authenticate` that omits
    // `platform` (the focus here is protocol-version negotiation) still succeeds.
    let mut protocol_config = test_protocol_config();
    protocol_config.sdk_compatibility.enforce = false;

    let game_server = EnhancedGameServer::new(
        server_config,
        protocol_config,
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::AuthMaintenanceConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![app_entry()],
    )
    .await
    .expect("server builds");

    start_server(game_server).await
}

async fn start_auth_disabled_server() -> std::net::SocketAddr {
    let mut server_config: ServerConfig = test_server_config();
    server_config.auth_enabled = false;

    let mut protocol_config = test_protocol_config();
    protocol_config.sdk_compatibility.enforce = false;

    let game_server = EnhancedGameServer::new(
        server_config,
        protocol_config,
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::AuthMaintenanceConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("server builds");

    start_server(game_server).await
}

async fn start_server(game_server: Arc<EnhancedGameServer>) -> std::net::SocketAddr {
    use axum::routing::get;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Mirror main.rs wiring: enhanced router nested under /v2, plus a top-level
    // /v3/ws alias sharing the same connection handler.
    let enhanced_router = create_router("http://localhost:3000").with_state(game_server.clone());
    let combined_router = axum::Router::new()
        .nest("/v2", enhanced_router)
        .route("/v3/ws", get(websocket_handler_v3))
        .fallback(|| async { "Use /v2/ws or /v3/ws" })
        .with_state(game_server);

    tokio::spawn(async move {
        axum::serve(
            listener,
            combined_router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    addr
}

async fn connect(addr: std::net::SocketAddr, path: &str) -> WsStream {
    let url = format!("ws://{addr}{path}");
    let (ws, _) = tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
        .await
        .expect("connect timeout")
        .expect("connect");
    ws
}

async fn next_server_message(ws: &mut WsStream) -> ServerMessage {
    next_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "next server message").await
}

/// Authenticate and return the `ProtocolInfo` payload (skips the preceding
/// `Authenticated` message).
async fn authenticate(ws: &mut WsStream, auth: ClientMessage) -> ServerMessage {
    let json = serde_json::to_string(&auth).unwrap();
    ws.send(Message::Text(json.into())).await.unwrap();

    // First the Authenticated success, then ProtocolInfo.
    let first = next_server_message(ws).await;
    assert!(
        matches!(first, ServerMessage::Authenticated { .. }),
        "expected Authenticated first, got {first:?}"
    );
    next_server_message(ws).await
}

#[tokio::test]
async fn v3_client_negotiates_v3_and_protocol_info_reports_it() {
    let addr = start_auth_server().await;
    let mut ws = connect(addr, "/v2/ws").await;

    let auth = ClientMessage::Authenticate {
        app_id: APP_ID.to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: Some(3),
        supported_transports: Some(vec![Transport::Relay, Transport::WebRtc]),
        supported_topologies: Some(vec![Topology::Relay, Topology::Mesh]),
    };

    match authenticate(&mut ws, auth).await {
        ServerMessage::ProtocolInfo(info) => {
            assert_eq!(info.protocol_version, Some(3));
            assert_eq!(info.min_protocol_version, Some(2));
            assert_eq!(info.max_protocol_version, Some(3));
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
}

#[tokio::test]
async fn v2_client_omitting_fields_is_recorded_as_v2() {
    let addr = start_auth_server().await;
    let mut ws = connect(addr, "/v2/ws").await;

    let auth = ClientMessage::Authenticate {
        app_id: APP_ID.to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };

    match authenticate(&mut ws, auth).await {
        ServerMessage::ProtocolInfo(info) => {
            assert_eq!(info.protocol_version, None);
            assert_eq!(info.min_protocol_version, None);
            assert_eq!(info.max_protocol_version, None);
            let value = serde_json::to_value(&info).expect("serializes");
            assert!(
                value.get("protocol_version").is_none()
                    && value.get("min_protocol_version").is_none()
                    && value.get("max_protocol_version").is_none(),
                "negotiated v2 ProtocolInfo must not serialize v3-only keys: {value}"
            );
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
}

#[tokio::test]
async fn v3_ws_alias_defaults_to_v3_when_client_omits_version() {
    let addr = start_auth_server().await;
    // Connect to the /v3/ws alias and omit protocol_version entirely.
    let mut ws = connect(addr, "/v3/ws").await;

    let auth = ClientMessage::Authenticate {
        app_id: APP_ID.to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };

    match authenticate(&mut ws, auth).await {
        ServerMessage::ProtocolInfo(info) => {
            assert_eq!(
                info.protocol_version,
                Some(3),
                "/v3/ws path should default the omitted version to 3"
            );
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
}

#[tokio::test]
async fn v3_ws_alias_respects_explicit_client_version_over_path_default() {
    let addr = start_auth_server().await;
    // Connect to /v3/ws but explicitly advertise v2: the explicit client value
    // must take precedence over the path default (3).
    let mut ws = connect(addr, "/v3/ws").await;

    let auth = ClientMessage::Authenticate {
        app_id: APP_ID.to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: Some(2),
        supported_transports: None,
        supported_topologies: None,
    };

    match authenticate(&mut ws, auth).await {
        ServerMessage::ProtocolInfo(info) => {
            assert_eq!(
                info.protocol_version, None,
                "explicit protocol_version:2 must beat the /v3/ws path default"
            );
            assert_eq!(info.min_protocol_version, None);
            assert_eq!(info.max_protocol_version, None);
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
}

#[tokio::test]
async fn v2_ws_alias_defaults_to_v2_when_client_omits_version() {
    let addr = start_auth_server().await;
    let mut ws = connect(addr, "/v2/ws").await;

    let auth = ClientMessage::Authenticate {
        app_id: APP_ID.to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };

    match authenticate(&mut ws, auth).await {
        ServerMessage::ProtocolInfo(info) => {
            assert_eq!(info.protocol_version, None);
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
}

#[tokio::test]
async fn auth_disabled_v3_ws_authenticate_still_negotiates_v3_webrtc() {
    let addr = start_auth_disabled_server().await;
    let mut ws = connect(addr, "/v3/ws").await;

    let auth = ClientMessage::Authenticate {
        app_id: APP_ID.to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: None,
        supported_transports: Some(vec![Transport::Relay, Transport::WebRtc]),
        supported_topologies: Some(vec![Topology::Relay, Topology::Mesh]),
    };

    match authenticate(&mut ws, auth).await {
        ServerMessage::ProtocolInfo(info) => {
            assert_eq!(
                info.protocol_version,
                Some(3),
                "auth-disabled /v3/ws Authenticate must still apply the path default"
            );
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
}

#[tokio::test]
async fn auth_disabled_v3_ws_respects_explicit_v2_without_version_fields() {
    let addr = start_auth_disabled_server().await;
    let mut ws = connect(addr, "/v3/ws").await;

    let auth = ClientMessage::Authenticate {
        app_id: APP_ID.to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: Some(2),
        supported_transports: Some(vec![Transport::Relay, Transport::WebRtc]),
        supported_topologies: Some(vec![Topology::Relay, Topology::Mesh]),
    };

    match authenticate(&mut ws, auth).await {
        ServerMessage::ProtocolInfo(info) => {
            assert_eq!(info.protocol_version, None);
            assert_eq!(info.min_protocol_version, None);
            assert_eq!(info.max_protocol_version, None);
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
}
