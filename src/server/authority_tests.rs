use super::{
    EnhancedGameServer, InMemoryDistributedLock, InMemoryMessageCoordinator, ServerConfig,
};
use crate::config::{
    CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
    TransportSecurityConfig, TurnConfig,
};
use crate::coordination::{
    CloseReason, ConnectionCloseListener, ConnectionCloseSignal, MessageCoordinator,
};
use crate::database::InMemoryDatabase;
use crate::protocol::{ClientMessage, ServerMessage};
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

fn assert_receiver_empty(receiver: &mut mpsc::Receiver<Arc<ServerMessage>>) {
    let pending = receiver.try_recv();
    assert!(
        matches!(pending, Err(mpsc::error::TryRecvError::Empty)),
        "{pending:?}"
    );
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

/// #826: closing before admission must leave both claims and releases uncommitted.
#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
async fn closing_socket_cancels_authority_waiting_for_room_mutation() {
    for become_authority in [true, false] {
        check_authority_admission(become_authority, AuthorityAdmission::CloseAtRoom).await;
    }
}

#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
async fn closing_socket_cancels_authority_waiting_for_route_lease() {
    for become_authority in [true, false] {
        check_authority_admission(become_authority, AuthorityAdmission::CloseAtRoute).await;
    }
}

#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
async fn open_socket_completes_authority_after_route_contention() {
    for become_authority in [true, false] {
        check_authority_admission(become_authority, AuthorityAdmission::StayOpen).await;
    }
}

#[tokio::test(start_paused = true)]
#[cfg_attr(miri, ignore)]
async fn closing_socket_retains_committed_authority_publication() {
    for become_authority in [true, false] {
        check_authority_admission(become_authority, AuthorityAdmission::CloseAfterCommit).await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthorityAdmission {
    CloseAtRoom,
    CloseAtRoute,
    StayOpen,
    CloseAfterCommit,
}

async fn check_authority_admission(become_authority: bool, action: AuthorityAdmission) {
    let wait_for_route = action != AuthorityAdmission::CloseAtRoom;
    let close_before_admission = matches!(
        action,
        AuthorityAdmission::CloseAtRoom | AuthorityAdmission::CloseAtRoute
    );
    let coordinator = Arc::new(InMemoryMessageCoordinator::new());
    let database = Arc::new(InMemoryDatabase::new());
    let server = super::room_service_tests::create_test_server_with_message_coordinator_and_lock(
        ServerConfig::default(),
        coordinator.clone(),
        Arc::new(InMemoryDistributedLock::new()),
        database.clone(),
    )
    .await;
    let (source, mut source_rx, _source_close) =
        register_client_with_close_listener(&server, "127.0.0.1:48271".parse().unwrap()).await;
    let (peer, mut peer_rx, _peer_close) =
        register_client_with_close_listener(&server, "127.0.0.1:48272".parse().unwrap()).await;
    let (absent, mut absent_rx, _absent_close) =
        register_client_with_close_listener(&server, "127.0.0.1:48273".parse().unwrap()).await;
    for (player, name) in [(source, "Source"), (peer, "Peer"), (absent, "Absent")] {
        server
            .handle_join_room(
                &player,
                "authority-close".into(),
                Some("ROLE01".into()),
                name.into(),
                Some(3),
                Some(true),
                None,
                None,
                None,
            )
            .await;
    }
    let room_id = server
        .get_client_room(&source)
        .await
        .expect("source joined");
    assert_eq!(server.get_client_room(&peer).await, Some(room_id));
    if become_authority {
        server.handle_authority_request(&source, false).await;
    }
    server.unregister_client(&absent).await;
    let replay = server.reconnection_manager.as_ref().expect("reconnection");
    assert!(replay
        .get_disconnected_players_in_room(&room_id)
        .await
        .contains(&absent));
    drain_receiver(&mut source_rx);
    drain_receiver(&mut peer_rx);
    drain_receiver(&mut absent_rx);

    let before = (!become_authority).then_some(source);
    let after = become_authority.then_some(source);
    let stored_authority = async || {
        server
            .database
            .get_room_by_id(&room_id)
            .await
            .unwrap()
            .unwrap()
            .authority_player
    };
    assert_eq!(stored_authority().await, before);

    let before_events = replay.get_missed_events(&room_id, 0).await.events.len();

    let room_gate = coordinator.lock_room_event_mutation(&room_id).await;
    let routing_writer = if wait_for_route {
        Some(coordinator.room_routing_gates.write(room_id).await)
    } else {
        None
    };
    let lifecycle = server.client_lifecycle(&source).expect("source lifecycle");
    let dispatch = server.handle_client_message_from_lifecycle(
        &source,
        ClientMessage::AuthorityRequest { become_authority },
        Arc::clone(&lifecycle),
    );
    tokio::pin!(dispatch);
    let attempts = room_gate.mutation_attempt_count_for_test();
    assert!(futures_util::poll!(&mut dispatch).is_pending());
    timeout(
        Duration::from_secs(1),
        room_gate.wait_for_mutation_attempt_after_for_test(attempts),
    )
    .await
    .expect("authority request must enter the room mutation queue");
    let mut room_gate = Some(room_gate);
    if wait_for_route {
        drop(room_gate.take());
        assert!(
            timeout(
                Duration::from_millis(10),
                coordinator.lock_room_event_mutation(&room_id)
            )
            .await
            .is_err(),
            "the request must hold the room gate while waiting for its live route"
        );
    }

    assert_eq!(stored_authority().await, before);
    assert_receiver_empty(&mut peer_rx);
    if close_before_admission {
        assert!(server
            .connection_manager
            .request_close_for(&source, CloseReason::Unregistered));
        timeout(Duration::from_millis(10), &mut dispatch)
            .await
            .expect("close must release the request while the admission gate is held");
        drop(
            timeout(Duration::from_millis(10), lifecycle.lock())
                .await
                .expect("close must release the source lifecycle"),
        );
        if wait_for_route {
            drop(
                timeout(
                    Duration::from_millis(10),
                    coordinator.lock_room_event_mutation(&room_id),
                )
                .await
                .expect("close must release the room gate before routing resumes"),
            );
        }
    }

    if action == AuthorityAdmission::CloseAfterCommit {
        database.pause_authority_request_after_commit_for_test();
    }
    drop(routing_writer);
    drop(room_gate);
    if action == AuthorityAdmission::CloseAfterCommit {
        timeout(
            Duration::from_secs(1),
            database.wait_for_authority_request_commit_for_test(),
        )
        .await
        .expect("authority must reach durable commit");
        assert!(server
            .connection_manager
            .request_close_for(&source, CloseReason::Unregistered));
        let lifecycle_wait = lifecycle.lock();
        tokio::pin!(lifecycle_wait);
        assert!(futures_util::poll!(&mut lifecycle_wait).is_pending());
        assert_receiver_empty(&mut peer_rx);
        database.release_authority_request_commit_for_test();
    }
    if close_before_admission {
        drop(coordinator.lock_room_event_mutation(&room_id).await);
        assert_eq!(stored_authority().await, before);
        assert_eq!(
            replay.get_missed_events(&room_id, 0).await.events.len(),
            before_events
        );
        assert_receiver_empty(&mut source_rx);
        assert_receiver_empty(&mut peer_rx);
    } else {
        timeout(Duration::from_secs(1), dispatch)
            .await
            .expect("live authority request");
        assert_eq!(stored_authority().await, after);
        let missed = replay.get_missed_events(&room_id, 0).await;
        assert!(
            matches!(
                missed.events.get(before_events..),
                Some([ServerMessage::AuthorityChanged { authority_player, .. }])
                    if *authority_player == after
            ),
            "{missed:?}"
        );
        if action == AuthorityAdmission::StayOpen {
            let messages = drain_receiver(&mut source_rx);
            let [response, change] = messages.as_slice() else {
                panic!("expected an authority response then a role change: {messages:?}");
            };
            assert!(
                matches!(
                    response.as_ref(),
                    ServerMessage::AuthorityResponse {
                        granted: true,
                        reason: None,
                        error_code: None
                    }
                ),
                "{response:?}"
            );
            assert!(
                matches!(
                    change.as_ref(),
                    ServerMessage::AuthorityChanged { authority_player, you_are_authority }
                        if *authority_player == after && *you_are_authority == become_authority
                ),
                "{change:?}"
            );
        }
        let peer_events = drain_receiver(&mut peer_rx);
        assert!(
            matches!(
                peer_events.as_slice(),
                [change] if matches!(change.as_ref(), ServerMessage::AuthorityChanged {
                    authority_player, you_are_authority: false
                } if *authority_player == after)
            ),
            "{peer_events:?}"
        );
    }
}
