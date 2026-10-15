use super::*;
use clap::Parser;
use futures_util::StreamExt;

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
