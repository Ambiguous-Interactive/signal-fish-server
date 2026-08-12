#[cfg(feature = "tls")]
use crate::security::VerifiedClientCertificate;
use crate::security::{ClientCertificateFingerprint, OriginPolicy};
use crate::server::EnhancedGameServer;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;
use std::sync::Arc;

use super::connection::handle_socket;
use super::token_binding::{
    client_token_binding_offer, negotiate_token_binding, TokenBindingProtocolOffer,
};

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
    origin_policy: Extension<OriginPolicy>,
) -> Response {
    websocket_handler_with_default(
        ws,
        connect_info,
        state,
        headers,
        fingerprint,
        #[cfg(feature = "tls")]
        None,
        origin_policy,
        2,
    )
    .await
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
    origin_policy: Extension<OriginPolicy>,
) -> Response {
    websocket_handler_with_default(
        ws,
        connect_info,
        state,
        headers,
        fingerprint,
        #[cfg(feature = "tls")]
        None,
        origin_policy,
        3,
    )
    .await
}

/// Listener-integrated v2 handler that consumes rustls-authenticated peer
/// certificate metadata. Kept crate-private so the public handler signature
/// remains source-compatible for library callers.
#[cfg(feature = "tls")]
pub(crate) async fn websocket_handler_with_verified_certificate(
    ws: WebSocketUpgrade,
    connect_info: ConnectInfo<SocketAddr>,
    state: State<Arc<EnhancedGameServer>>,
    headers: HeaderMap,
    fingerprint: Option<Extension<ClientCertificateFingerprint>>,
    verified_certificate: Option<Extension<VerifiedClientCertificate>>,
    origin_policy: Extension<OriginPolicy>,
) -> Response {
    websocket_handler_with_default(
        ws,
        connect_info,
        state,
        headers,
        fingerprint,
        verified_certificate,
        origin_policy,
        2,
    )
    .await
}

/// Listener-integrated v3 handler; see
/// [`websocket_handler_with_verified_certificate`].
#[cfg(feature = "tls")]
pub(crate) async fn websocket_handler_v3_with_verified_certificate(
    ws: WebSocketUpgrade,
    connect_info: ConnectInfo<SocketAddr>,
    state: State<Arc<EnhancedGameServer>>,
    headers: HeaderMap,
    fingerprint: Option<Extension<ClientCertificateFingerprint>>,
    verified_certificate: Option<Extension<VerifiedClientCertificate>>,
    origin_policy: Extension<OriginPolicy>,
) -> Response {
    websocket_handler_with_default(
        ws,
        connect_info,
        state,
        headers,
        fingerprint,
        verified_certificate,
        origin_policy,
        3,
    )
    .await
}

async fn websocket_handler_with_default(
    ws: WebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(server): State<Arc<EnhancedGameServer>>,
    headers: HeaderMap,
    fingerprint: Option<Extension<ClientCertificateFingerprint>>,
    #[cfg(feature = "tls")] verified_certificate: Option<Extension<VerifiedClientCertificate>>,
    Extension(origin_policy): Extension<OriginPolicy>,
    default_protocol_version: u16,
) -> Response {
    if !origin_policy.allows_upgrade(&headers) {
        tracing::warn!(client_ip = %addr.ip(), "WebSocket upgrade rejected: Origin not allowed");
        return (StatusCode::FORBIDDEN, "WebSocket Origin is not allowed").into_response();
    }

    if server.is_draining() {
        return (StatusCode::SERVICE_UNAVAILABLE, "server is draining").into_response();
    }

    let token_binding_cfg = server.token_binding_config().clone();
    let binding_offer = client_token_binding_offer(&headers, &token_binding_cfg.subprotocol);
    if rejects_unsupported_token_binding_offer(&token_binding_cfg, binding_offer) {
        tracing::warn!(client_ip = %addr.ip(), "Unsupported reserved token-binding subprotocol");
        return (
            StatusCode::BAD_REQUEST,
            "unsupported token binding subprotocol",
        )
            .into_response();
    }
    let client_offered_binding = binding_offer == TokenBindingProtocolOffer::Supported;
    let client_fingerprint = fingerprint.map(|Extension(fp)| fp);
    #[cfg(feature = "tls")]
    let client_fingerprint = match verified_certificate {
        Some(Extension(VerifiedClientCertificate(verified))) => verified,
        None => client_fingerprint,
    };

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
    // pre-handshake peer force megabytes of buffering per connection before
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

fn rejects_unsupported_token_binding_offer(
    config: &crate::config::TokenBindingConfig,
    offer: TokenBindingProtocolOffer,
) -> bool {
    config.enabled && offer == TokenBindingProtocolOffer::Unsupported
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_token_binding_versions_only_fail_closed_when_binding_is_enabled() {
        let disabled = crate::config::TokenBindingConfig::default();
        assert!(!disabled.enabled);
        assert!(!rejects_unsupported_token_binding_offer(
            &disabled,
            TokenBindingProtocolOffer::Unsupported
        ));

        let enabled = crate::config::TokenBindingConfig {
            enabled: true,
            ..disabled
        };
        assert!(rejects_unsupported_token_binding_offer(
            &enabled,
            TokenBindingProtocolOffer::Unsupported
        ));
        assert!(!rejects_unsupported_token_binding_offer(
            &enabled,
            TokenBindingProtocolOffer::Supported
        ));
    }
}
