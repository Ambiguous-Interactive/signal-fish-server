use crate::security::ClientCertificateFingerprint;
use crate::server::EnhancedGameServer;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Extension, State};
use axum::http::HeaderMap;
use axum::response::Response;
use std::net::SocketAddr;
use std::sync::Arc;

use super::connection::handle_socket;
use super::token_binding::{client_requested_subprotocol, negotiate_token_binding};

/// WebSocket handler for the game protocol
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(server): State<Arc<EnhancedGameServer>>,
    headers: HeaderMap,
    fingerprint: Option<Extension<ClientCertificateFingerprint>>,
) -> Response {
    let token_binding_cfg = server.token_binding_config().clone();
    let client_offered_binding =
        client_requested_subprotocol(&headers, &token_binding_cfg.subprotocol);
    let client_fingerprint = fingerprint.map(|Extension(fp)| fp);

    let binding_session = match negotiate_token_binding(
        &token_binding_cfg,
        client_offered_binding,
        &headers,
        client_fingerprint.as_ref(),
    ) {
        Ok(session) => session,
        Err(response) => return response,
    };

    let upgrade = if token_binding_cfg.enabled && client_offered_binding {
        ws.protocols([token_binding_cfg.subprotocol])
    } else {
        ws
    };

    upgrade.on_upgrade(move |socket| handle_socket(socket, server, addr, binding_session))
}
