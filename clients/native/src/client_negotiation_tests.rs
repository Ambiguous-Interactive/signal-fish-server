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

async fn held_negotiation(step: &'static str, budget: Budget, initiate: bool) {
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
        state.run_deadline = Deadline::after(now, Duration::from_millis(100));
    }
    let old_generation = 1;
    let mut operation = Box::pin(async {
        if pairing {
            state.establish_pair(peer, initiate).await
        } else {
            state.apply_signal(peer, signal).await
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
    let mut retry = Box::pin(state.request_pair_retry(peer, 1));
    tokio::select! {
        _ = entered.notified() => {}
        result = &mut retry => panic!("retry ended before the held close: {result:?}"),
    }
    tokio::time::advance(Duration::from_millis(249)).await;
    assert!(
        futures_util::poll!(retry.as_mut()).is_pending(),
        "close must remain held before its deadline"
    );
    tokio::time::advance(Duration::from_millis(2)).await;
    tokio::time::resume();
    retry
        .as_mut()
        .await
        .expect("retired close timeout must not end the relay session");
    drop(retry);
    assert!(
        state.engine.is_current_generation(peer, 2),
        "retry installs its fresh physical link"
    );
    assert_eq!(state.pair_retry_attempts.get(&peer), Some(&1));
    assert!(state.expected_peers.contains(&peer));
    state.engine.negotiation_hold = None;
    state.engine.remove_peer(peer).await.unwrap();
}
