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
                    ..
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

#[tokio::test]
async fn first_channel_closure_survives_later_labels_and_duplicate_reports() {
    let (events, mut received) = mpsc::unbounded_channel();
    let mut engine = Engine::new(
        EngineSettings {
            crippled: true,
            ..EngineSettings::default()
        },
        events,
    )
    .unwrap();
    let peer = PlayerId::from_u128(0x1264);
    engine.pair_with(peer, true, &[]).await.unwrap();
    let link = &engine.peers[&peer];
    let admission = &link.receive_admission;
    admission.report_channel_closed(
        &engine.events,
        peer,
        link.generation,
        RELIABLE_LABEL,
        ChannelCloseReason::CallbackError,
    );
    let first = engine.diagnostic_snapshot(peer)["channel_closure"].clone();
    assert_eq!(
        first,
        serde_json::json!({"label":RELIABLE_LABEL,"reason":"callback_error"})
    );
    admission.report_channel_closed(
        &engine.events,
        peer,
        link.generation,
        RELIABLE_LABEL,
        ChannelCloseReason::StreamEnded,
    );
    admission.report_channel_closed(
        &engine.events,
        peer,
        link.generation,
        UNRELIABLE_LABEL,
        ChannelCloseReason::CallbackClosed,
    );
    let mut observed = Vec::new();
    loop {
        let event = match received.try_recv() {
            Ok(event) => event,
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                panic!("live Engine retains its event sender")
            }
        };
        match event {
            EngineEvent::ChannelClosed {
                label,
                reason,
                generation,
                ..
            } => {
                assert_eq!(generation, link.generation);
                observed.push((label, reason));
            }
            event => assert!(
                matches!(
                    event,
                    EngineEvent::LocalCandidate { .. }
                        | EngineEvent::IceGatheringComplete { .. }
                        | EngineEvent::PcState { .. }
                ),
                "unconnected pair must not discard gameplay or admission events"
            ),
        }
    }
    assert_eq!(
        observed,
        vec![
            (RELIABLE_LABEL.into(), ChannelCloseReason::CallbackError),
            (UNRELIABLE_LABEL.into(), ChannelCloseReason::CallbackClosed)
        ]
    );
    assert_eq!(engine.diagnostic_snapshot(peer)["channel_closure"], first);
    engine.remove_peer(peer).await.unwrap();
}

/// Pending ICE releases its captured core query before retired transports can resume.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retired_links_cancel_pending_ice_and_terminal_results_cannot_commit() {
    use super::snapshot_tests::{startup_pair_with_held_open_events, HeldCoreSend};

    for terminal in [false, true] {
        for receiver_offers in [false, true] {
            let (mut pair, clock, _) =
                startup_pair_with_held_open_events(ChannelProfile::Godot, receiver_offers).await;
            let peer = pair.sender_id;
            let held =
                HeldCoreSend::start(&clock, pair.receiver.channel(peer, RELIABLE_LABEL).unwrap())
                    .await;
            let live = clock.observe_next_task();
            assert!(pair
                .receiver
                .start_remote_candidate(
                    peer,
                    "",
                    Deadline::after(tokio::time::Instant::now(), Duration::from_secs(5))
                )
                .await
                .is_none());
            assert_eq!(live.load(Ordering::SeqCst), 1);
            if terminal {
                pair.receiver.peers[&peer]
                    .receive_admission
                    .terminal
                    .store(true, Ordering::Relaxed);
                assert!(
                    !pair.receiver.accept_candidate_result(peer),
                    "terminal receipt admission cannot accept a late ICE completion"
                );
            } else {
                pair.receiver.remove_peer(peer).await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(1), async {
                while live.load(Ordering::SeqCst) != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("retirement cancels pending ICE captures before the RTC core is released");
            assert!(!pair.receiver.candidate_pending(peer));
            held.finish().await;
            loop {
                match pair.receiver_events.try_recv() {
                    Ok(event) => assert!(
                        !matches!(event, EngineEvent::RemoteCandidateComplete { .. }),
                        "cancelled ICE cannot publish a late completion"
                    ),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        panic!("the retained Engine must own its event sender")
                    }
                }
            }
            pair.receiver.remove_peer(peer).await.unwrap();
        }
    }
}

#[tokio::test]
/// Bound setup ownership and release every worker capture when its Engine drops.
async fn pending_setup_ownership_is_bounded_and_engine_drop_releases_every_worker() {
    let (tx, _events) = mpsc::unbounded_channel();
    let mut engine = Engine::new(EngineSettings::default(), tx).unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    engine.negotiation_hold = Some(NegotiationHold {
        step: "route",
        entered,
        release: release.clone(),
    });
    let deadline = Deadline::after(tokio::time::Instant::now(), Duration::from_secs(30));
    for id in 1..=u8::MAX {
        engine
            .start_negotiation(
                PlayerId::from_u128(u128::from(id)),
                Negotiation::Pair {
                    initiate: true,
                    ice_servers: vec![],
                },
                deadline,
            )
            .await
            .unwrap();
    }
    assert_eq!(engine.pending_negotiations.len(), usize::from(u8::MAX));
    engine
        .start_negotiation(
            PlayerId::from_u128(1),
            Negotiation::Pair {
                initiate: true,
                ice_servers: vec![],
            },
            deadline,
        )
        .await
        .expect("a duplicate pair directive remains idempotent at capacity");
    assert!(engine
        .start_negotiation(
            PlayerId::from_u128(256),
            Negotiation::Pair {
                initiate: true,
                ice_servers: vec![],
            },
            deadline
        )
        .await
        .is_err());
    assert!(!engine.is_paired(PlayerId::from_u128(256)));
    drop(engine);
    tokio::time::timeout(Duration::from_secs(1), async {
        while Arc::strong_count(&release) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Engine drop cancels every owned setup task");
}

#[tokio::test]
/// A shared route wait must release the owner and survive a sibling cancellation.
async fn pending_shared_route_lookup_releases_owner_and_cancels_waiting_peers() {
    let (tx, _events) = mpsc::unbounded_channel();
    let mut engine = Engine::new(EngineSettings::default(), tx).unwrap();
    let cache = engine.ice_route_sources.clone();
    let held_cache = cache.lock().await;
    let departed = PlayerId::from_u128(1);
    let retained = PlayerId::from_u128(2);
    let deadline = Deadline::after(tokio::time::Instant::now(), Duration::from_secs(5));
    for peer in [departed, retained] {
        tokio::time::timeout(
            Duration::from_millis(100),
            engine.start_negotiation(
                peer,
                Negotiation::Pair {
                    initiate: false,
                    ice_servers: vec![IceServer {
                        urls: vec!["stun:127.0.0.1:3478".into()],
                        username: None,
                        credential: None,
                    }],
                },
                deadline,
            ),
        )
        .await
        .expect("a busy shared route lookup cannot retain the owner")
        .unwrap();
        assert!(engine.negotiation_pending(peer));
    }
    engine.remove_peer(departed).await.unwrap();
    drop(held_cache);
    let completion = tokio::time::timeout(Duration::from_secs(2), engine.next_negotiation())
        .await
        .expect("the remaining route waiter completes after cancellation")
        .unwrap();
    assert_eq!(completion.peer, retained);
    completion.result.unwrap();
    assert!(!engine.is_paired(departed));
    assert!(engine.is_paired(retained));
    assert_eq!(
        cache.lock().await.as_ref().unwrap().1,
        vec![IpAddr::from(Ipv4Addr::LOCALHOST)]
    );
    engine.remove_peer(retained).await.unwrap();
}

#[tokio::test]
/// Removal must drop completed peer state before its old notification is read.
async fn peer_removal_drops_completed_setup_before_its_notification_is_consumed() {
    let (tx, _events) = mpsc::unbounded_channel();
    let mut engine = Engine::new(EngineSettings::default(), tx).unwrap();
    let release = Arc::new(tokio::sync::Notify::new());
    engine.negotiation_hold = Some(NegotiationHold {
        step: "offer_local",
        entered: Arc::new(tokio::sync::Notify::new()),
        release: release.clone(),
    });
    let peer = PlayerId::from_u128(1);
    engine
        .start_negotiation(
            peer,
            Negotiation::Pair {
                initiate: true,
                ice_servers: vec![],
            },
            Deadline::after(tokio::time::Instant::now(), Duration::from_secs(5)),
        )
        .await
        .unwrap();
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !engine.setup_worker_finished(peer) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("a completed worker queues its result");
    assert_eq!(
        Arc::strong_count(&release),
        3,
        "retain the actual completed worker"
    );
    engine.remove_peer(peer).await.unwrap();
    assert_eq!(
        Arc::strong_count(&release),
        2,
        "removal must drop completed peer state before consuming any old notification"
    );
    assert!(
        engine.next_negotiation().await.is_none(),
        "the retired result cannot return state"
    );
}

/// Pending and completed setup must release every bound UDP socket on removal or Engine drop.
#[tokio::test]
async fn cancelled_setup_releases_driver_sockets_before_queued_adoption() {
    for completed in [false, true] {
        for drop_engine in [false, true] {
            let (tx, mut events) = mpsc::unbounded_channel();
            let mut engine = Some(
                Engine::new(
                    EngineSettings {
                        disable_mdns: true,
                        ip_family: IpFamily::Ipv4,
                        ..EngineSettings::default()
                    },
                    tx,
                )
                .unwrap(),
            );
            let peer = PlayerId::from_u128(0x1517);
            let release = Arc::new(tokio::sync::Notify::new());
            engine.as_mut().unwrap().negotiation_hold = Some(NegotiationHold {
                step: "sdp_complete",
                entered: Arc::new(tokio::sync::Notify::new()),
                release: release.clone(),
            });
            engine
                .as_mut()
                .unwrap()
                .start_negotiation(
                    peer,
                    Negotiation::Pair {
                        initiate: true,
                        ice_servers: vec![],
                    },
                    Deadline::after(tokio::time::Instant::now(), Duration::from_secs(30)),
                )
                .await
                .unwrap();
            let mut addresses = BTreeSet::new();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match events.recv().await.unwrap() {
                        EngineEvent::LocalCandidate { gathered, .. } => {
                            assert_eq!(gathered.candidate_type, "host");
                            addresses.insert(SocketAddr::new(
                                gathered.address.parse().unwrap(),
                                gathered.port,
                            ));
                        }
                        EngineEvent::IceGatheringComplete { .. } => break,
                        _ => {}
                    }
                }
            })
            .await
            .expect("retain actual bound host candidate addresses");
            assert!(!addresses.is_empty());
            for address in &addresses {
                assert_eq!(tokio::net::UdpSocket::bind(address).await.unwrap_err().kind(), std::io::ErrorKind::AddrInUse,
                    "completed={completed}, drop_engine={drop_engine}: the live worker owns {address}");
            }
            if completed {
                release.notify_one();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !engine.as_ref().unwrap().setup_worker_finished(peer) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("retain completed peer state without adopting its result");
            }
            if drop_engine {
                drop(engine.take());
            } else {
                tokio::time::timeout(
                    Duration::from_millis(100),
                    engine.as_mut().unwrap().remove_peer(peer),
                )
                .await
                .expect("cancellation must release the session owner")
                .unwrap();
            }
            let mut rebound = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), async {
                for address in &addresses {
                    loop {
                        match tokio::net::UdpSocket::bind(address).await {
                            Ok(socket) => { rebound.push(socket); break; }
                            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => tokio::task::yield_now().await,
                            Err(error) => panic!("completed={completed}, drop_engine={drop_engine}: cannot rebind {address}: {error}"),
                        }
                    }
                }
            }).await.expect("cancellation drops the driver and every bound socket, even with an unread completion");
            assert_eq!(rebound.len(), addresses.len());
            release.notify_waiters();
            drop(engine);
        }
    }
}

#[tokio::test]
/// Keep established guarded gameplay available through SDP, loss, and cancellation.
async fn established_pair_stays_connected_during_pending_description() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for holds_offer in [false, true] {
            let settings = EngineSettings {
                channel_profile: profile,
                disable_mdns: true,
                ..EngineSettings::default()
            };
            let (sender_tx, sender_events) = mpsc::unbounded_channel();
            let (receiver_tx, receiver_events) = mpsc::unbounded_channel();
            let mut pair = super::receive_tests::LivePair {
                sender: Engine::new(settings, sender_tx).unwrap(),
                receiver: Engine::new(settings, receiver_tx).unwrap(),
                sender_events,
                receiver_events,
                sender_id: PlayerId::from_u128(0x853),
                receiver_id: PlayerId::from_u128(0x889),
            };
            for physical_generation in 0..4 {
                pair.negotiate().await;
                let remote_offer = pair.sender.peers[&pair.receiver_id]
                    .pc
                    .local_description()
                    .await
                    .unwrap()
                    .sdp;
                let remote_answer = pair.receiver.peers[&pair.sender_id]
                    .pc
                    .local_description()
                    .await
                    .unwrap()
                    .sdp;
                let (engine, peer, operation, step) = if holds_offer {
                    (
                        &mut pair.receiver,
                        pair.sender_id,
                        Negotiation::Offer(remote_offer),
                        "remote_offer",
                    )
                } else {
                    (
                        &mut pair.sender,
                        pair.receiver_id,
                        Negotiation::Answer(remote_answer),
                        "remote_answer",
                    )
                };
                let entered = Arc::new(tokio::sync::Notify::new());
                let release = Arc::new(tokio::sync::Notify::new());
                engine.negotiation_hold = Some(NegotiationHold {
                    step,
                    entered: entered.clone(),
                    release: release.clone(),
                });
                let generation = engine.peers[&peer].generation;
                assert_eq!(engine.connected_pair_count(), 1);
                assert!(engine
                    .start_negotiation(
                        peer,
                        operation,
                        Deadline::after(tokio::time::Instant::now(), Duration::from_secs(10)),
                    )
                    .await
                    .unwrap()
                    .is_none());
                entered.notified().await;
                assert!(engine.is_current_generation(peer, generation));
                assert_eq!(engine.connected_pair_count(), 1,
                    "profile={profile:?} holds_offer={holds_offer} generation={physical_generation}: pending SDP must preserve established readiness");
                exchange_pending_description_gameplay(&mut pair, physical_generation, "held").await;
                if physical_generation >= 2 {
                    let engine = if holds_offer {
                        &mut pair.receiver
                    } else {
                        &mut pair.sender
                    };
                    let captured = Arc::new(());
                    let retained = captured.clone();
                    let started = engine
                        .start_channel_send(
                            peer,
                            RELIABLE_LABEL,
                            "retired pending send".into(),
                            Deadline::after(tokio::time::Instant::now(), Duration::from_secs(10)),
                            move |channel, text| async move {
                                let _retained = retained;
                                std::future::pending::<()>().await;
                                channel.send_text(&text).await
                            },
                        )
                        .await;
                    assert!(started.is_none());
                    assert_eq!(Arc::strong_count(&captured), 2);
                    let channels: Vec<_> = profile
                        .labels()
                        .iter()
                        .map(|label| Arc::downgrade(&engine.channel(peer, label).unwrap()))
                        .collect();
                    if physical_generation == 2 {
                        engine.remove_peer(peer).await.unwrap();
                    } else {
                        let replacement =
                            Engine::new(engine.settings, engine.events.clone()).unwrap();
                        drop(std::mem::replace(engine, replacement));
                    }
                    engine.negotiation_hold = None;
                    assert_eq!(engine.connected_pair_count(), 0);
                    assert!(!engine.is_current_generation(peer, generation));
                    assert!(engine.send_channel(peer, RELIABLE_LABEL).await.is_none());
                    release.notify_one();
                    tokio::time::timeout(Duration::from_secs(1), async {
                        while channels.iter().any(|channel| channel.strong_count() != 0) {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("removing held SDP releases all established channel captures");
                    assert_eq!(
                        Arc::strong_count(&captured),
                        1,
                        "retirement cancels sends started while SDP owns the link"
                    );
                    assert!(engine.take_ready_negotiation().is_none());
                    pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                    pair.receiver.remove_peer(pair.sender_id).await.unwrap();
                    pair.negotiate().await;
                    let engine = if holds_offer {
                        &pair.receiver
                    } else {
                        &pair.sender
                    };
                    assert!(!engine.is_current_generation(peer, generation));
                    assert_eq!(engine.connected_pair_count(), 1);
                    exchange_pending_description_gameplay(
                        &mut pair,
                        physical_generation,
                        "cancelled-recovery",
                    )
                    .await;
                    pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                    pair.receiver.remove_peer(pair.sender_id).await.unwrap();
                    continue;
                }
                if physical_generation == 1 {
                    let (remote, peer) = if holds_offer {
                        (&pair.sender, pair.receiver_id)
                    } else {
                        (&pair.receiver, pair.sender_id)
                    };
                    remote
                        .channel(peer, UNRELIABLE_LABEL)
                        .unwrap()
                        .close()
                        .await
                        .unwrap();
                    let events = if holds_offer {
                        &mut pair.receiver_events
                    } else {
                        &mut pair.sender_events
                    };
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if let EngineEvent::ChannelClosed {
                                label,
                                generation: actual,
                                ..
                            } = events.recv().await.unwrap()
                            {
                                assert_eq!(label, UNRELIABLE_LABEL);
                                assert_eq!(actual, generation);
                                break;
                            }
                        }
                    })
                    .await
                    .expect("actual remote closure must invalidate pending established readiness");
                }
                let engine = if holds_offer {
                    &mut pair.receiver
                } else {
                    &mut pair.sender
                };
                let snapshot = engine.diagnostic_snapshot(peer);
                if physical_generation == 1 {
                    assert_eq!(
                        snapshot["channel_closure"]["label"], UNRELIABLE_LABEL,
                        "pending SDP must retain the actual closure cause"
                    );
                }
                assert_eq!(
                    engine.connected_pair_count(),
                    usize::from(physical_generation != 1)
                );
                assert_eq!(
                    engine.send_channel(peer, RELIABLE_LABEL).await.is_some(),
                    physical_generation != 1
                );
                let send_release = Arc::new(tokio::sync::Notify::new());
                if physical_generation == 0 {
                    let waiting = send_release.clone();
                    let started = engine
                        .start_channel_send(
                            peer,
                            RELIABLE_LABEL,
                            "send survives setup return".into(),
                            Deadline::after(tokio::time::Instant::now(), Duration::from_secs(10)),
                            move |channel, text| async move {
                                waiting.notified().await;
                                channel.send_text(&text).await
                            },
                        )
                        .await;
                    assert!(started.is_none());
                }
                release.notify_one();
                let completion =
                    tokio::time::timeout(Duration::from_secs(5), engine.next_negotiation())
                        .await
                        .unwrap()
                        .unwrap();
                /* Repeated answers may be refused by the signaling-state machine. */
                assert!(engine.is_current_generation(peer, generation));
                assert_eq!(
                    engine.connected_pair_count(),
                    usize::from(physical_generation != 1),
                    "{:?}",
                    completion.result
                );
                engine.negotiation_hold = None;
                if physical_generation == 0 {
                    send_release.notify_one();
                    let (sender, sender_events, receiver_events) = if holds_offer {
                        (
                            &mut pair.receiver,
                            &mut pair.receiver_events,
                            &mut pair.sender_events,
                        )
                    } else {
                        (
                            &mut pair.sender,
                            &mut pair.sender_events,
                            &mut pair.receiver_events,
                        )
                    };
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if let EngineEvent::ChannelSendComplete {
                                peer: actual,
                                label,
                                text,
                                result,
                                ..
                            } = sender_events.recv().await.unwrap()
                            {
                                assert_eq!(actual, peer);
                                assert_eq!(text, "send survives setup return");
                                result.unwrap();
                                assert!(sender.accept_channel_send(peer, &label));
                                break;
                            }
                        }
                        loop {
                            if let EngineEvent::ChannelMessage { text, .. } =
                                receiver_events.recv().await.unwrap()
                            {
                                assert_eq!(text, "send survives setup return");
                                break;
                            }
                        }
                    })
                    .await
                    .expect("setup return retains the pending send and its exact receipt");
                }
                if physical_generation != 1 {
                    exchange_pending_description_gameplay(
                        &mut pair,
                        physical_generation,
                        "restored",
                    )
                    .await;
                }
                pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                pair.receiver.remove_peer(pair.sender_id).await.unwrap();
            }
            pair.cleanup().await;
        }
    }
}

/// Exchange exact reliable gameplay through guarded channels at one setup phase.
async fn exchange_pending_description_gameplay(
    pair: &mut super::receive_tests::LivePair,
    physical_generation: usize,
    phase: &str,
) {
    for sender_offers in [false, true] {
        let prefix = format!("SDP-held-gameplay-{sender_offers}-{physical_generation}-{phase}:");
        let text = prefix.clone() + &"gameplay".repeat(512);
        assert!(text.len() > 4096);
        let mut exact = text.clone();
        exact.truncate(4096);
        assert_eq!(exact.len(), 4096);
        assert!(exact.starts_with(&prefix));
        let (sender, peer, receiver, receiver_events, sender_id, sender_events) = if sender_offers {
            (
                &mut pair.sender,
                pair.receiver_id,
                &mut pair.receiver,
                &mut pair.receiver_events,
                pair.sender_id,
                &mut pair.sender_events,
            )
        } else {
            (
                &mut pair.receiver,
                pair.sender_id,
                &mut pair.sender,
                &mut pair.sender_events,
                pair.receiver_id,
                &mut pair.receiver_events,
            )
        };
        for text in [exact, text] {
            let started = sender
                .start_channel_send(
                    peer,
                    RELIABLE_LABEL,
                    text.clone(),
                    Deadline::after(tokio::time::Instant::now(), Duration::from_secs(5)),
                    |channel, text| async move {
                        tokio::task::yield_now().await;
                        channel.send_text(&text).await
                    },
                )
                .await;
            assert!(
                started.is_none(),
                "the real send suspends under link ownership"
            );
            let duplicate = sender
                .start_channel_send(
                    peer,
                    RELIABLE_LABEL,
                    "duplicate must not send".into(),
                    Deadline::after(tokio::time::Instant::now(), Duration::from_secs(5)),
                    |_, _| async { panic!("one physical label can own only one pending send") },
                )
                .await;
            assert!(duplicate.is_none());
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = sender_events.recv().await.unwrap();
                    let event = if matches!(event, EngineEvent::ChannelSendComplete { .. }) {
                        sender.defer_negotiation_event(event).unwrap().expect(
                            "established send completion must reach the owner during pending SDP",
                        )
                    } else {
                        event
                    };
                    match event {
                        EngineEvent::ChannelSendComplete {
                            peer: actual_peer,
                            generation,
                            label,
                            text: actual,
                            result,
                        } => {
                            assert_eq!(actual_peer, peer);
                            assert!(sender.is_current_generation(peer, generation));
                            assert_eq!(label, RELIABLE_LABEL);
                            assert_eq!(actual, text);
                            result.expect(
                                "production exchange send remains usable through pending SDP",
                            );
                            assert!(sender.accept_channel_send(peer, &label));
                            break;
                        }
                        EngineEvent::ChannelReceiveRejected { .. }
                        | EngineEvent::ChannelClosed { .. } => {
                            panic!("a healthy SDP operation must not close gameplay")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("the physical send publishes its exact completion");
            let received = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match receiver_events.recv().await.unwrap() {
                        event @ EngineEvent::ChannelMessage { .. } => break event,
                        EngineEvent::ChannelReceiveRejected { .. }
                        | EngineEvent::ChannelClosed { .. } => {
                            panic!("held SDP must preserve the live gameplay stream")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("exact reliable gameplay progresses during held SDP");
            assert!(receiver.is_current_event(&received));
            let EngineEvent::ChannelMessage {
                peer,
                label,
                text: actual,
                ..
            } = received
            else {
                unreachable!()
            };
            assert_eq!(peer, sender_id);
            assert_eq!(label, RELIABLE_LABEL);
            assert_eq!(actual, text);
        }
    }
}

/// Pending SDP must not hide actual receive refusal or retained payload credit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_description_snapshot_retains_live_receive_refusal() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for receiver_offers in [false, true] {
            let settings = EngineSettings {
                channel_profile: profile,
                disable_mdns: true,
                ..EngineSettings::default()
            };
            let (sender_tx, sender_events) = mpsc::unbounded_channel();
            let (receiver_tx, receiver_events) = mpsc::unbounded_channel();
            let mut pair = super::receive_tests::LivePair {
                sender: Engine::new(settings, sender_tx).unwrap(),
                receiver: Engine::new(settings, receiver_tx).unwrap(),
                sender_events,
                receiver_events,
                sender_id: PlayerId::from_u128(0x1527),
                receiver_id: PlayerId::from_u128(0x1264),
            };
            pair.negotiate().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            let remote = pair.sender.peers[&pair.receiver_id]
                .pc
                .local_description()
                .await
                .unwrap()
                .sdp;
            let release = Arc::new(tokio::sync::Notify::new());
            let entered = Arc::new(tokio::sync::Notify::new());
            pair.receiver.negotiation_hold = Some(NegotiationHold {
                step: if receiver_offers {
                    "remote_answer"
                } else {
                    "remote_offer"
                },
                entered: entered.clone(),
                release: release.clone(),
            });
            pair.receiver
                .start_negotiation(
                    pair.sender_id,
                    if receiver_offers {
                        Negotiation::Answer(remote)
                    } else {
                        Negotiation::Offer(remote)
                    },
                    Deadline::after(tokio::time::Instant::now(), Duration::from_secs(30)),
                )
                .await
                .unwrap();
            entered.notified().await;
            let channel = pair
                .sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap();
            let mut retained = Vec::new();
            for sequence in 0..MAX_PENDING_CHANNEL_BYTES / MAX_CHANNEL_MESSAGE_BYTES {
                let payload =
                    vec![b'a' + u8::try_from(sequence).unwrap(); MAX_CHANNEL_MESSAGE_BYTES];
                channel.send(payload.as_slice().into()).await.unwrap();
                let event = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let event = pair.receiver_events.recv().await.unwrap();
                        match &event {
                            EngineEvent::ChannelMessage { text, .. } => {
                                assert_eq!(text.as_bytes(), payload);
                                break event;
                            }
                            EngineEvent::ChannelBinaryMessage { data, .. } => {
                                assert_eq!(data, &payload);
                                break event;
                            }
                            EngineEvent::ChannelReceiveRejected { .. }
                            | EngineEvent::ChannelClosed { .. } => {
                                panic!("the exact budget boundary remains admissible")
                            }
                            _ => {}
                        }
                    }
                })
                .await
                .expect("retain each exact receipt before filling the next credit");
                assert!(pair.receiver.is_current_event(&event));
                retained.push(event);
            }
            channel.send_text("overflow").await.unwrap();
            let failure = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match pair.receiver_events.recv().await.unwrap() {
                        EngineEvent::ChannelReceiveRejected { failure, .. } => break failure,
                        EngineEvent::ChannelMessage { .. }
                        | EngineEvent::ChannelBinaryMessage { .. } => {
                            panic!("the exhausted byte budget must refuse the next message")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("the real poller reports byte-credit refusal during held SDP");
            assert_eq!(failure.reason, ReceiveRejection::PendingBytes);
            assert_eq!(failure.message_bytes, 8);
            let snapshot = pair.receiver.diagnostic_snapshot(pair.sender_id);
            assert_eq!(snapshot["negotiation_pending"], true);
            assert_eq!(
                snapshot["receive_failure"],
                serde_json::to_value(failure).unwrap()
            );
            assert_eq!(
                snapshot["channels"][RELIABLE_LABEL]["pending_application"]["bytes"],
                MAX_PENDING_CHANNEL_BYTES
            );
            assert_eq!(
                snapshot["receive_budget"]["pending_bytes"],
                MAX_PENDING_CHANNEL_BYTES
            );
            assert_eq!(pair.receiver.connected_pair_count(), 0);
            assert!(pair
                .receiver
                .send_channel(pair.sender_id, RELIABLE_LABEL)
                .await
                .is_none());
            drop(retained);
            let drained = pair.receiver.diagnostic_snapshot(pair.sender_id);
            assert_eq!(drained["receive_failure"], snapshot["receive_failure"]);
            assert_eq!(drained["receive_budget"]["pending_bytes"], 0);
            assert_eq!(
                drained["channels"][RELIABLE_LABEL]["pending_application"]["messages"],
                0
            );
            pair.receiver.remove_peer(pair.sender_id).await.unwrap();
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            release.notify_one();
            pair.receiver.negotiation_hold = None;
            assert!(pair.receiver.diagnostic_snapshot(pair.sender_id).is_null());
            pair.negotiate().await;
            let fresh = pair.receiver.diagnostic_snapshot(pair.sender_id);
            assert!(fresh["receive_failure"].is_null());
            assert_eq!(fresh["channels"][RELIABLE_LABEL]["receipts"]["raw"], 0);
            exchange_pending_description_gameplay(&mut pair, 1, "after-receive-refusal").await;
            pair.cleanup().await;
        }
    }
}
