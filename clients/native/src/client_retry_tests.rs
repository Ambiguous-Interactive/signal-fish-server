// Regression #923: dispatch retries through the real native engine.
use super::*;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};

// Regression #1010: a held P2P gate must not restore removed plan obligations.
#[tokio::test]
async fn p2p_gate_does_not_signal_removed_peers() {
    for removal in [
        "departure",
        "relay plan",
        "replacement plan",
        "departure and rejoin",
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut server_ws = server.await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        let cli = Cli::parse_from([
            "native",
            "--server-url",
            &url,
            "--create-room",
            "--p2p-release-file",
            release.to_str().unwrap(),
        ]);
        let me = PlayerId::from_u128(1);
        let removed = PlayerId::from_u128(2);
        let retained = PlayerId::from_u128(3);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
        state.current_session_generation = Some(SessionGeneration::from_u128(1));
        state.webrtc_plan_seen = true;
        state.present.insert(removed);
        for peer in [removed, retained] {
            state
                .handle_server_message(ServerMessage::NewPeer {
                    peer_id: peer,
                    you_initiate: true,
                })
                .await
                .unwrap();
        }
        match removal {
            "departure" | "departure and rejoin" => state
                .handle_server_message(ServerMessage::PlayerLeft {
                    player_id: removed,
                    epoch: None,
                    final_seq: None,
                })
                .await
                .unwrap(),
            _ => {
                let transport = if removal == "relay plan" {
                    "relay"
                } else {
                    "webrtc"
                };
                let peers = if removal == "relay plan" {
                    json!([])
                } else {
                    json!([{
                        "player_id": retained, "player_name": "retained", "is_authority": false,
                        "initiate": true,
                    }])
                };
                let plan = serde_json::from_value(json!({
                    "type": "SessionPlan", "data": {
                        "generation": SessionGeneration::from_u128(2),
                        "topology": if transport == "relay" { "relay" } else { "mesh" },
                        "transport": transport, "peers": peers, "fallback": "relay",
                    }
                }))
                .unwrap();
                state.handle_server_message(plan).await.unwrap();
            }
        }
        assert!(
            !state.pending_pair_directives.contains_key(&removed),
            "{removal}: removed directive remains queued"
        );
        state
            .handle_signal(
                removed,
                state.current_session_generation.unwrap(),
                json!({"Offer": "late offer from removed peer"}),
            )
            .await
            .unwrap();
        assert!(
            !state.pending_signals.contains_key(&removed),
            "{removal}: removed directive accepted a late signal"
        );
        let rejoined = removal == "departure and rejoin";
        if rejoined {
            // The returning identity now answers. Do not use its old offerer role.
            state
                .handle_server_message(ServerMessage::NewPeer {
                    peer_id: removed,
                    you_initiate: false,
                })
                .await
                .unwrap();
        }
        std::fs::write(&release, b"go").unwrap();
        state.process_p2p_gate(Instant::now()).await.unwrap();
        assert_eq!(
            state.engine.is_paired(removed),
            rejoined,
            "{removal}: gate used a removed directive"
        );
        assert_eq!(
            state.expected_peers.contains(&removed),
            rejoined,
            "{removal}: gate restored removed debt"
        );
        if rejoined {
            assert_eq!(state.pair_roles.get(&removed), Some(&false));
        }
        let expects_retained = removal != "relay plan";
        assert_eq!(
            state.engine.is_paired(retained),
            expects_retained,
            "{removal}"
        );
        if expects_retained {
            let frame = tokio::time::timeout(Duration::from_secs(1), server_ws.next())
                .await
                .expect("retained offer arrives before the deadline")
                .expect("signaling socket remains open")
                .expect("signaling frame is valid");
            let Message::Text(text) = frame else {
                panic!("{removal}: retained offer must use a text frame");
            };
            let ClientMessage::Signal { to, signal, .. } = serde_json::from_str(&text).unwrap()
            else {
                panic!("{removal}: retained offer must be a Signal");
            };
            assert_eq!(to, retained, "{removal}: only the retained offerer signals");
            assert!(
                signal.get("Offer").and_then(|sdp| sdp.as_str()).is_some(),
                "{removal}: retained signal must carry an offer"
            );
        }
        let unexpected_signal =
            tokio::time::timeout(Duration::from_millis(30), server_ws.next()).await;
        assert!(
            unexpected_signal.is_err(),
            "{removal}: unexpected extra signaling or closed socket: {unexpected_signal:?}"
        );
        state.engine.remove_peer(removed).await.unwrap();
        state.engine.remove_peer(retained).await.unwrap();
    }
}

// Regression #1003: queued signaling must run between deferred peer sends.
#[tokio::test]
async fn deferred_exchange_mesh_services_pong_and_relay_between_held_sends() {
    for unreliable in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut server_ws = server.await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        let mut args = vec![
            "native",
            "--server-url",
            &url,
            "--create-room",
            "--exchange",
            "--exchange-release-file",
            release.to_str().unwrap(),
        ];
        if unreliable {
            args.extend([
                "--unreliable-exchange-release-file",
                release.to_str().unwrap(),
            ]);
        }
        let cli = Cli::parse_from(args);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.current_session_generation = Some(SessionGeneration::from_u128(3));
        state.webrtc_plan_seen = true;
        let mut remotes = Vec::new();
        let mut remote_receivers = Vec::new();
        for id in 2..=5 {
            let peer = PlayerId::from_u128(id);
            state.establish_pair(peer, true).await.unwrap();
            let (remote, opens, receiver) =
                open_physical_pair_with_events(&mut state, &mut server_ws, peer, None).await;
            remote_receivers.push(receiver);
            for event in opens {
                state.handle_engine_event(event).await.unwrap();
            }
            state.ice_gathering_complete.insert(peer);
            if unreliable {
                state.exchange_ledger.note_sent(peer, RELIABLE_LABEL);
                state
                    .exchange_ledger
                    .note_received(peer, RELIABLE_LABEL.to_string());
            }
            remotes.push(remote);
        }
        state.exchange_released = unreliable;
        let label = if unreliable {
            UNRELIABLE_LABEL
        } else {
            RELIABLE_LABEL
        };
        let healthy_peer = PlayerId::from_u128(3);
        let healthy_channels: Vec<_> = [RELIABLE_LABEL, UNRELIABLE_LABEL]
            .map(|label| state.engine.channel(healthy_peer, label).unwrap())
            .into();
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        state.exchange_send_hook = Some(Box::new(move |channel, text| {
            let tx = started_tx.clone();
            let healthy = healthy_channels
                .iter()
                .any(|other| std::sync::Arc::ptr_eq(other, &channel));
            Box::pin(async move {
                if healthy {
                    channel.send_text(&text).await
                } else {
                    let _ = tx.send(());
                    std::future::pending().await
                }
            })
        }));
        std::fs::write(&release, b"go").unwrap();
        state.pong_deadline = Some(Instant::now() + PONG_TIMEOUT);
        let sender = PlayerId::from_u128(20);
        state.accountability = DeliveryAccountability::new(true);
        let sender_info: PlayerInfo = serde_json::from_value(json!({
            "id": sender, "name": "sender", "is_authority": false, "is_ready": true,
            "epoch": 1, "seq": 0,
        }))
        .unwrap();
        state
            .accountability
            .rebaseline_snapshot(&[sender_info])
            .unwrap();
        let healthy_return_channels: Vec<_> = [RELIABLE_LABEL, UNRELIABLE_LABEL]
            .map(|label| {
                (
                    label,
                    remotes.get(1).unwrap().channel(state.my_id, label).unwrap(),
                )
            })
            .into();
        let task = tokio::spawn(async move {
            started_rx.recv().await.unwrap();
            for (label, channel) in healthy_return_channels {
                channel
                    .send_text(
                        &json!({"from": healthy_peer, "channel": label, "seq": 0}).to_string(),
                    )
                    .await
                    .unwrap();
            }
            server_ws
                .send(Message::Text(
                    serde_json::to_string(&ServerMessage::Pong).unwrap().into(),
                ))
                .await
                .unwrap();
            let relay = ServerMessage::GameData {
                from_player: sender,
                data: json!({"relay_msg": 1}),
                seq: Some(1),
                epoch: Some(1),
                class: None,
                key: None,
            };
            server_ws
                .send(Message::Text(serde_json::to_string(&relay).unwrap().into()))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(1500)).await;
            server_ws.close(None).await.unwrap();
        });
        let started = Instant::now();
        let result = state.run_loop().await;
        let elapsed = started.elapsed();
        eprintln!(
            "deferred gate unreliable={unreliable}: signaling close consumed after {elapsed:?}"
        );
        task.await.unwrap();
        assert!(result.is_err(), "server close ends the run");
        assert!(
            state.relay_received_from.contains(&sender),
            "queued relay must be consumed"
        );
        assert!(
            state.pong_deadline.is_none(),
            "queued Pong clears the pending response"
        );
        assert!(
            !state.pong_grace_applied,
            "on-time Pong must clear its deadline"
        );
        assert!(
            elapsed < Duration::from_millis(2800),
            "mesh delayed signaling for {elapsed:?}"
        );
        assert!(state.exchange_ledger.has_sent(healthy_peer, label));
        assert!(state.engine.is_paired(healthy_peer));
        assert!(state
            .exchange_ledger
            .received
            .get(&healthy_peer)
            .unwrap()
            .contains(label));
        assert_eq!(state.relay_received_from.len(), 1);
        let duplicate = state
            .accountability
            .record_game_data(sender, Some(1), Some(1), None, None)
            .unwrap_err();
        assert!(
            duplicate.contains("duplicate/backward"),
            "seq1 was consumed exactly once: {duplicate}"
        );
        assert_eq!(
            state
                .accountability
                .record_game_data(sender, Some(2), Some(1), None, None)
                .unwrap(),
            GameDataDisposition::Apply
        );
        assert!(!state
            .exchange_ledger
            .has_sent(PlayerId::from_u128(2), label));
        assert!(!state.exchange_ledger.unmet_criteria().is_empty());
        for mut remote in remotes {
            remote.remove_peer(state.my_id).await.unwrap();
        }
    }
}

// Regressions #1025/#1047: decoder-ready signaling runs between channel sends.
#[tokio::test]
async fn immediate_exchange_services_relay_before_the_next_label() {
    use tokio_tungstenite::tungstenite::protocol::{
        frame::{
            coding::{Data, OpCode},
            Frame,
        },
        Role,
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("ws://{address}");
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(tcp).await.unwrap()
    });
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let mut server_ws = server.await.unwrap();
    let cli = Cli::parse_from([
        "native",
        "--server-url",
        &url,
        "--create-room",
        "--exchange",
    ]);
    let peer = PlayerId::from_u128(2);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.current_session_generation = Some(SessionGeneration::from_u128(3));
    state.webrtc_plan_seen = true;
    state.establish_pair(peer, true).await.unwrap();
    let (mut remote, opens) = open_physical_pair(&mut state, &mut server_ws, peer, None).await;
    let sends = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let send_count = sends.clone();
    state.exchange_send_hook = Some(Box::new(move |channel, text| {
        send_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { channel.send_text(&text).await })
    }));
    for event in opens {
        state.handle_engine_event(event).await.unwrap();
    }
    state.process_deferred_exchange().await.unwrap();
    assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(state.deferred_exchange.len(), 1);
    state.pong_deadline = Some(Instant::now() + PONG_TIMEOUT);

    // Kernel peek does not establish decoder readiness on an asynchronous socket.
    // Seed complete server frames after the first real send, before loop arbitration.
    let mut queued = Vec::new();
    for message in [
        ServerMessage::Pong,
        ServerMessage::GameData {
            from_player: peer,
            data: json!({"relay_msg": "during immediate exchange"}),
            seq: None,
            epoch: None,
            class: None,
            key: None,
        },
    ] {
        Frame::message(
            serde_json::to_vec(&message).unwrap(),
            OpCode::Data(Data::Text),
            true,
        )
        .format(&mut queued)
        .unwrap();
    }
    Frame::close(None).format(&mut queued).unwrap();
    state.ws = tokio_tungstenite::WebSocketStream::from_partially_read(
        state.ws.into_inner(),
        queued,
        Role::Client,
        None,
    )
    .await;
    let result = tokio::time::timeout(Duration::from_secs(5), state.run_loop())
        .await
        .expect("signaling must remain responsive during immediate exchange");
    let error = result.expect_err("the queued close must end the session");
    assert!(
        error.message.contains("websocket closed by server"),
        "{error:?}"
    );
    assert!(
        state.relay_received_from.contains(&peer),
        "apply queued relay traffic"
    );
    assert!(state.pong_deadline.is_none(), "consume the queued Pong");
    assert_eq!(
        sends.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "consume ready signaling after the first send before starting the next label"
    );
    assert!(state.exchange_ledger.has_sent(peer, RELIABLE_LABEL));
    assert!(!state.exchange_ledger.has_sent(peer, UNRELIABLE_LABEL));
    assert!(!state.exchange_ledger.unmet_criteria().is_empty());
    remote.remove_peer(state.my_id).await.unwrap();
}

// A continuously ready callback stream must still let queued exchange advance.
#[tokio::test]
async fn deferred_exchange_advances_before_a_ready_callback_burst_finishes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(tcp).await.unwrap()
    });
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _server_ws = server.await.unwrap();
    let cli = Cli::parse_from([
        "native",
        "--server-url",
        &url,
        "--create-room",
        "--exchange",
    ]);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    let peer = PlayerId::from_u128(2);
    state.current_session_generation = Some(SessionGeneration::from_u128(3));
    state.establish_pair(peer, true).await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(2), state.engine_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let (_, generation) = event.peer_generation();
    let (tx, rx) = mpsc::unbounded_channel();
    state.engine_rx = rx;
    for _ in 0..1000 {
        tx.send(EngineEvent::ChannelClosed {
            peer: PlayerId::from_u128(99),
            generation: 0,
            label: RELIABLE_LABEL.to_string(),
        })
        .unwrap();
    }
    tx.send(EngineEvent::ChannelClosed {
        peer,
        generation,
        label: RELIABLE_LABEL.to_string(),
    })
    .unwrap();
    state.connected_pairs.insert(peer);
    state.exchange_ledger.note_connected(peer);
    state.queue_exchange(peer, &[RELIABLE_LABEL]);
    state.exchange_send_hook = Some(Box::new(|_, _| Box::pin(async { Ok(()) })));
    state.run_deadline = Deadline::after(Instant::now(), Duration::from_millis(100));
    assert_eq!(
        tokio::task::unconstrained(state.run_loop()).await.unwrap(),
        EXIT_CRITERIA_UNMET
    );
    assert!(
        state.exchange_ledger.has_sent(peer, RELIABLE_LABEL),
        "the deferred send must run before the last queued callback retires its channel"
    );
}

// Deferred work must not transfer to a replacement link or extend the run window.
#[tokio::test]
async fn deferred_exchange_fences_replacements_deadlines_and_signaling_failure() {
    for mode in [
        "replacement",
        "old-channel-replacement",
        "deadline",
        "signaling-failure",
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let _server_ws = server.await.unwrap();
        let cli = Cli::parse_from([
            "native",
            "--server-url",
            &url,
            "--create-room",
            "--exchange",
        ]);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        let peer = PlayerId::from_u128(2);
        state.webrtc_plan_seen = true;
        state.current_session_generation = Some(SessionGeneration::from_u128(3));
        state.transport_status = Some(true);
        if mode == "old-channel-replacement" {
            state.establish_pair(peer, true).await.unwrap();
        }
        state.exchange_ledger.note_connected(peer);
        state.connected_pairs.insert(peer);
        state.queue_exchange(peer, &[RELIABLE_LABEL]);
        match mode {
            "replacement" | "old-channel-replacement" => {
                if mode == "old-channel-replacement" {
                    state.prepare_retained_pair_replacement(peer).await;
                }
                state.establish_pair(peer, true).await.unwrap();
                assert!(state.engine.channel(peer, RELIABLE_LABEL).is_some());
                state.connected_pairs.insert(peer);
                state.process_deferred_exchange().await.unwrap();
                assert!(
                    state.engine.is_paired(peer),
                    "old queued work cannot retire a replacement"
                );
                assert_eq!(state.transport_status, Some(true));
            }
            "deadline" => {
                state.run_deadline = Deadline::after(Instant::now(), Duration::ZERO);
                assert_eq!(state.run_loop().await.unwrap(), EXIT_CRITERIA_UNMET);
                assert_eq!(
                    state.deferred_exchange.len(),
                    1,
                    "expired runs cannot start queued sends"
                );
            }
            _ => {
                state.ws.close(None).await.unwrap();
                let error = state.process_deferred_exchange().await.unwrap_err();
                assert_eq!(error.code, EXIT_CONNECTION_ERROR);
                assert!(!state.connected_pairs.contains(&peer));
            }
        }
        assert!(!state.exchange_ledger.has_sent(peer, RELIABLE_LABEL));
        assert!(!state.exchange_ledger.unmet_criteria().is_empty());
    }
}

// Regression #1002: a held local send must release the orchestrator and preserve relay.
#[tokio::test]
async fn stalled_exchange_send_preserves_relay_and_unpaid_debt() {
    struct SendGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for SendGuard {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    for (label, mode) in [RELIABLE_LABEL, UNRELIABLE_LABEL]
        .into_iter()
        .flat_map(|label| {
            ["pending", "run-deadline", "ready-late", "expired"].map(|mode| (label, mode))
        })
    {
        let diagnostics = super::peer_loss_tests::DiagnosticCapture::default();
        let _subscriber = tracing::subscriber::set_default(diagnostics.subscriber());
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
        state.current_session_generation = Some(SessionGeneration::from_u128(3));
        state.webrtc_plan_seen = true;
        state.establish_pair(peer, true).await.unwrap();
        let (mut remote, opens) = open_physical_pair(&mut state, &mut server_ws, peer, None).await;
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        state.exchange_ledger.note_connected(peer);
        let other_label = if label == RELIABLE_LABEL {
            UNRELIABLE_LABEL
        } else {
            RELIABLE_LABEL
        };
        state.exchange_ledger.note_sent(peer, other_label);
        if mode == "run-deadline" {
            state.run_deadline = Deadline::after(Instant::now(), Duration::from_millis(50));
        } else if mode == "expired" {
            state.run_deadline = Deadline::after(Instant::now(), Duration::ZERO);
        }
        let clock_guard = if mode == "ready-late" {
            tokio::time::pause();
            Some(tokio::spawn(async {
                loop {
                    tokio::task::yield_now().await;
                }
            }))
        } else {
            None
        };
        let started = Instant::now();
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut calls = 0;
        let result = tokio::time::timeout(
            CHANNEL_SEND_TIMEOUT + Duration::from_secs(1),
            state.send_exchange_with(peer, &[label], |_channel, _text| {
                calls += 1;
                let dropped = dropped.clone();
                async move {
                    let _guard = SendGuard(dropped);
                    if mode == "ready-late" {
                        tokio::time::advance(CHANNEL_SEND_TIMEOUT).await;
                        Ok(())
                    } else if mode == "expired" {
                        Ok(())
                    } else {
                        std::future::pending::<webrtc::error::Result<()>>().await
                    }
                }
            }),
        )
        .await
        .expect("a pending local send must retire its link before the outer watchdog");
        if let Some(guard) = clock_guard {
            guard.abort();
            tokio::time::resume();
        }
        assert_eq!(calls, usize::from(mode != "expired"), "mode={mode}");
        assert_eq!(
            dropped.load(std::sync::atomic::Ordering::SeqCst),
            mode != "expired",
            "the completed or timed-out send future must release its captured state"
        );
        result.expect("a local send timeout must retain the signaling connection");
        if mode == "pending" {
            assert!(started.elapsed() >= CHANNEL_SEND_TIMEOUT);
        }
        if mode == "run-deadline" {
            assert!(started.elapsed() < CHANNEL_SEND_TIMEOUT);
        }
        let losses = diagnostics.losses();
        assert_eq!(losses.len(), 1, "{label}/{mode}: {losses:?}");
        assert!(losses[0]
            .0
            .contains(&format!("data-channel send failed on {label}")));
        assert_eq!(losses[0].1["generation"], 1);
        for required in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            assert_eq!(losses[0].1["channels"][required]["registered"], true);
            assert_eq!(losses[0].1["channels"][required]["open_observed"], true);
        }
        assert!(!state.engine.is_paired(peer));
        assert!(!state.connected_pairs.contains(&peer));
        assert_eq!(state.transport_status, Some(false));
        assert!(!state.exchange_ledger.has_sent(peer, label));
        assert!(state.exchange_ledger.has_sent(peer, other_label));
        assert!(!state.exchange_ledger.unmet_criteria().is_empty());
        wire::send_game_data(&mut state.ws, json!({"after_stalled_send": label}))
            .await
            .unwrap();
        state.send_message(&ClientMessage::Ping).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut fallback_seen = false;
            let mut relay_seen = false;
            loop {
                let frame = server_ws.next().await.unwrap().unwrap();
                if let Message::Text(text) = frame {
                    match serde_json::from_str::<ClientMessage>(&text).unwrap() {
                        ClientMessage::TransportStatus {
                            transport: Transport::WebRtc,
                            connected: false,
                        } => fallback_seen = true,
                        ClientMessage::GameData { data, .. } => {
                            assert_eq!(data["after_stalled_send"], label);
                            relay_seen = true;
                        }
                        ClientMessage::Ping => {
                            assert!(fallback_seen && relay_seen);
                            break;
                        }
                        _ => {}
                    }
                }
            }
        })
        .await
        .expect("relay and Ping remain writable after local send timeout");
        remote.remove_peer(state.my_id).await.unwrap();
    }
}

// Regression #1000: a local channel send failure must preserve the relay floor.
#[tokio::test]
async fn exchange_send_after_channel_close_preserves_relay_and_debt() {
    for (trigger, closed_label) in [
        ("exchange_gate", RELIABLE_LABEL),
        ("exchange_gate", UNRELIABLE_LABEL),
        ("unreliable_gate", UNRELIABLE_LABEL),
        ("immediate", RELIABLE_LABEL),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut server_ws = server.await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let release_path = directory.path().join("exchange-release");
        let mut arguments = vec![
            "reference-native",
            "--server-url",
            &url,
            "--create-room",
            "--disable-mdns",
            "--exchange",
            "--exchange-release-file",
            release_path.to_str().unwrap(),
            "--p2p-retry-count",
            "1",
        ];
        if trigger == "unreliable_gate" {
            arguments.extend([
                "--unreliable-exchange-release-file",
                release_path.to_str().unwrap(),
            ]);
        }
        let cli = Cli::parse_from(arguments);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.current_session_generation = Some(SessionGeneration::from_u128(3));
        state.webrtc_plan_seen = true;
        state.establish_pair(peer, true).await.unwrap();
        let (mut remote, opens) = open_physical_pair(&mut state, &mut server_ws, peer, None).await;
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        assert!(state.connected_pairs.contains(&peer));
        state.ice_gathering_complete.insert(peer);
        if trigger == "unreliable_gate" {
            std::fs::write(&release_path, b"release").unwrap();
            state.process_exchange_gate(Instant::now()).await.unwrap();
            while !state.deferred_exchange.is_empty() {
                state.process_deferred_exchange().await.unwrap();
            }
            remote
                .channel(state.my_id, RELIABLE_LABEL)
                .unwrap()
                .send_text(&json!({"from": peer, "channel": RELIABLE_LABEL, "seq": 0}).to_string())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !state.exchange_reliable_gate_ready() {
                    let event = state.engine_rx.recv().await.unwrap();
                    state.handle_engine_event(event).await.unwrap();
                }
            })
            .await
            .unwrap();
        }
        state
            .engine
            .channel(peer, closed_label)
            .unwrap()
            .close()
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(state.engine_rx.recv().await.unwrap(), EngineEvent::ChannelClosed { label, .. } if label == closed_label) {
                    break;
                }
            }
        }).await.expect("hold the terminal channel callback before the exchange send");
        std::fs::write(&release_path, b"release").unwrap();
        let result = match trigger {
            "unreliable_gate" => state.process_unreliable_exchange_gate(Instant::now()).await,
            "immediate" => {
                state
                    .send_exchange(peer, &[RELIABLE_LABEL, UNRELIABLE_LABEL])
                    .await
            }
            _ => state.process_exchange_gate(Instant::now()).await,
        };
        result.expect("closed P2P sends must retain the live signaling connection");
        while !state.deferred_exchange.is_empty() {
            state.process_deferred_exchange().await.unwrap();
        }
        assert_eq!(state.transport_status, Some(false));
        assert!(!state.connected_pairs.contains(&peer));
        assert!(!state.engine.is_paired(peer));
        assert!(!state.exchange_ledger.has_sent(peer, closed_label));
        assert_eq!(
            state.exchange_ledger.has_sent(peer, RELIABLE_LABEL),
            trigger == "unreliable_gate"
        );
        assert!(!state.exchange_ledger.unmet_criteria().is_empty());
        wire::send_game_data(&mut state.ws, json!({"after_p2p_failure": trigger}))
            .await
            .unwrap();
        state.send_message(&ClientMessage::Ping).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut fallback_seen = false;
            let mut relay_seen = false;
            loop {
                let frame = server_ws.next().await.unwrap().unwrap();
                if let Message::Text(text) = frame {
                    match serde_json::from_str::<ClientMessage>(&text).unwrap() {
                        ClientMessage::GameData { data, .. } => {
                            assert_eq!(data["after_p2p_failure"], trigger);
                            relay_seen = true;
                        }
                        ClientMessage::TransportStatus {
                            transport: Transport::WebRtc,
                            connected: false,
                        } => fallback_seen = true,
                        ClientMessage::Ping => {
                            assert!(fallback_seen && relay_seen);
                            break;
                        }
                        _ => {}
                    }
                }
            }
        })
        .await
        .expect("relay control frames remain writable after P2P send failure");
        remote.remove_peer(state.my_id).await.unwrap();
        state.request_pair_retry(peer, 1).await.unwrap();
        let (mut replacement, opens) =
            open_physical_pair(&mut state, &mut server_ws, peer, None).await;
        state.exchange_released = true;
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        assert!(state.connected_pairs.contains(&peer));
        while !state.deferred_exchange.is_empty() {
            state.process_deferred_exchange().await.unwrap();
        }
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            assert!(state.exchange_ledger.has_sent(peer, label));
            replacement
                .channel(state.my_id, label)
                .unwrap()
                .send_text(&json!({"from": peer, "channel": label, "seq": 0}).to_string())
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while !state.exchange_ledger.unmet_criteria().is_empty() {
                let event = state.engine_rx.recv().await.unwrap();
                state.handle_engine_event(event).await.unwrap();
            }
        })
        .await
        .expect("fresh transport discharges remaining send and receipt debt");
        state.remove_pair_obligation(peer).await;
        replacement.remove_peer(state.my_id).await.unwrap();
    }
}

#[tokio::test]
async fn exchange_missing_channel_preserves_debt_and_signaling_errors() {
    for signaling_closed in [false, true] {
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
        ]);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.webrtc_plan_seen = true;
        state.transport_status = Some(true);
        state.connected_pairs.insert(peer);
        state.exchange_ledger.note_connected(peer);
        if signaling_closed {
            state.ws.close(None).await.unwrap();
        }
        let result = state
            .send_exchange(peer, &[RELIABLE_LABEL, UNRELIABLE_LABEL])
            .await;
        if signaling_closed {
            assert_eq!(result.unwrap_err().code, EXIT_CONNECTION_ERROR);
        } else {
            result.unwrap();
            assert_eq!(state.transport_status, Some(false));
            state.send_message(&ClientMessage::Ping).await.unwrap();
        }
        assert!(state.exchange_ledger.sent.is_empty());
        assert!(!state.exchange_ledger.unmet_criteria().is_empty());
    }
}

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
                let signal = &message["data"]["signal"];
                let attempt = state.pair_retry_attempts.get(&peer).copied().unwrap_or(0);
                let signal = signal_for_retry_attempt(signal.clone(), attempt)
                    .unwrap_or(serde_json::Value::Null);
                if let Some(sdp) = signal["Offer"].as_str() {
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
        let attempt = state.pair_retry_attempts.get(&peer).copied().unwrap_or(0);
        if attempt > 0 {
            let generation = state.current_session_generation.unwrap();
            state
                .handle_signal(
                    peer,
                    generation,
                    wrap_retry_signal(json!({"Answer": answer}), attempt),
                )
                .await
                .unwrap();
        } else {
            assert_eq!(attempt, 0);
            state.engine.handle_answer(peer, answer).await.unwrap();
        }
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
                        let attempt = state.pair_retry_attempts.get(&peer).copied().unwrap_or(0);
                        if attempt > 0 {
                            let generation = state.current_session_generation.unwrap();
                            state.handle_signal(peer, generation, wrap_retry_signal(json!({"IceCandidate": candidate_json}), attempt)).await.unwrap();
                        } else {
                            assert_eq!(attempt, 0);
                            state.engine.handle_remote_candidate(peer, &candidate_json).await.unwrap();
                        }
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

/*
    Regression #1022: the retry marker and old answer cross in opposite
    directions. Sender FIFO cannot fence the replacement link.
*/
#[tokio::test]
async fn retry_discards_crossing_old_answer_before_fresh_answer() {
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
        "1",
    ]);
    let me = PlayerId::from_u128(1);
    let peer = PlayerId::from_u128(2);
    let generation = SessionGeneration::from_u128(3);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
    state.current_session_generation = Some(generation);
    state.establish_pair(peer, true).await.unwrap();
    let offer = server_ws
        .next()
        .await
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    let offer: serde_json::Value = serde_json::from_str(&offer).unwrap();
    let (remote_tx, _remote_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut remote = Engine::new(cli.engine_settings(), remote_tx).unwrap();
    remote.pair_with(me, false, &[]).await.unwrap();
    let delayed_answer = remote
        .handle_offer(
            me,
            offer["data"]["signal"]["Offer"]
                .as_str()
                .unwrap()
                .to_string(),
        )
        .await
        .unwrap();

    state.request_pair_retry(peer, 1).await.unwrap();
    // The marker precedes this offer on the requester's own wire.
    let marker = server_ws
        .next()
        .await
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    let marker: serde_json::Value = serde_json::from_str(&marker).unwrap();
    assert_eq!(marker["data"]["signal"]["PairRetry"], 1);
    let offer = server_ws
        .next()
        .await
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap();
    let offer: serde_json::Value = serde_json::from_str(&offer).unwrap();
    // The delayed answer was already in flight before the remote saw the marker.
    state
        .handle_signal(peer, generation, json!({"Answer": delayed_answer}))
        .await
        .unwrap();

    remote.remove_peer(me).await.unwrap();
    assert_eq!(offer["data"]["signal"]["RetrySignal"]["attempt"], 1);
    let fresh_offer = offer["data"]["signal"]["RetrySignal"]["signal"]["Offer"]
        .as_str()
        .unwrap()
        .to_string();
    let (mut fresh_remote, opens, mut remote_events) =
        open_physical_pair_with_events(&mut state, &mut server_ws, peer, Some(fresh_offer)).await;
    assert_eq!(opens.len(), 2);
    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        fresh_remote
            .channel(me, label)
            .unwrap()
            .send_text("fresh gameplay")
            .await
            .unwrap();
        state
            .engine
            .channel(peer, label)
            .unwrap()
            .send_text("fresh reply")
            .await
            .unwrap();
    }
    let mut local_received = BTreeSet::new();
    let mut remote_received = BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while local_received.len() < 2 || remote_received.len() < 2 {
            tokio::select! {
                event = state.engine_rx.recv() => if let EngineEvent::ChannelMessage { label, text, .. } = event.unwrap() {
                    assert_eq!(text, "fresh gameplay");
                    local_received.insert(label);
                },
                event = remote_events.recv() => if let EngineEvent::ChannelMessage { label, text, .. } = event.unwrap() {
                    assert_eq!(text, "fresh reply");
                    remote_received.insert(label);
                },
            }
        }
    }).await.expect("replacement channels exchange fresh gameplay in both directions");
    state.engine.remove_peer(peer).await.unwrap();
    fresh_remote.remove_peer(me).await.unwrap();
}

#[test]
fn retry_signal_epochs_preserve_default_wire_and_reject_stale_payloads() {
    for payload in [
        json!({"Offer":"sdp"}),
        json!({"Answer":"sdp"}),
        json!({"IceCandidate":"ice"}),
    ] {
        assert_eq!(wrap_retry_signal(payload.clone(), 0), payload);
        assert_eq!(
            signal_for_retry_attempt(payload.clone(), 0),
            Some(payload.clone())
        );
        assert!(signal_for_retry_attempt(payload.clone(), 1).is_none());
        for attempt in [1, 2, u8::MAX] {
            let wrapped = wrap_retry_signal(payload.clone(), attempt);
            assert_eq!(
                signal_for_retry_attempt(wrapped.clone(), attempt),
                Some(payload.clone())
            );
            assert!(signal_for_retry_attempt(wrapped.clone(), 0).is_none());
            assert!(signal_for_retry_attempt(wrapped, attempt.saturating_sub(1)).is_none());
        }
    }
    let marker = json!({"PairRetry": 2});
    for attempt in [0, 1, 2] {
        assert_eq!(wrap_retry_signal(marker.clone(), attempt), marker);
        assert_eq!(
            signal_for_retry_attempt(marker.clone(), attempt),
            Some(marker.clone())
        );
    }
    for invalid in [
        json!({"RetrySignal":{"attempt":0,"signal":{"Answer":"sdp"}}}),
        json!({"RetrySignal":{"attempt":2,"signal":{"Answer":"sdp"}}}),
        json!({"RetrySignal":{"attempt":"1","signal":{"Answer":"sdp"}}}),
        json!({"RetrySignal":{"attempt":1,"signal":{"PairRetry":1}}}),
        json!({"RetrySignal":{"attempt":1,"signal":{"Other":"sdp"}}}),
        json!({"RetrySignal":{"attempt":1}}),
        json!({"RetrySignal":{"attempt":1,"signal":{"Answer":"sdp"},"extra":true}}),
        json!({"RetrySignal":{"attempt":1,"signal":{"Answer":"sdp"}},"extra":true}),
    ] {
        assert!(
            signal_for_retry_attempt(invalid.clone(), 1).is_none(),
            "accepted {invalid}"
        );
    }
}

// Regression #1034: send evidence must fit the actual hold or linger window.
#[tokio::test]
async fn exchange_sends_preserve_the_active_success_window() {
    for (label, phase, p2p_timeout) in
        [RELIABLE_LABEL, UNRELIABLE_LABEL]
            .into_iter()
            .flat_map(|label| {
                [
                    "held",
                    "released-during-send",
                    "queued-release-during-send",
                    "linger-before",
                    "linger-exact",
                    "linger-after",
                    "replacement-linger-before",
                    "replacement-linger-exact",
                    "replacement-linger-after",
                    "released",
                ]
                .into_iter()
                .flat_map(move |phase| ["0", "15"].map(|timeout| (label, phase, timeout)))
            })
    {
        let previously_sent = phase.starts_with("replacement-");
        let phase = phase.strip_prefix("replacement-").unwrap_or(phase);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut server_ws = server.await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        if !matches!(
            phase,
            "held" | "released-during-send" | "queued-release-during-send"
        ) {
            std::fs::write(&release, "release").unwrap();
        }
        let cli = Cli::parse_from([
            "native",
            "--server-url",
            &url,
            "--create-room",
            "--exchange",
            "--success-release-file",
            release.to_str().unwrap(),
            "--p2p-timeout-secs",
            p2p_timeout,
        ]);
        let me = PlayerId::from_u128(1);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
        state.current_session_generation = Some(SessionGeneration::from_u128(3));
        state.webrtc_plan_seen = true;
        state.establish_pair(peer, true).await.unwrap();
        let (mut remote, opens) = open_physical_pair(&mut state, &mut server_ws, peer, None).await;
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        state.deferred_exchange.clear();
        state.game_started = true;
        state.members_seen.insert(peer);
        state.peer_status_from.insert(peer);
        state.exchange_ledger.reset_incarnation(peer);
        state.exchange_ledger.note_connected(peer);
        for other in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            if phase != "queued-release-during-send" || other != label {
                state.exchange_ledger.note_received(peer, other.to_string());
            }
            if other != label {
                state.exchange_ledger.note_sent(peer, other);
            }
        }
        if previously_sent {
            state.exchange_ledger.note_sent(peer, label);
            state.exchange_ledger.note_link_open(peer);
            for other in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                if other != label {
                    state.exchange_ledger.note_sent(peer, other);
                }
            }
        }
        let clock_guard = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        tokio::time::pause();
        state.run_deadline = Deadline::after(Instant::now(), Duration::ZERO);
        state.next_ping_at = Instant::now() + Duration::from_secs(60);
        state.success_criteria_reported = true;
        state.success_hold_started_at = Some(Instant::now());
        let (messages, queued) = tokio::sync::mpsc::unbounded_channel();
        if phase == "queued-release-during-send" {
            state.engine_rx = queued;
            assert!(state.engine.is_current_generation(peer, 1));
        }
        if phase.starts_with("linger-") {
            state.linger_until = Some(Instant::now() + Duration::from_millis(100));
        }
        let elapsed = Duration::from_millis(match phase {
            "linger-before" => 99,
            "linger-exact" => 100,
            _ => 101,
        });
        let mut calls = 0;
        state
            .send_exchange_with(peer, &[label], |_, _| {
                calls += 1;
                let release = release.clone();
                let messages = messages.clone();
                async move {
                    if matches!(phase, "released-during-send" | "queued-release-during-send") {
                        tokio::time::advance(Duration::from_millis(25)).await;
                        if phase == "queued-release-during-send" {
                            messages
                                .send(EngineEvent::ChannelMessage {
                                    peer,
                                    generation: 1,
                                    label: label.to_string(),
                                    text: json!({"from": peer, "channel": label, "seq": 0})
                                        .to_string(),
                                    queue_permit: crate::engine::MessageQueuePermit::for_test(),
                                    received_at: Instant::now(),
                                })
                                .unwrap();
                        }
                        tokio::time::advance(Duration::from_millis(25)).await;
                        std::fs::write(release, "release").unwrap();
                        tokio::time::advance(elapsed - Duration::from_millis(50)).await;
                    } else {
                        tokio::time::advance(elapsed).await;
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();
        let valid = matches!(
            phase,
            "held" | "linger-before" | "released-during-send" | "queued-release-during-send"
        );
        assert_eq!(
            state.exchange_ledger.has_sent(peer, label),
            valid || previously_sent,
            "{label}/{phase}/p2p={p2p_timeout}/previously_sent={previously_sent}: completion must fit the active success window"
        );
        assert_eq!(
            calls,
            usize::from(phase != "released"),
            "expired sends cannot start"
        );
        assert_eq!(
            state.engine.is_paired(peer),
            valid,
            "failed sends retire their physical link"
        );
        assert_eq!(
            state.process_timers().await.unwrap(),
            if valid {
                None
            } else if previously_sent {
                Some(EXIT_SUCCESS)
            } else {
                Some(EXIT_CRITERIA_UNMET)
            },
            "{label}/{phase}/p2p={p2p_timeout}: timers preserve admission and arrival ownership"
        );
        if matches!(phase, "released-during-send" | "queued-release-during-send") {
            tokio::time::advance(EXIT_LINGER).await;
            assert_eq!(state.process_timers().await.unwrap(), Some(EXIT_SUCCESS));
        }
        if phase == "linger-before" {
            tokio::time::advance(Duration::from_millis(1)).await;
            assert_eq!(state.process_timers().await.unwrap(), Some(EXIT_SUCCESS));
        }
        clock_guard.abort();
        tokio::time::resume();
        state.engine.remove_peer(peer).await.unwrap();
        remote.remove_peer(me).await.unwrap();
    }
}

// Regression #1035: a replacement repays remote receipts lost on the old link.
#[tokio::test]
async fn physical_replacement_resends_probes_without_erasing_logical_evidence() {
    for (missing_label, retry) in [RELIABLE_LABEL, UNRELIABLE_LABEL]
        .into_iter()
        .flat_map(|label| [false, true].map(|retry| (label, retry)))
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut server_ws = server.await.unwrap();
        let cli = Cli::parse_from([
            "native",
            "--server-url",
            &url,
            "--create-room",
            "--disable-mdns",
            "--exchange",
            "--p2p-retry-count",
            "1",
        ]);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.current_session_generation = Some(SessionGeneration::from_u128(3));
        state.establish_pair(peer, true).await.unwrap();
        let (mut remote, opens, mut remote_events) =
            open_physical_pair_with_events(&mut state, &mut server_ws, peer, None).await;
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        state.deferred_exchange.clear();
        state
            .send_exchange_with(
                peer,
                &[RELIABLE_LABEL, UNRELIABLE_LABEL],
                |channel, text| async move {
                    if channel.label().await.unwrap() != missing_label {
                        channel.send_text(&text).await?;
                    }
                    Ok(())
                },
            )
            .await
            .unwrap();
        let mut remote_receipts = BTreeSet::new();
        tokio::time::timeout(Duration::from_secs(2), async {
            while remote_receipts.is_empty() {
                if let EngineEvent::ChannelMessage { label, .. } =
                    remote_events.recv().await.unwrap()
                {
                    remote_receipts.insert(label);
                }
            }
        })
        .await
        .unwrap();
        assert!(!remote_receipts.contains(missing_label));
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            assert!(state.exchange_ledger.has_sent(peer, label));
            state.exchange_ledger.note_received(peer, label.to_string());
        }
        if retry {
            state.rebuild_pair(peer, 1, false).await.unwrap();
        } else {
            state.prepare_retained_pair_replacement(peer).await;
            state.establish_pair(peer, true).await.unwrap();
        }
        remote.remove_peer(state.my_id).await.unwrap();
        assert!(state.exchange_ledger.unmet_criteria().is_empty());
        state.game_started = true;
        state.members_seen.insert(peer);
        state.peer_status_from.insert(peer);
        let original_linger_until = Some(Instant::now() + EXIT_LINGER);
        state.linger_until = original_linger_until;
        let (mut replacement, opens, mut replacement_events) =
            open_physical_pair_with_events(&mut state, &mut server_ws, peer, None).await;
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        state.selected_pair_evidence.clear(peer);
        assert!(state.exchange_ledger.unmet_criteria().is_empty());
        assert!(
            !state.criteria_met(),
            "queued replacement probes must hold success"
        );
        state.arm_success_linger(Instant::now()).await.unwrap();
        assert_eq!(
            state.linger_until, original_linger_until,
            "resends retain the original linger cutoff"
        );
        while !state.deferred_exchange.is_empty() {
            state.process_deferred_exchange().await.unwrap();
        }
        assert!(state.criteria_met());
        tokio::time::timeout(Duration::from_secs(1), async {
            while remote_receipts.len() < 2 {
                if let EngineEvent::ChannelMessage { label, .. } =
                    replacement_events.recv().await.unwrap()
                {
                    remote_receipts.insert(label);
                }
            }
        })
        .await
        .expect("replacement must repay the remote endpoint's missing old-link receipt");
        let mut duplicate_sends = 0;
        state
            .send_exchange_with(peer, &[RELIABLE_LABEL, UNRELIABLE_LABEL], |_, _| {
                duplicate_sends += 1;
                async { Ok(()) }
            })
            .await
            .unwrap();
        assert_eq!(duplicate_sends, 0, "one probe per label per physical link");
        assert!(state.exchange_ledger.unmet_criteria().is_empty());
        state.remove_pair_obligation(peer).await;
        replacement.remove_peer(state.my_id).await.unwrap();
    }
}

// Audit #826/#853/#889: control probe, open, loss, and replacement dispatch separately.
#[tokio::test]
async fn probe_before_open_and_loss_before_send_preserve_debt_until_replacement() {
    for initiate in [false, true] {
        for early_label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            let (ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
            let cli = Cli::parse_from([
                "native",
                "--server-url",
                "ws://unused.invalid",
                "--create-room",
                "--disable-mdns",
                "--exchange",
                "--p2p-retry-count",
                "1",
            ]);
            let me = PlayerId::from_u128(1);
            let peer = PlayerId::from_u128(2);
            let mut state = super::tests::fixture_orchestrator(&cli, ws, me);
            state.current_session_generation = Some(SessionGeneration::from_u128(3));
            state.establish_pair(peer, initiate).await.unwrap();
            let (mut remote, opens, _remote_events) =
                open_physical_pair_with_events(&mut state, &mut server_ws, peer, None).await;
            let late_label = if early_label == RELIABLE_LABEL {
                UNRELIABLE_LABEL
            } else {
                RELIABLE_LABEL
            };
            let mut delayed_receipt = None;
            let context = format!("initiate={initiate}, early_label={early_label}");
            let original_generation = opens[0].peer_generation().1;
            for label in [early_label, late_label] {
                remote
                    .channel(me, label)
                    .unwrap()
                    .send_text(&json!({"from": peer, "channel": label, "seq": 0}).to_string())
                    .await
                    .unwrap();
                let receipt = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if let event @ EngineEvent::ChannelMessage { .. } =
                            state.engine_rx.recv().await.unwrap()
                        {
                            break event;
                        }
                    }
                })
                .await
                .expect("real probe arrives while local open callbacks are held");
                let EngineEvent::ChannelMessage {
                    peer: received_peer,
                    generation,
                    label: ref received_label,
                    ..
                } = receipt
                else {
                    unreachable!()
                };
                assert_eq!(received_peer, peer, "{context}: receipt sender");
                assert_eq!(
                    generation, original_generation,
                    "{context}: receipt generation"
                );
                assert_eq!(received_label, label, "{context}: receipt label");
                if label == early_label {
                    state.handle_engine_event(receipt).await.unwrap();
                } else {
                    delayed_receipt = Some(receipt);
                }
            }
            assert!(state.connected_pairs.is_empty());
            assert!(state.exchange_ledger.received[&peer].contains(early_label));
            for event in opens {
                state.handle_engine_event(event).await.unwrap();
            }
            assert!(state.connected_pairs.contains(&peer));
            assert_eq!(state.deferred_exchange.len(), 2);
            assert!(state.exchange_ledger.sent.is_empty());
            state
                .engine
                .channel(peer, early_label)
                .unwrap()
                .close()
                .await
                .unwrap();
            let closed = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let event @ EngineEvent::ChannelClosed { .. } =
                        state.engine_rx.recv().await.unwrap()
                    {
                        break event;
                    }
                }
            })
            .await
            .expect("required-channel close overtakes deferred sends");
            let EngineEvent::ChannelClosed {
                peer: closed_peer,
                generation,
                label: ref closed_label,
            } = closed
            else {
                unreachable!()
            };
            assert_eq!(closed_peer, peer, "{context}: closed sender");
            assert_eq!(
                generation, original_generation,
                "{context}: closed generation"
            );
            assert_eq!(closed_label, early_label, "{context}: closed label");
            state.handle_engine_event(closed).await.unwrap();
            assert!(!state.engine.is_paired(peer));
            assert_eq!(
                state.deferred_exchange.len(),
                2,
                "old sends remain queued for identity fencing"
            );
            assert_eq!(state.exchange_ledger.unmet_criteria().len(), 2);
            assert!(state.exchange_ledger.received[&peer].contains(early_label));

            state.rebuild_pair(peer, 1, false).await.unwrap();
            remote.remove_peer(me).await.unwrap();
            let (mut replacement, opens, mut replacement_events) =
                open_physical_pair_with_events(&mut state, &mut server_ws, peer, None).await;
            for event in opens {
                state.handle_engine_event(event).await.unwrap();
            }
            state
                .handle_engine_event(delayed_receipt.unwrap())
                .await
                .unwrap();
            assert!(
                !state.exchange_ledger.received[&peer].contains(late_label),
                "{context}: old generation cannot repay missing receive debt"
            );
            assert_eq!(state.deferred_exchange.len(), 4);
            for _ in 0..2 {
                state.process_deferred_exchange().await.unwrap();
            }
            assert!(
                state.exchange_ledger.sent.is_empty(),
                "{context}: old queued sends cannot credit the replacement"
            );
            while !state.deferred_exchange.is_empty() {
                state.process_deferred_exchange().await.unwrap();
            }
            let mut labels = BTreeSet::new();
            tokio::time::timeout(Duration::from_secs(2), async {
                while labels.len() < 2 {
                    if let EngineEvent::ChannelMessage { label, text, .. } =
                        replacement_events.recv().await.unwrap()
                    {
                        assert!(is_exchange_probe(me, &label, &text));
                        assert!(labels.insert(label), "replacement sends each probe once");
                    }
                }
            })
            .await
            .expect("both replacement probes reach the remote endpoint");
            assert_eq!(
                state.exchange_ledger.unmet_criteria(),
                [format!(
                    "exchange incomplete (received from {peer}; missing {late_label})"
                )]
            );
            replacement
                .channel(me, late_label)
                .unwrap()
                .send_text(&json!({"from": peer, "channel": late_label, "seq": 0}).to_string())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !state.exchange_ledger.unmet_criteria().is_empty() {
                    let event = state.engine_rx.recv().await.unwrap();
                    state.handle_engine_event(event).await.unwrap();
                }
            })
            .await
            .expect("fresh replacement receipt repays the remaining debt");
            state.remove_pair_obligation(peer).await;
            replacement.remove_peer(me).await.unwrap();
        }
    }
}

// Audit #826/#853/#889: the callback producer can observe loss before dispatch.
#[tokio::test]
async fn deferred_exchange_respects_observed_loss_before_close_dispatch() {
    let mut failures = Vec::new();
    for initiate in [false, true] {
        for closed_label in [None, Some(RELIABLE_LABEL), Some(UNRELIABLE_LABEL)] {
            let (ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
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
            state.establish_pair(peer, initiate).await.unwrap();
            let (mut remote, opens, mut remote_events) =
                open_physical_pair_with_events(&mut state, &mut server_ws, peer, None).await;
            for event in opens {
                state.handle_engine_event(event).await.unwrap();
            }
            let surviving_label = if closed_label == Some(RELIABLE_LABEL) {
                UNRELIABLE_LABEL
            } else {
                RELIABLE_LABEL
            };
            state
                .deferred_exchange
                .retain(|send| send.label == surviving_label);
            assert_eq!(state.deferred_exchange.len(), 1);
            let survivor = state.engine.channel(peer, surviving_label).unwrap();
            let closed = if let Some(label) = closed_label {
                state
                    .engine
                    .channel(peer, label)
                    .unwrap()
                    .close()
                    .await
                    .unwrap();
                Some(
                    tokio::time::timeout(Duration::from_secs(2), async {
                        loop {
                            let event = state.engine_rx.recv().await.unwrap();
                            if let EngineEvent::ChannelClosed {
                                label: observed, ..
                            } = &event
                            {
                                if observed == label {
                                    break event;
                                }
                            }
                        }
                    })
                    .await
                    .expect("producer observes required-channel loss before dispatch"),
                )
            } else {
                None
            };
            assert_eq!(
                survivor.ready_state().await.unwrap(),
                rtc::data_channel::RTCDataChannelState::Open
            );
            assert!(
                state.connected_pairs.contains(&peer),
                "application has not consumed loss"
            );
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let send_calls = calls.clone();
            state.exchange_send_hook = Some(Box::new(move |channel, text| {
                send_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Box::pin(async move { channel.send_text(&text).await })
            }));
            state.process_deferred_exchange().await.unwrap();
            let sent = state
                .exchange_ledger
                .has_sent_on_current_link(peer, surviving_label);
            let calls = calls.load(std::sync::atomic::Ordering::Relaxed);
            if sent != closed_label.is_none() || calls != usize::from(closed_label.is_none()) {
                failures.push(format!(
                    "initiate={initiate}, closed={closed_label:?}: send calls={calls}, credited={sent}"
                ));
            }
            if let Some(closed) = closed {
                assert!(!state.exchange_ledger.unmet_criteria().is_empty());
                state.handle_engine_event(closed).await.unwrap();
                assert!(!state.connected_pairs.contains(&peer));
            } else {
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if let EngineEvent::ChannelMessage { label, text, .. } =
                            remote_events.recv().await.unwrap()
                        {
                            assert_eq!(label, surviving_label);
                            assert!(is_exchange_probe(me, &label, &text));
                            break;
                        }
                    }
                })
                .await
                .expect("healthy deferred probe reaches the remote channel");
            }
            state.remove_pair_obligation(peer).await;
            remote.remove_peer(me).await.unwrap();
        }
    }
    assert!(
        failures.is_empty(),
        "observed-loss histories failed: {failures:#?}"
    );
}
