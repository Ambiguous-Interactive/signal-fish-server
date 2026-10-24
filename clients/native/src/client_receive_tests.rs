use super::*;
use clap::Parser;
use futures_util::StreamExt;

/*
    Regression #1041: apply complete authoritative frames before a timer can
    declare success from the previous membership or pairing state.
*/
#[tokio::test]
async fn ready_authoritative_frames_prevent_success_from_obsolete_state() {
    use futures_util::SinkExt;

    let mut failures = Vec::new();
    for window in ["cutoff", "linger"] {
        for kind in ["none", "pong", "reconnect", "plan"] {
            for prefix in [0, 32, 256] {
                let (ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
                let directory = tempfile::tempdir().unwrap();
                let mut cli = Cli::parse_from([
                    "reference-native",
                    "--server-url",
                    "ws://unused.invalid",
                    "--create-room",
                    "--peers",
                    "1",
                ]);
                cli.p2p_release_file = Some(directory.path().join("hold-p2p"));
                let me = PlayerId::from_u128(1);
                let peer = PlayerId::from_u128(2);
                let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
                state.negotiated_version = 3;
                state.accountability = DeliveryAccountability::new(true);
                state.game_started = true;
                state.lobby_state = Some(LobbyState::Finalized);
                state.p2p_released = false;
                state.next_ping_at = Instant::now() + Duration::from_secs(30);
                assert!(
                    state.criteria_met(),
                    "{window}/{kind}: establish old success"
                );
                let message = if matches!(kind, "none" | "pong") {
                    ServerMessage::Pong
                } else if kind == "reconnect" {
                    ServerMessage::PlayerReconnected {
                        player_id: peer,
                        epoch: Some(2),
                        player: None,
                    }
                } else {
                    serde_json::from_value(json!({
                        "type": "SessionPlan", "data": {
                            "generation": SessionGeneration::from_u128(2),
                            "topology": "mesh", "transport": "webrtc", "fallback": "relay",
                            "peers": [{"player_id": peer, "player_name": "peer",
                                "is_authority": false, "initiate": false}]
                        }
                    }))
                    .unwrap()
                };
                let text = serde_json::to_string(&message).unwrap();
                let mut frame_size = if kind == "none" {
                    0
                } else {
                    text.len() + if text.len() < 126 { 2 } else { 4 }
                };
                let pong = serde_json::to_string(&ServerMessage::Pong).unwrap();
                for _ in 0..prefix {
                    server_ws
                        .send(Message::Text(pong.clone().into()))
                        .await
                        .unwrap();
                    frame_size += pong.len() + 2;
                }
                if kind != "none" {
                    server_ws.send(Message::Text(text.into())).await.unwrap();
                }
                let tokio_tungstenite::MaybeTlsStream::Plain(tcp) = state.ws.get_ref() else {
                    panic!("fixture must use plain TCP");
                };
                let mut peeked = [0_u8; 16384];
                tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        if frame_size == 0 || tcp.peek(&mut peeked).await.unwrap() >= frame_size {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("complete authoritative frame must be readable before timers");
                let now = Instant::now();
                state.run_deadline = if window == "cutoff" {
                    now.into()
                } else {
                    state.linger_until = Some(now);
                    (now + Duration::from_millis(50)).into()
                };
                let result = tokio::time::timeout(Duration::from_secs(1), state.run_loop())
                    .await
                    .expect("ready input must not postpone the run cutoff forever");
                if prefix == 256 {
                    match result {
                        Err(error) if error.code == EXIT_CONNECTION_ERROR
                            && error.message.contains("server input did not settle") => {},
                        other => failures.push(format!("{window}/{kind}/{prefix}: expected unsettled input error, got {other:?}")),
                    }
                    continue;
                }
                let result = result.unwrap();
                let harmless = matches!(kind, "none" | "pong");
                let expected = if harmless {
                    EXIT_SUCCESS
                } else {
                    EXIT_CRITERIA_UNMET
                };
                if result != expected || state.criteria_met() != harmless {
                    failures.push(format!(
                        "{window}/{kind}/{prefix}: exit={result}, expected={expected}, unmet={:?}",
                        state.unmet_criteria()
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "ready frame histories failed: {failures:#?}"
    );
}

// Regression #990: readiness recovery must not wait for a delayed rejection.
#[tokio::test]
async fn readiness_recovery_reissues_start_before_a_delayed_rejection() {
    for rejection_before_ready in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut server_ws = server.await.unwrap();
        let cli = Cli::parse_from(["reference-native", "--server-url", &url, "--create-room"]);
        let creator = PlayerId::from_u128(1);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, creator);
        state.present.insert(peer);
        state.ready_sent = true;
        let snapshot = |all_ready| ServerMessage::LobbyStateChanged {
            lobby_state: LobbyState::Lobby,
            ready_players: if all_ready {
                vec![creator, peer]
            } else {
                vec![creator]
            },
            all_ready,
        };
        let rejected = || ServerMessage::Error {
            message: "room is not all ready".into(),
            error_code: Some(ErrorCode::GameStartNotReady),
        };
        state.handle_server_message(snapshot(true)).await.unwrap();
        let first = server_ws.next().await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_str::<ClientMessage>(first.to_text().unwrap()).unwrap(),
            ClientMessage::StartGame
        ));
        state.handle_server_message(snapshot(true)).await.unwrap();
        state.handle_server_message(snapshot(false)).await.unwrap();
        state.handle_server_message(snapshot(false)).await.unwrap();
        let unexpected = tokio::time::timeout(Duration::from_millis(25), server_ws.next()).await;
        assert!(
            unexpected.is_err(),
            "unchanged all-ready and unready snapshots must remain silent"
        );
        assert!(!state.start_game_gate.all_current_ready(&state.present));
        if rejection_before_ready {
            state.handle_server_message(rejected()).await.unwrap();
        }
        state.handle_server_message(snapshot(true)).await.unwrap();
        let second = tokio::time::timeout(Duration::from_secs(1), server_ws.next())
            .await
            .expect("readiness recovery must retry before a delayed rejection")
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<ClientMessage>(second.to_text().unwrap()).unwrap(),
            ClientMessage::StartGame
        ));
        state.handle_server_message(snapshot(true)).await.unwrap();
        if !rejection_before_ready {
            state.handle_server_message(rejected()).await.unwrap();
        }
        state
            .handle_server_message(ServerMessage::GameStarting {
                peer_connections: vec![],
            })
            .await
            .unwrap();
        state.handle_server_message(snapshot(true)).await.unwrap();
        let unexpected = tokio::time::timeout(Duration::from_millis(25), server_ws.next()).await;
        assert!(
            unexpected.is_err(),
            "duplicate snapshots and finalization must not send extra commands"
        );
    }
}

// Regression #987: write backpressure must not consume the response window.
#[tokio::test]
async fn pong_response_window_starts_after_the_ping_write_finishes() {
    let (mut ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
    let url = format!("ws://{}", server_ws.get_ref().local_addr().unwrap());
    let payload = vec![42; 8 * 1024 * 1024];
    let frame = Message::Binary(payload.into());
    let queued = crate::socket_tests::queue_until_blocked(&mut ws, frame.clone()).await;
    let cli = Cli::parse_from(["reference-native", "--server-url", &url, "--create-room"]);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.next_ping_at = Instant::now();
    {
        let timer = state.process_timers();
        tokio::pin!(timer);
        let observation = tokio::time::timeout(Duration::from_millis(50), &mut timer).await;
        assert!(
            observation.is_err(),
            "establish socket backpressure before advancing the clock: {observation:?}"
        );
        tokio::time::pause();
        let clock_guard = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        tokio::time::advance(Duration::from_secs(12)).await;
        let drain = async {
            for _ in 0..queued {
                assert_eq!(server_ws.next().await.unwrap().unwrap(), frame);
            }
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
                    player: None,
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
                    received_at: tokio::time::Instant::now(),
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
                    received_at: tokio::time::Instant::now(),
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
            if let Err(error) = result {
                assert_eq!(error.code, EXIT_CONNECTION_ERROR);
            } else {
                accepted_late.push(elapsed);
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

// Regression #1031: queue service must preserve the original receipt window.
#[tokio::test]
async fn queued_exchange_receipts_complete_before_the_run_cutoff() {
    for (label, mode, p2p_timeout) in
        [RELIABLE_LABEL, UNRELIABLE_LABEL]
            .into_iter()
            .flat_map(|label| {
                [
                    "queued",
                    "on-time",
                    "exact",
                    "late",
                    "queued-exact",
                    "queued-late",
                    "stale",
                    "retired",
                    "retired-exact",
                    "retired-late",
                    "logical-replacement",
                    "closed-before",
                    "closed-after",
                    "held-restore",
                    "before-held",
                    "at-held-start",
                    "at-release-observation",
                    "released",
                    "released-linger",
                    "expired-linger",
                    "queued-linger",
                    "queued-linger-exact",
                    "queued-linger-late",
                ]
                .into_iter()
                .flat_map(move |mode| ["0", "15"].map(|timeout| (label, mode, timeout)))
            })
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let _server_ws = server.await.unwrap();
        let release_dir = tempfile::tempdir().unwrap();
        let release_file = release_dir.path().join("release");
        let mut args = vec![
            "reference-native",
            "--server-url",
            &url,
            "--create-room",
            "--exchange",
            "--peers",
            "1",
            "--p2p-timeout-secs",
            p2p_timeout,
        ];
        if matches!(
            mode,
            "held-restore"
                | "released"
                | "released-linger"
                | "expired-linger"
                | "before-held"
                | "at-held-start"
                | "at-release-observation"
        ) || mode.starts_with("queued-linger")
        {
            args.extend(["--success-release-file", release_file.to_str().unwrap()]);
        }
        let cli = Cli::parse_from(args);
        if matches!(
            mode,
            "released" | "released-linger" | "expired-linger" | "at-release-observation"
        ) || mode.starts_with("queued-linger")
        {
            std::fs::write(&release_file, "release").unwrap();
        }
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.engine.pair_with(peer, false, &[]).await.unwrap();
        state.game_started = true;
        state.connected_pairs.insert(peer);
        state.exchange_ledger.note_connected(peer);
        for other in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            state.exchange_ledger.note_sent(peer, other);
            if other != label {
                state.exchange_ledger.note_received(peer, other.to_string());
            }
        }
        let (events, queued) = tokio::sync::mpsc::unbounded_channel();
        state.engine_rx = queued;
        tokio::time::pause();
        let started_at = Instant::now();
        state.run_deadline = Deadline::after(started_at, Duration::from_secs(1));
        state.next_ping_at = Instant::now() + Duration::from_secs(60);
        let elapsed = match mode {
            "exact" | "queued-exact" | "retired-exact" => Duration::from_secs(1),
            "late"
            | "retired-late"
            | "queued-late"
            | "held-restore"
            | "released"
            | "released-linger"
            | "expired-linger"
            | "before-held"
            | "at-held-start"
            | "at-release-observation"
            | "queued-linger"
            | "queued-linger-exact"
            | "queued-linger-late" => Duration::from_millis(1001),
            _ => Duration::from_millis(999),
        };
        tokio::time::advance(elapsed).await;
        state.success_criteria_reported = matches!(
            mode,
            "held-restore"
                | "released"
                | "released-linger"
                | "expired-linger"
                | "before-held"
                | "at-held-start"
                | "at-release-observation"
        ) || mode.starts_with("queued-linger");
        if state.success_criteria_reported {
            state.success_hold_started_at = Some(match mode {
                "before-held" => Instant::now() + Duration::from_millis(1),
                "at-held-start" => Instant::now(),
                _ => started_at,
            });
            if !matches!(mode, "held-restore" | "before-held" | "at-held-start") {
                state.success_release_observed_at = Some(match mode {
                    "at-release-observation" => Instant::now(),
                    "released" => Instant::now() - Duration::from_millis(1),
                    _ => started_at,
                });
            }
        }
        if matches!(mode, "released-linger" | "queued-linger") {
            state.linger_until = Some(Instant::now() + EXIT_LINGER);
        } else if matches!(mode, "expired-linger" | "queued-linger-exact") {
            state.linger_until = Some(Instant::now());
        } else if mode == "queued-linger-late" {
            state.linger_until = Some(Instant::now() - Duration::from_millis(1));
        }
        let event = EngineEvent::ChannelMessage {
            peer,
            generation: if mode == "stale" { 0 } else { 1 },
            label: label.to_string(),
            text: json!({"from": peer, "channel": label, "seq": 0}).to_string(),
            queue_permit: crate::engine::MessageQueuePermit::for_test(),
            received_at: Instant::now(),
        };
        // Capture the admitted receipt before physical or logical retirement.
        // Physical loss preserves it; a newer authoritative link invalidates it.
        if mode.starts_with("retired") {
            state.engine.remove_peer(peer).await.unwrap();
        } else if mode == "logical-replacement" {
            state.prepare_retained_pair_replacement(peer).await;
        }
        let closed = || EngineEvent::ChannelClosed {
            peer,
            generation: 1,
            label: label.to_string(),
        };
        if mode == "closed-before" {
            events.send(closed()).unwrap();
        }
        if matches!(
            mode,
            "on-time"
                | "exact"
                | "late"
                | "held-restore"
                | "released"
                | "released-linger"
                | "expired-linger"
        ) {
            state.handle_engine_event(event).await.unwrap();
        } else {
            events.send(event).unwrap();
        }
        if mode == "closed-after" {
            events.send(closed()).unwrap();
        }
        if mode == "queued-linger" {
            tokio::time::advance(EXIT_LINGER).await;
        } else if mode == "before-held" {
            tokio::time::advance(Duration::from_millis(2)).await;
        }
        if elapsed < Duration::from_secs(1) {
            tokio::time::advance(Duration::from_secs(1) - elapsed).await;
        }
        let result = state.process_timers().await.unwrap();
        let valid = matches!(
            mode,
            "queued"
                | "on-time"
                | "closed-after"
                | "closed-before"
                | "retired"
                | "held-restore"
                | "at-held-start"
                | "released-linger"
                | "queued-linger"
        );
        assert_eq!(
            state.exchange_ledger.unmet_criteria().is_empty(),
            valid,
            "{label}/{mode}/p2p={p2p_timeout}: arrival and logical obligation own eligibility"
        );
        if mode == "before-held" {
            assert_eq!(
                result, None,
                "the current hold stays active without old receipt credit"
            );
            std::fs::write(&release_file, "release").unwrap();
            assert_eq!(
                state.process_timers().await.unwrap(),
                Some(EXIT_CRITERIA_UNMET)
            );
        } else if matches!(mode, "held-restore" | "at-held-start") {
            assert_eq!(result, None, "held success keeps serving fresh exchange");
            std::fs::write(&release_file, "release").unwrap();
            assert_eq!(state.process_timers().await.unwrap(), None);
            tokio::time::advance(EXIT_LINGER).await;
            assert_eq!(state.process_timers().await.unwrap(), Some(EXIT_SUCCESS));
        } else if mode == "released-linger" {
            assert_eq!(result, None, "active linger still defers cutoff");
            tokio::time::advance(EXIT_LINGER).await;
            assert_eq!(state.process_timers().await.unwrap(), Some(EXIT_SUCCESS));
        } else {
            assert_eq!(
                result,
                Some(if valid {
                    EXIT_SUCCESS
                } else {
                    EXIT_CRITERIA_UNMET
                }),
                "{label}/{mode}/p2p={p2p_timeout}: retain original cutoff"
            );
        }
        assert!(
            state.engine_rx.is_empty(),
            "cutoff must consume the queued events"
        );
        tokio::time::resume();
        state.engine.remove_peer(peer).await.unwrap();
    }
}

// Regression #1037: delayed relay timers cannot complete expired runs.
#[tokio::test]
async fn relay_timer_cannot_pay_send_debt_at_or_after_run_cutoff() {
    use futures_util::FutureExt;

    for encoding in [
        GameDataEncoding::Json,
        GameDataEncoding::MessagePack,
        GameDataEncoding::Rkyv,
        GameDataEncoding::Protobuf,
    ] {
        for (window, offset_ms, permitted) in [
            ("run", 999, true),
            ("run", 1000, false),
            ("run", 1001, false),
            ("hold", 1001, true),
            ("linger", 1001, true),
            ("expired-linger", 1250, false),
        ] {
            let (ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
            let mut cli = Cli::parse_from([
                "reference-native",
                "--server-url",
                "ws://unused.invalid",
                "--create-room",
                "--peers",
                "1",
                "--relay-payload",
                "cutoff-probe",
            ]);
            let directory = tempfile::tempdir().unwrap();
            let release = directory.path().join("release");
            if window != "run" {
                cli.success_release_file = Some(release.clone());
                if window != "hold" {
                    std::fs::write(&release, "release").unwrap();
                }
            }
            let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
            state.game_started = true;
            state.game_data_encoding = encoding;
            let started = Instant::now();
            state.run_deadline = (started + Duration::from_secs(1)).into();
            state.relay_send_at = Some(started + RELAY_SEND_SETTLE);
            if window != "run" {
                // Keep the established harness window policy as a compatibility
                // control at the relay timer seam.
                state.success_criteria_reported = true;
                state.success_hold_started_at = Some(started);
                if window != "hold" {
                    state.linger_until = Some(started + Duration::from_millis(1250));
                }
            }
            state.next_ping_at = started + Duration::from_secs(30);
            tokio::time::pause();
            tokio::time::advance(Duration::from_millis(offset_ms)).await;
            let outcome = state.process_timers().await.unwrap();
            if permitted {
                assert!(
                    state.relay_sent,
                    "{encoding:?}: on-time relay remains available"
                );
                assert!(outcome.is_none());
                let frame = server_ws.next().await.unwrap().unwrap();
                match encoding {
                    GameDataEncoding::Json => assert_eq!(
                        serde_json::from_str::<serde_json::Value>(frame.to_text().unwrap())
                            .unwrap(),
                        json!({"type":"GameData", "data":{"data":{"relay_msg":"cutoff-probe"}}}),
                    ),
                    GameDataEncoding::MessagePack => assert_eq!(
                        rmp_serde::from_slice::<serde_json::Value>(&frame.into_data()).unwrap(),
                        json!({"relay_msg":"cutoff-probe"}),
                    ),
                    GameDataEncoding::Rkyv | GameDataEncoding::Protobuf => {
                        assert_eq!(
                            frame.into_data().as_ref(),
                            wire::encode_opaque_relay_probe(
                                PlayerId::from_u128(1),
                                b"cutoff-probe"
                            )
                        );
                    }
                }
            } else {
                assert!(
                    !state.relay_sent,
                    "{encoding:?}/{offset_ms}: expired relay must retain send debt"
                );
                assert_eq!(outcome, Some(EXIT_CRITERIA_UNMET));
                assert!(
                    server_ws.next().now_or_never().is_none(),
                    "expired relay must not reach the socket"
                );
            }
            tokio::time::resume();
        }
    }
}

// Regression #1037: a socket write cannot renew the run window.
#[tokio::test]
async fn blocked_relay_write_keeps_the_original_run_cutoff() {
    for encoding in [
        GameDataEncoding::Json,
        GameDataEncoding::MessagePack,
        GameDataEncoding::Rkyv,
        GameDataEncoding::Protobuf,
    ] {
        let (mut ws, _server_ws) = crate::socket_tests::websocket_pair().await;
        let frame = Message::Binary(vec![42; 8 * 1024 * 1024].into());
        crate::socket_tests::queue_until_blocked(&mut ws, frame).await;
        let cli = Cli::parse_from([
            "reference-native",
            "--server-url",
            "ws://unused.invalid",
            "--create-room",
            "--peers",
            "1",
            "--relay-payload",
            "blocked-probe",
        ]);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.game_data_encoding = encoding;
        let started = Instant::now();
        state.run_deadline = (started + Duration::from_secs(1)).into();
        {
            let write = state.send_relay_payload();
            tokio::pin!(write);
            assert!(
                tokio::time::timeout(Duration::from_millis(25), &mut write)
                    .await
                    .is_err(),
                "fixture must block before the run cutoff"
            );
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::time::resume();
            let result = tokio::time::timeout(Duration::from_millis(100), &mut write).await;
            assert!(
                result.is_ok(),
                "{encoding:?}: blocked relay write must stop at the original cutoff"
            );
            assert_eq!(result.unwrap().unwrap_err().code, EXIT_CONNECTION_ERROR);
        }
        assert!(
            !state.relay_sent,
            "{encoding:?}: a canceled write keeps its debt"
        );
    }
}

// Regression #1039: a ready socket and due run timer may win the same select.
#[tokio::test]
async fn relay_receipt_selected_at_run_cutoff_cannot_complete_the_run() {
    use futures_util::{FutureExt, SinkExt};

    let mut invalid_outcomes = Vec::new();
    for encoding in [
        GameDataEncoding::Json,
        GameDataEncoding::MessagePack,
        GameDataEncoding::Rkyv,
        GameDataEncoding::Protobuf,
    ] {
        // Either input or timer may win at expiry. Neither may pay late debt.
        for offset_ms in [1000, 1001] {
            let (ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
            let cli = Cli::parse_from([
                "reference-native",
                "--server-url",
                "ws://unused.invalid",
                "--create-room",
                "--peers",
                "2",
                "--relay-payload",
                "probe",
            ]);
            let me = PlayerId::from_u128(1);
            let peer = PlayerId::from_u128(2);
            let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
            state.game_started = true;
            state.relay_sent = true;
            state.present.insert(peer);
            state.members_seen.insert(peer);
            state.game_data_encoding = encoding;
            state.negotiated_version = 3;
            state.accountability = DeliveryAccountability::new(true);
            let player = serde_json::from_value(json!({
                "id": peer, "name": "peer", "is_authority": false,
                "is_ready": true, "epoch": 1, "seq": 0,
            }))
            .unwrap();
            state.accountability.note_player_joined(&player).unwrap();
            let frame = relay_receipt_frame(encoding, peer, 3);
            tokio::time::pause();
            let clock_guard = tokio::spawn(async {
                loop {
                    tokio::task::yield_now().await;
                }
            });
            let started = Instant::now();
            state.run_deadline = (started + Duration::from_secs(1)).into();
            let result;
            {
                let run = state.run_loop();
                tokio::pin!(run);
                assert!(
                    run.as_mut().now_or_never().is_none(),
                    "enter the real input select before expiry"
                );
                tokio::time::advance(Duration::from_millis(offset_ms)).await;
                server_ws.send(frame).await.unwrap();
                for _ in 0..32 {
                    tokio::task::yield_now().await;
                }
                result = if let Some(result) = run.as_mut().now_or_never() {
                    result.unwrap()
                } else {
                    tokio::time::advance(EXIT_LINGER).await;
                    run.await.unwrap()
                };
            }
            let permitted = offset_ms < 1000;
            let expected = if permitted {
                EXIT_SUCCESS
            } else {
                EXIT_CRITERIA_UNMET
            };
            if result != expected || state.relay_received_from.contains(&peer) != permitted {
                invalid_outcomes.push((encoding, offset_ms, result));
            }
            clock_guard.abort();
            tokio::time::resume();
        }
    }
    assert!(
        invalid_outcomes.is_empty(),
        "selected receipt outcomes violated the run window: {invalid_outcomes:?}"
    );
}

fn relay_receipt_frame(encoding: GameDataEncoding, peer: PlayerId, version: u16) -> Message {
    if encoding == GameDataEncoding::Json {
        return Message::Text(
            serde_json::to_string(&ServerMessage::GameData {
                from_player: peer,
                data: json!({"relay_msg": "probe"}),
                seq: (version >= 3).then_some(1),
                epoch: (version >= 3).then_some(1),
                class: None,
                key: None,
            })
            .unwrap()
            .into(),
        );
    }
    let payload = if encoding == GameDataEncoding::MessagePack {
        rmp_serde::to_vec_named(&json!({"relay_msg": "probe"})).unwrap()
    } else {
        wire::encode_opaque_relay_probe(peer, b"probe")
    };
    if version == 2 {
        if encoding == GameDataEncoding::MessagePack {
            #[derive(serde::Serialize)]
            struct LegacyFrame {
                from_player: PlayerId,
                encoding: GameDataEncoding,
                payload: serde_bytes::ByteBuf,
            }
            return Message::Binary(
                rmp_serde::to_vec_named(&LegacyFrame {
                    from_player: peer,
                    encoding,
                    payload: payload.into(),
                })
                .unwrap()
                .into(),
            );
        }
        return Message::Binary(payload.into());
    }
    Message::Binary(
        rmp_serde::to_vec_named(&wire::V3BinaryGameDataFrame {
            from_player: peer,
            encoding,
            payload,
            seq: 1,
            epoch: 1,
        })
        .unwrap()
        .into(),
    )
}

// Regression #1039: decoding after expiry preserves an earlier observation.
#[tokio::test]
async fn relay_receipts_keep_their_observed_run_hold_and_linger_windows() {
    for version in [2, 3] {
        for encoding in [
            GameDataEncoding::Json,
            GameDataEncoding::MessagePack,
            GameDataEncoding::Rkyv,
            GameDataEncoding::Protobuf,
        ] {
            for (window, observed_ms, processed_ms, permitted) in [
                ("run", 999, 1001, true),
                ("run", 1000, 1001, false),
                ("run", 1001, 1001, false),
                ("hold", 1001, 1002, true),
                ("linger", 1249, 1251, true),
                ("linger", 1250, 1251, false),
            ] {
                let (ws, _server_ws) = crate::socket_tests::websocket_pair().await;
                let mut cli = Cli::parse_from([
                    "reference-native",
                    "--server-url",
                    "ws://unused.invalid",
                    "--create-room",
                    "--peers",
                    "2",
                    "--relay-payload",
                    "probe",
                ]);
                let directory = tempfile::tempdir().unwrap();
                let release = directory.path().join("release");
                if window != "run" {
                    cli.success_release_file = Some(release.clone());
                    if window == "linger" {
                        std::fs::write(&release, "release").unwrap();
                    }
                }
                let peer = PlayerId::from_u128(2);
                let mut state =
                    super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
                state.game_data_encoding = encoding;
                state.negotiated_version = version;
                state.accountability = DeliveryAccountability::new(version >= 3);
                let player = serde_json::from_value(json!({
                "id": peer, "name": "peer", "is_authority": false,
                "is_ready": true, "epoch": (version >= 3).then_some(1), "seq": (version >= 3).then_some(0),
            }))
            .unwrap();
                state.accountability.note_player_joined(&player).unwrap();
                tokio::time::pause();
                let started = Instant::now();
                state.run_deadline = (started + Duration::from_secs(1)).into();
                if window != "run" {
                    state.success_criteria_reported = true;
                    state.success_hold_started_at = Some(started);
                    if window == "linger" {
                        state.linger_until = Some(started + Duration::from_millis(1250));
                        state.success_release_observed_at = Some(started + Duration::from_secs(1));
                    }
                }
                let observed_at = started + Duration::from_millis(observed_ms);
                tokio::time::advance(Duration::from_millis(processed_ms)).await;
                match relay_receipt_frame(encoding, peer, version) {
                    Message::Text(text) => state
                        .handle_server_message_at(
                            wire::decode_server_text(&text).unwrap(),
                            observed_at,
                        )
                        .await
                        .unwrap(),
                    Message::Binary(bytes) => state
                        .handle_binary_game_data(&bytes, observed_at)
                        .await
                        .unwrap(),
                    _ => unreachable!(),
                }
                assert_eq!(state.relay_received_from.contains(&peer), permitted,
                "{encoding:?}/{window}/{observed_ms}: credit uses observation, not decode completion");
                if version >= 3 {
                    let error = state
                        .accountability
                        .record_game_data(peer, Some(1), Some(1), None, None)
                        .unwrap_err();
                    assert!(error.contains("duplicate/backward"),
                "{encoding:?}/{window}: even ineligible receipts retain delivery accounting: {error}");
                }
                tokio::time::resume();
            }
        }
    }
}

// Audit #853/#889: a remote probe can precede local remote-channel admission.
#[tokio::test]
async fn delayed_remote_channel_admission_preserves_early_exchange_probes() {
    for held_label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        let (ws, _server_ws) = crate::socket_tests::websocket_pair().await;
        let cli = Cli::parse_from([
            "native",
            "--server-url",
            "ws://unused.invalid",
            "--create-room",
            "--disable-mdns",
            "--exchange",
        ]);
        let me = PlayerId::from_u128(1);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
        state.current_session_generation = Some(SessionGeneration::from_u128(3));
        state.establish_pair(peer, false).await.unwrap();
        let (remote_tx, mut remote_events) = mpsc::unbounded_channel();
        let mut remote = Engine::new(cli.engine_settings(), remote_tx).unwrap();
        let offer = remote.pair_with(me, true, &[]).await.unwrap().unwrap();
        let answer = state.engine.handle_offer(peer, offer).await.unwrap();
        remote.handle_answer(me, answer).await.unwrap();
        let mut held_channel = None;
        let mut remote_opens = BTreeSet::new();
        tokio::time::timeout(Duration::from_secs(15), async {
            while held_channel.is_none() || remote_opens.len() != 2 {
                tokio::select! {
                    event = state.engine_rx.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            remote.handle_remote_candidate(me, &candidate_json).await.unwrap();
                        }
                        EngineEvent::RemoteChannel { label, channel, .. } if label == held_label => {
                            assert!(held_channel.replace(channel).is_none());
                        }
                        event => state.handle_engine_event(event).await.unwrap(),
                    },
                    event = remote_events.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            state.engine.handle_remote_candidate(peer, &candidate_json).await.unwrap();
                        }
                        EngineEvent::ChannelOpen { label, .. } => {
                            assert!(remote_opens.insert(label));
                        }
                        EngineEvent::ChannelClosed { label, .. } => panic!("remote channel closed: {label}"),
                        _ => {}
                    },
                }
            }
        }).await.expect("offerer opens both channels while one responder admission is held");
        assert!(!state.connected_pairs.contains(&peer));
        assert!(state.engine.channel(peer, held_label).is_none());
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            remote
                .channel(me, label)
                .unwrap()
                .send_text(&json!({"from": peer, "channel": label, "seq": 0}).to_string())
                .await
                .unwrap();
        }
        let admitted_label = if held_label == RELIABLE_LABEL {
            UNRELIABLE_LABEL
        } else {
            RELIABLE_LABEL
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while !state
                .exchange_ledger
                .received
                .get(&peer)
                .is_some_and(|labels| labels.contains(admitted_label))
            {
                let event = state.engine_rx.recv().await.unwrap();
                state.handle_engine_event(event).await.unwrap();
            }
        })
        .await
        .expect("the admitted sibling receives its probe before pair formation");
        assert!(!state.connected_pairs.contains(&peer));
        assert!(!state.exchange_ledger.received[&peer].contains(held_label));
        // The stack increments this counter when it decodes a complete SCTP
        // message, before the application polls the held channel. No sleep or
        // cross-stream ordering assumption establishes this delivery barrier.
        tokio::time::timeout(Duration::from_secs(2), async {
            while state
                .engine
                .channel_received_messages(peer, held_label)
                .await
                != 1
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the held probe reaches the receiver before channel admission");
        let held_channel = held_channel.unwrap();
        state
            .engine
            .store_remote_channel(peer, held_label.into(), held_channel.clone())
            .await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while !state.connected_pairs.contains(&peer)
                || state.exchange_ledger.received[&peer].len() != 2
            {
                let event = state.engine_rx.recv().await.unwrap();
                state.handle_engine_event(event).await.unwrap();
            }
        })
        .await
        .expect("late admission drains the already queued open and probe");
        assert_eq!(
            state.exchange_ledger.received[&peer],
            BTreeSet::from([RELIABLE_LABEL.to_string(), UNRELIABLE_LABEL.to_string(),])
        );
        assert_eq!(state.deferred_exchange.len(), 2);
        state
            .engine
            .store_remote_channel(peer, held_label.into(), held_channel)
            .await;
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            assert!(
                !state.engine.note_channel_open(peer, label).await,
                "repeated admission and open observation cannot form another pair"
            );
        }
        assert_eq!(state.deferred_exchange.len(), 2);
        while !state.deferred_exchange.is_empty() {
            state.process_deferred_exchange().await.unwrap();
        }
        assert!(state.exchange_ledger.unmet_criteria().is_empty());
        let mut receipts = BTreeSet::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while receipts.len() != 2 {
                if let EngineEvent::ChannelMessage { label, text, .. } =
                    remote_events.recv().await.unwrap()
                {
                    assert!(is_exchange_probe(me, &label, &text));
                    assert!(receipts.insert(label));
                }
            }
        })
        .await
        .expect("both reply probes reach the offerer after delayed admission");
        state.remove_pair_obligation(peer).await;
        remote.remove_peer(me).await.unwrap();
    }
}

// Regression #1067: retry work must not hide a timely queued Pong.
#[tokio::test]
async fn queued_liveness_survives_negotiation_during_pong_drain_grace() {
    use crate::engine::NegotiationHold;
    use std::sync::Arc;
    use tokio_tungstenite::tungstenite::protocol::{
        frame::{
            coding::{Data, OpCode},
            Frame,
        },
        Role,
    };

    let (ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
    let cli = Cli::parse_from([
        "reference-native",
        "--server-url",
        "ws://127.0.0.1",
        "--create-room",
        "--disable-mdns",
        "--p2p-retry-count",
        "1",
    ]);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.current_session_generation = Some(SessionGeneration::from_u128(3));
    state.next_ping_at = Instant::now();
    assert!(state.process_timers().await.unwrap().is_none());
    let ping = server_ws.next().await.unwrap().unwrap();
    let ping: ClientMessage = serde_json::from_str(ping.to_text().unwrap()).unwrap();
    assert!(matches!(ping, ClientMessage::Ping));
    let mut queued = Vec::new();
    Frame::message(
        serde_json::to_vec(&ServerMessage::Pong).unwrap(),
        OpCode::Data(Data::Text),
        true,
    )
    .format(&mut queued)
    .unwrap();
    state.ws = tokio_tungstenite::WebSocketStream::from_partially_read(
        state.ws.into_inner(),
        queued,
        Role::Client,
        None,
    )
    .await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    state.engine.negotiation_hold = Some(NegotiationHold {
        step: "route",
        entered: entered.clone(),
        release: release.clone(),
    });
    tokio::time::pause();
    let clock_guard = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    let peer = PlayerId::from_u128(2);
    state.expected_peers.insert(peer);
    state.pair_roles.insert(peer, false);
    state.p2p_deadline = Some(Deadline::after(
        Instant::now(),
        PONG_TIMEOUT + Duration::from_secs(10),
    ));
    state.p2p_retry_at = state.pong_deadline;
    tokio::time::advance(PONG_TIMEOUT).await;
    let mut pairing = Box::pin(state.process_timers());
    tokio::select! {
        _ = entered.notified() => {},
        result = &mut pairing => panic!("pairing ended before hold: {result:?}"),
    }
    tokio::time::advance(PONG_DRAIN_GRACE + Duration::from_millis(1)).await;
    release.notify_one();
    assert!(pairing.await.unwrap().is_none());
    assert!(state.pong_grace_applied);
    assert_eq!(state.pair_retry_attempts.get(&peer), Some(&1));
    assert!(state
        .process_timers()
        .await
        .expect("read the queued timely Pong before keepalive failure")
        .is_none());
    let renewed_deadline = state
        .pong_deadline
        .expect("new Ping retains its own response debt");
    assert_eq!(
        renewed_deadline,
        checked_deadline(Instant::now(), PONG_TIMEOUT)
    );
    assert!(!state.pong_grace_applied);
    assert!(state.process_timers().await.unwrap().is_none());
    assert_eq!(state.pong_deadline, Some(renewed_deadline));
    clock_guard.abort();
    tokio::time::resume();
    state.engine.negotiation_hold = None;
    state.engine.remove_peer(peer).await.unwrap();
}

#[tokio::test]
async fn keepalive_final_poll_preserves_frame_errors_and_bounded_control_work() {
    use tokio_tungstenite::tungstenite::protocol::{
        frame::{
            coding::{Data, OpCode},
            Frame,
        },
        Role,
    };

    for (kind, prefix) in [
        ("pong", 0),
        ("pong", 32),
        ("relay", 32),
        ("missing", 0),
        ("control", 32),
        ("control", MAX_KEEPALIVE_CONTROL_FRAMES),
        ("malformed", 32),
        ("close", 32),
        ("pong", MAX_KEEPALIVE_CONTROL_FRAMES),
    ] {
        let (ws, _server_ws) = crate::socket_tests::websocket_pair().await;
        let cli = Cli::parse_from([
            "reference-native",
            "--server-url",
            "ws://127.0.0.1",
            "--create-room",
        ]);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        let mut queued = Vec::new();
        for _ in 0..prefix {
            Frame::pong(Vec::new()).format(&mut queued).unwrap();
        }
        match kind {
            "pong" | "relay" => {
                let message = if kind == "pong" {
                    ServerMessage::Pong
                } else {
                    ServerMessage::GameData {
                        from_player: peer,
                        data: json!({"relay_msg":"during pairing"}),
                        seq: None,
                        epoch: None,
                        class: None,
                        key: None,
                    }
                };
                Frame::message(
                    serde_json::to_vec(&message).unwrap(),
                    OpCode::Data(Data::Text),
                    true,
                )
                .format(&mut queued)
                .unwrap();
            }
            "malformed" => {
                Frame::message(b"invalid-json".to_vec(), OpCode::Data(Data::Text), true)
                    .format(&mut queued)
                    .unwrap();
            }
            "close" => {
                Frame::close(None).format(&mut queued).unwrap();
            }
            _ => {}
        }
        state.ws = tokio_tungstenite::WebSocketStream::from_partially_read(
            state.ws.into_inner(),
            queued,
            Role::Client,
            None,
        )
        .await;
        state.pong_deadline = Some(Instant::now());
        state.pong_grace_applied = true;
        let result = state.process_timers().await;
        if matches!(kind, "pong" | "relay") && prefix < MAX_KEEPALIVE_CONTROL_FRAMES {
            assert!(result.unwrap().is_none(), "{kind}/{prefix}");
            assert!(state.pong_deadline.is_none(), "{kind}/{prefix}");
            assert_eq!(state.relay_received_from.contains(&peer), kind == "relay");
        } else {
            let error = result.expect_err(&format!("{kind}/{prefix}"));
            assert_eq!(
                error.code,
                if kind == "malformed" {
                    EXIT_PROTOCOL_ERROR
                } else {
                    EXIT_CONNECTION_ERROR
                }
            );
            if kind == "close" {
                assert!(
                    error.message.contains("websocket closed by server"),
                    "{error:?}"
                );
            } else if kind != "malformed" {
                assert!(
                    error.message.contains("server did not answer Ping"),
                    "{error:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn admitted_exchange_receipts_survive_physical_retirement_only() {
    let mut failures = Vec::new();
    for retirement in ["loss", "timeout", "replacement", "departure", "incarnation"] {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            for late in [false, true] {
                let (ws, _server_ws) = crate::socket_tests::websocket_pair().await;
                let cli = Cli::parse_from([
                    "reference-native",
                    "--server-url",
                    "ws://unused.invalid",
                    "--create-room",
                    "--exchange",
                ]);
                let me = PlayerId::from_u128(1);
                let peer = PlayerId::from_u128(2);
                let other = PlayerId::from_u128(3);
                let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
                state.engine.pair_with(peer, false, &[]).await.unwrap();
                state.expected_peers.insert(peer);
                state.exchange_ledger.note_connected(peer);
                let received_at = Instant::now();
                state.run_deadline = (received_at + Duration::from_secs(10)).into();
                let receipt = |generation, received_at| EngineEvent::ChannelMessage {
                    peer,
                    generation,
                    label: label.into(),
                    text: json!({"from": peer, "channel": label, "seq": 0}).to_string(),
                    queue_permit: crate::engine::MessageQueuePermit::for_test(),
                    received_at,
                };
                let event = receipt(
                    1,
                    if late {
                        received_at + Duration::from_secs(11)
                    } else {
                        received_at
                    },
                );
                match retirement {
                    "loss" => state
                        .handle_peer_transport_loss(peer, "controlled loss")
                        .await
                        .unwrap(),
                    "timeout" => {
                        let result = state
                            .engine
                            .negotiate(
                                peer,
                                Negotiation::Candidate(String::new()),
                                received_at.into(),
                            )
                            .await;
                        assert!(result.is_err());
                    }
                    "replacement" => state.prepare_retained_pair_replacement(peer).await,
                    "departure" => state.remove_pair_obligation(peer).await,
                    "incarnation" => state.reset_peer_exchange_incarnation(peer).await,
                    _ => unreachable!(),
                }
                // A different peer takes generation 2 before this peer retries.
                // Queued receipts must retain their original peer ownership.
                state.engine.pair_with(other, false, &[]).await.unwrap();
                state.engine.pair_with(peer, false, &[]).await.unwrap();
                state.expected_peers.insert(peer);
                state.exchange_ledger.note_connected(peer);
                assert!(state.engine.is_current_generation(peer, 3));
                assert!(!state.engine.owns_pending_receipt(other, 1));
                assert!(!state.engine.owns_pending_receipt(peer, 4));
                state.handle_engine_event(event).await.unwrap();
                let observed = state
                    .exchange_ledger
                    .received
                    .get(&peer)
                    .is_some_and(|labels| labels.contains(label));
                let expected = !late && matches!(retirement, "loss" | "timeout");
                if observed != expected {
                    failures.push(format!("{retirement}/{label}/late={late}: received={observed}, expected={expected}"));
                }
                // Keeping receipt history must not revive old lifecycle work.
                state
                    .handle_engine_event(EngineEvent::IceGatheringComplete {
                        peer,
                        generation: 1,
                    })
                    .await
                    .unwrap();
                assert!(!state.ice_gathering_complete.contains(&peer));
                state
                    .handle_engine_event(EngineEvent::ChannelClosed {
                        peer,
                        generation: 1,
                        label: label.into(),
                    })
                    .await
                    .unwrap();
                assert!(state.engine.is_current_generation(peer, 3));
                // Logical invalidation must permit fresh physical receipts.
                state
                    .handle_engine_event(receipt(3, received_at))
                    .await
                    .unwrap();
                assert!(state.exchange_ledger.received[&peer].contains(label));
                state.engine.remove_peer(peer).await.unwrap();
                state.engine.remove_peer(other).await.unwrap();
                state.remove_pair_obligation(peer).await;
                state.remove_pair_obligation(other).await;
                assert!(!state.engine.owns_pending_receipt(peer, 1));
                assert!(!state.engine.owns_pending_receipt(peer, 3));
                assert!(!state.engine.owns_pending_receipt(other, 2));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "admitted receipt histories: {failures:#?}"
    );
}

#[tokio::test]
async fn live_admitted_exchange_receipts_survive_negotiation_timeout() {
    use crate::engine::{Engine, EngineSettings};

    let (ws, _server_ws) = crate::socket_tests::websocket_pair().await;
    let cli = Cli::parse_from([
        "reference-native",
        "--server-url",
        "ws://unused.invalid",
        "--create-room",
        "--exchange",
        "--disable-mdns",
    ]);
    let me = PlayerId::from_u128(1);
    let peer = PlayerId::from_u128(2);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
    let (sender_tx, mut sender_rx) = mpsc::unbounded_channel();
    let mut sender = Engine::new(
        EngineSettings {
            disable_mdns: true,
            ..EngineSettings::default()
        },
        sender_tx,
    )
    .unwrap();
    let offer = sender.pair_with(me, true, &[]).await.unwrap().unwrap();
    state.engine.pair_with(peer, false, &[]).await.unwrap();
    let answer = state.engine.handle_offer(peer, offer).await.unwrap();
    sender.handle_answer(me, answer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut sender_connected = false;
        let mut receiver_connected = false;
        while !sender_connected || !receiver_connected {
            tokio::select! {
                event = sender_rx.recv() => match event.unwrap() {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        state.engine.handle_remote_candidate(peer, &candidate_json).await.unwrap();
                    }
                    EngineEvent::RemoteChannel { label, channel, .. } => {
                        sender.store_remote_channel(me, label, channel).await;
                    }
                    EngineEvent::ChannelOpen { label, .. } => {
                        sender_connected |= sender.note_channel_open(me, &label).await;
                    }
                    _ => {}
                },
                event = state.engine_rx.recv() => match event.unwrap() {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        sender.handle_remote_candidate(me, &candidate_json).await.unwrap();
                    }
                    EngineEvent::RemoteChannel { label, channel, .. } => {
                        state.engine.store_remote_channel(peer, label, channel).await;
                    }
                    EngineEvent::ChannelOpen { label, .. } => {
                        receiver_connected |= state.engine.note_channel_open(peer, &label).await;
                    }
                    _ => {}
                },
            }
        }
    })
    .await
    .expect("real peer engines open both channels");
    state.expected_peers.insert(peer);
    state.exchange_ledger.note_connected(peer);
    state.run_deadline = (Instant::now() + Duration::from_secs(30)).into();
    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        sender
            .channel(me, label)
            .unwrap()
            .send_text(&json!({"from": peer, "channel": label, "seq": 0}).to_string())
            .await
            .unwrap();
    }
    // Hold real published callbacks, including their shared queue permits,
    // until the connection has been physically removed by a timeout.
    let receipts = tokio::time::timeout(Duration::from_secs(15), async {
        let mut receipts = Vec::new();
        while receipts.len() < 2 {
            match state.engine_rx.recv().await.unwrap() {
                event @ EngineEvent::ChannelMessage { .. } => receipts.push(event),
                EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => {
                    panic!("healthy link closed before its receipts were admitted");
                }
                _ => {}
            }
        }
        receipts
    })
    .await
    .expect("both remote exchange messages reach the engine queue");
    assert!(state.exchange_ledger.received.is_empty());
    assert!(state
        .engine
        .negotiate(
            peer,
            Negotiation::Candidate(String::new()),
            Instant::now().into(),
        )
        .await
        .is_err());
    assert!(!state.engine.is_paired(peer));
    for event in receipts {
        assert!(!state.engine.is_current_event(&event));
        state.handle_engine_event(event).await.unwrap();
    }
    assert!(state.exchange_ledger.received[&peer].contains(RELIABLE_LABEL));
    assert!(state.exchange_ledger.received[&peer].contains(UNRELIABLE_LABEL));
    sender.remove_peer(me).await.unwrap();
    state.remove_pair_obligation(peer).await;
}
