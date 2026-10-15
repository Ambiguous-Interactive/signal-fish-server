use super::*;
use clap::Parser;
use futures_util::StreamExt;

// Regression #987: write backpressure must not consume the response window.
#[tokio::test]
async fn pong_response_window_starts_after_the_ping_write_finishes() {
    use futures_util::SinkExt;
    use tokio_tungstenite::MaybeTlsStream;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("ws://{address}");
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(tcp).await.unwrap()
    });
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_send_buffer_size(1024).unwrap();
    let tcp = socket.connect(address).await.unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async(&url, MaybeTlsStream::Plain(tcp))
        .await
        .unwrap();
    let mut server_ws = server.await.unwrap();
    let payload = vec![42; 8 * 1024 * 1024];
    ws.feed(Message::Binary(payload.clone().into()))
        .await
        .unwrap();
    let cli = Cli::parse_from(["reference-native", "--server-url", &url, "--create-room"]);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.next_ping_at = Instant::now();
    {
        let timer = state.process_timers();
        tokio::pin!(timer);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut timer)
                .await
                .is_err(),
            "establish socket backpressure before advancing the clock"
        );
        tokio::time::pause();
        let clock_guard = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        tokio::time::advance(Duration::from_secs(12)).await;
        let drain = async {
            assert_eq!(
                server_ws.next().await.unwrap().unwrap(),
                Message::Binary(payload.into())
            );
            let ping = server_ws.next().await.unwrap().unwrap();
            let ping: ClientMessage = serde_json::from_str(ping.to_text().unwrap()).unwrap();
            assert!(matches!(ping, ClientMessage::Ping));
        };
        let (result, ()) = tokio::join!(timer, drain);
        assert!(result.unwrap().is_none());
        clock_guard.abort();
    }
    let completed = Instant::now();
    assert_eq!(
        state.pong_deadline,
        Some(checked_deadline(completed, PONG_TIMEOUT)),
        "a slow successful Ping write must leave the complete response window"
    );
    assert_eq!(
        state.next_ping_at,
        checked_deadline(completed, PING_INTERVAL)
    );
    tokio::time::advance(PONG_TIMEOUT - Duration::from_millis(1)).await;
    assert!(state.process_timers().await.unwrap().is_none());
    assert!(
        !state.pong_grace_applied,
        "retain the full normal response window"
    );
    state
        .handle_server_message(ServerMessage::Pong)
        .await
        .unwrap();
    assert!(
        state.pong_deadline.is_none(),
        "an on-time response keeps the connection alive"
    );
    tokio::time::resume();
}

#[tokio::test(start_paused = true)]
async fn unanswered_ping_still_expires_after_its_response_window_and_drain_grace() {
    use futures_util::FutureExt;

    let clock_guard = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(tcp).await.unwrap()
    });
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let mut server_ws = server.await.unwrap();
    let cli = Cli::parse_from(["reference-native", "--server-url", &url, "--create-room"]);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.next_ping_at = Instant::now();
    assert!(state.process_timers().await.unwrap().is_none());
    let frame = server_ws.next().await.unwrap().unwrap();
    let ping: ClientMessage = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    assert!(matches!(ping, ClientMessage::Ping));
    let response_deadline = state.pong_deadline.unwrap();
    tokio::time::advance(PONG_TIMEOUT).await;
    assert!(state.process_timers().await.unwrap().is_none());
    assert!(state.pong_grace_applied);
    assert_eq!(
        state.pong_deadline,
        Some(checked_deadline(response_deadline, PONG_DRAIN_GRACE))
    );
    assert!(
        server_ws.next().now_or_never().is_none(),
        "only one Ping may be outstanding"
    );
    tokio::time::advance(PONG_DRAIN_GRACE).await;
    assert_eq!(
        state.process_timers().await.unwrap_err().code,
        EXIT_CONNECTION_ERROR
    );
    clock_guard.abort();
}

#[tokio::test]
async fn channel_termination_reports_fallback_and_preserves_a_newer_link() {
    for receive_rejected in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut server_ws = server.await.unwrap();
        let cli = Cli::parse_from(["reference-native", "--server-url", &url, "--create-room"]);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.engine.pair_with(peer, false, &[]).await.unwrap();
        state.webrtc_plan_seen = true;
        state.expected_peers.insert(peer);
        state.connected_pairs.insert(peer);
        state.ice_gathering_complete.insert(peer);
        state.transport_status = Some(true);

        let terminal_event = |label: &str| {
            if receive_rejected {
                EngineEvent::ChannelReceiveRejected {
                    peer,
                    generation: 1,
                    label: label.to_owned(),
                }
            } else {
                EngineEvent::ChannelClosed {
                    peer,
                    generation: 1,
                    label: label.to_owned(),
                }
            }
        };
        state
            .handle_engine_event(terminal_event(RELIABLE_LABEL))
            .await
            .unwrap();
        assert!(!state.engine.is_paired(peer));
        assert!(!state.connected_pairs.contains(&peer));
        assert!(!state.ice_gathering_complete.contains(&peer));
        assert!(state.expected_peers.contains(&peer));
        assert_eq!(state.transport_status, Some(false));
        let frame = tokio::time::timeout(Duration::from_secs(5), server_ws.next())
            .await
            .expect("fallback status reaches the signaling server")
            .unwrap()
            .unwrap();
        let frame: ClientMessage = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert!(matches!(
            frame,
            ClientMessage::TransportStatus {
                transport: Transport::WebRtc,
                connected: false
            }
        ));

        state.engine.pair_with(peer, false, &[]).await.unwrap();
        state.connected_pairs.insert(peer);
        state.transport_status = Some(true);
        state
            .handle_engine_event(terminal_event(UNRELIABLE_LABEL))
            .await
            .unwrap();
        assert!(state.engine.is_current_generation(peer, 2));
        assert!(state.connected_pairs.contains(&peer));
        assert_eq!(state.transport_status, Some(true));
        state.engine.remove_peer(peer).await.unwrap();
    }
}

// Regression #937: a new epoch fences old callbacks before its plan arrives.
#[tokio::test]
async fn fresh_membership_retires_old_links_before_the_replacement_plan() {
    for reconnect in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let _server_ws = server.await.unwrap();
        let cli = Cli::parse_from([
            "reference-native",
            "--server-url",
            &url,
            "--create-room",
            "--exchange",
            "--p2p-retry-count",
            "2",
        ]);
        let me = PlayerId::from_u128(1);
        let peer = PlayerId::from_u128(2);
        let sibling = PlayerId::from_u128(3);
        let generation = SessionGeneration::from_u128(4);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
        state.negotiated_version = 3;
        state.accountability = DeliveryAccountability::new(true);
        state.current_session_generation = Some(generation);
        let membership = |epoch| {
            if reconnect {
                ServerMessage::PlayerReconnected {
                    player_id: peer,
                    epoch: Some(epoch),
                }
            } else {
                serde_json::from_value::<ServerMessage>(json!({
                    "type": "PlayerJoined", "data": {"player": {
                        "id": peer, "name": "peer", "is_authority": false,
                        "is_ready": false, "connected_at": "2026-10-13T00:00:00Z",
                        "connection_info": null, "epoch": epoch, "seq": 0
                    }}
                }))
                .unwrap()
            }
        };
        state.handle_server_message(membership(1)).await.unwrap();
        state.lobby_state = Some(LobbyState::Finalized);
        for current_peer in [peer, sibling] {
            state
                .engine
                .pair_with(current_peer, false, &[])
                .await
                .unwrap();
            state.expected_peers.insert(current_peer);
            state.pair_roles.insert(current_peer, false);
            state.connected_pairs.insert(current_peer);
            state.exchange_ledger.note_connected(current_peer);
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                state.exchange_ledger.note_sent(current_peer, label);
                state
                    .exchange_ledger
                    .note_received(current_peer, label.to_string());
            }
        }
        state.handle_server_message(membership(1)).await.unwrap();
        assert!(
            state.engine.is_current_generation(peer, 1),
            "duplicate epoch keeps the link"
        );
        state.handle_server_message(membership(2)).await.unwrap();
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            state
                .handle_engine_event(EngineEvent::ChannelMessage {
                    peer,
                    generation: 1,
                    label: label.to_string(),
                    text: json!({"from": peer, "channel": label, "seq": 0}).to_string(),
                    queue_permit: crate::engine::MessageQueuePermit::for_test(),
                })
                .await
                .unwrap();
        }
        assert!(
            !state.exchange_ledger.received.contains_key(&peer),
            "reconnect={reconnect}: old-link callbacks cannot become fresh receipts"
        );
        assert!(!state.engine.is_paired(peer));
        assert!(!state.connected_pairs.contains(&peer));
        assert!(state.expected_peers.contains(&peer));
        assert!(state.pending_membership_plans.contains_key(&peer));
        assert!(state.engine.is_current_generation(sibling, 2));
        assert!(state.connected_pairs.contains(&sibling));
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            assert!(state.exchange_ledger.received[&sibling].contains(label));
        }
        state.p2p_deadline = Some(Deadline::after(Instant::now(), Duration::from_secs(60)));
        state
            .handle_signal(peer, generation, json!({"PairRetry": 1}))
            .await
            .unwrap();
        state
            .handle_signal(peer, generation, json!({"IceCandidate": "old-candidate"}))
            .await
            .unwrap();
        state.retry_incomplete_pairs(Instant::now()).await.unwrap();
        state
            .handle_server_message(ServerMessage::NewPeer {
                peer_id: peer,
                you_initiate: false,
            })
            .await
            .unwrap();
        assert!(
            !state.engine.is_paired(peer),
            "retries cannot bypass the membership plan"
        );
        assert!(
            !state.pending_signals.contains_key(&peer),
            "old signals cannot cross the plan boundary"
        );
        let plan = serde_json::from_value::<ServerMessage>(json!({
            "type": "SessionPlan", "data": {
                "generation": SessionGeneration::from_u128(5),
                "topology": "mesh", "transport": "webrtc", "fallback": "relay",
                "peers": [{"player_id": peer, "player_name": "peer", "is_authority": false, "initiate": false},
                          {"player_id": sibling, "player_name": "sibling", "is_authority": false, "initiate": false}]
            }
        })).unwrap();
        state.handle_server_message(plan).await.unwrap();
        assert!(state.pending_membership_plans.is_empty());
        assert!(state.engine.is_current_generation(peer, 3));
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            state.exchange_ledger.note_sent(peer, label);
            state
                .handle_engine_event(EngineEvent::ChannelMessage {
                    peer,
                    generation: 3,
                    label: label.to_string(),
                    text: json!({"from": peer, "channel": label, "seq": 0}).to_string(),
                    queue_permit: crate::engine::MessageQueuePermit::for_test(),
                })
                .await
                .unwrap();
        }
        assert!(
            state.exchange_ledger.unmet_criteria().is_empty(),
            "replacement receipts satisfy fresh exchange"
        );
        state.handle_server_message(membership(2)).await.unwrap();
        assert!(
            state.engine.is_current_generation(peer, 3),
            "duplicate keeps replacement"
        );
        assert!(state.exchange_ledger.unmet_criteria().is_empty());
        state.engine.remove_peer(peer).await.unwrap();
        state.engine.remove_peer(sibling).await.unwrap();
    }
}

// Regression #974: response preludes must not renew a command's wait.
#[tokio::test]
async fn handshake_commands_bound_delayed_results_after_preludes() {
    use futures_util::SinkExt;
    let mut failures = Vec::new();
    for command in ["authenticate", "join", "restore"] {
        for outcome in ["on-time", "missing", "late"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                tcp.set_nodelay(true).unwrap();
                tokio_tungstenite::accept_async(tcp).await.unwrap()
            });
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let mut server_ws = server.await.unwrap();
            let cli = Cli::parse_from(["reference-native", "--server-url", &url, "--create-room"]);
            let me = PlayerId::from_u128(1);
            let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
            state.accountability = DeliveryAccountability::new(true);
            let reconnect = ClientMessage::Reconnect {
                player_id: me,
                room_id: state.room_id,
                auth_token: "test-restore".into(),
            };
            let baseline = json!({
                "room_id": state.room_id, "room_code": "ABC123", "player_id": me,
                "game_name": "handshake", "max_players": 2, "supports_authority": false,
                "current_players": [{"id": me, "name": "self", "is_authority": false,
                    "is_ready": false, "epoch": 1, "seq": 0}],
                "is_authority": false, "lobby_state": "lobby", "ready_players": [],
                "relay_type": "none", "missed_events": [], "sender_watermarks": [],
                "replay": "complete", "reconnection_token": "replacement"
            });
            let terminal = match command {
                "authenticate" => json!({"type": "ProtocolInfo", "data": {"protocol_version": 3}}),
                "join" => json!({"type": "RoomJoined", "data": baseline}),
                _ => json!({"type": "Reconnected", "data": baseline}),
            };
            // Socket I/O must not auto-advance Tokio time. Keep the runtime runnable.
            let clock_guard = tokio::spawn(async {
                loop {
                    tokio::task::yield_now().await;
                }
            });
            tokio::time::pause();
            let started = Instant::now();
            let mut operation = Box::pin(async {
                match command {
                    "authenticate" => authenticate(&mut state.ws, &cli).await.map(|_| ()),
                    "join" => join_room(&mut state.ws, &cli, true).await.map(|_| ()),
                    _ => state.reconnect_room(&reconnect).await.map(|_| ()),
                }
            });
            assert!(futures_util::poll!(operation.as_mut()).is_pending());
            let request = server_ws.next().await.unwrap().unwrap();
            let request: ClientMessage = serde_json::from_str(request.to_text().unwrap()).unwrap();
            assert!(matches!(
                (command, request),
                ("authenticate", ClientMessage::Authenticate { .. })
                    | ("join", ClientMessage::JoinRoom { .. })
                    | ("restore", ClientMessage::Reconnect { .. })
            ));
            assert_eq!(
                Instant::now(),
                started,
                "socket readiness must not move the clock"
            );
            tokio::time::advance(HANDSHAKE_TIMEOUT - Duration::from_millis(1)).await;
            let prelude = if command == "authenticate" {
                json!({"type": "Authenticated", "data": {
                    "app_name": "default",
                    "rate_limits": {"per_minute": 60, "per_hour": 3600, "per_day": 86400}
                }})
            } else {
                json!({"type": "RelayStats", "data": {
                    "interval_ms": 1000, "sent_to_you": 0,
                    "dropped_for_you": 0, "backpressure_events": 0
                }})
            };
            server_ws
                .send(Message::Text(prelude.to_string().into()))
                .await
                .unwrap();
            for _ in 0..32 {
                let observed = futures_util::poll!(operation.as_mut());
                assert!(
                    observed.is_pending(),
                    "{command}/{outcome}: premature result {observed:?}"
                );
                tokio::task::yield_now().await;
            }
            if outcome != "on-time" {
                // Cross a full tick of Tokio's millisecond timer wheel.
                tokio::time::advance(Duration::from_millis(2)).await;
            }
            if outcome != "missing" {
                server_ws
                    .send(Message::Text(terminal.to_string().into()))
                    .await
                    .unwrap();
            }
            let mut observed = std::task::Poll::Pending;
            let io_deadline = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < io_deadline {
                observed = futures_util::poll!(operation.as_mut());
                if observed.is_ready() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            match (outcome, observed) {
                ("on-time", std::task::Poll::Ready(Ok(()))) => {}
                ("missing" | "late", std::task::Poll::Ready(Err(error))) => {
                    assert_eq!(error.code, EXIT_CONNECTION_ERROR);
                }
                (_, result) => failures.push(format!("{command}/{outcome}: {result:?}")),
            }
            drop(operation);
            clock_guard.abort();
            tokio::time::resume();
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// Regression #983: buffered controls must share the original command cutoff.
#[tokio::test]
async fn handshake_deadline_stops_a_ready_control_sequence() {
    use std::task::Poll;

    let mut polls = 0;
    let mut input = futures_util::stream::poll_fn(|_| {
        polls += 1;
        if polls <= 8 {
            std::thread::sleep(Duration::from_millis(5));
            Poll::Ready(Some(Ok(Message::Ping(vec![].into()))))
        } else {
            Poll::Ready(Some(Ok(Message::Text(r#"{"type":"Pong"}"#.into()))))
        }
    });
    let error = next_handshake_message(
        &mut input,
        Deadline::after(Instant::now(), Duration::from_millis(10)),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, EXIT_CONNECTION_ERROR);
    assert!(
        polls < 8,
        "the inner wait read {polls} frames past the command cutoff"
    );
}

// Regression #985: the command cutoff also owns a write's ready completion.
#[tokio::test(start_paused = true)]
async fn handshake_writes_keep_the_original_command_cutoff() {
    use tokio_tungstenite::tungstenite::Error as WebSocketError;

    let budget = Duration::from_secs(2);
    let mut accepted_late = Vec::new();
    for elapsed in [
        budget - Duration::from_millis(1),
        budget,
        budget + Duration::from_millis(1),
    ] {
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let mut sink = Box::pin(futures_util::sink::unfold(
            Some(released),
            |mut released, _frame: Message| async move {
                released.take().unwrap().await.unwrap();
                Ok::<_, WebSocketError>(released)
            },
        ));
        let write = send_handshake_message(
            &mut sink,
            &ClientMessage::Ping,
            Deadline::after(Instant::now(), budget),
        );
        tokio::pin!(write);
        assert!(futures_util::poll!(write.as_mut()).is_pending());
        tokio::time::advance(elapsed).await;
        release.send(()).unwrap();
        let result = write.await;
        if elapsed >= budget {
            if result.is_ok() {
                accepted_late.push(elapsed);
            } else {
                assert_eq!(result.unwrap_err().code, EXIT_CONNECTION_ERROR);
            }
        } else {
            assert!(result.is_ok(), "on-time command: {result:?}");
        }
    }
    assert!(
        accepted_late.is_empty(),
        "late command writes reported success: {accepted_late:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn handshake_writes_reject_expired_commands_without_polling_the_sink() {
    let calls = std::cell::Cell::new(0);
    let mut sink = Box::pin(futures_util::sink::unfold((), |(), _frame: Message| {
        calls.set(calls.get() + 1);
        std::future::ready(Ok::<_, tokio_tungstenite::tungstenite::Error>(()))
    }));
    for budget in [Duration::ZERO, Duration::from_millis(1)] {
        let deadline = Deadline::after(Instant::now(), budget);
        tokio::time::advance(budget).await;
        let result = send_handshake_message(&mut sink, &ClientMessage::Ping, deadline).await;
        assert_eq!(result.unwrap_err().code, EXIT_CONNECTION_ERROR);
        assert_eq!(calls.get(), 0, "expired commands cannot reach the sink");
    }
}
