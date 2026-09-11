use super::{EnhancedGameServer, ServerConfig};
use crate::config::{
    CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
    TransportSecurityConfig, TurnConfig,
};
use crate::coordination::{CloseReason, ConnectionCloseListener, ConnectionCloseSignal};
use crate::protocol::ServerMessage;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

async fn create_test_server_with(config: ServerConfig) -> Arc<EnhancedGameServer> {
    EnhancedGameServer::new(
        config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        TurnConfig::default(),
        crate::database::DatabaseConfig::InMemory,
        MetricsConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("failed to construct test server")
}

async fn register_client_with_close_listener(
    server: &EnhancedGameServer,
    addr: SocketAddr,
) -> (
    crate::protocol::PlayerId,
    mpsc::Receiver<Arc<ServerMessage>>,
    ConnectionCloseListener,
) {
    let (sender, receiver) = mpsc::channel(8);
    let (close_signal, close_listener) = ConnectionCloseSignal::channel();
    let player_id = server
        .connection_manager
        .register_client(sender, close_signal, addr, server.instance_id)
        .await
        .expect("client registration succeeds");
    (player_id, receiver, close_listener)
}

fn drain_receiver(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) -> Vec<Arc<ServerMessage>> {
    let mut messages = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(message) => messages.push(message),
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                return messages;
            }
        }
    }
}

/// A roomless `RequestAuthority` frame is answered 1:1 with a
/// `granted: false` `AuthorityResponse`. Authority requests carry no
/// per-kind budget, so those denials charge the per-connection error-reply
/// budget (issue #518): the exhausting denial is withheld, the farewell
/// rides instead, and the connection closes with the semantic
/// `4006 inbound_rate_limited` reason.
#[tokio::test]
async fn roomless_authority_denials_exhaust_the_error_reply_budget() {
    let mut config = ServerConfig::default();
    config.rate_limit_config.max_inbound_error_replies = 2;
    let server = create_test_server_with(config).await;
    let (player, mut receiver, close_listener) =
        register_client_with_close_listener(&server, "127.0.0.1:48270".parse().unwrap()).await;

    // Exactly the budgeted denials are delivered.
    for _ in 0..2 {
        server.handle_authority_request(&player, true).await;
        let denial = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("denial should arrive in time")
            .expect("channel stays open");
        assert!(
            matches!(
                denial.as_ref(),
                ServerMessage::AuthorityResponse { granted: false, .. }
            ),
            "the budgeted denial must still be delivered, got {denial:?}"
        );
    }

    // The exhausting denial is withheld; the farewell rides instead and the
    // semantic close reason is pinned.
    server.handle_authority_request(&player, true).await;
    let drained = drain_receiver(&mut receiver);
    assert!(
        !drained
            .iter()
            .any(|message| matches!(message.as_ref(), ServerMessage::AuthorityResponse { .. })),
        "an exhausted budget must withhold the denial, got {drained:?}"
    );
    assert!(
        drained
            .iter()
            .any(|message| matches!(message.as_ref(), ServerMessage::Error { .. })),
        "the budget-exhaustion farewell must be sent"
    );
    assert_eq!(
        close_listener.requested_reason(),
        Some(CloseReason::InboundRateLimited),
        "the exhausted budget must close with the 4006 reason"
    );
}
