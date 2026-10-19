use super::*;

// Regression #905: a stopped WebRTC driver must retire its required channels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ended_channel_streams_publish_one_close_unless_receive_rejected() {
    for close_peer in [true, false] {
        for receive_rejected in [false, true] {
            let mut pair = super::receive_tests::LivePair::connect().await;
            let link = &pair.receiver.peers[&pair.sender_id];
            let generation = link.generation;
            *link.receive_admission.rejected.lock().await = receive_rejected;
            if close_peer {
                // Keep the engine generation present while the stack ends its
                // event streams. This represents an unexpected driver stop.
                link.pc.close().await.unwrap();
            } else {
                for channel in link.channels.values() {
                    channel.close().await.unwrap();
                }
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                while !link.channel_tasks.iter().all(|task| task.is_finished()) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("terminal streams stop both required channel pollers");

            let mut closed = BTreeSet::new();
            loop {
                let event = match pair.receiver_events.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        panic!("the retained receiver engine must retain its event sender")
                    }
                };
                if let EngineEvent::ChannelClosed {
                    peer,
                    generation: event_generation,
                    label,
                } = event
                {
                    assert_eq!(peer, pair.sender_id);
                    assert_eq!(event_generation, generation);
                    assert!(closed.insert(label), "one terminal event per channel");
                }
            }
            assert_eq!(
                closed,
                if receive_rejected {
                    BTreeSet::new()
                } else {
                    BTreeSet::from([RELIABLE_LABEL.to_owned(), UNRELIABLE_LABEL.to_owned()])
                },
                "close_peer={close_peer}, receive_rejected={receive_rejected}"
            );
            pair.cleanup().await;
        }
    }
}

// Regression #874: retired pollers must release channels without stack OnClose.
#[tokio::test]
async fn retired_peer_pollers_release_unconnected_channels() {
    for initiate in [true, false] {
        for drop_engine in [false, true] {
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut engine = Some(
                Engine::new(
                    EngineSettings {
                        crippled: true,
                        ip_family: IpFamily::Ipv4,
                        ..EngineSettings::default()
                    },
                    tx,
                )
                .unwrap(),
            );
            let peer = PlayerId::from_u128(0x870);
            for _ in 0..if drop_engine { 1 } else { 3 } {
                let current = engine.as_mut().unwrap();
                current.pair_with(peer, initiate, &[]).await.unwrap();
                if !initiate {
                    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                        let channel = current.peers[&peer]
                            .pc
                            .create_data_channel(
                                label,
                                Some(RTCDataChannelInit {
                                    ordered: label == RELIABLE_LABEL,
                                    max_retransmits: (label == UNRELIABLE_LABEL).then_some(0),
                                    ..RTCDataChannelInit::default()
                                }),
                            )
                            .await
                            .unwrap();
                        current
                            .store_remote_channel(peer, label.into(), channel)
                            .await;
                    }
                }
                let channels: Vec<_> = [RELIABLE_LABEL, UNRELIABLE_LABEL]
                    .into_iter()
                    .map(|label| Arc::downgrade(&current.channel(peer, label).unwrap()))
                    .collect();
                assert_eq!(current.peers[&peer].channel_tasks.len(), 2);
                tokio::task::yield_now().await;
                if drop_engine {
                    drop(engine.take());
                } else {
                    current.remove_peer(peer).await.unwrap();
                }
                tokio::time::timeout(Duration::from_secs(1), async {
                    while channels.iter().any(|channel| channel.strong_count() != 0) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap_or_else(|_| {
                    panic!("initiate={initiate}, drop_engine={drop_engine}: retired tasks retain channels")
                });
            }
        }
    }
}

// Regression #979: a ready stack operation must not bypass an expired deadline.
#[tokio::test]
async fn ready_negotiation_respects_the_absolute_deadline() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut engine = Engine::new(EngineSettings::default(), tx).unwrap();
    let peer = PlayerId::from_u128(979);
    tokio::time::pause();
    for expired in [false, true] {
        engine.pair_with(peer, false, &[]).await.unwrap();
        let generation = engine.peers[&peer].generation;
        let deadline = Deadline::after(tokio::time::Instant::now(), Duration::from_secs(1));
        if expired {
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        let result = engine
            .negotiate(peer, Negotiation::Candidate(String::new()), deadline)
            .await;
        assert_eq!(result.is_err(), expired, "expired={expired}: {result:?}");
        assert_eq!(engine.is_current_generation(peer, generation), !expired);
        if !expired {
            assert_eq!(engine.peers[&peer].pending_candidates.len(), 1);
        }
        engine.remove_peer(peer).await.unwrap();
    }
}

#[tokio::test]
async fn ready_negotiation_completion_rechecks_the_absolute_deadline() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let mut engine = Engine::new(EngineSettings::default(), tx).unwrap();
    let peer = PlayerId::from_u128(979);
    tokio::time::pause();
    for expired in [false, true] {
        engine.pair_with(peer, false, &[]).await.unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        engine.negotiation_hold = Some(NegotiationHold {
            step: "candidate_complete",
            entered: entered.clone(),
            release: release.clone(),
        });
        let deadline = Deadline::after(tokio::time::Instant::now(), Duration::from_secs(1));
        let mut operation =
            Box::pin(engine.negotiate(peer, Negotiation::Candidate(String::new()), deadline));
        tokio::select! {
            _ = entered.notified() => {}
            result = &mut operation => panic!("candidate did not reach the hold: {result:?}"),
        }
        if expired {
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        release.notify_one();
        let result = operation.await;
        assert_eq!(result.is_err(), expired, "expired={expired}: {result:?}");
        assert_eq!(engine.is_paired(peer), !expired);
        engine.negotiation_hold = None;
        engine.remove_peer(peer).await.unwrap();
    }
}

// Investigation #853/#889: distinguish a rejected negotiation input from a
// poisoned link. Keep the same physical generation through both failure paths.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_negotiation_inputs_preserve_bidirectional_required_channels() {
    for inject_initiator in [false, true] {
        let (a_tx, mut a_events) = mpsc::unbounded_channel();
        let (b_tx, mut b_events) = mpsc::unbounded_channel();
        let settings = EngineSettings {
            disable_mdns: true,
            ..EngineSettings::default()
        };
        let mut a = Engine::new(settings, a_tx).unwrap();
        let mut b = Engine::new(settings, b_tx).unwrap();
        let a_id = PlayerId::from_u128(0x853);
        let b_id = PlayerId::from_u128(0x889);
        let offer = a.pair_with(b_id, true, &[]).await.unwrap().unwrap();
        b.pair_with(a_id, false, &[]).await.unwrap();
        let (injected, peer) = if inject_initiator {
            (&mut a, b_id)
        } else {
            (&mut b, a_id)
        };
        let generation = injected.peers[&peer].generation;
        let deadline = Deadline::after(tokio::time::Instant::now(), Duration::from_secs(10));
        let invalid_sdp = if inject_initiator {
            Negotiation::Answer("invalid SDP".into())
        } else {
            Negotiation::Offer("invalid SDP".into())
        };
        assert!(injected
            .negotiate(peer, invalid_sdp, deadline)
            .await
            .is_err());
        // Invalid trickle input is buffered before SDP, then rejected by the
        // real stack during flush. The subsequent valid SDP must still apply.
        injected
            .handle_remote_candidate(peer, "invalid candidate")
            .await
            .unwrap();
        assert_eq!(injected.peers[&peer].pending_candidates.len(), 1);
        assert!(injected.is_current_generation(peer, generation));
        assert!(!injected.peers[&peer].remote_description_set);
        let answer = b.handle_offer(a_id, offer).await.unwrap();
        a.handle_answer(b_id, answer).await.unwrap();
        let (injected, peer) = if inject_initiator {
            (&mut a, b_id)
        } else {
            (&mut b, a_id)
        };
        assert!(injected.peers[&peer].pending_candidates.is_empty());
        assert!(injected
            .negotiate(
                peer,
                Negotiation::Candidate("invalid candidate".into()),
                deadline
            )
            .await
            .is_err());
        assert!(injected.is_current_generation(peer, generation));
        assert!(!injected.peers[&peer]
            .receive_admission
            .terminal
            .load(Ordering::Relaxed));

        tokio::time::timeout(Duration::from_secs(15), async {
            while a.connected_pair_count() != 1 || b.connected_pair_count() != 1 {
                tokio::select! {
                    event = a_events.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => b.handle_remote_candidate(a_id, &candidate_json).await.unwrap(),
                        EngineEvent::RemoteChannel { label, channel, .. } => a.store_remote_channel(b_id, label, channel).await,
                        EngineEvent::ChannelOpen { label, .. } => { a.note_channel_open(b_id, &label).await; }
                        EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => panic!("initiator loses its required channel after rejected input"),
                        _ => {}
                    },
                    event = b_events.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => a.handle_remote_candidate(b_id, &candidate_json).await.unwrap(),
                        EngineEvent::RemoteChannel { label, channel, .. } => b.store_remote_channel(a_id, label, channel).await,
                        EngineEvent::ChannelOpen { label, .. } => { b.note_channel_open(a_id, &label).await; }
                        EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => panic!("responder loses its required channel after rejected input"),
                        _ => {}
                    },
                }
            }
        }).await.expect("both roles open both channels after rejected SDP and ICE");
        // Reliable ordered traffic brackets the unreliable probe. A missing
        // unreliable receipt alone is not evidence of reliable delivery loss.
        for (sender, peer, events) in [(&a, b_id, &mut b_events), (&b, a_id, &mut a_events)] {
            for (label, text) in [
                (RELIABLE_LABEL, "before"),
                (UNRELIABLE_LABEL, "unreliable"),
                (RELIABLE_LABEL, "after"),
            ] {
                sender
                    .channel(peer, label)
                    .unwrap()
                    .send_text(text)
                    .await
                    .unwrap();
            }
            let mut reliable = Vec::new();
            let mut unreliable = false;
            tokio::time::timeout(Duration::from_secs(5), async {
                while reliable.len() != 2 || !unreliable {
                    match events.recv().await.unwrap() {
                        EngineEvent::ChannelMessage { label, text, .. }
                            if label == RELIABLE_LABEL =>
                        {
                            reliable.push(text)
                        }
                        EngineEvent::ChannelMessage { label, text, .. } => {
                            assert_eq!(label, UNRELIABLE_LABEL);
                            assert_eq!(text, "unreliable");
                            unreliable = true;
                        }
                        EngineEvent::ChannelClosed { .. }
                        | EngineEvent::ChannelReceiveRejected { .. } => {
                            panic!("recovered link loses a required channel")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("both required channels receive after rejected input");
            assert_eq!(reliable, ["before", "after"]);
        }
        a.remove_peer(b_id).await.unwrap();
        b.remove_peer(a_id).await.unwrap();
    }
}
