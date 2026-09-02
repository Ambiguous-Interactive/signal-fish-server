#[cfg(feature = "tls")]
use crate::security::VerifiedClientCertificate;
use crate::security::{ClientCertificateFingerprint, OriginPolicy};
use crate::server::EnhancedGameServer;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Extension, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;
use std::sync::Arc;

use super::connection::handle_socket;
use super::token_binding::{
    client_token_binding_offer, negotiate_token_binding, TokenBindingProtocolOffer,
};

/// Correlation identifier returned on every application-handled WebSocket
/// upgrade response, including successful `101` responses and deliberate HTTP
/// rejections. Reverse proxies pass this response header through by default, so
/// one client observation can be joined to both proxy and server logs.
pub const WEBSOCKET_REQUEST_ID_HEADER: &str = "x-signal-fish-request-id";

/// Machine-readable application outcome returned beside
/// [`WEBSOCKET_REQUEST_ID_HEADER`]. A response without these headers does not
/// prove that it completed this handler: it may have failed earlier (including
/// during framework extraction) or an intermediary may have stripped the
/// response headers. Correlate it with listener and reverse-proxy evidence.
pub const WEBSOCKET_UPGRADE_OUTCOME_HEADER: &str = "x-signal-fish-upgrade-outcome";

/// Maximum aggregate server-to-client WebSocket application payload, in
/// bytes, for this deployment. Native clients can discover the limit from the
/// HTTP upgrade response; protocol-v3 clients also receive it in
/// `ProtocolInfo` for browser compatibility.
pub const WEBSOCKET_MAX_OUTBOUND_MESSAGE_SIZE_HEADER: &str =
    "x-signal-fish-max-outbound-message-size";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpgradeOutcome {
    Accepted,
    RejectedOrigin,
    RejectedDraining,
    RejectedTokenBindingOffer,
    RejectedTokenBindingNegotiation,
    /// Server-fault rejection (HTTP 5xx): the upgrade was refused because of a
    /// server-side condition, not client input. Kept distinct from the
    /// client-fault negotiation lane so a config regression or CSPRNG failure
    /// cannot masquerade as (or inflate) a client-attack signal in per-peer
    /// rejection windows and dashboards.
    RejectedServerFault,
}

impl UpgradeOutcome {
    const fn header_value(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::RejectedOrigin => "rejected_origin",
            Self::RejectedDraining => "rejected_draining",
            Self::RejectedTokenBindingOffer => "rejected_token_binding_offer",
            Self::RejectedTokenBindingNegotiation => "rejected_token_binding_negotiation",
            Self::RejectedServerFault => "rejected_server_fault",
        }
    }

    const fn metric(self) -> crate::metrics::WebSocketUpgradeOutcome {
        match self {
            Self::Accepted => crate::metrics::WebSocketUpgradeOutcome::Accepted,
            Self::RejectedOrigin => crate::metrics::WebSocketUpgradeOutcome::RejectedOrigin,
            Self::RejectedDraining => crate::metrics::WebSocketUpgradeOutcome::RejectedDraining,
            Self::RejectedTokenBindingOffer => {
                crate::metrics::WebSocketUpgradeOutcome::RejectedTokenBindingOffer
            }
            Self::RejectedTokenBindingNegotiation => {
                crate::metrics::WebSocketUpgradeOutcome::RejectedTokenBindingNegotiation
            }
            Self::RejectedServerFault => {
                crate::metrics::WebSocketUpgradeOutcome::RejectedServerFault
            }
        }
    }
}

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
    let request_id = uuid::Uuid::new_v4().to_string();
    server.metrics().increment_websocket_upgrade_attempts();

    if !origin_policy.allows_upgrade(&headers) {
        return finish_upgrade_response(
            &server,
            addr,
            &request_id,
            UpgradeOutcome::RejectedOrigin,
            (StatusCode::FORBIDDEN, "WebSocket Origin is not allowed").into_response(),
        );
    }

    if server.is_draining() {
        return finish_upgrade_response(
            &server,
            addr,
            &request_id,
            UpgradeOutcome::RejectedDraining,
            (StatusCode::SERVICE_UNAVAILABLE, "server is draining").into_response(),
        );
    }

    let token_binding_cfg = server.token_binding_config().clone();
    let binding_offer = client_token_binding_offer(&headers, &token_binding_cfg.subprotocol);
    if rejects_unsupported_token_binding_offer(&token_binding_cfg, binding_offer) {
        return finish_upgrade_response(
            &server,
            addr,
            &request_id,
            UpgradeOutcome::RejectedTokenBindingOffer,
            (
                StatusCode::BAD_REQUEST,
                "unsupported token binding subprotocol",
            )
                .into_response(),
        );
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
        Err(response) => {
            let outcome = negotiation_outcome_for(response.status());
            return finish_upgrade_response(&server, addr, &request_id, outcome, response);
        }
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

    let socket_server = Arc::clone(&server);
    let failure_server = Arc::clone(&server);
    let failure_request_id = request_id.clone();
    let response = upgrade
        .on_failed_upgrade(move |error| {
            // The 101 + correlation headers were already sent, but the socket
            // handover failed (transport died before hyper yielded the upgraded
            // stream). Without this signal the accepted lane claims a
            // connection that never existed.
            failure_server
                .metrics()
                .increment_websocket_upgrades_failed_after_accept();
            tracing::warn!(
                request_id = %failure_request_id,
                peer_ip = %addr.ip(),
                %error,
                "WebSocket upgrade failed after the 101 response; the accepted \
                 upgrade never became a socket"
            );
        })
        .on_upgrade(move |socket| {
            handle_socket(
                socket,
                socket_server,
                addr,
                binding_session,
                default_protocol_version,
            )
        });
    finish_upgrade_response(
        &server,
        addr,
        &request_id,
        UpgradeOutcome::Accepted,
        response,
    )
}

/// Classify a refused token-binding negotiation by fault owner: 5xx responses
/// are server-fault conditions (an invalid config that bypassed process
/// validation, a CSPRNG failure), everything else accuses the client's offer.
/// Misattributing a server fault to the client-fault lane would shape
/// per-peer rejection windows as if the clients were attacking.
fn negotiation_outcome_for(status: StatusCode) -> UpgradeOutcome {
    if status.is_server_error() {
        UpgradeOutcome::RejectedServerFault
    } else {
        UpgradeOutcome::RejectedTokenBindingNegotiation
    }
}

fn finish_upgrade_response(
    server: &EnhancedGameServer,
    addr: SocketAddr,
    request_id: &str,
    outcome: UpgradeOutcome,
    mut response: Response,
) -> Response {
    server
        .metrics()
        .record_websocket_upgrade_outcome(outcome.metric());

    if let Ok(request_id) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(
            HeaderName::from_static(WEBSOCKET_REQUEST_ID_HEADER),
            request_id,
        );
    }
    response.headers_mut().insert(
        HeaderName::from_static(WEBSOCKET_UPGRADE_OUTCOME_HEADER),
        HeaderValue::from_static(outcome.header_value()),
    );
    if let Ok(max_outbound_message_size) =
        HeaderValue::from_str(&server.config().max_outbound_message_size.to_string())
    {
        response.headers_mut().insert(
            HeaderName::from_static(WEBSOCKET_MAX_OUTBOUND_MESSAGE_SIZE_HEADER),
            max_outbound_message_size,
        );
    }

    if outcome == UpgradeOutcome::Accepted {
        tracing::info!(
            request_id,
            peer_ip = %addr.ip(),
            outcome = outcome.header_value(),
            http_status = response.status().as_u16(),
            "WebSocket upgrade accepted"
        );
    } else {
        server.upgrade_rejection_log().record(
            addr.ip(),
            outcome.header_value(),
            request_id,
            response.status().as_u16(),
        );
    }

    response
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
    fn negotiation_failures_split_by_fault_owner() {
        for status in [
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert_eq!(
                negotiation_outcome_for(status),
                UpgradeOutcome::RejectedServerFault,
                "{status} is a server fault, not a client-fault negotiation rejection"
            );
        }
        for status in [StatusCode::BAD_REQUEST, StatusCode::UNAUTHORIZED] {
            assert_eq!(
                negotiation_outcome_for(status),
                UpgradeOutcome::RejectedTokenBindingNegotiation,
                "{status} accuses the client's offer"
            );
        }
    }

    /// A server-fault rejection must land in its own outcome lane (header +
    /// counter), never in the client-fault negotiation lane: per-peer
    /// rejection windows treat that lane as an attack signal.
    #[tokio::test]
    async fn server_fault_rejections_land_in_their_own_outcome_lane() {
        let server = EnhancedGameServer::new(
            crate::server::ServerConfig::default(),
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            crate::config::SessionConfig::default(),
            crate::config::TurnConfig::default(),
            crate::database::DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("construct server-fault lane test server");

        let response = finish_upgrade_response(
            &server,
            "127.0.0.1:3536".parse().expect("test address parses"),
            "00000000-0000-4000-8000-000000000003",
            UpgradeOutcome::RejectedServerFault,
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid token binding configuration",
            )
                .into_response(),
        );

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response
                .headers()
                .get(WEBSOCKET_UPGRADE_OUTCOME_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("rejected_server_fault")
        );

        let upgrades = server
            .metrics()
            .snapshot()
            .await
            .connections
            .websocket_upgrades;
        // `attempts` stays 0 here: only the handler entry point increments it;
        // this test drives `finish_upgrade_response` directly.
        assert_eq!(
            upgrades,
            crate::metrics::WebSocketUpgradeMetrics {
                rejected_server_fault: 1,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn every_handled_upgrade_response_advertises_the_configured_outbound_limit() {
        let config = crate::server::ServerConfig {
            max_message_size: 12_345,
            max_signal_bytes: 12_345,
            // Pairing-legal: the relay envelope headroom above the inbound
            // cap (constructor guard).
            max_outbound_message_size: 12_345
                + crate::config::defaults::RELAY_ENVELOPE_HEADROOM_BYTES,
            ..crate::server::ServerConfig::default()
        };
        let server = EnhancedGameServer::new(
            config,
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            crate::config::SessionConfig::default(),
            crate::config::TurnConfig::default(),
            crate::database::DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("construct header test server");

        let response = finish_upgrade_response(
            &server,
            "127.0.0.1:3536".parse().expect("test address parses"),
            "00000000-0000-4000-8000-000000000001",
            UpgradeOutcome::RejectedDraining,
            StatusCode::SERVICE_UNAVAILABLE.into_response(),
        );

        assert_eq!(
            response
                .headers()
                .get(WEBSOCKET_MAX_OUTBOUND_MESSAGE_SIZE_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some("12601"),
            "the header must advertise the configured outbound cap"
        );
    }

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

    /// Throttling only shapes logging: every rejected upgrade keeps the exact
    /// status code and header set, whether it is the source's first warning,
    /// a suppressed repeat, or a boundary summary.
    #[tokio::test]
    async fn throttled_rejection_logging_keeps_responses_byte_for_byte_compatible() {
        let server = EnhancedGameServer::new(
            crate::server::ServerConfig::default(),
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            crate::config::SessionConfig::default(),
            crate::config::TurnConfig::default(),
            crate::database::DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("construct rejection-response test server");

        let addr = "127.0.0.1:3536".parse().expect("test address parses");
        let request_id = "00000000-0000-4000-8000-000000000002";
        let outcomes = [
            (UpgradeOutcome::RejectedOrigin, StatusCode::FORBIDDEN),
            // A suppressed repeat from the same peer...
            (UpgradeOutcome::RejectedOrigin, StatusCode::FORBIDDEN),
            // ...and a distinct outcome lane that keeps its own first warning.
            (
                UpgradeOutcome::RejectedTokenBindingOffer,
                StatusCode::BAD_REQUEST,
            ),
        ];

        for (outcome, expected_status) in outcomes {
            let response = finish_upgrade_response(
                &server,
                addr,
                request_id,
                outcome,
                (expected_status, "rejected").into_response(),
            );

            assert_eq!(response.status(), expected_status);
            let headers = response.headers();
            assert_eq!(
                headers.get(WEBSOCKET_REQUEST_ID_HEADER),
                Some(&HeaderValue::from_static(request_id))
            );
            assert_eq!(
                headers.get(WEBSOCKET_UPGRADE_OUTCOME_HEADER),
                Some(&HeaderValue::from_static(outcome.header_value()))
            );
            assert_eq!(
                headers
                    .get(WEBSOCKET_MAX_OUTBOUND_MESSAGE_SIZE_HEADER)
                    .and_then(|value| value.to_str().ok()),
                Some(
                    server
                        .config()
                        .max_outbound_message_size
                        .to_string()
                        .as_str()
                )
            );
        }
    }
}
