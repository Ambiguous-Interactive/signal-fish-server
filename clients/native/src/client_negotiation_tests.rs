// Regression #961: cancellation must retire a link before relay and timer work resumes.
use super::*;
use crate::engine::NegotiationHold;
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq)]
enum Budget {
    P2p,
    Run,
    Zero,
    HeldSuccess,
}

/// Exercise cancellation, receipt preservation, and recovery at one suspended setup phase.
async fn held_negotiation(step: &'static str, budget: Budget, initiate: bool) {
    let diagnostics = super::peer_loss_tests::DiagnosticCapture::default();
    let _subscriber = tracing::subscriber::set_default(diagnostics.subscriber());
    let run_first = matches!(budget, Budget::Run | Budget::Zero);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(tcp).await.unwrap()
    });
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let mut server_ws = server.await.unwrap();
    let mut cli = Cli::parse_from([
        "reference-native",
        "--server-url",
        &url,
        "--create-room",
        "--disable-mdns",
        "--p2p-timeout-secs",
        if run_first { "10" } else { "1" },
        "--p2p-retry-count",
        "0",
        "--run-for-secs",
        if run_first { "1" } else { "10" },
    ]);
    let held_success = tempfile::tempdir().unwrap();
    if budget == Budget::Zero {
        cli.p2p_timeout_secs = 0;
    }
    if budget == Budget::HeldSuccess {
        cli.success_release_file = Some(held_success.path().join("release"));
    }
    let peer = PlayerId::from_u128(2);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.webrtc_plan_seen = true;
    state.current_session_generation = Some(SessionGeneration::from_u128(3));
    state.expected_peers.insert(peer);
    state.pair_roles.insert(peer, initiate);
    let pairing = matches!(
        step,
        "route" | "build" | "reliable" | "unreliable" | "offer" | "offer_local"
    );
    let (remote_tx, _remote_events) = mpsc::unbounded_channel();
    let mut remote = Engine::new(cli.engine_settings(), remote_tx).unwrap();
    let mut signal = json!({});
    if !pairing {
        let offer = state.engine.pair_with(peer, initiate, &[]).await.unwrap();
        if initiate {
            remote.pair_with(state.my_id, false, &[]).await.unwrap();
            let answer = remote
                .handle_offer(state.my_id, offer.unwrap())
                .await
                .unwrap();
            signal = json!({"Answer": answer});
        } else {
            let offer = remote
                .pair_with(state.my_id, true, &[])
                .await
                .unwrap()
                .unwrap();
            signal = json!({"Offer": offer});
        }
        if step == "buffered_ice" {
            state
                .engine
                .handle_remote_candidate(peer, "")
                .await
                .unwrap();
        }
        if step == "live_ice" {
            state.apply_signal(peer, signal).await.unwrap();
            signal = json!({"IceCandidate": ""});
        }
    }
    state.send_message(&ClientMessage::Ping).await.unwrap();
    loop {
        let frame = server_ws.next().await.unwrap().unwrap();
        let message: ClientMessage = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        if matches!(message, ClientMessage::Ping) {
            break;
        }
    }
    if step == "live_ice" {
        state.connected_pairs.insert(peer);
        state.transport_status = Some(true);
        state.ice_gathering_complete.insert(peer);
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    state.engine.negotiation_hold = Some(NegotiationHold {
        step,
        entered: entered.clone(),
        release: release.clone(),
    });
    tokio::time::pause();
    let now = Instant::now();
    state.run_deadline = Deadline::after(now, Duration::from_secs(if run_first { 1 } else { 10 }));
    state.p2p_deadline = Some(Deadline::after(
        now,
        Duration::from_secs(if run_first { 10 } else { 1 }),
    ));
    if budget == Budget::HeldSuccess {
        state.success_criteria_reported = true;
        state.success_hold_started_at = Some(now);
        state.run_deadline = Deadline::after(now, Duration::from_millis(100));
    }
    let old_generation = 1;
    let mut operation = Box::pin(async {
        if pairing {
            state.establish_pair(peer, initiate).await
        } else {
            state.apply_signal(peer, signal).await?;
            while state.engine.candidate_pending(peer) {
                let event = state.engine_rx.recv().await.unwrap();
                if matches!(event, EngineEvent::RemoteCandidateComplete { .. }) {
                    state.handle_engine_event(event).await?;
                }
            }
            Ok(())
        }
    });
    tokio::select! {
        _ = entered.notified() => {}
        result = &mut operation => panic!("{step}: operation ended before the suspension: {result:?}"),
    }
    if budget == Budget::P2p {
        for message in [
            ServerMessage::Pong,
            ServerMessage::GameData {
                from_player: peer,
                data: json!({"relay_msg": "during negotiation"}),
                seq: None,
                epoch: None,
                class: None,
                key: None,
            },
        ] {
            server_ws
                .send(Message::Text(
                    serde_json::to_string(&message).unwrap().into(),
                ))
                .await
                .unwrap();
        }
    }
    tokio::time::advance(Duration::from_millis(900)).await;
    assert!(
        futures_util::poll!(operation.as_mut()).is_pending(),
        "{step}: no early cancellation"
    );
    tokio::time::advance(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_millis(500), &mut operation)
        .await
        .unwrap_or_else(|_| panic!("{step}: negotiation blocks the expired deadline"))
        .unwrap();
    drop(operation);
    assert!(
        !state.engine.is_paired(peer),
        "{step}: retire partial state before resuming"
    );
    assert!(!state.engine.is_current_generation(peer, old_generation));
    let losses = diagnostics.losses();
    assert_eq!(
        losses.len(),
        usize::from(!matches!(step, "route" | "build")),
        "{step}: {losses:?}"
    );
    if let Some((prefix, snapshot)) = losses.first() {
        assert!(prefix.contains("negotiation deadline expired"), "{prefix}");
        assert!(prefix.contains(&peer.to_string()), "{prefix}");
        assert_eq!(snapshot["generation"], old_generation);
    }

    assert!(
        state.expected_peers.contains(&peer),
        "{step}: retain logical obligations"
    );
    release.notify_waiters();
    state.engine.negotiation_hold = None;
    if run_first {
        assert_eq!(
            state.process_timers().await.unwrap(),
            Some(EXIT_CRITERIA_UNMET)
        );
    } else {
        assert_eq!(state.process_timers().await.unwrap(), None);
        assert_eq!(state.transport_status, Some(false));
    }
    if budget == Budget::P2p {
        state.pong_deadline = Some(Instant::now() + Duration::from_secs(2));
        state.run_deadline = Deadline::after(Instant::now(), Duration::from_millis(500));
        assert_eq!(state.run_loop().await.unwrap(), EXIT_CRITERIA_UNMET);
        assert_eq!(
            state.relay_received_from,
            BTreeSet::from([peer]),
            "{step}: consume queued relay data"
        );
        assert!(
            state.pong_deadline.is_none(),
            "{step}: consume queued server liveness"
        );
    }
    // The original operation no longer owns a future that can send stale SDP.
    state.send_message(&ClientMessage::Ping).await.unwrap();
    loop {
        let frame = server_ws.next().await.unwrap().unwrap();
        let message: ClientMessage = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        if matches!(message, ClientMessage::Ping) {
            break;
        }
        assert!(
            !matches!(message, ClientMessage::Signal { .. }),
            "{step}: cancelled work sent SDP"
        );
    }
    tokio::time::resume();
    remote.remove_peer(state.my_id).await.unwrap();
    let offer = state.engine.pair_with(peer, initiate, &[]).await.unwrap();
    assert!(
        state.engine.is_current_generation(peer, 2),
        "{step}: fresh physical generation"
    );
    state
        .handle_engine_event(EngineEvent::ChannelOpen {
            peer,
            generation: old_generation,
            label: RELIABLE_LABEL.to_owned(),
        })
        .await
        .unwrap();
    assert!(
        !state.connected_pairs.contains(&peer),
        "{step}: old events cannot revive the pair"
    );
    let (mut fresh_remote, _opens, mut fresh_events) =
        super::retry_tests::open_physical_pair_with_events(&mut state, &mut server_ws, peer, offer)
            .await;
    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        state
            .engine
            .channel(peer, label)
            .unwrap()
            .send_text("fresh-local")
            .await
            .unwrap();
        fresh_remote
            .channel(state.my_id, label)
            .unwrap()
            .send_text("fresh-remote")
            .await
            .unwrap();
    }
    let mut receipts = BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while receipts.len() < 4 {
            tokio::select! {
                event = state.engine_rx.recv() => {
                    let event = event.unwrap();
                    if !state.engine.is_current_event(&event) { continue; }
                    if let EngineEvent::ChannelMessage { label, text, .. } = event {
                        assert_eq!(text, "fresh-remote");
                        assert!(receipts.insert(("local", label)));
                    }
                }
                event = fresh_events.recv() => {
                    if let EngineEvent::ChannelMessage { label, text, .. } = event.unwrap() {
                        assert_eq!(text, "fresh-local");
                        assert!(receipts.insert(("remote", label)));
                    }
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{step}: fresh bidirectional exchange did not finish"));
    state.engine.remove_peer(peer).await.unwrap();
    fresh_remote.remove_peer(state.my_id).await.unwrap();
}

macro_rules! deadline_cases {
    ($p2p:ident, $run:ident, $step:literal, $initiate:literal) => {
        #[tokio::test]
        async fn $p2p() {
            held_negotiation($step, Budget::P2p, $initiate).await;
        }
        #[tokio::test]
        async fn $run() {
            held_negotiation($step, Budget::Run, $initiate).await;
        }
    };
}

deadline_cases!(route_initiator_p2p, route_initiator_run, "route", true);
deadline_cases!(route_responder_p2p, route_responder_run, "route", false);
deadline_cases!(build_initiator_p2p, build_initiator_run, "build", true);
deadline_cases!(build_responder_p2p, build_responder_run, "build", false);
deadline_cases!(reliable_p2p, reliable_run, "reliable", true);
deadline_cases!(unreliable_p2p, unreliable_run, "unreliable", true);
deadline_cases!(offer_p2p, offer_run, "offer", true);
deadline_cases!(offer_local_p2p, offer_local_run, "offer_local", true);
deadline_cases!(remote_offer_p2p, remote_offer_run, "remote_offer", false);
deadline_cases!(remote_answer_p2p, remote_answer_run, "remote_answer", true);
deadline_cases!(
    offer_buffered_ice_p2p,
    offer_buffered_ice_run,
    "buffered_ice",
    false
);
deadline_cases!(
    answer_buffered_ice_p2p,
    answer_buffered_ice_run,
    "buffered_ice",
    true
);
deadline_cases!(answer_p2p, answer_run, "answer", false);
deadline_cases!(answer_local_p2p, answer_local_run, "answer_local", false);
deadline_cases!(offer_live_ice_p2p, offer_live_ice_run, "live_ice", false);
deadline_cases!(answer_live_ice_p2p, answer_live_ice_run, "live_ice", true);

#[tokio::test]
async fn zero_p2p_preserves_late_negotiation_until_the_run_deadline() {
    for (step, initiate) in [
        ("route", true),
        ("build", false),
        ("offer_local", true),
        ("remote_offer", false),
        ("remote_answer", true),
        ("buffered_ice", false),
        ("answer_local", false),
        ("live_ice", true),
    ] {
        held_negotiation(step, Budget::Zero, initiate).await;
    }
}

#[tokio::test]
async fn held_success_preserves_the_run_deadline_exemption() {
    for (step, initiate) in [
        ("route", true),
        ("build", false),
        ("offer_local", true),
        ("remote_offer", false),
        ("remote_answer", true),
        ("buffered_ice", false),
        ("answer_local", false),
        ("live_ice", true),
    ] {
        held_negotiation(step, Budget::HeldSuccess, initiate).await;
    }
}

#[tokio::test]
async fn negotiation_future_keeps_nested_retry_stack_bounded() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut engine = Engine::new(Default::default(), tx).unwrap();
    let operation = engine.negotiate(
        PlayerId::from_u128(2),
        Negotiation::Pair {
            initiate: true,
            ice_servers: Vec::new(),
        },
        Deadline::after(Instant::now(), Duration::from_secs(1)),
    );
    let bytes = std::mem::size_of_val(&operation);
    assert!(
        bytes < 4096,
        "nested retry frames must not retain the stack operation inline: {bytes} bytes"
    );
}

/// A bounded background close must leave coordinated retry available.
#[tokio::test]
async fn stalled_peer_close_does_not_abort_a_coordinated_retry() {
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
        "--disable-mdns",
    ]);
    let peer = PlayerId::from_u128(2);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.current_session_generation = Some(SessionGeneration::from_u128(3));
    state.establish_pair(peer, false).await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    state.engine.negotiation_hold = Some(NegotiationHold {
        step: "close",
        entered: entered.clone(),
        release: Arc::new(tokio::sync::Notify::new()),
    });
    tokio::time::pause();
    tokio::time::timeout(
        Duration::from_millis(100),
        state.request_pair_retry(peer, 1),
    )
    .await
    .expect("the fresh retry must not wait for the old close deadline")
    .expect("a pending retired close must not end the relay session");
    entered.notified().await;
    tokio::time::resume();
    assert!(
        state.engine.is_current_generation(peer, 2),
        "retry installs its fresh physical link"
    );
    assert_eq!(state.pair_retry_attempts.get(&peer), Some(&1));
    assert!(state.expected_peers.contains(&peer));
    state.engine.negotiation_hold = None;
    state.engine.remove_peer(peer).await.unwrap();
}

// Regression #972: one peer must not renew the remaining plan's connection window.
#[tokio::test]
async fn planned_peers_share_the_connection_deadline() {
    for (step, gated, partial) in [
        ("offer", false, false),
        ("offer_local", false, false),
        ("offer", true, false),
        ("offer", false, true),
        ("offer_local", true, true),
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
            "reference-native",
            "--server-url",
            &url,
            "--create-room",
            "--disable-mdns",
            "--p2p-timeout-secs",
            "1",
            "--p2p-retry-count",
            "0",
            "--run-for-secs",
            "10",
        ]);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        let peers = [PlayerId::from_u128(2), PlayerId::from_u128(3)];
        let plan: ServerMessage = serde_json::from_value(json!({
            "type": "SessionPlan", "data": {
                "generation": PlayerId::from_u128(4), "topology": "mesh", "transport": "webrtc",
                "host": null, "direct_endpoint": null, "ice_servers": [], "fallback": "relay",
                "peers": peers.map(|player_id| json!({
                    "player_id": player_id, "player_name": "peer", "is_authority": false, "initiate": true
                }))
            }
        })).unwrap();
        if gated {
            state.p2p_released = false;
            state.handle_server_message(plan.clone()).await.unwrap();
        }
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        state.engine.negotiation_hold = Some(NegotiationHold {
            step,
            entered: entered.clone(),
            release: release.clone(),
        });
        tokio::time::pause();
        let started = Instant::now();
        let mut operation = Box::pin(async {
            if gated {
                state.process_p2p_gate(started).await
            } else {
                state.handle_server_message(plan).await
            }
        });
        tokio::select! {
            _ = entered.notified() => {}
            result = &mut operation => panic!("{step}/{gated}: ended before held negotiation: {result:?}"),
        }
        if partial {
            tokio::time::advance(Duration::from_millis(700)).await;
            release.notify_one();
            tokio::select! {
                _ = entered.notified() => {}
                result = &mut operation => panic!("{step}/{gated}: ended before second held negotiation: {result:?}"),
            }
        }
        tokio::time::advance(Duration::from_millis(if partial { 299 } else { 999 })).await;
        assert!(
            futures_util::poll!(operation.as_mut()).is_pending(),
            "{step}/{gated}: no early expiry"
        );
        tokio::time::advance(Duration::from_millis(1)).await;
        let result = tokio::time::timeout(Duration::from_millis(100), operation.as_mut()).await;
        assert!(
            result.is_ok(),
            "{step}/{gated}: later peers renew the expired plan window"
        );
        result.unwrap().unwrap();
        drop(operation);
        assert_eq!(state.expected_peers, BTreeSet::from(peers));
        for (index, peer) in peers.into_iter().enumerate() {
            assert_eq!(
                state.engine.is_paired(peer),
                partial && index == 0,
                "{step}/{gated}: expired plan must not retain links"
            );
        }
        assert!(state.p2p_deadline.unwrap().is_due(Instant::now()));
        assert_eq!(state.process_timers().await.unwrap(), None);
        assert_eq!(state.transport_status, Some(false));
        tokio::time::resume();
        state.engine.negotiation_hold = None;
        for peer in peers {
            state.engine.remove_peer(peer).await.unwrap();
        }
    }
}

// Regression #979 review: established links do not inherit the setup cutoff.
#[tokio::test]
async fn connected_peer_ice_uses_a_fresh_bounded_operation_window() {
    for expired in [false, true] {
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
            "1",
            "--run-for-secs",
            "10",
        ]);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.webrtc_plan_seen = true;
        state.expected_peers.insert(peer);
        state.pair_roles.insert(peer, true);
        let offer = state.engine.pair_with(peer, true, &[]).await.unwrap();
        let (mut remote, opens, mut remote_events) =
            super::retry_tests::open_physical_pair_with_events(
                &mut state,
                &mut server_ws,
                peer,
                offer,
            )
            .await;
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        assert!(state.connected_pairs.contains(&peer));
        assert_eq!(state.transport_status, Some(true));
        tokio::time::pause();
        let clock_guard = tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        });
        let now = Instant::now();
        state.p2p_deadline = Some(Deadline::after(now, Duration::from_secs(1)));
        state.run_deadline = Deadline::after(now, Duration::from_secs(10));
        tokio::time::advance(Duration::from_millis(900)).await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        state.engine.negotiation_hold = Some(NegotiationHold {
            step: "candidate_complete",
            entered: entered.clone(),
            release: release.clone(),
        });
        let mut operation = Box::pin(state.negotiate(peer, Negotiation::Candidate(String::new())));
        tokio::select! {
            _ = entered.notified() => {}
            result = &mut operation => panic!("live candidate did not reach the hold: {result:?}"),
        }
        tokio::time::advance(Duration::from_millis(if expired { 1000 } else { 100 })).await;
        release.notify_one();
        let result = operation.await.unwrap();
        clock_guard.abort();
        tokio::time::resume();
        state.engine.negotiation_hold = None;
        assert_eq!(result.is_err(), expired, "expired={expired}: {result:?}");
        assert_eq!(state.engine.is_paired(peer), !expired);
        assert_eq!(state.connected_pairs.contains(&peer), !expired);
        assert_eq!(state.transport_status, Some(!expired));
        if !expired {
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                state
                    .engine
                    .channel(peer, label)
                    .unwrap()
                    .send_text("after-setup-cutoff")
                    .await
                    .unwrap();
            }
            let mut received = BTreeSet::new();
            tokio::time::timeout(Duration::from_secs(5), async {
                while received.len() < 2 {
                    if let EngineEvent::ChannelMessage { label, text, .. } =
                        remote_events.recv().await.unwrap()
                    {
                        assert_eq!(text, "after-setup-cutoff");
                        assert!(received.insert(label));
                    }
                }
            })
            .await
            .expect("both live channels still carry gameplay after the setup deadline");
        }
        state.engine.remove_peer(peer).await.unwrap();
        remote.remove_peer(state.my_id).await.unwrap();
    }
}

// Regression #1087: setup failure resolves fallback before the P2P deadline.
#[tokio::test]
async fn initial_pair_creation_failure_reports_fallback_and_recovers() {
    for (initiate, sibling_connected) in
        [(false, false), (true, false), (false, true), (true, true)]
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
            "reference-native",
            "--server-url",
            &url,
            "--create-room",
            "--disable-mdns",
            "--p2p-timeout-secs",
            "30",
            "--run-for-secs",
            "60",
        ]);
        let peer = PlayerId::from_u128(2);
        let healthy = PlayerId::from_u128(4);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.webrtc_plan_seen = true;
        state.current_session_generation = Some(SessionGeneration::from_u128(3));
        state.establish_pair(healthy, false).await.unwrap();
        let mut sibling = if sibling_connected {
            let (remote, opens, _) = super::retry_tests::open_physical_pair_with_events(
                &mut state,
                &mut server_ws,
                healthy,
                None,
            )
            .await;
            for event in opens {
                state.handle_engine_event(event).await.unwrap();
            }
            assert_eq!(state.transport_status, Some(true));
            Some(remote)
        } else {
            None
        };
        state.last_ice_servers = vec![signal_fish_server::protocol::IceServer {
            urls: vec!["invalid:ice-endpoint".into()],
            username: None,
            credential: None,
        }];
        state.establish_pair(peer, initiate).await.unwrap();
        assert!(
            !state.engine.is_paired(peer),
            "invalid configuration fails setup"
        );
        assert!(
            state.engine.is_paired(healthy),
            "another peer remains paired"
        );
        assert!(state.expected_peers.contains(&peer));
        assert!(!state.p2p_deadline.unwrap().is_due(Instant::now()));
        assert_eq!(
            state.transport_status,
            Some(sibling_connected),
            "report current availability now"
        );
        state.send_message(&ClientMessage::Ping).await.unwrap();
        let reports = tokio::time::timeout(Duration::from_secs(1), async {
            let mut reports = Vec::new();
            loop {
                let frame = server_ws.next().await.unwrap().unwrap();
                let message: ClientMessage =
                    serde_json::from_str(frame.to_text().unwrap()).unwrap();
                match message {
                    ClientMessage::TransportStatus { connected, .. } => reports.push(connected),
                    ClientMessage::Ping => return reports,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            reports,
            vec![sibling_connected],
            "failed peer cannot clear a healthy sibling"
        );
        state.last_ice_servers.clear();
        state.request_pair_retry(peer, 1).await.unwrap();
        let (mut remote, opens, mut receipts) = super::retry_tests::open_physical_pair_with_events(
            &mut state,
            &mut server_ws,
            peer,
            None,
        )
        .await;
        for event in opens {
            state.handle_engine_event(event).await.unwrap();
        }
        assert_eq!(state.transport_status, Some(true));
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            state
                .engine
                .channel(peer, label)
                .unwrap()
                .send_text("fresh-gameplay")
                .await
                .unwrap();
        }
        let mut labels = BTreeSet::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while labels.len() < 2 {
                if let EngineEvent::ChannelMessage { label, text, .. } =
                    receipts.recv().await.unwrap()
                {
                    assert_eq!(text, "fresh-gameplay");
                    assert!(labels.insert(label));
                }
            }
        })
        .await
        .expect("fresh retry channels carry gameplay");
        assert!(state.engine.is_paired(healthy));
        state.engine.remove_peer(peer).await.unwrap();
        state.engine.remove_peer(healthy).await.unwrap();
        remote.remove_peer(state.my_id).await.unwrap();
        if let Some(remote) = &mut sibling {
            remote.remove_peer(state.my_id).await.unwrap();
        }
    }
}

/// A live ICE query must not hold mesh gameplay, relay input, or liveness.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_ice_does_not_block_the_session_owner() {
    use crate::engine::snapshot_tests::{startup_pair_with_held_open_events, HeldCoreSend};
    use crate::engine::{ChannelProfile, EngineSettings};

    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for owner_offers in [false, true] {
            let (mut pair, clock, _) =
                startup_pair_with_held_open_events(profile, owner_offers).await;
            let peer = pair.sender_id;
            let (ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
            let cli = Cli::parse_from([
                "native",
                "--server-url",
                "ws://unused.invalid",
                "--create-room",
            ]);
            let mut state = super::tests::fixture_orchestrator(&cli, ws, pair.receiver_id);
            std::mem::swap(&mut state.engine, &mut pair.receiver);
            std::mem::swap(&mut state.engine_rx, &mut pair.receiver_events);
            state.expected_peers.insert(peer);
            let other_peer = PlayerId::from_u128(0x1518);
            let (other_tx, mut other_events) = mpsc::unbounded_channel();
            let mut other = Engine::new(
                EngineSettings {
                    disable_mdns: true,
                    channel_profile: profile,
                    ..EngineSettings::default()
                },
                other_tx,
            )
            .unwrap();
            super::readiness_tests::connect_owner_peer(
                &mut state,
                &mut other,
                &mut other_events,
                other_peer,
                !owner_offers,
                None,
            )
            .await;
            let before = state.engine.diagnostic_snapshot(peer)["remote_candidate_count"]
                .as_u64()
                .unwrap();
            let held =
                HeldCoreSend::start(&clock, state.engine.channel(peer, RELIABLE_LABEL).unwrap())
                    .await;
            let result = tokio::time::timeout(
                Duration::from_millis(100),
                state.apply_signal(peer, json!({"IceCandidate": ""})),
            )
            .await;
            if result.is_err() {
                held.finish().await;
                panic!("profile={profile:?}, owner_offers={owner_offers}: live ICE must leave the owner available while the actual RTC core is held");
            }
            result.unwrap().unwrap();
            assert!(state.engine.candidate_pending(peer));
            let captured = state.pending_signal_deadlines[&peer];
            for _ in 0..MAX_PENDING_SIGNALS_PER_PEER {
                state
                    .apply_signal(peer, json!({"IceCandidate": ""}))
                    .await
                    .unwrap();
            }
            assert_eq!(
                state.pending_signals[&peer].len(),
                MAX_PENDING_SIGNALS_PER_PEER
            );
            assert_eq!(
                state.engine.diagnostic_snapshot(peer)["remote_candidate_count"],
                before + 1,
                "queued candidates cannot overtake the pending stack call"
            );
            assert_eq!(
                state.negotiation_deadline(peer),
                captured,
                "queued work retains the original absolute operation budget"
            );

            other
                .channel(state.my_id, RELIABLE_LABEL)
                .unwrap()
                .send_text("exact-mesh-during-live-ice")
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = state.engine_rx.recv().await.unwrap();
                    let exact = matches!(&event, EngineEvent::ChannelMessage { peer, label, text, .. } if *peer == other_peer && label == RELIABLE_LABEL && text == "exact-mesh-during-live-ice");
                    state.handle_engine_event(event).await.unwrap();
                    if exact { break; }
                }
            }).await.expect("independent real mesh gameplay progresses while live ICE is pending");
            state.pong_deadline = Some(Instant::now());
            for message in [
                ServerMessage::Pong,
                ServerMessage::GameData {
                    from_player: other_peer,
                    data: json!({"relay_msg": "exact-relay-during-live-ice"}),
                    seq: None,
                    epoch: None,
                    class: None,
                    key: None,
                },
            ] {
                server_ws
                    .send(Message::Text(
                        serde_json::to_string(&message).unwrap().into(),
                    ))
                    .await
                    .unwrap();
                let frame = state.ws.next().await;
                state
                    .handle_server_input(frame, Instant::now())
                    .await
                    .unwrap();
            }
            assert!(state.pong_deadline.is_none());
            assert!(state.relay_received_from.contains(&other_peer));
            assert!(state.engine.candidate_pending(peer));
            held.finish().await;
            tokio::time::timeout(Duration::from_secs(5), async {
                while state.engine.candidate_pending(peer)
                    || state.pending_signals.contains_key(&peer)
                {
                    let event = state.engine_rx.recv().await.unwrap();
                    state.handle_engine_event(event).await.unwrap();
                }
            })
            .await
            .expect("released ICE drains the exact ordered candidate tail");
            assert_eq!(
                state.engine.diagnostic_snapshot(peer)["remote_candidate_count"],
                before + 1 + u64::try_from(MAX_PENDING_SIGNALS_PER_PEER).unwrap()
            );
            assert!(!state.pending_signal_deadlines.contains_key(&peer));
            state.engine.remove_peer(peer).await.unwrap();
            state.engine.remove_peer(other_peer).await.unwrap();
            other.remove_peer(state.my_id).await.unwrap();
        }
    }
}

/// A saturated peer falls back without ending relay, and old results cannot cancel fresh ICE.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_ice_overflow_keeps_relay_and_fresh_link_recovery_live() {
    use crate::engine::snapshot_tests::{startup_pair_with_held_open_events, HeldCoreSend};
    use crate::engine::ChannelProfile;
    use std::sync::atomic::Ordering;

    for owner_offers in [false, true] {
        let (mut pair, clock, _) =
            startup_pair_with_held_open_events(ChannelProfile::Reference, owner_offers).await;
        let peer = pair.sender_id;
        let (ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
        let cli = Cli::parse_from([
            "native",
            "--server-url",
            "ws://unused.invalid",
            "--create-room",
            "--disable-mdns",
        ]);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, pair.receiver_id);
        std::mem::swap(&mut state.engine, &mut pair.receiver);
        std::mem::swap(&mut state.engine_rx, &mut pair.receiver_events);
        state.expected_peers.insert(peer);
        state.pair_roles.insert(peer, owner_offers);
        let held =
            HeldCoreSend::start(&clock, state.engine.channel(peer, RELIABLE_LABEL).unwrap()).await;
        state
            .apply_signal(peer, json!({"IceCandidate": ""}))
            .await
            .unwrap();
        assert!(state.engine.candidate_pending(peer));
        held.finish().await;
        let old_result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = state.engine_rx.recv().await.unwrap();
                if matches!(event, EngineEvent::RemoteCandidateComplete { .. }) {
                    break event;
                }
            }
        })
        .await
        .expect("retain the completed old-generation ICE result before retirement");
        for _ in 0..MAX_PENDING_SIGNALS_PER_PEER {
            state
                .apply_signal(peer, json!({"IceCandidate": ""}))
                .await
                .unwrap();
        }
        state
            .apply_signal(peer, json!({"IceCandidate": ""}))
            .await
            .expect("peer overload must not terminate the session");
        assert!(!state.engine.is_paired(peer));
        assert!(!state.pending_signals.contains_key(&peer));
        assert!(!state.pending_signal_deadlines.contains_key(&peer));
        assert!(state.expected_peers.contains(&peer));
        state.pong_deadline = Some(Instant::now());
        for message in [
            ServerMessage::Pong,
            ServerMessage::GameData {
                from_player: peer,
                data: json!({"relay_msg": "exact-after-ice-overflow"}),
                seq: None,
                epoch: None,
                class: None,
                key: None,
            },
        ] {
            server_ws
                .send(Message::Text(
                    serde_json::to_string(&message).unwrap().into(),
                ))
                .await
                .unwrap();
            let frame = state.ws.next().await;
            state
                .handle_server_input(frame, Instant::now())
                .await
                .unwrap();
        }
        assert!(state.pong_deadline.is_none());
        assert!(state.relay_received_from.contains(&peer));
        let offer = state
            .engine
            .pair_with(peer, owner_offers, &[])
            .await
            .unwrap();
        let (mut fresh, _, mut fresh_events) = super::retry_tests::open_physical_pair_with_events(
            &mut state,
            &mut server_ws,
            peer,
            offer,
        )
        .await;
        let held =
            HeldCoreSend::start(&clock, state.engine.channel(peer, RELIABLE_LABEL).unwrap()).await;
        let live = clock.observe_next_task();
        state
            .apply_signal(peer, json!({"IceCandidate": ""}))
            .await
            .unwrap();
        state
            .apply_signal(peer, json!({"IceCandidate": ""}))
            .await
            .unwrap();
        state.handle_engine_event(old_result).await.unwrap();
        assert!(
            state.engine.candidate_pending(peer),
            "old completion cannot consume fresh pending ICE"
        );
        assert_eq!(live.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.pending_signals[&peer].len(),
            1,
            "old completion cannot drain fresh signaling"
        );
        held.finish().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.engine.candidate_pending(peer) || state.pending_signals.contains_key(&peer)
            {
                let event = state.engine_rx.recv().await.unwrap();
                state.handle_engine_event(event).await.unwrap();
            }
        })
        .await
        .expect("fresh ordered ICE completes after core release");
        state
            .engine
            .channel(peer, RELIABLE_LABEL)
            .unwrap()
            .send_text("exact-fresh-local")
            .await
            .unwrap();
        fresh
            .channel(state.my_id, RELIABLE_LABEL)
            .unwrap()
            .send_text("exact-fresh-remote")
            .await
            .unwrap();
        let mut local_received = false;
        let mut remote_received = false;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !local_received || !remote_received {
                tokio::select! {
                    event = state.engine_rx.recv() => {
                        if let EngineEvent::ChannelMessage { text, label, .. } = event.unwrap() {
                            assert_eq!(label, RELIABLE_LABEL);
                            if text == "held-readiness" { continue; }
                            assert_eq!(text, "exact-fresh-remote");
                            assert!(!local_received);
                            local_received = true;
                        }
                    }
                    event = fresh_events.recv() => {
                        if let EngineEvent::ChannelMessage { text, label, .. } = event.unwrap() {
                            assert_eq!(label, RELIABLE_LABEL);
                            if text == "held-readiness" { continue; }
                            assert_eq!(text, "exact-fresh-local");
                            assert!(!remote_received);
                            remote_received = true;
                        }
                    }
                }
            }
        })
        .await
        .expect("fresh link preserves exact reliable bytes in both directions");
        state.engine.remove_peer(peer).await.unwrap();
        fresh.remove_peer(state.my_id).await.unwrap();
    }
}

/// A queued live candidate inherits the first operation's absolute cutoff.
#[tokio::test]
async fn queued_ice_cannot_renew_the_original_operation_deadline() {
    use crate::engine::snapshot_tests::{startup_pair_with_held_open_events, HeldCoreSend};
    use crate::engine::ChannelProfile;

    let (mut pair, clock, _) =
        startup_pair_with_held_open_events(ChannelProfile::Godot, false).await;
    let peer = pair.sender_id;
    let (ws, _server_ws) = crate::socket_tests::websocket_pair().await;
    let cli = Cli::parse_from([
        "native",
        "--server-url",
        "ws://unused.invalid",
        "--create-room",
    ]);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, pair.receiver_id);
    std::mem::swap(&mut state.engine, &mut pair.receiver);
    std::mem::swap(&mut state.engine_rx, &mut pair.receiver_events);
    state.expected_peers.insert(peer);
    let held =
        HeldCoreSend::start(&clock, state.engine.channel(peer, RELIABLE_LABEL).unwrap()).await;
    tokio::time::pause();
    let clock_guard = tokio::spawn(async {
        loop {
            tokio::task::yield_now().await;
        }
    });
    state.run_deadline = Deadline::after(Instant::now(), Duration::from_millis(500));
    state
        .apply_signal(peer, json!({"IceCandidate": ""}))
        .await
        .unwrap();
    state
        .apply_signal(peer, json!({"IceCandidate": ""}))
        .await
        .unwrap();
    let original = state.pending_signal_deadlines[&peer];
    held.finish().await;
    let first = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = state.engine_rx.recv().await.unwrap();
            if matches!(event, EngineEvent::RemoteCandidateComplete { .. }) {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        &first,
        EngineEvent::RemoteCandidateComplete {
            result: CandidateApplyResult::Applied,
            ..
        }
    ));
    state.run_deadline = Deadline::after(Instant::now(), Duration::from_secs(10));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    state.engine.negotiation_hold = Some(NegotiationHold {
        step: "live_ice",
        entered: entered.clone(),
        release: release.clone(),
    });
    state.handle_engine_event(first).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    assert_eq!(state.pending_signal_deadlines[&peer], original);
    tokio::time::advance(Duration::from_millis(499)).await;
    loop {
        match state.engine_rx.try_recv() {
            Ok(event) => assert!(
                !matches!(event, EngineEvent::RemoteCandidateComplete { .. }),
                "queued ICE must not expire before its original cutoff"
            ),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                panic!("the retained Engine owns its event sender")
            }
        }
    }
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(Instant::now(), original.finite().unwrap());
    clock_guard.abort();
    let expired = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let event = state.engine_rx.recv().await.unwrap();
            if matches!(event, EngineEvent::RemoteCandidateComplete { .. }) {
                break event;
            }
        }
    })
    .await
    .expect("queued ICE must expire at the first deadline despite the extended session window");
    assert!(matches!(
        &expired,
        EngineEvent::RemoteCandidateComplete {
            result: CandidateApplyResult::DeadlineElapsed,
            ..
        }
    ));
    state.handle_engine_event(expired).await.unwrap();
    assert!(!state.engine.is_paired(peer));
    assert!(!state.pending_signals.contains_key(&peer));
    assert!(!state.pending_signal_deadlines.contains_key(&peer));
    assert!(state.expected_peers.contains(&peer));
    release.notify_waiters();
    tokio::time::resume();
}
