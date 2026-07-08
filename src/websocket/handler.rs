use crate::security::ClientCertificateFingerprint;
use crate::server::EnhancedGameServer;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;
use std::sync::Arc;

use super::connection::handle_socket;
use super::token_binding::{client_requested_subprotocol, negotiate_token_binding};

/// WebSocket handler for the `/v2/ws` endpoint.
///
/// Clients that omit `Authenticate.protocol_version` on this path are treated as
/// pure v2 (the negotiation floor).
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    connect_info: ConnectInfo<SocketAddr>,
    state: State<Arc<EnhancedGameServer>>,
    headers: HeaderMap,
    fingerprint: Option<Extension<ClientCertificateFingerprint>>,
) -> Response {
    websocket_handler_with_default(ws, connect_info, state, headers, fingerprint, 2).await
}

/// WebSocket handler for the `/v3/ws` alias.
///
/// Shares the exact same connection loop as `/v2/ws`; the only difference is the
/// fallback protocol version used when the client omits `protocol_version`. The
/// server still clamps the result into the configured `[min, max]` range, so this
/// alias never forces a version the deployment does not support.
pub async fn websocket_handler_v3(
    ws: WebSocketUpgrade,
    connect_info: ConnectInfo<SocketAddr>,
    state: State<Arc<EnhancedGameServer>>,
    headers: HeaderMap,
    fingerprint: Option<Extension<ClientCertificateFingerprint>>,
) -> Response {
    websocket_handler_with_default(ws, connect_info, state, headers, fingerprint, 3).await
}

async fn websocket_handler_with_default(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(server): State<Arc<EnhancedGameServer>>,
    headers: HeaderMap,
    fingerprint: Option<Extension<ClientCertificateFingerprint>>,
    default_protocol_version: u16,
) -> Response {
    if server.is_draining() {
        return (StatusCode::SERVICE_UNAVAILABLE, "server is draining").into_response();
    }

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

    // Transport-layer frame/message cap. The application-level
    // `security.max_message_size` check in the receive loop can only run after
    // the WebSocket library has buffered an entire inbound message, so without
    // this cap the library defaults (16 MiB frames / 64 MiB messages) let an
    // unauthenticated peer force megabytes of buffering per connection before
    // the polite `MessageTooLarge` rejection executes. The 2x headroom keeps
    // the application check the authority for the polite error path
    // (slightly-oversized messages still get an explicit error frame on a
    // surviving connection); only grossly oversized frames die here.
    let transport_cap = server.config().max_message_size.saturating_mul(2);
    let ws = ws
        .max_frame_size(transport_cap)
        .max_message_size(transport_cap);

    let upgrade = if token_binding_cfg.enabled && client_offered_binding {
        ws.protocols([token_binding_cfg.subprotocol])
    } else {
        ws
    };

    upgrade.on_upgrade(move |socket| {
        handle_socket(
            socket,
            server,
            addr,
            binding_session,
            default_protocol_version,
        )
    })
}
