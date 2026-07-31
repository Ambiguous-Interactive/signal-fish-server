//! End-to-end protocol v3 (P1) negotiation tests through the real WebSocket
//! stack: a client advertising `protocol_version: 3` is negotiated to v3 and
//! sees `ProtocolInfo.protocol_version == Some(3)`, while a client omitting the
//! fields is recorded as v2 / relay-only. Also exercises the `/v3/ws` alias
//! default.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::{SinkExt, StreamExt};
use signal_fish_server::config::AppAuthEntry;
use signal_fish_server::protocol::{
    ClientMessage, ServerMessage, Topology, Transport, PROTOCOL_INFO_TRANSPORT_WEBSOCKET,
};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::{create_router, websocket_handler_v3};
use std::sync::Arc;
use test_helpers::{test_protocol_config, test_server_config, RunningTestServer};
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

async fn start_auth_server() -> RunningTestServer {
    start_server_with_auth_and_stats(true, 0).await
}

async fn start_auth_disabled_server() -> RunningTestServer {
    start_server_with_auth_and_stats(false, 0).await
}

async fn start_server_with_auth_and_stats(
    auth_enabled: bool,
    delivery_stats_interval_secs: u64,
) -> RunningTestServer {
    let mut server_config: ServerConfig = test_server_config();
    server_config.auth_enabled = auth_enabled;
    server_config.websocket_config.delivery_stats_interval_secs = delivery_stats_interval_secs;

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
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        auth_enabled.then(app_entry).into_iter().collect(),
    )
    .await
    .expect("server builds");

    start_server(game_server).await
}

async fn start_server(game_server: Arc<EnhancedGameServer>) -> RunningTestServer {
    use axum::routing::get;

    // Mirror main.rs wiring: enhanced router nested under /v2, plus a top-level
    // /v3/ws alias sharing the same connection handler.
    let enhanced_router = create_router("http://localhost:3000").with_state(game_server.clone());
    let combined_router = axum::Router::new()
        .nest("/v2", enhanced_router)
        .route("/v3/ws", get(websocket_handler_v3))
        .fallback(|| async { "Use /v2/ws or /v3/ws" })
        .with_state(game_server.clone());

    RunningTestServer::spawn(game_server, combined_router).await
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
    let running_server = start_auth_server().await;
    let addr = running_server.addr();
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
            assert_eq!(
                info.max_protocol_version,
                Some(3),
                "default deployment ceiling is v3"
            );
            assert_eq!(
                info.transports,
                Some(vec![PROTOCOL_INFO_TRANSPORT_WEBSOCKET.to_string()])
            );
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn delivery_advisories_wait_for_authenticated_protocol_info() {
    for auth_enabled in [true, false] {
        let running_server = start_server_with_auth_and_stats(auth_enabled, 1).await;
        let addr = running_server.addr();
        let mut ws = connect(addr, "/v3/ws").await;

        let early_message =
            tokio::time::timeout(tokio::time::Duration::from_millis(1_200), ws.next()).await;
        assert!(
            early_message.is_err(),
            "auth_enabled={auth_enabled}: delivery advisory arrived before optional Authenticate"
        );

        match authenticate(&mut ws, version_only_auth(Some(3))).await {
            ServerMessage::ProtocolInfo(info) => assert_eq!(info.protocol_version, Some(3)),
            other => panic!("expected ProtocolInfo, got {other:?}"),
        }

        assert!(
            matches!(
                next_server_message(&mut ws).await,
                ServerMessage::RelayStats { .. }
            ),
            "auth_enabled={auth_enabled}: expected RelayStats after ProtocolInfo"
        );
        match next_server_message(&mut ws).await {
            ServerMessage::DeliveryReport(report) => assert!(report.gaps.is_empty()),
            other => panic!(
                "auth_enabled={auth_enabled}: expected trailing counter snapshot, got {other:?}"
            ),
        }
        running_server.shutdown().await;
    }
}

#[tokio::test]
async fn auth_disabled_endpoint_default_starts_advisories_after_first_application_baseline() {
    let running_server = start_server_with_auth_and_stats(false, 1).await;
    let addr = running_server.addr();
    let mut ws = connect(addr, "/v3/ws").await;
    ws.send(Message::Text(
        serde_json::to_string(&ClientMessage::Ping).unwrap().into(),
    ))
    .await
    .unwrap();

    assert!(matches!(
        next_server_message(&mut ws).await,
        ServerMessage::Pong
    ));
    assert!(matches!(
        next_server_message(&mut ws).await,
        ServerMessage::RelayStats { .. }
    ));
    match next_server_message(&mut ws).await {
        ServerMessage::DeliveryReport(report) => assert!(report.gaps.is_empty()),
        other => panic!("expected trailing counter snapshot, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn auth_disabled_binary_rejection_starts_endpoint_default_advisories() {
    let running_server = start_server_with_auth_and_stats(false, 1).await;
    let addr = running_server.addr();
    let mut ws = connect(addr, "/v3/ws").await;
    ws.send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();

    match next_server_message(&mut ws).await {
        ServerMessage::Error { error_code, .. } => {
            assert_eq!(
                error_code,
                Some(signal_fish_server::protocol::ErrorCode::InvalidInput)
            );
        }
        other => panic!("expected binary-format rejection, got {other:?}"),
    }
    assert!(matches!(
        next_server_message(&mut ws).await,
        ServerMessage::RelayStats { .. }
    ));
    match next_server_message(&mut ws).await {
        ServerMessage::DeliveryReport(report) => assert!(report.gaps.is_empty()),
        other => panic!("expected trailing counter snapshot, got {other:?}"),
    }
    running_server.shutdown().await;
}

// ---------------------------------------------------------------------------
// Protocol v3: the [2, 3] clamp matrix end-to-end (matches the config-level
// matrix in tests/protocol_v3_negotiation.rs::v3_negotiation_clamp_matrix).
// ---------------------------------------------------------------------------

/// Build an Authenticate message advertising only `protocol_version`.
fn version_only_auth(protocol_version: Option<u16>) -> ClientMessage {
    ClientMessage::Authenticate {
        app_id: APP_ID.to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version,
        supported_transports: None,
        supported_topologies: None,
    }
}

#[tokio::test]
async fn v3_client_negotiates_v3_on_default_server() {
    let running_server = start_auth_server().await;
    let addr = running_server.addr();
    let mut ws = connect(addr, "/v2/ws").await;

    match authenticate(&mut ws, version_only_auth(Some(3))).await {
        ServerMessage::ProtocolInfo(info) => {
            assert_eq!(info.protocol_version, Some(3), "client asks 3 => gets 3");
            assert_eq!(info.min_protocol_version, Some(2));
            assert_eq!(info.max_protocol_version, Some(3));
            assert_eq!(
                info.transports,
                Some(vec![PROTOCOL_INFO_TRANSPORT_WEBSOCKET.to_string()])
            );
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn future_v4_client_is_clamped_to_v3() {
    // A stale v3-era (or future) client that still advertises 4/5 negotiates
    // down to the build ceiling (3) — v4+ is not a negotiated version.
    let running_server = start_auth_server().await;
    let addr = running_server.addr();
    let mut ws = connect(addr, "/v2/ws").await;

    for asked in [Some(4), Some(5)] {
        match authenticate(&mut ws, version_only_auth(asked)).await {
            ServerMessage::ProtocolInfo(info) => {
                assert_eq!(
                    info.protocol_version,
                    Some(3),
                    "client asks {asked:?} => clamped down to the build ceiling (3)"
                );
                assert_eq!(
                    info.transports,
                    Some(vec![PROTOCOL_INFO_TRANSPORT_WEBSOCKET.to_string()])
                );
            }
            other => panic!("expected ProtocolInfo, got {other:?}"),
        }
        ws = connect(addr, "/v2/ws").await;
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn v2_client_stays_v2_on_default_server() {
    let running_server = start_auth_server().await;
    let addr = running_server.addr();
    let mut ws = connect(addr, "/v2/ws").await;

    match authenticate(&mut ws, version_only_auth(Some(2))).await {
        ServerMessage::ProtocolInfo(info) => {
            // A v2-negotiated client gets the FROZEN v2 ProtocolInfo: the v3
            // version-negotiation fields are additive (gated on negotiated >= 3)
            // and so are all absent. `None` here is exactly "not upgraded" —
            // a v3 client would instead see `protocol_version = Some(3)`.
            assert_eq!(
                info.protocol_version, None,
                "v3 is opt-in: a v2 client is never upgraded (no version echo)"
            );
            assert_eq!(info.min_protocol_version, None);
            assert_eq!(info.max_protocol_version, None);
            assert_eq!(info.transports, None);
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn v3_client_is_clamped_to_v2_when_deployment_caps_at_v2() {
    // Deployment clamps `protocol.max_protocol_version` back to 2 (pure v2): a
    // v3 client is negotiated down to 2 and told so via ProtocolInfo.
    let mut server_config: ServerConfig = test_server_config();
    server_config.auth_enabled = true;

    let mut protocol_config = test_protocol_config();
    protocol_config.sdk_compatibility.enforce = false;
    protocol_config.max_protocol_version = 2;

    let game_server = EnhancedGameServer::new(
        server_config,
        protocol_config,
        signal_fish_server::config::RelayTypeConfig::default(),
        signal_fish_server::config::SessionConfig::default(),
        signal_fish_server::config::TurnConfig::default(),
        signal_fish_server::database::DatabaseConfig::InMemory,
        signal_fish_server::config::MetricsConfig::default(),
        signal_fish_server::config::CoordinationConfig::default(),
        signal_fish_server::config::TransportSecurityConfig::default(),
        vec![app_entry()],
    )
    .await
    .expect("server builds");
    let running_server = start_server(game_server).await;
    let addr = running_server.addr();

    let mut ws = connect(addr, "/v2/ws").await;
    match authenticate(&mut ws, version_only_auth(Some(3))).await {
        ServerMessage::ProtocolInfo(info) => {
            // Clamped down to 2 (< 3), so the client is served the frozen v2
            // ProtocolInfo with no version fields — observably distinct from an
            // un-clamped v3 client, which would see `protocol_version = Some(3)`.
            assert_eq!(
                info.protocol_version, None,
                "config-clamped server negotiates a v3 client down to 2 (frozen v2 ProtocolInfo)"
            );
            assert_eq!(info.max_protocol_version, None);
            assert_eq!(info.transports, None);
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn v2_client_omitting_fields_is_recorded_as_v2() {
    let running_server = start_auth_server().await;
    let addr = running_server.addr();
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
            assert_eq!(info.transports, None);
            let value = serde_json::to_value(&info).expect("serializes");
            assert!(
                value.get("protocol_version").is_none()
                    && value.get("min_protocol_version").is_none()
                    && value.get("max_protocol_version").is_none()
                    && value.get("transports").is_none(),
                "negotiated v2 ProtocolInfo must not serialize v3-only keys: {value}"
            );
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn v3_ws_alias_defaults_to_v3_when_client_omits_version() {
    let running_server = start_auth_server().await;
    let addr = running_server.addr();
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
            assert_eq!(
                info.transports,
                Some(vec![PROTOCOL_INFO_TRANSPORT_WEBSOCKET.to_string()])
            );
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn v3_ws_alias_respects_explicit_client_version_over_path_default() {
    let running_server = start_auth_server().await;
    let addr = running_server.addr();
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
            assert_eq!(info.transports, None);
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn v2_ws_alias_defaults_to_v2_when_client_omits_version() {
    let running_server = start_auth_server().await;
    let addr = running_server.addr();
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
            assert_eq!(info.transports, None);
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn auth_disabled_v3_ws_authenticate_still_negotiates_v3_webrtc() {
    let running_server = start_auth_disabled_server().await;
    let addr = running_server.addr();
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
            assert_eq!(
                info.transports,
                Some(vec![PROTOCOL_INFO_TRANSPORT_WEBSOCKET.to_string()])
            );
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn auth_disabled_v3_ws_respects_explicit_v2_without_version_fields() {
    let running_server = start_auth_disabled_server().await;
    let addr = running_server.addr();
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
            assert_eq!(info.transports, None);
        }
        other => panic!("expected ProtocolInfo, got {other:?}"),
    }
    running_server.shutdown().await;
}
