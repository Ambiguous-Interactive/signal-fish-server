use super::readiness_tests::connect_owner_peer;
use super::*;
use crate::engine::receive_tests::LivePair;
use crate::engine::snapshot_tests::{startup_pair_with_held_open_events, HeldClock, HeldCoreSend};
use crate::engine::{ChannelProfile, EngineSettings};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use std::sync::{atomic::Ordering, Arc};

/// #1512/#853/#889: pending sends leave mesh gameplay and WebSocket input runnable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_exchange_send_leaves_owner_available() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for owner_offers in [false, true] {
            let (mut pair, clock, opens) =
                startup_pair_with_held_open_events(profile, owner_offers).await;
            let peer = pair.sender_id;
            let (ws, mut server_ws) = crate::socket_tests::websocket_pair().await;
            let cli = Cli::parse_from([
                "native",
                "--server-url",
                "ws://unused.invalid",
                "--create-room",
                "--exchange",
            ]);
            let mut state = super::tests::fixture_orchestrator(&cli, ws, pair.receiver_id);
            std::mem::swap(&mut state.engine, &mut pair.receiver);
            std::mem::swap(&mut state.engine_rx, &mut pair.receiver_events);
            for event in opens {
                state.handle_engine_event(event).await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                while !state.connected_pairs.contains(&peer) {
                    let event = state.engine_rx.recv().await.unwrap();
                    state.handle_engine_event(event).await.unwrap();
                }
            })
            .await
            .unwrap();
            let other_peer = PlayerId::from_u128(0x1512);
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
            connect_owner_peer(
                &mut state,
                &mut other,
                &mut other_events,
                other_peer,
                !owner_offers,
                None,
            )
            .await;
            state.deferred_exchange.clear();
            state.exchange_ledger.note_connected(peer);
            state.exchange_ledger.note_connected(other_peer);
            state.queue_exchange(peer, &[RELIABLE_LABEL, UNRELIABLE_LABEL, RELIABLE_LABEL]);
            state.queue_exchange(other_peer, &[RELIABLE_LABEL, UNRELIABLE_LABEL]);
            let held =
                HeldCoreSend::start(&clock, state.engine.channel(peer, RELIABLE_LABEL).unwrap())
                    .await;
            let live = clock.observe_next_task();
            let dispatched = tokio::time::timeout(Duration::from_millis(100), async {
                while !state.deferred_exchange.is_empty() {
                    state.process_deferred_exchange().await.unwrap();
                }
            })
            .await;
            if dispatched.is_err() {
                held.finish().await;
                panic!("profile={profile:?}, owner_offers={owner_offers}: exchange dispatch must return while the actual RTC core is held");
            }
            assert_eq!(live.load(Ordering::SeqCst), 1);
            assert!(!state
                .exchange_ledger
                .has_sent_on_current_link(peer, RELIABLE_LABEL));
            assert!(!state
                .exchange_ledger
                .has_sent_on_current_link(peer, UNRELIABLE_LABEL));
            let exact = vec![b'M'; 4096];
            other
                .channel(state.my_id, RELIABLE_LABEL)
                .unwrap()
                .send(exact.as_slice().into())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = state.engine_rx.recv().await.unwrap();
                    let matches = match &event {
                        EngineEvent::ChannelMessage {
                            peer, label, text, ..
                        } => {
                            *peer == other_peer
                                && label == RELIABLE_LABEL
                                && text.as_bytes() == exact
                        }
                        EngineEvent::ChannelBinaryMessage {
                            peer, label, data, ..
                        } => *peer == other_peer && label == RELIABLE_LABEL && data == &exact,
                        _ => false,
                    };
                    state.handle_engine_event(event).await.unwrap();
                    if matches {
                        break;
                    }
                }
            })
            .await
            .expect("independent exact reliable receipt reaches the owner while a send is pending");
            server_ws
                .send(Message::Text(
                    serde_json::to_string(&ServerMessage::Pong).unwrap().into(),
                ))
                .await
                .unwrap();
            state.pong_deadline = Some(Instant::now());
            let frame = state.ws.next().await;
            state
                .handle_server_input(frame, Instant::now())
                .await
                .unwrap();
            assert!(state.pong_deadline.is_none());
            // Both independent sends must reach their remote endpoint before release.
            let mut received = BTreeSet::new();
            tokio::time::timeout(Duration::from_secs(5), async {
                while received.len() < 2 {
                    if let EngineEvent::ChannelMessage { label, text, .. } =
                        other_events.recv().await.unwrap()
                    {
                        assert!(is_exchange_probe(state.my_id, &label, &text));
                        assert!(received.insert(label));
                    }
                }
            })
            .await
            .expect("independent mesh exchange progresses with the first core held");
            held.finish().await;
            // The held send precedes the two exchange probes on the remote link.
            let mut probes = BTreeSet::new();
            tokio::time::timeout(Duration::from_secs(5), async {
                while probes.len() < 2 || !state.exchange_ledger.has_sent_on_current_link(peer, RELIABLE_LABEL)
                    || !state.exchange_ledger.has_sent_on_current_link(peer, UNRELIABLE_LABEL) {
                    tokio::select! {
                        event = state.engine_rx.recv() => state.handle_engine_event(event.unwrap()).await.unwrap(),
                        event = pair.sender_events.recv() => if let EngineEvent::ChannelMessage { label, text, .. } = event.unwrap() {
                            if text == "held-readiness" { continue; }
                            assert!(is_exchange_probe(state.my_id, &label, &text));
                            assert!(probes.insert(label), "one send per label despite duplicate queued work");
                        }
                    }
                }
            }).await.expect("released sends complete and pay only their own physical-link debt");
            assert!(!state.engine.accept_channel_send(peer, RELIABLE_LABEL));
            state.engine.remove_peer(peer).await.unwrap();
            state.engine.remove_peer(other_peer).await.unwrap();
            pair.sender.remove_peer(state.my_id).await.unwrap();
            other.remove_peer(state.my_id).await.unwrap();
        }
    }
}

/// Create a real open exchange link with an observable RTC runtime.
async fn exchange_fixture<'a>(
    cli: &'a Cli,
    profile: ChannelProfile,
    owner_offers: bool,
) -> (Orchestrator<'a>, LivePair, Arc<HeldClock>) {
    let (mut pair, clock, opens) = startup_pair_with_held_open_events(profile, owner_offers).await;
    let (ws, _server_ws) = crate::socket_tests::websocket_pair().await;
    let mut state = super::tests::fixture_orchestrator(cli, ws, pair.receiver_id);
    std::mem::swap(&mut state.engine, &mut pair.receiver);
    std::mem::swap(&mut state.engine_rx, &mut pair.receiver_events);
    for event in opens {
        state.handle_engine_event(event).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while !state.connected_pairs.contains(&pair.sender_id) {
            let event = state.engine_rx.recv().await.unwrap();
            state.handle_engine_event(event).await.unwrap();
        }
    })
    .await
    .unwrap();
    state.deferred_exchange.clear();
    state.exchange_ledger.note_connected(pair.sender_id);
    (state, pair, clock)
}

/// Retain a real send result before its owner consumes it.
async fn next_send_result(state: &mut Orchestrator<'_>) -> EngineEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = state.engine_rx.recv().await.unwrap();
            if matches!(event, EngineEvent::ChannelSendComplete { .. }) {
                return event;
            }
            state.handle_engine_event(event).await.unwrap();
        }
    })
    .await
    .expect("link-owned send publishes its completion")
}

/// #1512/#1264: receive refusal and closure fence completed sends before owner dispatch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_exchange_send_cannot_revive_a_terminal_or_replaced_link() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for owner_offers in [false, true] {
            for receive_refused in [false, true] {
                let cli = Cli::parse_from([
                    "native",
                    "--server-url",
                    "ws://unused.invalid",
                    "--create-room",
                    "--exchange",
                ]);
                let (mut state, mut pair, clock) =
                    exchange_fixture(&cli, profile, owner_offers).await;
                let peer = pair.sender_id;
                let generation = state.engine.diagnostic_snapshot(peer)["generation"]
                    .as_u64()
                    .unwrap();
                let held = HeldCoreSend::start(
                    &clock,
                    state.engine.channel(peer, RELIABLE_LABEL).unwrap(),
                )
                .await;
                state.queue_exchange(peer, &[RELIABLE_LABEL]);
                state.process_deferred_exchange().await.unwrap();
                held.finish().await;
                let completion = next_send_result(&mut state).await;
                assert!(matches!(
                    &completion,
                    EngineEvent::ChannelSendComplete { result: Ok(()), .. }
                ));
                assert!(!state
                    .exchange_ledger
                    .has_sent_on_current_link(peer, RELIABLE_LABEL));
                tokio::time::timeout(Duration::from_secs(5), async {
                    for expected in [
                        "held-readiness".to_owned(),
                        format!(
                            r#"{{"from":"{}","channel":"reliable","seq":0}}"#,
                            state.my_id
                        ),
                    ] {
                        loop {
                            if let EngineEvent::ChannelMessage { label, text, .. } =
                                pair.sender_events.recv().await.unwrap()
                            {
                                assert_eq!(label, RELIABLE_LABEL);
                                assert_eq!(
                                    text, expected,
                                    "initial reliable stream retains its complete prefix"
                                );
                                break;
                            }
                        }
                    }
                })
                .await
                .expect("initial reliable stream reaches its remote endpoint before terminal loss");
                // Keep the admitted input alive to charge exactly the message limit.
                let mut retained = Vec::new();
                let terminal = if receive_refused {
                    let channel = pair.sender.channel(state.my_id, RELIABLE_LABEL).unwrap();
                    for _ in 0..=256 {
                        channel.send_text("").await.unwrap();
                    }
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            let event = state.engine_rx.recv().await.unwrap();
                            match event {
                                event @ EngineEvent::ChannelMessage { .. } => retained.push(event),
                                event @ EngineEvent::ChannelReceiveRejected { .. } => break event,
                                EngineEvent::ChannelClosed { .. } => {
                                    panic!("receive refusal must retain its own cause")
                                }
                                event => state.handle_engine_event(event).await.unwrap(),
                            }
                        }
                    })
                    .await
                    .expect("actual receive message budget refuses the 257th packet")
                } else {
                    pair.sender
                        .channel(state.my_id, UNRELIABLE_LABEL)
                        .unwrap()
                        .close()
                        .await
                        .unwrap();
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            let event = state.engine_rx.recv().await.unwrap();
                            if matches!(event, EngineEvent::ChannelClosed { .. }) {
                                break event;
                            }
                            state.handle_engine_event(event).await.unwrap();
                        }
                    })
                    .await
                    .expect("actual required-channel closure becomes terminal")
                };
                let snapshot = state.engine.diagnostic_snapshot(peer);
                if let EngineEvent::ChannelReceiveRejected { failure, .. } = &terminal {
                    assert_eq!(
                        failure.reason,
                        crate::engine::ReceiveRejection::PendingMessages
                    );
                    assert_eq!(failure.message_bytes, 0);
                    assert_eq!(failure.pending_messages, 256);
                    assert_eq!(retained.len(), 256);
                    assert_eq!(snapshot["receive_rejection"], "pending_messages");
                    assert_eq!(
                        snapshot["channels"][RELIABLE_LABEL]["receipts"],
                        json!({"raw":257,"admitted":256,"rejected":1,"discarded":0})
                    );
                } else {
                    assert!(snapshot["receive_failure"].is_null());
                    assert!(!snapshot["channel_closure"].is_null());
                }
                state.handle_engine_event(completion).await.unwrap();
                assert!(
                    !state
                        .exchange_ledger
                        .has_sent_on_current_link(peer, RELIABLE_LABEL),
                    "terminal result cannot pay send debt"
                );
                state.handle_engine_event(terminal).await.unwrap();
                assert!(!state.engine.is_paired(peer));
                pair.sender.remove_peer(state.my_id).await.unwrap();
                drop(retained);
                connect_owner_peer(
                    &mut state,
                    &mut pair.sender,
                    &mut pair.sender_events,
                    peer,
                    owner_offers,
                    None,
                )
                .await;
                let fresh = state.engine.diagnostic_snapshot(peer)["generation"]
                    .as_u64()
                    .unwrap();
                assert_ne!(fresh, generation);
                let fresh_hold = HeldCoreSend::start(
                    &clock,
                    state.engine.channel(peer, RELIABLE_LABEL).unwrap(),
                )
                .await;
                let fresh_task = clock.observe_next_task();
                state.deferred_exchange.clear();
                state.queue_exchange(peer, &[RELIABLE_LABEL]);
                state.process_deferred_exchange().await.unwrap();
                assert_eq!(fresh_task.load(Ordering::SeqCst), 1);
                for result in [Ok(()), Err("stale send failure".to_string())] {
                    state
                        .handle_engine_event(EngineEvent::ChannelSendComplete {
                            peer,
                            generation,
                            label: RELIABLE_LABEL.into(),
                            text: "stale".into(),
                            result,
                        })
                        .await
                        .unwrap();
                }
                assert!(state.engine.is_current_generation(peer, fresh));
                assert!(!state
                    .exchange_ledger
                    .has_sent_on_current_link(peer, RELIABLE_LABEL));
                assert_eq!(
                    fresh_task.load(Ordering::SeqCst),
                    1,
                    "stale results cannot cancel a pending send on the fresh link"
                );
                fresh_hold.finish().await;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !state
                        .exchange_ledger
                        .has_sent_on_current_link(peer, RELIABLE_LABEL)
                    {
                        let event = state.engine_rx.recv().await.unwrap();
                        state.handle_engine_event(event).await.unwrap();
                    }
                })
                .await
                .expect("fresh physical link repays its own reliable send debt");
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let EngineEvent::ChannelMessage {
                            label,
                            text,
                            generation,
                            ..
                        } = pair.sender_events.recv().await.unwrap()
                        {
                            assert_eq!(label, RELIABLE_LABEL);
                            assert!(pair.sender.is_current_generation(state.my_id, generation));
                            if text == "held-readiness" {
                                continue;
                            }
                            assert!(is_exchange_probe(state.my_id, &label, &text));
                            break;
                        }
                    }
                })
                .await
                .expect("fresh generation starts with the exact reliable probe");
                state.engine.remove_peer(peer).await.unwrap();
                pair.sender.remove_peer(state.my_id).await.unwrap();
            }
        }
    }
}

/// #1512: pending work releases captures on every physical-link retirement path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_exchange_send_is_cancelled_by_retirement_and_keeps_its_deadline() {
    for retirement in ["remove", "room-left", "replacement", "drop", "deadline"] {
        let cli = Cli::parse_from([
            "native",
            "--server-url",
            "ws://unused.invalid",
            "--create-room",
            "--exchange",
        ]);
        let (mut state, mut pair, clock) =
            exchange_fixture(&cli, ChannelProfile::Godot, false).await;
        let peer = pair.sender_id;
        let held =
            HeldCoreSend::start(&clock, state.engine.channel(peer, RELIABLE_LABEL).unwrap()).await;
        let live = clock.observe_next_task();
        if retirement == "deadline" {
            state.run_deadline = Deadline::after(Instant::now(), Duration::from_millis(50));
        }
        state.queue_exchange(peer, &[RELIABLE_LABEL]);
        state.process_deferred_exchange().await.unwrap();
        assert_eq!(live.load(Ordering::SeqCst), 1);
        if retirement == "deadline" {
            let completion = next_send_result(&mut state).await;
            assert!(
                matches!(&completion, EngineEvent::ChannelSendComplete { result: Err(message), .. } if message.contains("exceeded its exchange deadline"))
            );
            assert!(!state
                .exchange_ledger
                .has_sent_on_current_link(peer, RELIABLE_LABEL));
            state.handle_engine_event(completion).await.unwrap();
            assert!(!state.engine.is_paired(peer));
        } else {
            match retirement {
                "remove" => state.remove_pair_obligation(peer).await,
                "room-left" => state
                    .handle_server_message(ServerMessage::RoomLeft)
                    .await
                    .unwrap(),
                "replacement" => state.prepare_retained_pair_replacement(peer).await,
                "drop" => {
                    let (tx, _) = mpsc::unbounded_channel();
                    drop(std::mem::replace(
                        &mut state.engine,
                        Engine::new(EngineSettings::default(), tx).unwrap(),
                    ));
                }
                _ => unreachable!(),
            }
        }
        tokio::time::timeout(Duration::from_millis(100), async {
            while live.load(Ordering::SeqCst) != 0 { tokio::task::yield_now().await; }
        }).await.expect("retirement or the original deadline releases pending send captures while the core stays held");
        assert!(!state.exchange_ledger.has_sent(peer, RELIABLE_LABEL));
        held.finish().await;
        // The deliberate held send is allowed; the cancelled exchange must never reach SCTP.
        pair.sender.remove_peer(state.my_id).await.unwrap();
        loop {
            match pair.sender_events.try_recv() {
                Ok(EngineEvent::ChannelMessage { text, .. }) => assert_eq!(
                    text, "held-readiness",
                    "cancelled exchange cannot run after core release"
                ),
                Ok(EngineEvent::ChannelBinaryMessage { .. }) => {
                    panic!("cancelled exchange cannot send binary gameplay")
                }
                Ok(_) => {}
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    panic!("the retained remote Engine owns its event sender")
                }
            }
        }
    }
}
