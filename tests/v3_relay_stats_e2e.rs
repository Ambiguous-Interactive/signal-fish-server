//! End-to-end protocol v3 `RelayStats` emission tests through the real
//! WebSocket stack.
//!
//! The contract under test (config-gated, default OFF):
//!
//! - with `websocket.delivery_stats_interval_secs > 0`, a connection that
//!   negotiated v3 receives periodic `RelayStats` frames with plausible
//!   cumulative fields;
//! - a pre-v3 (v2) connection on the SAME deployment never receives one (the
//!   v3 gate is enforced at emission);
//! - with the default config (interval 0), nobody receives one — not even a
//!   v3 connection.

mod test_helpers;
mod websocket_test_helpers;

use futures_util::SinkExt;
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::{ClientMessage, ServerMessage};
use signal_fish_server::server::EnhancedGameServer;
use signal_fish_server::websocket::create_router;
use std::sync::Arc;
use test_helpers::{create_test_server_with_config, test_server_config, RunningTestServer};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use websocket_test_helpers::{
    maybe_next_matching_server_message_within, next_matching_server_message_within, WsStream,
};

const SERVER_MESSAGE_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(10);

/// Absence window for the negative cases: several times the 1s emission
/// interval, so an erroneous emission would land well inside it.
const ABSENCE_WINDOW: tokio::time::Duration = tokio::time::Duration::from_millis(2_500);

fn v3_protocol_config() -> ProtocolConfig {
    let mut protocol_config = ProtocolConfig::default();
    protocol_config.sdk_compatibility.enforce = false;
    protocol_config
}

async fn start_test_server(
    delivery_stats_interval_secs: u64,
) -> (RunningTestServer, Arc<EnhancedGameServer>) {
    let mut server_config = test_server_config();
    server_config.websocket_config.delivery_stats_interval_secs = delivery_stats_interval_secs;
    let server = create_test_server_with_config(server_config, v3_protocol_config()).await;

    let router = create_router("http://localhost:3000").with_state(server.clone());
    let running_server = RunningTestServer::spawn(server.clone(), router).await;

    (running_server, server)
}

async fn connect(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = tokio::time::timeout(tokio::time::Duration::from_secs(10), connect_async(&url))
        .await
        .expect("websocket connect timed out")
        .expect("websocket connect failed");
    ws
}

/// Authenticate advertising `protocol_version`; drains the Authenticated +
/// ProtocolInfo handshake.
async fn authenticate(ws: &mut WsStream, protocol_version: u16) {
    let auth = ClientMessage::Authenticate {
        app_id: "v3-relay-stats-test".to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: Some(protocol_version),
        supported_transports: None,
        supported_topologies: None,
    };
    let json = serde_json::to_string(&auth).expect("serialize Authenticate");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send Authenticate");

    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "Authenticated response",
        |message| matches!(message, ServerMessage::Authenticated { .. }).then_some(()),
    )
    .await;
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "ProtocolInfo response",
        |message| matches!(message, ServerMessage::ProtocolInfo(_)).then_some(()),
    )
    .await;
}

#[tokio::test]
async fn v3_client_receives_periodic_relay_stats_when_enabled() {
    let (running_server, _server) = start_test_server(1).await;
    let addr = running_server.addr();
    let mut ws = connect(addr).await;
    authenticate(&mut ws, 3).await;

    // Two consecutive frames prove periodic emission (not a one-shot), and
    // the cumulative counters must be plausible and monotonic.
    let mut previous_sent = 0u64;
    for nth in 0..2 {
        let (interval_ms, sent_to_you, dropped_for_you, backpressure_events) =
            next_matching_server_message_within(
                &mut ws,
                SERVER_MESSAGE_TIMEOUT,
                "periodic RelayStats frame",
                |message| match message {
                    ServerMessage::RelayStats {
                        interval_ms,
                        sent_to_you,
                        dropped_for_you,
                        backpressure_events,
                    } => Some((
                        interval_ms,
                        sent_to_you,
                        dropped_for_you,
                        backpressure_events,
                    )),
                    _ => None,
                },
            )
            .await;

        assert_eq!(
            interval_ms, 1_000,
            "interval_ms must echo the configured interval"
        );
        // The Authenticated + ProtocolInfo responses ride the reliable
        // delivery path, so the cumulative ledger has counted them.
        assert!(
            sent_to_you >= 2,
            "cumulative sent_to_you must count the handshake frames (frame {nth}: {sent_to_you})"
        );
        assert!(
            sent_to_you >= previous_sent,
            "cumulative counters must be monotonic"
        );
        previous_sent = sent_to_you;
        assert_eq!(dropped_for_you, 0, "a healthy idle connection has no drops");
        assert_eq!(
            backpressure_events, 0,
            "a healthy idle connection has no backpressure"
        );
    }
    running_server.shutdown().await;
}

#[tokio::test]
async fn v2_client_never_receives_relay_stats_even_when_enabled() {
    let (running_server, _server) = start_test_server(1).await;
    let addr = running_server.addr();
    let mut ws = connect(addr).await;
    authenticate(&mut ws, 2).await;

    let stray = maybe_next_matching_server_message_within(
        &mut ws,
        ABSENCE_WINDOW,
        "RelayStats absence for a v2 connection",
        |message| match message {
            ServerMessage::RelayStats { .. } => Some(()),
            _ => None,
        },
    )
    .await;
    assert!(
        stray.is_none(),
        "RelayStats is part of the v3 reliability surface and must never be \
         emitted to a pre-v3 (v2) connection"
    );
    running_server.shutdown().await;
}

#[tokio::test]
async fn nobody_receives_relay_stats_with_default_config() {
    // Default config: delivery_stats_interval_secs = 0 (disabled).
    let (running_server, _server) = start_test_server(0).await;
    let addr = running_server.addr();
    let mut ws = connect(addr).await;
    authenticate(&mut ws, 3).await;

    let stray = maybe_next_matching_server_message_within(
        &mut ws,
        ABSENCE_WINDOW,
        "RelayStats absence with emission disabled",
        |message| match message {
            ServerMessage::RelayStats { .. } => Some(()),
            _ => None,
        },
    )
    .await;
    assert!(
        stray.is_none(),
        "the default configuration must emit no RelayStats frames at all"
    );
    running_server.shutdown().await;
}
