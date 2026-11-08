use super::*;
use crate::engine::snapshot_tests::{startup_pair_with_held_open_events, HeldCoreSend};
use crate::engine::{ChannelProfile, EngineSettings, ORDERED_LABEL};
use clap::Parser;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::Ordering;

/// Open a real peer link, optionally retaining its owner open callbacks.
async fn connect_owner_peer(
    state: &mut Orchestrator<'_>,
    remote: &mut Engine,
    remote_events: &mut mpsc::UnboundedReceiver<EngineEvent>,
    peer: PlayerId,
    owner_offers: bool,
    mut held_open_events: Option<&mut Vec<EngineEvent>>,
) {
    state.connected_pairs.remove(&peer);
    if owner_offers {
        let offer = state
            .engine
            .pair_with(peer, true, &[])
            .await
            .unwrap()
            .unwrap();
        remote.pair_with(state.my_id, false, &[]).await.unwrap();
        let answer = remote.handle_offer(state.my_id, offer).await.unwrap();
        state.engine.handle_answer(peer, answer).await.unwrap();
    } else {
        let offer = remote
            .pair_with(state.my_id, true, &[])
            .await
            .unwrap()
            .unwrap();
        state.engine.pair_with(peer, false, &[]).await.unwrap();
        let answer = state.engine.handle_offer(peer, offer).await.unwrap();
        remote.handle_answer(state.my_id, answer).await.unwrap();
    }
    let required_channels = state.engine.diagnostic_snapshot(peer)["channels"]
        .as_object()
        .unwrap()
        .len();
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut remote_ready = false;
        while !remote_ready || match held_open_events.as_ref() {
            Some(opens) => opens.len() < required_channels,
            None => !state.connected_pairs.contains(&peer),
        } {
            tokio::select! {
                event = state.engine_rx.recv() => {
                    let event = event.unwrap();
                    if !state.engine.is_current_event(&event) { continue; }
                    match event {
                    EngineEvent::LocalCandidate { peer: target, candidate_json, .. } if target == peer => {
                        remote.handle_remote_candidate(state.my_id, &candidate_json).await.unwrap();
                    }
                    event @ EngineEvent::ChannelOpen { peer: target, .. } if target == peer && held_open_events.is_some() => {
                        held_open_events.as_mut().unwrap().push(event);
                    }
                    event => state.handle_engine_event(event).await.unwrap(),
                    }
                },
                event = remote_events.recv() => {
                    let event = event.unwrap();
                    if !remote.is_current_event(&event) { continue; }
                    match event {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        state.engine.handle_remote_candidate(peer, &candidate_json).await.unwrap();
                    }
                    EngineEvent::RemoteChannel { label, channel, .. } => {
                        remote.store_remote_channel(state.my_id, label, channel).await;
                    }
                    EngineEvent::ChannelOpen { label, .. } => {
                        remote_ready |= remote.note_channel_open(state.my_id, &label).await;
                    }
                    EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => panic!("healthy startup must not terminate"),
                    EngineEvent::ChannelMessage { .. } | EngineEvent::ChannelBinaryMessage { .. } => panic!("startup must not discard gameplay"),
                    _ => {}
                    }
                },
            }
        }
    }).await.expect("production owner and remote open every required channel");
}

/// Retain one exact reliable payload and its receive permit.
async fn exact_receipt(
    events: &mut mpsc::UnboundedReceiver<EngineEvent>,
    peer: PlayerId,
    expected: &[u8],
) -> EngineEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap();
            match &event {
                EngineEvent::ChannelMessage {
                    peer: sender,
                    label,
                    text,
                    ..
                } => {
                    assert_eq!(*sender, peer);
                    assert_eq!(label, RELIABLE_LABEL);
                    assert_eq!(text.as_bytes(), expected);
                    return event;
                }
                EngineEvent::ChannelBinaryMessage {
                    peer: sender,
                    label,
                    data,
                    ..
                } => {
                    assert_eq!(*sender, peer);
                    assert_eq!(label, RELIABLE_LABEL);
                    assert_eq!(data, expected);
                    return event;
                }
                EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => {
                    panic!("exact gameplay must not become terminal input")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("exact reliable gameplay receipt")
}

/// #1510/#853/#889: a busy startup query must leave mesh gameplay and relay work runnable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_open_callback_does_not_wait_for_the_rtc_core_on_the_owner() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for receiver_offers in [false, true] {
            let (mut pair, clock, opens) =
                startup_pair_with_held_open_events(profile, receiver_offers).await;
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
            let other_peer = PlayerId::from_u128(0x1510);
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
                !receiver_offers,
                None,
            )
            .await;
            let generation = state.engine.diagnostic_snapshot(peer)["generation"]
                .as_u64()
                .unwrap();
            assert!(!state.engine.accept_pair_ready(peer, generation));
            let held =
                HeldCoreSend::start(&clock, state.engine.channel(peer, RELIABLE_LABEL).unwrap())
                    .await;
            let live = clock.observe_next_task();
            let labels: &[&str] = if profile == ChannelProfile::Godot {
                &[RELIABLE_LABEL, UNRELIABLE_LABEL, ORDERED_LABEL]
            } else {
                &[RELIABLE_LABEL, UNRELIABLE_LABEL]
            };
            let dispatched = tokio::time::timeout(Duration::from_millis(100), async {
                for event in opens {
                    state.handle_engine_event(event).await.unwrap();
                }
                for &label in labels {
                    state
                        .handle_engine_event(EngineEvent::ChannelOpen {
                            peer,
                            generation,
                            label: label.into(),
                        })
                        .await
                        .unwrap();
                }
            })
            .await;
            if dispatched.is_err() {
                held.finish().await;
                panic!("profile={profile:?}, receiver_offers={receiver_offers}: channel-open dispatch must leave the owner available while the actual RTC core is held");
            }
            assert_eq!(
                live.load(Ordering::SeqCst),
                1,
                "one pending readiness query per link"
            );
            assert!(!state.connected_pairs.contains(&peer));
            assert!(
                !state.engine.accept_pair_ready(peer, generation),
                "a pending query cannot commit readiness"
            );
            assert!(!state
                .engine
                .accept_pair_ready(peer, generation.wrapping_add(1)));
            let independent = vec![b'M'; 4096];
            other
                .channel(state.my_id, RELIABLE_LABEL)
                .unwrap()
                .send(independent.as_slice().into())
                .await
                .unwrap();
            let receipt = exact_receipt(&mut state.engine_rx, other_peer, &independent).await;
            state.handle_engine_event(receipt).await.unwrap();
            let reverse = vec![b'R'; 4096];
            state
                .engine
                .send_channel(other_peer, RELIABLE_LABEL)
                .await
                .unwrap()
                .send(reverse.as_slice().into())
                .await
                .unwrap();
            drop(exact_receipt(&mut other_events, state.my_id, &reverse).await);
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
            assert!(
                state.pong_deadline.is_none(),
                "relay input stays responsive while readiness waits"
            );
            assert_eq!(
                live.load(Ordering::SeqCst),
                1,
                "the core remains held through independent gameplay and relay input"
            );
            held.finish().await;
            drop(exact_receipt(&mut pair.sender_events, state.my_id, b"held-readiness").await);
            tokio::time::timeout(Duration::from_secs(5), async {
                while !state.connected_pairs.contains(&peer) {
                    let event = state.engine_rx.recv().await.unwrap();
                    state.handle_engine_event(event).await.unwrap();
                }
            })
            .await
            .expect("released core completes the original readiness check");
            assert!(
                !state.engine.accept_pair_ready(peer, generation),
                "readiness commits once"
            );
            let expected = vec![b'F'; 4096];
            pair.sender
                .channel(state.my_id, RELIABLE_LABEL)
                .unwrap()
                .send(expected.as_slice().into())
                .await
                .unwrap();
            drop(exact_receipt(&mut state.engine_rx, peer, &expected).await);
            state.engine.remove_peer(peer).await.unwrap();
            state.engine.remove_peer(other_peer).await.unwrap();
            pair.sender.remove_peer(state.my_id).await.unwrap();
            other.remove_peer(state.my_id).await.unwrap();
        }
    }
}

/// #1510/#1264/#1244: queued readiness cannot revive closed or replaced channels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_readiness_respects_terminal_channels_and_exact_fresh_startup() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for receiver_offers in [false, true] {
            let (mut pair, clock, opens) =
                startup_pair_with_held_open_events(profile, receiver_offers).await;
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
            let generation = state.engine.diagnostic_snapshot(peer)["generation"]
                .as_u64()
                .unwrap();
            let expected = vec![b'K'; 4096];
            pair.sender
                .channel(state.my_id, RELIABLE_LABEL)
                .unwrap()
                .send(expected.as_slice().into())
                .await
                .unwrap();
            let retained = exact_receipt(&mut state.engine_rx, peer, &expected).await;
            let held =
                HeldCoreSend::start(&clock, state.engine.channel(peer, RELIABLE_LABEL).unwrap())
                    .await;
            for event in opens {
                state.handle_engine_event(event).await.unwrap();
            }
            held.finish().await;
            drop(exact_receipt(&mut pair.sender_events, state.my_id, b"held-readiness").await);
            let ready = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = state.engine_rx.recv().await.unwrap();
                    if matches!(event, EngineEvent::PairReady { .. }) {
                        break event;
                    }
                }
            })
            .await
            .expect("retain an actual completed readiness result before owner consumption");
            pair.sender
                .channel(state.my_id, UNRELIABLE_LABEL)
                .unwrap()
                .close()
                .await
                .unwrap();
            let closed = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = state.engine_rx.recv().await.unwrap();
                    if matches!(event, EngineEvent::ChannelClosed { .. }) {
                        break event;
                    }
                }
            })
            .await
            .expect("real channel closure becomes terminal before readiness is consumed");
            state.handle_engine_event(ready).await.unwrap();
            assert!(
                !state.connected_pairs.contains(&peer),
                "terminal completion cannot report a connected pair"
            );
            state.handle_engine_event(closed).await.unwrap();
            assert!(!state.engine.is_paired(peer));
            assert_eq!(
                state.engine.diagnostic_snapshot(peer),
                serde_json::Value::Null
            );
            pair.sender.remove_peer(state.my_id).await.unwrap();
            let mut replacement_opens = Vec::new();
            connect_owner_peer(
                &mut state,
                &mut pair.sender,
                &mut pair.sender_events,
                peer,
                receiver_offers,
                Some(&mut replacement_opens),
            )
            .await;
            let fresh_generation = state.engine.diagnostic_snapshot(peer)["generation"]
                .as_u64()
                .unwrap();
            assert_ne!(fresh_generation, generation);
            let held =
                HeldCoreSend::start(&clock, state.engine.channel(peer, RELIABLE_LABEL).unwrap())
                    .await;
            for event in replacement_opens {
                state.handle_engine_event(event).await.unwrap();
            }
            held.finish().await;
            drop(exact_receipt(&mut pair.sender_events, state.my_id, b"held-readiness").await);
            let fresh_ready = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = state.engine_rx.recv().await.unwrap();
                    if matches!(event, EngineEvent::PairReady { .. }) {
                        break event;
                    }
                }
            })
            .await
            .expect("fresh readiness succeeds before its result is consumed");
            assert_eq!(fresh_ready.peer_generation(), (peer, fresh_generation));
            assert!(!state.connected_pairs.contains(&peer));
            state
                .handle_engine_event(EngineEvent::PairReady { peer, generation })
                .await
                .unwrap();
            assert!(
                !state.connected_pairs.contains(&peer),
                "stale readiness cannot connect a replacement waiting for its own result"
            );
            assert!(
                !state.engine.accept_pair_ready(peer, generation),
                "the Engine rejects old generation even when the current readiness query succeeded"
            );
            state.handle_engine_event(fresh_ready).await.unwrap();
            assert!(state.connected_pairs.contains(&peer));
            state
                .handle_engine_event(EngineEvent::PairReady { peer, generation })
                .await
                .unwrap();
            assert!(
                !state.p2p_reconnected_pairs.contains(&peer),
                "stale readiness cannot report the fresh link again"
            );
            assert_eq!(
                state.engine.diagnostic_snapshot(peer)["receive_budget"]["pending_messages"],
                1,
                "the old exact receipt remains charged across physical replacement"
            );
            match &retained {
                EngineEvent::ChannelMessage { text, .. } => assert_eq!(text.as_bytes(), &expected),
                EngineEvent::ChannelBinaryMessage { data, .. } => assert_eq!(data, &expected),
                _ => unreachable!(),
            }
            drop(retained);
            for sequence in 0_u32..32 {
                let mut body = vec![b'F'; 4096];
                body[..4].copy_from_slice(&sequence.to_be_bytes());
                pair.sender
                    .channel(state.my_id, RELIABLE_LABEL)
                    .unwrap()
                    .send(body.as_slice().into())
                    .await
                    .unwrap();
                let event = exact_receipt(&mut state.engine_rx, peer, &body).await;
                assert_eq!(event.peer_generation(), (peer, fresh_generation));
                drop(event);
            }
            let reverse = vec![b'B'; 4096];
            state
                .engine
                .send_channel(peer, RELIABLE_LABEL)
                .await
                .unwrap()
                .send(reverse.as_slice().into())
                .await
                .unwrap();
            drop(exact_receipt(&mut pair.sender_events, state.my_id, &reverse).await);
            state.engine.remove_peer(peer).await.unwrap();
            pair.sender.remove_peer(state.my_id).await.unwrap();
        }
    }
}
