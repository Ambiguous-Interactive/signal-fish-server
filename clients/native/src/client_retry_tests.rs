// Regression #923: dispatch retries through the real native engine.
use super::*;
use clap::Parser;
use futures_util::StreamExt;

/*
    Keep real open callbacks queued so tests control their dispatch time.
*/
async fn open_physical_pair(
    state: &mut Orchestrator<'_>,
    server_ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    peer: PlayerId,
    saved_offer: Option<String>,
) -> (crate::engine::Engine, Vec<EngineEvent>) {
    let (engine, opens, _) = Box::pin(open_physical_pair_with_events(
        state,
        server_ws,
        peer,
        saved_offer,
    ))
    .await;
    (engine, opens)
}

pub(super) async fn open_physical_pair_with_events(
    state: &mut Orchestrator<'_>,
    server_ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    peer: PlayerId,
    saved_offer: Option<String>,
) -> (
    crate::engine::Engine,
    Vec<EngineEvent>,
    tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
) {
    let initiate = state.pair_roles[&peer];
    let mut offer = saved_offer;
    state.send_message(&ClientMessage::Ping).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = server_ws.next().await.unwrap().unwrap();
            if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
                let message: serde_json::Value = serde_json::from_str(&text).unwrap();
                if let Some(sdp) = message["data"]["signal"]["Offer"].as_str() {
                    offer = Some(sdp.to_string());
                }
                if message["type"] == "Ping" {
                    break;
                }
            }
        }
    })
    .await
    .expect("drain the current offer through the signaling socket");
    let (remote_tx, mut remote_events) = tokio::sync::mpsc::unbounded_channel();
    let mut remote = crate::engine::Engine::new(state.cli.engine_settings(), remote_tx).unwrap();
    if initiate {
        remote.pair_with(state.my_id, false, &[]).await.unwrap();
        let answer = remote
            .handle_offer(state.my_id, offer.expect("initiator sent its SDP offer"))
            .await
            .unwrap();
        state.engine.handle_answer(peer, answer).await.unwrap();
    } else {
        let offer = remote
            .pair_with(state.my_id, true, &[])
            .await
            .unwrap()
            .unwrap();
        let answer = state.engine.handle_offer(peer, offer).await.unwrap();
        remote.handle_answer(state.my_id, answer).await.unwrap();
    }
    let mut opens = Vec::new();
    let mut local_opens = BTreeSet::new();
    let mut remote_opens = BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        while local_opens.len() < 2 || remote_opens.len() < 2 {
            tokio::select! {
                event = state.engine_rx.recv() => {
                    let event = event.unwrap();
                    if !state.engine.is_current_event(&event) { continue; }
                    match event {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            remote.handle_remote_candidate(state.my_id, &candidate_json).await.unwrap();
                        }
                        EngineEvent::RemoteChannel { label, channel, .. } => {
                            state.engine.store_remote_channel(peer, label, channel).await;
                        }
                        event @ EngineEvent::ChannelOpen { .. } => {
                            let EngineEvent::ChannelOpen { label, .. } = &event else { unreachable!() };
                            assert!(local_opens.insert(label.clone()), "duplicate local open during negotiation");
                            opens.push(event);
                        }
                        EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => {
                            panic!("local required channel failed during negotiation");
                        }
                        EngineEvent::PcState { state, .. } if is_terminal_peer_connection_state(&state) => {
                            panic!("local peer connection terminated during negotiation: {state}");
                        }
                        _ => {}
                    }
                }
                event = remote_events.recv() => match event.unwrap() {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        state.engine.handle_remote_candidate(peer, &candidate_json).await.unwrap();
                    }
                    EngineEvent::RemoteChannel { label, channel, .. } => {
                        remote.store_remote_channel(state.my_id, label, channel).await;
                    }
                    EngineEvent::ChannelOpen { label, .. } => {
                        assert!(remote_opens.insert(label), "duplicate remote open during negotiation");
                    }
                    EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => {
                        panic!("remote required channel failed during negotiation");
                    }
                    EngineEvent::PcState { state, .. } if is_terminal_peer_connection_state(&state) => {
                        panic!("remote peer connection terminated during negotiation: {state}");
                    }
                    _ => {}
                }
            }
        }
    }).await.expect("both real local data channels open");
    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        assert!(local_opens.contains(label));
        assert!(remote_opens.contains(label));
        for channel in [
            state.engine.channel(peer, label),
            remote.channel(state.my_id, label),
        ] {
            assert_eq!(
                channel.unwrap().ready_state().await.unwrap(),
                rtc::data_channel::RTCDataChannelState::Open
            );
        }
    }
    (remote, opens, remote_events)
}

#[tokio::test]
async fn formed_pair_accepts_remote_retry_after_window_expiry() {
    for initiate in [false, true] {
        for close_before_marker in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                tokio_tungstenite::accept_async(tcp).await.unwrap()
            });
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let mut server_ws = server.await.unwrap();
            let cli = Cli::parse_from([
                "reference-native",
                "--server-url",
                &url,
                "--create-room",
                "--disable-mdns",
                "--p2p-retry-count",
                "2",
                "--p2p-timeout-secs",
                "0",
            ]);
            let peer = PlayerId::from_u128(2);
            let generation = SessionGeneration::from_u128(3);
            let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
            state.current_session_generation = Some(generation);
            state.negotiated_version = 3;
            state.webrtc_plan_seen = true;
            state.establish_pair(peer, initiate).await.unwrap();
            // A due local attempt must not publish a marker without rebuilding.
            state.request_pair_retry(peer, 1).await.unwrap();
            state.send_message(&ClientMessage::Ping).await.unwrap();
            let mut initial_offer = None;
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let frame = server_ws.next().await.unwrap().unwrap();
                    if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
                        let message: serde_json::Value = serde_json::from_str(&text).unwrap();
                        assert!(
                            message["data"]["signal"]["PairRetry"].is_null(),
                            "an expired local retry cannot make only the remote endpoint rebuild"
                        );
                        if let Some(sdp) = message["data"]["signal"]["Offer"].as_str() {
                            initial_offer = Some(sdp.to_string());
                        }
                        if message["type"] == "Ping" {
                            break;
                        }
                    }
                }
            })
            .await
            .unwrap();
            // An expired, never-formed link cannot reopen its initial window.
            state.process_timers().await.unwrap();
            state
                .handle_signal(peer, generation, json!({"PairRetry": 1}))
                .await
                .unwrap();
            assert!(state.pair_retry_attempts.is_empty());
            let (_initial_remote, opens) =
                open_physical_pair(&mut state, &mut server_ws, peer, initial_offer).await;
            for event in opens {
                state.handle_engine_event(event).await.unwrap();
            }
            assert!(state.connected_pairs.contains(&peer));
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                state.exchange_ledger.note_sent(peer, label);
                state.exchange_ledger.note_received(peer, label.to_string());
            }
            state.process_timers().await.unwrap();
            assert!(state.p2p_deadline.is_none());
            if close_before_marker {
                state
                    .handle_engine_event(EngineEvent::ChannelClosed {
                        peer,
                        generation: 1,
                        label: RELIABLE_LABEL.to_string(),
                    })
                    .await
                    .unwrap();
            }
            state
                .handle_signal(peer, generation, json!({"PairRetry": 1}))
                .await
                .unwrap();
            assert_eq!(state.pair_retry_attempts.get(&peer), Some(&1),
                "initiate={initiate}, close_before_marker={close_before_marker}: formed peer must join remote rebuild without a local harness gate");
            assert!(state.engine.is_current_generation(peer, 2));
            assert!(state.retrying_pairs.contains(&peer));
            assert!(!state.connected_pairs.contains(&peer));
            assert_eq!(state.pair_roles.get(&peer), Some(&initiate));
            assert!(state.exchange_ledger.unmet_criteria().is_empty());
            assert!(state
                .unmet_criteria()
                .iter()
                .any(|reason| reason.contains("pair retry")));
            for (from, wire_generation, payload) in [
                (peer, generation, json!({"PairRetry": 1})),
                (peer, generation, json!({"PairRetry": 0})),
                (peer, generation, json!({"PairRetry": 3})),
                (peer, generation, json!({"PairRetry": "2"})),
                (
                    peer,
                    SessionGeneration::from_u128(4),
                    json!({"PairRetry": 2}),
                ),
                (PlayerId::from_u128(4), generation, json!({"PairRetry": 1})),
            ] {
                state
                    .handle_signal(from, wire_generation, payload)
                    .await
                    .unwrap();
                assert!(state.engine.is_current_generation(peer, 2));
            }
            // A live window stays fixed even when a previously formed peer retries.
            let deadline = Deadline::after(Instant::now(), Duration::from_secs(60));
            state.p2p_deadline = Some(deadline);
            state
                .handle_signal(peer, generation, json!({"PairRetry": 2}))
                .await
                .unwrap();
            assert_eq!(state.p2p_deadline.unwrap().finite(), deadline.finite());
            assert!(state.engine.is_current_generation(peer, 3));
            state
                .handle_signal(peer, generation, json!({"PairRetry": 1}))
                .await
                .unwrap();
            assert!(state.engine.is_current_generation(peer, 3));
            state.p2p_deadline = Some(Deadline::after(Instant::now(), Duration::ZERO));
            state.process_timers().await.unwrap();
            assert!(state.retrying_pairs.is_empty());
            let (_replacement_remote, opens) =
                open_physical_pair(&mut state, &mut server_ws, peer, None).await;
            for event in opens {
                state.handle_engine_event(event).await.unwrap();
            }
            assert!(
                state.p2p_reconnected_pairs.contains(&peer),
                "replacement opening after wait expiry must still report recovery"
            );
            // A newer authoritative physical generation cannot inherit formation.
            state.prepare_retained_pair_replacement(peer).await;
            assert!(!state.formed_pairs.contains(&peer));
            state.p2p_deadline = None;
            state
                .handle_engine_event(EngineEvent::ChannelOpen {
                    peer,
                    generation: 3,
                    label: RELIABLE_LABEL.to_string(),
                })
                .await
                .unwrap();
            state
                .handle_signal(peer, generation, json!({"PairRetry": 1}))
                .await
                .unwrap();
            assert!(state.pair_retry_attempts.is_empty());
            assert!(!state.engine.is_paired(peer));
            state.remove_pair_obligation(peer).await;
            assert!(!state.formed_pairs.contains(&peer));
        }
    }
}

// Regression #930: recovery describes a retained logical pair, not a retry marker.
#[tokio::test]
async fn authoritative_replacement_reports_recovery_before_and_after_timeout() {
    for initiate in [false, true] {
        for expired in [false, true] {
            for previously_formed in [false, true] {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("ws://{}", listener.local_addr().unwrap());
                let server = tokio::spawn(async move {
                    let (tcp, _) = listener.accept().await.unwrap();
                    tokio_tungstenite::accept_async(tcp).await.unwrap()
                });
                let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
                let mut server_ws = server.await.unwrap();
                let cli = Cli::parse_from([
                    "reference-native",
                    "--server-url",
                    &url,
                    "--create-room",
                    "--disable-mdns",
                    "--p2p-timeout-secs",
                    "60",
                ]);
                let peer = PlayerId::from_u128(2);
                let mut state =
                    super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
                state.negotiated_version = 3;
                let plan = |generation| {
                    serde_json::from_value::<ServerMessage>(json!({
                    "type": "SessionPlan", "data": {
                        "generation": SessionGeneration::from_u128(generation),
                        "topology": "mesh", "transport": "webrtc", "fallback": "relay",
                        "peers": [{"player_id": peer, "player_name": "peer", "is_authority": false, "initiate": initiate}]
                    }
                })).unwrap()
                };
                state.handle_server_message(plan(3)).await.unwrap();
                let mut initial_remote = None;
                if previously_formed {
                    let (remote, opens) =
                        open_physical_pair(&mut state, &mut server_ws, peer, None).await;
                    initial_remote = Some(remote);
                    for event in opens {
                        state.handle_engine_event(event).await.unwrap();
                    }
                    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                        state.exchange_ledger.note_sent(peer, label);
                        state.exchange_ledger.note_received(peer, label.to_string());
                    }
                }
                state.handle_server_message(plan(4)).await.unwrap();
                state.handle_server_message(plan(4)).await.unwrap();
                assert!(
                    state.engine.is_current_generation(peer, 2),
                    "duplicate plan cannot rebuild"
                );
                assert!(state.pair_retry_attempts.is_empty());
                if expired {
                    state.p2p_deadline = Some(Deadline::after(Instant::now(), Duration::ZERO));
                    state.process_timers().await.unwrap();
                    assert!(state.retrying_pairs.is_empty());
                }
                // Old callbacks cannot report recovery for the new generation.
                for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                    state
                        .handle_engine_event(EngineEvent::ChannelOpen {
                            peer,
                            generation: 1,
                            label: label.to_string(),
                        })
                        .await
                        .unwrap();
                }
                assert!(!state.connected_pairs.contains(&peer));
                assert!(!state.p2p_reconnected_pairs.contains(&peer));
                let (_replacement_remote, mut opens) =
                    open_physical_pair(&mut state, &mut server_ws, peer, None).await;
                let reliable = opens.iter().position(|event| matches!(event, EngineEvent::ChannelOpen { label, .. } if label == RELIABLE_LABEL)).unwrap();
                state
                    .handle_engine_event(opens.remove(reliable))
                    .await
                    .unwrap();
                assert!(
                    !state.p2p_reconnected_pairs.contains(&peer),
                    "one channel cannot report recovery"
                );
                for label in [UNRELIABLE_LABEL, RELIABLE_LABEL, UNRELIABLE_LABEL] {
                    state
                        .handle_engine_event(EngineEvent::ChannelOpen {
                            peer,
                            generation: 2,
                            label: label.to_string(),
                        })
                        .await
                        .unwrap();
                }
                assert_eq!(state.p2p_reconnected_pairs.contains(&peer), previously_formed,
                    "initiate={initiate}, expired={expired}, previously_formed={previously_formed}: replacement reports recovery only for a previously formed logical pair");
                assert!(state.pair_connected_reported.contains(&peer));
                assert!(state.connected_pairs.contains(&peer));
                if previously_formed {
                    assert!(state.exchange_ledger.unmet_criteria().is_empty());
                }
                state
                    .handle_server_message(ServerMessage::PlayerLeft {
                        player_id: peer,
                        epoch: None,
                        final_seq: None,
                    })
                    .await
                    .unwrap();
                assert!(!state.pair_connected_reported.contains(&peer));
                assert!(!state.p2p_reconnected_pairs.contains(&peer));
                state.handle_server_message(plan(5)).await.unwrap();
                let (_rejoined_remote, _opens) =
                    open_physical_pair(&mut state, &mut server_ws, peer, None).await;
                for generation in [2, 3] {
                    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                        state
                            .handle_engine_event(EngineEvent::ChannelOpen {
                                peer,
                                generation,
                                label: label.to_string(),
                            })
                            .await
                            .unwrap();
                    }
                    assert!(
                        !state.p2p_reconnected_pairs.contains(&peer),
                        "departure clears the logical recovery history"
                    );
                }
                assert!(state.connected_pairs.contains(&peer));
                state.remove_pair_obligation(peer).await;
                drop(initial_remote);
            }
        }
    }
}

#[tokio::test]
async fn authoritative_recovery_cannot_release_reserved_retry_exchange() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(tcp).await.unwrap()
    });
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _server_ws = server.await.unwrap();
    let gate = tempfile::NamedTempFile::new().unwrap();
    let path = gate.path().to_str().unwrap();
    let cli = Cli::parse_from([
        "reference-native",
        "--server-url",
        &url,
        "--create-room",
        "--disable-mdns",
        "--exchange",
        "--exchange-release-file",
        path,
        "--p2p-rebuild-release-file",
        path,
        "--p2p-retry-count",
        "2",
    ]);
    let peer = PlayerId::from_u128(2);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.expected_peers.insert(peer);
    state.connected_pairs.insert(peer);
    state.p2p_reconnected_pairs.insert(peer);
    state.p2p_rebuild_released = true;
    // Completed earlier exchange avoids requiring physical channels in this gate control.
    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        state.exchange_ledger.note_sent(peer, label);
    }
    for attempt in [None, Some(1), Some(2)] {
        if let Some(attempt) = attempt {
            state.pair_retry_attempts.insert(peer, attempt);
        }
        state.process_exchange_gate(Instant::now()).await.unwrap();
        assert_eq!(
            state.exchange_released,
            attempt == Some(2),
            "only the reserved PairRetry generation can release the held exchange: {attempt:?}"
        );
    }
}

// A busy dispatcher must not turn historical open edges into live recovery.
#[tokio::test]
async fn queued_opens_after_physical_close_do_not_report_pair_recovery() {
    for closed_label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut server_ws = server.await.unwrap();
        let held_exchange = tempfile::tempdir().unwrap();
        let release_path = held_exchange.path().join("release");
        let cli = Cli::parse_from([
            "reference-native",
            "--server-url",
            &url,
            "--create-room",
            "--disable-mdns",
            "--exchange",
            "--exchange-release-file",
            release_path.to_str().unwrap(),
        ]);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.current_session_generation = Some(SessionGeneration::from_u128(3));
        state.negotiated_version = 3;
        state.webrtc_plan_seen = true;
        state.establish_pair(peer, true).await.unwrap();
        let (mut initial_remote, opens) =
            open_physical_pair(&mut state, &mut server_ws, peer, None).await;
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        assert!(state.formed_pairs.contains(&peer));
        assert!(state.exchange_ledger.sent.is_empty());
        state.prepare_retained_pair_replacement(peer).await;
        state.exchange_released = true;
        state.establish_pair(peer, true).await.unwrap();
        let (mut replacement_remote, opens) =
            open_physical_pair(&mut state, &mut server_ws, peer, None).await;
        state
            .engine
            .channel(peer, closed_label)
            .unwrap()
            .close()
            .await
            .unwrap();
        let closed_event = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = state.engine_rx.recv().await.unwrap();
                if matches!(&event, EngineEvent::ChannelClosed { label, generation: 2, .. } if label == closed_label) { break event; }
            }
        }).await.expect("required physical channel closes before callbacks are dispatched");
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        assert!(!state.connected_pairs.contains(&peer));
        assert!(!state.p2p_reconnected_pairs.contains(&peer));
        assert_eq!(state.engine.connected_pair_count(), 0);
        assert!(state.exchange_ledger.sent.is_empty());
        state.handle_engine_event(closed_event).await.unwrap();
        assert_eq!(state.transport_status, Some(false));
        assert!(!state.engine.is_paired(peer));
        assert!(state.exchange_ledger.sent.is_empty());
        assert!(!state.exchange_ledger.unmet_criteria().is_empty());
        state.remove_pair_obligation(peer).await;
        initial_remote.remove_peer(state.my_id).await.unwrap();
        replacement_remote.remove_peer(state.my_id).await.unwrap();
    }
}

// Regression #976: a failed retry status write must terminate the connection.
#[tokio::test]
async fn remote_retry_preserves_terminal_websocket_write_errors() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(tcp).await.unwrap()
    });
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _server_ws = server.await.unwrap();
    ws.close(None).await.unwrap();
    let cli = Cli::parse_from([
        "reference-native",
        "--server-url",
        &url,
        "--create-room",
        "--p2p-retry-count",
        "1",
    ]);
    let peer = PlayerId::from_u128(2);
    let generation = SessionGeneration::from_u128(3);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.current_session_generation = Some(generation);
    state.webrtc_plan_seen = true;
    state.transport_status = Some(true);
    state.expected_peers.insert(peer);
    state.formed_pairs.insert(peer);
    state.connected_pairs.insert(peer);
    state.pair_roles.insert(peer, false);
    let error = state
        .handle_signal(peer, generation, json!({"PairRetry": 1}))
        .await
        .expect_err("a failed fallback write must not become a nonfatal signal error");
    assert_eq!(error.code, EXIT_CONNECTION_ERROR);
}
