use super::{receive_tests::LivePair, *};

// Regression #905: peer shutdown must close channels and permit a fresh link.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_peer_close_retires_both_required_channels() {
    for initiator_closes in [true, false] {
        let mut pair = LivePair::connect().await;
        let old_sender_generation = pair.sender.peers[&pair.receiver_id].generation;
        let old_receiver_generation = pair.receiver.peers[&pair.sender_id].generation;
        let (closer, remaining, events, closed_peer, remaining_peer) = if initiator_closes {
            (
                &mut pair.sender,
                &pair.receiver,
                &mut pair.receiver_events,
                pair.sender_id,
                pair.receiver_id,
            )
        } else {
            (
                &mut pair.receiver,
                &pair.sender,
                &mut pair.sender_events,
                pair.receiver_id,
                pair.sender_id,
            )
        };
        let generation = remaining.peers[&closed_peer].generation;
        closer.remove_peer(remaining_peer).await.unwrap();
        // DTLS shutdown must retire channels promptly. Waiting for an ICE
        // disconnect timeout would hide the missing channel-close notification.
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut closed = std::collections::HashSet::new();
            while closed.len() != 2 {
                if let EngineEvent::ChannelClosed {
                    peer,
                    generation: event_generation,
                    label,
                } = events.recv().await.unwrap()
                {
                    assert_eq!(peer, closed_peer);
                    assert_eq!(event_generation, generation);
                    assert!(matches!(label.as_str(), RELIABLE_LABEL | UNRELIABLE_LABEL));
                    assert!(closed.insert(label), "one close event per channel");
                }
            }
        })
        .await
        .expect("remote peer close retires both required channels before ICE timeout");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !remaining.peers[&closed_peer]
                .channel_tasks
                .iter()
                .all(|task| task.is_finished())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("closed channels release both event producers");
        loop {
            let event = match events.try_recv() {
                Ok(event) => event,
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    panic!("the remaining engine must retain its event sender")
                }
            };
            assert!(
                !matches!(event, EngineEvent::ChannelClosed { .. }),
                "finished producers cannot leave duplicate close callbacks"
            );
        }

        pair.sender.remove_peer(pair.receiver_id).await.unwrap();
        pair.receiver.remove_peer(pair.sender_id).await.unwrap();
        assert!(pair.sender.peers.is_empty());
        assert!(pair.receiver.peers.is_empty());
        pair.negotiate().await;
        assert_ne!(
            pair.sender.peers[&pair.receiver_id].generation,
            old_sender_generation
        );
        assert_ne!(
            pair.receiver.peers[&pair.sender_id].generation,
            old_receiver_generation
        );
        for sender_sends in [true, false] {
            let (sender, receiver, events, receiver_id, sender_id) = if sender_sends {
                (
                    &pair.sender,
                    &pair.receiver,
                    &mut pair.receiver_events,
                    pair.receiver_id,
                    pair.sender_id,
                )
            } else {
                (
                    &pair.receiver,
                    &pair.sender,
                    &mut pair.sender_events,
                    pair.sender_id,
                    pair.receiver_id,
                )
            };
            let generation = receiver.peers[&sender_id].generation;
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                let text = format!("fresh-{sender_sends}-{label}");
                sender
                    .channel(receiver_id, label)
                    .unwrap()
                    .send_text(&text)
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        match events.recv().await.unwrap() {
                            EngineEvent::ChannelMessage {
                                peer,
                                generation: event_generation,
                                label: received_label,
                                text: received_text,
                                ..
                            } => {
                                assert_eq!(peer, sender_id);
                                assert_eq!(event_generation, generation);
                                assert_eq!(received_label, label);
                                assert_eq!(received_text, text);
                                break;
                            }
                            EngineEvent::ChannelClosed { .. }
                            | EngineEvent::ChannelReceiveRejected { .. } => {
                                panic!("fresh required channels must remain usable")
                            }
                            _ => {}
                        }
                    }
                })
                .await
                .expect("fresh required channels deliver in both directions");
            }
        }
        pair.cleanup().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_live_channel_opens_connect_each_pair_once() {
    let mut pair = LivePair::connect().await;
    for (engine, peer) in [
        (&mut pair.sender, pair.receiver_id),
        (&mut pair.receiver, pair.sender_id),
    ] {
        assert_eq!(engine.connected_pair_count(), 1);
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL, RELIABLE_LABEL] {
            assert!(!engine.note_channel_open(peer, label).await);
        }
        assert_eq!(engine.connected_pair_count(), 1);
    }
    pair.cleanup().await;
}

/*
    The driver can open and close a channel while its callbacks stay queued.
*/
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_channel_opens_do_not_connect_a_closed_pair() {
    for closed_label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        let (a_tx, mut a_events) = mpsc::unbounded_channel();
        let (b_tx, mut b_events) = mpsc::unbounded_channel();
        let settings = EngineSettings {
            disable_mdns: true,
            ..EngineSettings::default()
        };
        let mut a = Engine::new(settings, a_tx).unwrap();
        let mut b = Engine::new(settings, b_tx).unwrap();
        let a_id = PlayerId::from_u128(0xa);
        let b_id = PlayerId::from_u128(0xb);
        let offer = a.pair_with(b_id, true, &[]).await.unwrap().unwrap();
        b.pair_with(a_id, false, &[]).await.unwrap();
        let answer = b.handle_offer(a_id, offer).await.unwrap();
        a.handle_answer(b_id, answer).await.unwrap();
        let mut a_opens = Vec::new();
        let mut b_opens = Vec::new();
        tokio::time::timeout(Duration::from_secs(15), async {
            while a_opens.len() < 2 || b_opens.len() < 2 {
                tokio::select! {
                    event = a_events.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            b.handle_remote_candidate(a_id, &candidate_json).await.unwrap();
                        }
                        EngineEvent::RemoteChannel { label, channel, .. } => {
                            a.store_remote_channel(b_id, label, channel).await;
                        }
                        event @ EngineEvent::ChannelOpen { .. } => a_opens.push(event),
                        _ => {}
                    },
                    event = b_events.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            a.handle_remote_candidate(b_id, &candidate_json).await.unwrap();
                        }
                        EngineEvent::RemoteChannel { label, channel, .. } => {
                            b.store_remote_channel(a_id, label, channel).await;
                        }
                        event @ EngineEvent::ChannelOpen { .. } => b_opens.push(event),
                        _ => {}
                    },
                }
            }
        })
        .await
        .expect("real channels open before the delayed callback drain");
        a.channel(b_id, closed_label)
            .unwrap()
            .close()
            .await
            .unwrap();
        // Check the initiating side immediately after close(), before draining
        // its terminal notifications. The physical state already forbids sends.
        for event in &a_opens {
            if let EngineEvent::ChannelOpen { label, .. } = event {
                assert!(
                    !a.note_channel_open(b_id, label).await,
                    "queued opens must not connect a closing {closed_label} channel"
                );
            }
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut a_closed = false;
            let mut b_closed = false;
            while !a_closed || !b_closed {
                tokio::select! {
                    event = a_events.recv(), if !a_closed => {
                        if let EngineEvent::ChannelClosed { label, .. } = event.unwrap() {
                            a_closed = label == closed_label;
                        }
                    }
                    event = b_events.recv(), if !b_closed => {
                        if let EngineEvent::ChannelClosed { label, .. } = event.unwrap() {
                            b_closed = label == closed_label;
                        }
                    }
                }
            }
        })
        .await
        .expect("the physical channel closes before queued opens are handled");
        for (engine, peer, opens) in [(&mut a, b_id, a_opens), (&mut b, a_id, b_opens)] {
            for event in opens {
                assert!(
                    engine.is_current_event(&event),
                    "closure does not change the incarnation"
                );
                if let EngineEvent::ChannelOpen { label, .. } = event {
                    assert!(!engine.note_channel_open(peer, &label).await,
                        "queued opens must not declare a pair connected after {closed_label} closes");
                }
            }
            assert_eq!(engine.connected_pair_count(), 0);
            engine.remove_peer(peer).await.unwrap();
        }
    }
}

// Regression #1046: queued opens must honor an already observed terminal state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_opens_respect_observed_peer_connection_failure() {
    for initiator in [true, false] {
        for state in [
            RTCPeerConnectionState::Disconnected,
            RTCPeerConnectionState::Failed,
            RTCPeerConnectionState::Closed,
        ] {
            let mut pair = LivePair::connect().await;
            let (engine, peer) = if initiator {
                (&mut pair.sender, pair.receiver_id)
            } else {
                (&mut pair.receiver, pair.sender_id)
            };
            let link = engine.peers.get_mut(&peer).unwrap();
            // Retain real open channels but hold their application-level formation.
            link.open_labels.clear();
            link.pair_connected = false;
            let handler = PeerHandler {
                peer,
                generation: link.generation,
                crippled: false,
                events: engine.events.clone(),
                channel_admission: link.channel_admission.clone(),
                receive_admission: link.receive_admission.clone(),
            };
            handler.on_connection_state_change(state).await;
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                assert!(
                    engine.channel(peer, label).is_some(),
                    "retain handles for cleanup"
                );
                assert_eq!(
                    engine.send_channel(peer, label).await.is_some(),
                    state == RTCPeerConnectionState::Disconnected,
                    "initiator={initiator}, state={state:?}: observed failure fences channel use"
                );
            }
            let mut formed = false;
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                formed |= engine.note_channel_open(peer, label).await;
            }
            assert_eq!(
                formed,
                state == RTCPeerConnectionState::Disconnected,
                "initiator={initiator}, state={state:?}: terminal callback precedes queued opens"
            );
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            pair.receiver.remove_peer(pair.sender_id).await.unwrap();
            pair.negotiate().await;
            assert_eq!(pair.sender.connected_pair_count(), 1);
            assert_eq!(pair.receiver.connected_pair_count(), 1);
            let (fresh_engine, fresh_peer) = if initiator {
                (&mut pair.sender, pair.receiver_id)
            } else {
                (&mut pair.receiver, pair.sender_id)
            };
            let fresh_link = fresh_engine.peers.get_mut(&fresh_peer).unwrap();
            fresh_link.open_labels.clear();
            fresh_link.pair_connected = false;
            handler
                .on_connection_state_change(RTCPeerConnectionState::Failed)
                .await;
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                assert!(fresh_engine.send_channel(fresh_peer, label).await.is_some());
            }
            assert!(
                !fresh_engine
                    .note_channel_open(fresh_peer, RELIABLE_LABEL)
                    .await
            );
            assert!(
                fresh_engine
                    .note_channel_open(fresh_peer, UNRELIABLE_LABEL)
                    .await,
                "retired callbacks cannot terminate a fresh link"
            );
            pair.cleanup().await;
        }
    }
}
