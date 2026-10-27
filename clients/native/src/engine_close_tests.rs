use super::{receive_tests::LivePair, *};

// Regression #1219: empty-message SCTP padding is not an application byte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_channel_messages_count_zero_application_bytes_through_reset() {
    async fn assert_stats(
        engine: &Engine,
        peer: PlayerId,
        label: &str,
        sending: bool,
        expected: (u32, u64, rtc::data_channel::RTCDataChannelState),
    ) {
        let reports = engine.peers[&peer]
            .pc
            .get_stats(std::time::Instant::now(), StatsSelector::None)
            .await;
        let stats = reports
            .iter()
            .find_map(|entry| match entry {
                RTCStatsReportEntry::DataChannel(stats) if stats.label == label => Some(stats),
                _ => None,
            })
            .expect("live and closed channels retain statistics");
        let (messages, bytes) = if sending {
            (stats.messages_sent, stats.bytes_sent)
        } else {
            (stats.messages_received, stats.bytes_received)
        };
        assert_eq!(
            (messages, bytes, stats.state),
            expected,
            "label={label}, sending={sending}"
        );
    }
    for initiator_sends in [true, false] {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            let mut pair = LivePair::connect().await;
            if !initiator_sends {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            let channel = pair.sender.channel(pair.receiver_id, label).unwrap();
            let mut expected_bytes = 0_u64;
            for (index, (binary, body)) in [(false, ""), (false, "鱼"), (true, ""), (true, "tail")]
                .into_iter()
                .enumerate()
            {
                if binary {
                    channel.send(body.as_bytes().into()).await.unwrap();
                } else {
                    channel.send_text(body).await.unwrap();
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let EngineEvent::ChannelMessage {
                            label: received_label,
                            text,
                            ..
                        } = pair.receiver_events.recv().await.unwrap()
                        {
                            assert_eq!(received_label, label);
                            assert_eq!(text, body);
                            break;
                        }
                    }
                })
                .await
                .expect("each complete message arrives before the next send");
                expected_bytes += u64::try_from(body.len()).unwrap();
                for (endpoint, peer, sending) in [
                    (&pair.sender, pair.receiver_id, true),
                    (&pair.receiver, pair.sender_id, false),
                ] {
                    assert_stats(
                        endpoint,
                        peer,
                        label,
                        sending,
                        (
                            u32::try_from(index + 1).unwrap(),
                            expected_bytes,
                            rtc::data_channel::RTCDataChannelState::Open,
                        ),
                    )
                    .await;
                }
            }
            let remote = pair.receiver.channel(pair.sender_id, label).unwrap();
            channel.close().await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let local_removed = matches!(
                        channel.ready_state().await,
                        Err(webrtc::error::Error::ErrDataChannelClosed)
                    );
                    let remote_removed = matches!(
                        remote.ready_state().await,
                        Err(webrtc::error::Error::ErrDataChannelClosed)
                    );
                    if local_removed && remote_removed {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("reciprocal reset retires both channel handles");
            for (endpoint, peer, sending) in [
                (&pair.sender, pair.receiver_id, true),
                (&pair.receiver, pair.sender_id, false),
            ] {
                assert_stats(
                    endpoint,
                    peer,
                    label,
                    sending,
                    (4, 7, rtc::data_channel::RTCDataChannelState::Closed),
                )
                .await;
            }
            pair.cleanup().await;
        }
    }
}

// Regression #1214: completed SCTP reset must preserve idempotent cleanup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_channel_reset_keeps_public_close_idempotent() {
    for initiator_closes in [true, false] {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            let pair = LivePair::connect().await;
            let local = pair.sender.channel(pair.receiver_id, label).unwrap();
            let remote = pair.receiver.channel(pair.sender_id, label).unwrap();
            let closer = if initiator_closes { &local } else { &remote };
            closer.close().await.unwrap();
            closer.close().await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let local_removed = matches!(
                        local.ready_state().await,
                        Err(webrtc::error::Error::ErrDataChannelClosed)
                    );
                    let remote_removed = matches!(
                        remote.ready_state().await,
                        Err(webrtc::error::Error::ErrDataChannelClosed)
                    );
                    if local_removed && remote_removed {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("reciprocal reset removes both completed core channels");
            for channel in [&local, &remote] {
                channel.close().await.expect("closed handles close again");
                channel.close().await.expect("repeated cleanup is a no-op");
                assert_channel_closed(channel.send_text("after-reset").await);
                assert_channel_closed(channel.send(b"after-reset".as_slice().into()).await);
                assert_channel_closed(channel.writable().await);
            }
            pair.cleanup().await;
            for channel in [&local, &remote] {
                channel
                    .close()
                    .await
                    .expect("peer cleanup keeps close idempotent");
            }
        }
    }
}

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

// Regression #1188: delayed channel admission must retain its terminal callback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_remote_channel_admission_preserves_accepted_input_and_close() {
    for (held_label, handle_queued_opens) in [
        (RELIABLE_LABEL, false),
        (UNRELIABLE_LABEL, false),
        (RELIABLE_LABEL, true),
        (UNRELIABLE_LABEL, true),
    ] {
        let (sender_tx, mut sender_events) = mpsc::unbounded_channel();
        let (receiver_tx, mut receiver_events) = mpsc::unbounded_channel();
        let settings = EngineSettings {
            disable_mdns: true,
            ..EngineSettings::default()
        };
        let mut sender = Engine::new(settings, sender_tx).unwrap();
        let mut receiver = Engine::new(settings, receiver_tx).unwrap();
        let sender_id = PlayerId::from_u128(0x1187);
        let receiver_id = PlayerId::from_u128(0x1188);
        let offer = sender
            .pair_with(receiver_id, true, &[])
            .await
            .unwrap()
            .unwrap();
        receiver.pair_with(sender_id, false, &[]).await.unwrap();
        let answer = receiver.handle_offer(sender_id, offer).await.unwrap();
        sender.handle_answer(receiver_id, answer).await.unwrap();
        let held = tokio::time::timeout(Duration::from_secs(15), async {
            let mut held = None;
            let mut sender_open = BTreeSet::new();
            let mut sibling_open = false;
            while held.is_none() || sender_open.len() != 2 || !sibling_open {
                tokio::select! {
                    event = sender_events.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            receiver.handle_remote_candidate(sender_id, &candidate_json).await.unwrap();
                        }
                        EngineEvent::ChannelOpen { label, .. } => { sender_open.insert(label); }
                        _ => {}
                    },
                    event = receiver_events.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            sender.handle_remote_candidate(receiver_id, &candidate_json).await.unwrap();
                        }
                        EngineEvent::RemoteChannel { label, channel, .. } => {
                            if label == held_label { held = Some(channel); }
                            else { receiver.store_remote_channel(sender_id, label, channel).await; }
                        }
                        EngineEvent::ChannelOpen { label, .. } => {
                            assert_ne!(label, held_label);
                            assert!(!receiver.note_channel_open(sender_id, &label).await);
                            sibling_open = true;
                        }
                        _ => {}
                    },
                }
            }
            held.unwrap()
        }).await.expect("both real channels open before the announcement drain");
        let channel = sender.channel(receiver_id, held_label).unwrap();
        channel
            .send_text("accepted-before-delayed-admission")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while channel.outstanding_bytes().await.unwrap() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("peer acknowledges the admitted input");
        channel.close().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !matches!(
                held.ready_state().await,
                Err(_) | Ok(rtc::data_channel::RTCDataChannelState::Closed)
            ) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("remote channel retires while its announcement waits");
        receiver
            .store_remote_channel(sender_id, held_label.into(), held)
            .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut messages = Vec::new();
            loop {
                match receiver_events.recv().await.unwrap() {
                    EngineEvent::ChannelMessage { label, text, .. } => {
                        assert_eq!(label, held_label);
                        assert_eq!(text, "accepted-before-delayed-admission");
                        messages.push(text);
                        assert_eq!(messages.len(), 1, "one accepted input cannot duplicate");
                    }
                    EngineEvent::ChannelClosed { label, .. } if label == held_label => {
                        if !handle_queued_opens {
                            assert_eq!(messages, ["accepted-before-delayed-admission"]);
                        }
                        break;
                    }
                    EngineEvent::ChannelOpen { label, .. } if handle_queued_opens => {
                        assert!(!receiver.note_channel_open(sender_id, &label).await);
                    }
                    EngineEvent::ChannelReceiveRejected { .. } => {
                        panic!("valid input must survive delayed admission")
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap_or_else(|error| panic!("{held_label}: queued input and close must drain: {error}"));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !receiver.peers[&sender_id]
                .channel_tasks
                .last()
                .unwrap()
                .is_finished()
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retired channel releases its poller");
        assert_eq!(receiver.connected_pair_count(), 0);
        loop {
            match receiver_events.try_recv() {
                Ok(event) => assert!(
                    !matches!(event, EngineEvent::ChannelClosed { label, .. } if label == held_label),
                    "the selected retired channel closes exactly once"
                ),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    panic!("live engines retain their event senders");
                }
            }
        }
        let old_generation = receiver.peers[&sender_id].generation;
        sender.remove_peer(receiver_id).await.unwrap();
        receiver.remove_peer(sender_id).await.unwrap();
        let mut pair = LivePair {
            sender,
            receiver,
            sender_events,
            receiver_events,
            sender_id,
            receiver_id,
        };
        pair.negotiate().await;
        assert_ne!(pair.receiver.peers[&sender_id].generation, old_generation);
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            pair.sender
                .channel(receiver_id, label)
                .unwrap()
                .send_text("fresh-after-delayed-close")
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match pair.receiver_events.recv().await.unwrap() {
                        EngineEvent::ChannelMessage {
                            label: received,
                            text,
                            ..
                        } => {
                            assert_eq!(received, label);
                            assert_eq!(text, "fresh-after-delayed-close");
                            break;
                        }
                        EngineEvent::ChannelClosed { .. }
                        | EngineEvent::ChannelReceiveRejected { .. } => {
                            panic!("fresh generation must deliver on both required channels");
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("fresh generation delivers after delayed closure");
        }
        pair.cleanup().await;
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

type RuntimeFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[derive(Debug)]
struct ResetReactorTiming {
    epoch: std::time::Instant,
    last_poll_started_ns: AtomicU64,
    last_poll_finished_ns: AtomicU64,
    last_wake_ns: AtomicU64,
    longest_poll_ns: AtomicU64,
}

impl Default for ResetReactorTiming {
    fn default() -> Self {
        Self {
            epoch: std::time::Instant::now(),
            last_poll_started_ns: AtomicU64::new(0),
            last_poll_finished_ns: AtomicU64::new(0),
            last_wake_ns: AtomicU64::new(0),
            longest_poll_ns: AtomicU64::new(0),
        }
    }
}

impl ResetReactorTiming {
    fn elapsed_ns(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

#[derive(Debug, Default)]
struct ResetPacketProgress {
    timing: ResetReactorTiming,
    sent: AtomicU64,
    application_sent: AtomicU64,
    received: AtomicU64,
    dropped_datagrams: AtomicU64,
    dropped_transmits: AtomicU64,
    drop_next_application: AtomicBool,
    reactor_polls: AtomicU64,
    reactor_pending_polls: AtomicU64,
    reactor_completions: AtomicU64,
    reactor_wakes: AtomicU64,
    reactor_wakes_at_last_poll: AtomicU64,
    paused_polls: AtomicU64,
}

impl ResetPacketProgress {
    /* Independent loads can overlap reactor work; these are not causal verdicts. */
    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "elapsed_ns": self.timing.elapsed_ns(),
            "last_poll_started_ns": self.timing.last_poll_started_ns.load(Ordering::SeqCst),
            "last_poll_finished_ns": self.timing.last_poll_finished_ns.load(Ordering::SeqCst),
            "last_wake_ns": self.timing.last_wake_ns.load(Ordering::SeqCst),
            "longest_poll_ns": self.timing.longest_poll_ns.load(Ordering::SeqCst),
            "sent": self.sent.load(Ordering::SeqCst),
            "application_sent": self.application_sent.load(Ordering::SeqCst),
            "received": self.received.load(Ordering::SeqCst),
            "dropped_datagrams": self.dropped_datagrams.load(Ordering::SeqCst),
            "dropped_transmits": self.dropped_transmits.load(Ordering::SeqCst),
            "reactor_polls": self.reactor_polls.load(Ordering::SeqCst),
            "reactor_pending_polls": self.reactor_pending_polls.load(Ordering::SeqCst),
            "reactor_completions": self.reactor_completions.load(Ordering::SeqCst),
            "reactor_wakes": self.reactor_wakes.load(Ordering::SeqCst),
            "reactor_wakes_at_last_poll": self.reactor_wakes_at_last_poll.load(Ordering::SeqCst),
            "paused_polls": self.paused_polls.load(Ordering::SeqCst),
        })
    }
}

#[derive(Debug)]
struct ResetUdpSocket {
    inner: Arc<dyn webrtc::runtime::AsyncUdpSocket>,
    progress: Arc<ResetPacketProgress>,
}

impl webrtc::runtime::AsyncUdpSocket for ResetUdpSocket {
    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn poll_send(
        &self,
        cx: &mut std::task::Context<'_>,
        transmit: &webrtc::runtime::Transmit<'_>,
    ) -> std::task::Poll<std::io::Result<usize>> {
        /* DTLS application records contain encrypted SCTP. Their chunk type is unknown. */
        let datagrams = u64::try_from(
            transmit.contents.len().div_ceil(
                transmit
                    .segment_size
                    .unwrap_or(transmit.contents.len())
                    .max(1),
            ),
        )
        .unwrap();
        if transmit.contents.first() == Some(&23)
            && self
                .progress
                .drop_next_application
                .swap(false, Ordering::SeqCst)
        {
            self.progress
                .dropped_datagrams
                .fetch_add(datagrams, Ordering::SeqCst);
            self.progress
                .dropped_transmits
                .fetch_add(1, Ordering::SeqCst);
            return std::task::Poll::Ready(Ok(transmit.contents.len()));
        }
        let result = self.inner.poll_send(cx, transmit);
        if matches!(result, std::task::Poll::Ready(Ok(_))) {
            self.progress.sent.fetch_add(datagrams, Ordering::SeqCst);
            if transmit.contents.first() == Some(&23) {
                self.progress
                    .application_sent
                    .fetch_add(datagrams, Ordering::SeqCst);
            }
        }
        result
    }

    fn poll_recv(
        &self,
        cx: &mut std::task::Context<'_>,
        buffers: &mut [std::io::IoSliceMut<'_>],
        metadata: &mut [webrtc::runtime::RecvMeta],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let result = self.inner.poll_recv(cx, buffers, metadata);
        if let std::task::Poll::Ready(Ok(count)) = result {
            let datagrams: usize = metadata
                .iter()
                .take(count)
                .map(|message| message.len.div_ceil(message.stride.max(1)))
                .sum();
            self.progress
                .received
                .fetch_add(u64::try_from(datagrams).unwrap(), Ordering::SeqCst);
        }
        result
    }

    fn max_gso_segments(&self) -> usize {
        self.inner.max_gso_segments()
    }

    fn max_gro_segments(&self) -> usize {
        self.inner.max_gro_segments()
    }
}

#[tokio::test]
async fn reset_packet_injector_preserves_control_traffic_and_drops_application_once() {
    use webrtc::runtime::AsyncUdpSocket;

    let progress = Arc::new(ResetPacketProgress::default());
    let socket = ResetUdpSocket {
        inner: default_runtime()
            .unwrap()
            .wrap_udp_socket(std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
            .unwrap(),
        progress: progress.clone(),
    };
    let remote = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    progress.drop_next_application.store(true, Ordering::SeqCst);
    for packet in [&[0, 1][..], &[22, 2], &[23, 3], &[23, 4]] {
        assert_eq!(
            socket
                .send_to(packet, remote.local_addr().unwrap())
                .await
                .unwrap(),
            packet.len()
        );
    }
    let mut buffer = [0; 64];
    for expected in [&[0, 1][..], &[22, 2], &[23, 4]] {
        let (length, _) =
            tokio::time::timeout(Duration::from_secs(1), remote.recv_from(&mut buffer))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(&buffer[..length], expected);
    }
    remote
        .send_to(&[23, 5], socket.local_addr().unwrap())
        .await
        .unwrap();
    let (length, _) = tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buffer[..length], &[23, 5]);
    let snapshot = progress.snapshot();
    assert_eq!(snapshot["sent"], 3);
    assert_eq!(snapshot["received"], 1);
    assert_eq!(snapshot["dropped_datagrams"], 1);
    assert_eq!(snapshot["dropped_transmits"], 1);

    progress.drop_next_application.store(true, Ordering::SeqCst);
    let batch = webrtc::runtime::Transmit {
        destination: remote.local_addr().unwrap(),
        ecn: None,
        contents: &[23; 9],
        segment_size: Some(4),
        src_ip: None,
    };
    assert_eq!(
        std::future::poll_fn(|cx| socket.poll_send(cx, &batch))
            .await
            .unwrap(),
        9
    );
    let snapshot = progress.snapshot();
    assert_eq!(snapshot["dropped_transmits"], 2);
    assert_eq!(snapshot["dropped_datagrams"], 4);
    assert_eq!(snapshot["sent"], 3);
}

#[derive(Debug)]
struct ResetReactorWake {
    task: Arc<std::sync::Mutex<Option<std::task::Waker>>>,
    progress: Arc<ResetPacketProgress>,
}

/* Count only wakes from the wrapped driver, not fixture pause/resume wakes. */
impl std::task::Wake for ResetReactorWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.progress
            .timing
            .last_wake_ns
            .fetch_max(self.progress.timing.elapsed_ns(), Ordering::SeqCst);
        self.progress.reactor_wakes.fetch_add(1, Ordering::SeqCst);
        let task = self.task.lock().unwrap().clone();
        if let Some(task) = task {
            task.wake();
        }
    }
}

#[derive(Debug)]
struct PausableReactor {
    inner: Arc<dyn Runtime>,
    paused: Arc<AtomicBool>,
    reactor_selected: AtomicBool,
    dedicated: bool,
    waker: Arc<std::sync::Mutex<Option<std::task::Waker>>>,
    held: Arc<tokio::sync::Notify>,
    progress: Arc<ResetPacketProgress>,
}

impl PausableReactor {
    fn new() -> Arc<Self> {
        Self::with_dedicated_reactor(true)
    }

    fn with_dedicated_reactor(dedicated: bool) -> Arc<Self> {
        Arc::new(Self {
            inner: default_runtime().unwrap(),
            paused: Arc::new(AtomicBool::new(false)),
            reactor_selected: AtomicBool::new(false),
            dedicated,
            waker: Arc::new(std::sync::Mutex::new(None)),
            held: Arc::new(tokio::sync::Notify::new()),
            progress: Arc::new(ResetPacketProgress::default()),
        })
    }

    fn reactor_future(&self, mut future: RuntimeFuture<'static, ()>) -> RuntimeFuture<'static, ()> {
        let paused = self.paused.clone();
        let waker = self.waker.clone();
        let held = self.held.clone();
        let progress = self.progress.clone();
        let driver_waker = std::task::Waker::from(Arc::new(ResetReactorWake {
            task: waker.clone(),
            progress: progress.clone(),
        }));
        Box::pin(std::future::poll_fn(move |cx| {
            *waker.lock().unwrap() = Some(cx.waker().clone());
            if paused.load(Ordering::SeqCst) {
                progress.paused_polls.fetch_add(1, Ordering::SeqCst);
                held.notify_one();
                return std::task::Poll::Pending;
            }
            progress.reactor_wakes_at_last_poll.store(
                progress.reactor_wakes.load(Ordering::SeqCst),
                Ordering::SeqCst,
            );
            let poll_started = progress.timing.elapsed_ns();
            progress
                .timing
                .last_poll_started_ns
                .store(poll_started, Ordering::SeqCst);
            progress.reactor_polls.fetch_add(1, Ordering::SeqCst);
            let result = future
                .as_mut()
                .poll(&mut std::task::Context::from_waker(&driver_waker));
            let poll_finished = progress.timing.elapsed_ns();
            progress
                .timing
                .last_poll_finished_ns
                .store(poll_finished, Ordering::SeqCst);
            progress
                .timing
                .longest_poll_ns
                .fetch_max(poll_finished.saturating_sub(poll_started), Ordering::SeqCst);
            if result.is_pending() {
                progress
                    .reactor_pending_polls
                    .fetch_add(1, Ordering::SeqCst);
            } else {
                progress.reactor_completions.fetch_add(1, Ordering::SeqCst);
            }
            result
        }))
    }

    async fn pause(&self) {
        let previous_hold = self.progress.paused_polls.load(Ordering::SeqCst);
        self.paused.store(true, Ordering::SeqCst);
        self.waker.lock().unwrap().as_ref().unwrap().wake_by_ref();
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.progress.paused_polls.load(Ordering::SeqCst) == previous_hold {
                self.held.notified().await;
            }
        })
        .await
        .expect("the reactor acknowledges this hold");
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.waker.lock().unwrap().as_ref().unwrap().wake_by_ref();
    }
}

// #1120: distinguish a pending driver from an unscheduled wake or terminal driver.
#[test]
fn reset_reactor_evidence_distinguishes_pending_woken_and_finished() {
    struct TaskWake(AtomicU64);
    impl std::task::Wake for TaskWake {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let runtime = PausableReactor::with_dedicated_reactor(false);
    let task = Arc::new(TaskWake(AtomicU64::new(0)));
    let task_waker = std::task::Waker::from(task.clone());
    let mut context = std::task::Context::from_waker(&task_waker);
    let child_waker = Arc::new(std::sync::Mutex::new(None));
    let captured = child_waker.clone();
    let progress_in_poll = runtime.progress.clone();
    let mut driver = runtime.reactor_future(Box::pin(std::future::poll_fn(move |cx| {
        *captured.lock().unwrap() = Some(cx.waker().clone());
        let during_poll = progress_in_poll.snapshot();
        assert!(
            during_poll["last_poll_started_ns"].as_u64().unwrap()
                >= during_poll["last_poll_finished_ns"].as_u64().unwrap()
        );
        assert!(
            during_poll["reactor_polls"].as_u64().unwrap()
                > during_poll["reactor_pending_polls"].as_u64().unwrap()
                    + during_poll["reactor_completions"].as_u64().unwrap()
        );
        std::task::Poll::Pending
    })));
    assert!(driver.as_mut().poll(&mut context).is_pending());
    let pending = runtime.progress.snapshot();
    assert_eq!(pending["reactor_polls"], 1);
    assert_eq!(pending["reactor_pending_polls"], 1);
    assert_eq!(pending["reactor_wakes"], 0);
    assert_eq!(pending["reactor_completions"], 0);
    let poll_started = pending["last_poll_started_ns"].as_u64().unwrap();
    let poll_finished = pending["last_poll_finished_ns"].as_u64().unwrap();
    assert!(poll_finished >= poll_started);
    assert_eq!(pending["longest_poll_ns"], poll_finished - poll_started);
    assert_eq!(pending["last_wake_ns"], 0);

    let saved_child_waker = child_waker.lock().unwrap().as_ref().unwrap().clone();
    saved_child_waker.wake_by_ref();
    let unscheduled = runtime.progress.snapshot();
    assert_eq!(unscheduled["reactor_polls"], pending["reactor_polls"]);
    assert_eq!(unscheduled["reactor_wakes"], 1);
    assert_eq!(unscheduled["reactor_wakes_at_last_poll"], 0);
    assert!(unscheduled["last_wake_ns"].as_u64().unwrap() >= poll_finished);
    assert_eq!(unscheduled["last_poll_started_ns"], poll_started);
    assert_eq!(unscheduled["last_poll_finished_ns"], poll_finished);
    assert_eq!(
        task.0.load(Ordering::SeqCst),
        1,
        "the original task is woken"
    );

    runtime.paused.store(true, Ordering::SeqCst);
    assert!(driver.as_mut().poll(&mut context).is_pending());
    let held = runtime.progress.snapshot();
    assert_eq!(held["paused_polls"], 1);
    assert_eq!(held["reactor_polls"], 1, "the held driver is not polled");
    assert_eq!(held["reactor_pending_polls"], 1);
    assert_eq!(held["last_poll_started_ns"], poll_started);
    assert_eq!(held["last_poll_finished_ns"], poll_finished);
    runtime.paused.store(false, Ordering::SeqCst);
    let replacement_task = Arc::new(TaskWake(AtomicU64::new(0)));
    let replacement_waker = std::task::Waker::from(replacement_task.clone());
    let mut replacement_context = std::task::Context::from_waker(&replacement_waker);
    assert!(driver.as_mut().poll(&mut replacement_context).is_pending());
    assert_eq!(runtime.progress.snapshot()["reactor_pending_polls"], 2);
    assert_eq!(runtime.progress.snapshot()["reactor_wakes_at_last_poll"], 1);
    saved_child_waker.wake_by_ref();
    assert_eq!(task.0.load(Ordering::SeqCst), 1, "do not wake the old task");
    assert_eq!(replacement_task.0.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.progress.snapshot()["reactor_wakes"], 2);

    let mut completed = runtime.reactor_future(Box::pin(async {}));
    assert!(completed.as_mut().poll(&mut context).is_ready());
    let finished = runtime.progress.snapshot();
    assert_eq!(finished["reactor_polls"], 3);
    assert_eq!(finished["reactor_pending_polls"], 2);
    assert_eq!(finished["reactor_completions"], 1);
    assert!(
        finished["last_poll_finished_ns"].as_u64().unwrap()
            >= finished["last_poll_started_ns"].as_u64().unwrap()
    );
    assert!(finished["longest_poll_ns"].as_u64().unwrap() >= poll_finished - poll_started);
}

impl Runtime for PausableReactor {
    fn spawn(
        &self,
        future: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    ) -> Box<dyn JoinHandle> {
        if !self.reactor_selected.swap(true, Ordering::Relaxed) {
            if self.dedicated {
                self.spawn_reactor(0, future)
            } else {
                self.inner.spawn(self.reactor_future(future))
            }
        } else {
            self.inner.spawn(future)
        }
    }

    fn spawn_reactor(
        &self,
        pool: usize,
        future: RuntimeFuture<'static, ()>,
    ) -> Box<dyn JoinHandle> {
        self.inner.spawn_reactor(pool, self.reactor_future(future))
    }

    fn wrap_udp_socket(
        &self,
        socket: std::net::UdpSocket,
    ) -> std::io::Result<Arc<dyn webrtc::runtime::AsyncUdpSocket>> {
        Ok(Arc::new(ResetUdpSocket {
            inner: self.inner.wrap_udp_socket(socket)?,
            progress: self.progress.clone(),
        }))
    }
    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> std::io::Result<Arc<dyn webrtc::runtime::AsyncTcpListener>> {
        self.inner.wrap_tcp_listener(listener)
    }
    fn connect_tcp<'a>(
        &'a self,
        address: SocketAddr,
    ) -> RuntimeFuture<'a, std::io::Result<Arc<dyn webrtc::runtime::AsyncTcpStream>>> {
        self.inner.connect_tcp(address)
    }
    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> RuntimeFuture<'a, std::io::Result<Vec<SocketAddr>>> {
        self.inner.resolve_host(host)
    }
    fn sleep(
        &self,
        duration: Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        self.inner.sleep(duration)
    }
    fn interval(&self, duration: Duration) -> Box<dyn webrtc::runtime::AsyncInterval> {
        self.inner.interval(duration)
    }
    fn block_on(&self, future: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>>) {
        self.inner.block_on(future);
    }
}

// Regression #1071: local closing must retire the pair before reciprocal reset.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_channel_closing_retires_pair_before_remote_reset() {
    for initiator_closes in [true, false] {
        for closed_label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            let (sender_tx, sender_events) = mpsc::unbounded_channel();
            let (receiver_tx, receiver_events) = mpsc::unbounded_channel();
            let settings = EngineSettings {
                disable_mdns: true,
                ..EngineSettings::default()
            };
            let remote_runtime = PausableReactor::new();
            let mut sender = Engine::new(settings, sender_tx).unwrap();
            let mut receiver = Engine::new(settings, receiver_tx).unwrap();
            if initiator_closes {
                receiver.runtime = remote_runtime.clone();
            } else {
                sender.runtime = remote_runtime.clone();
            }
            let mut pair = LivePair {
                sender,
                receiver,
                sender_events,
                receiver_events,
                sender_id: PlayerId::from_u128(0x826),
                receiver_id: PlayerId::from_u128(0x827),
            };
            pair.negotiate().await;
            if !initiator_closes {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            remote_runtime.pause().await;
            let generation = pair.sender.peers[&pair.receiver_id].generation;
            let channel = pair.sender.channel(pair.receiver_id, closed_label).unwrap();
            channel.close().await.unwrap();
            channel.close().await.unwrap();
            assert_eq!(
                channel.ready_state().await.unwrap(),
                rtc::data_channel::RTCDataChannelState::Closing
            );
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let EngineEvent::ChannelClosed {
                        peer,
                        generation: event_generation,
                        label,
                    } = pair.sender_events.recv().await.unwrap()
                    {
                        assert_eq!(
                            (peer, event_generation, label.as_str()),
                            (pair.receiver_id, generation, closed_label)
                        );
                        break;
                    }
                }
            })
            .await
            .expect("local close publishes pair loss while the remote reset is held");
            assert_eq!(
                channel.ready_state().await.unwrap(),
                rtc::data_channel::RTCDataChannelState::Closing
            );
            assert!(pair.sender.peers[&pair.receiver_id]
                .receive_admission
                .terminal
                .load(Ordering::Relaxed));
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                assert!(pair
                    .sender
                    .send_channel(pair.receiver_id, label)
                    .await
                    .is_none());
            }
            remote_runtime.resume();
            let sibling_label = if closed_label == RELIABLE_LABEL {
                UNRELIABLE_LABEL
            } else {
                RELIABLE_LABEL
            };
            let sibling = pair
                .receiver
                .channel(pair.sender_id, sibling_label)
                .unwrap();
            sibling.send_text("after-local-close").await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while sibling.outstanding_bytes().await.unwrap() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the sibling probe reaches SCTP before its reset");
            sibling.close().await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !pair.sender.peers[&pair.receiver_id]
                    .channel_tasks
                    .iter()
                    .all(|task| task.is_finished())
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("both local channel producers finish");
            let mut sibling_closes = 0;
            loop {
                let event = match pair.sender_events.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        panic!("the retained engine must keep its event sender")
                    }
                };
                match event {
                    EngineEvent::ChannelClosed { label, .. } => {
                        assert_eq!(
                            label, sibling_label,
                            "the closing channel must not publish twice"
                        );
                        sibling_closes += 1;
                    }
                    EngineEvent::ChannelMessage { .. } => {
                        panic!("closing fences later sibling input")
                    }
                    _ => {}
                }
            }
            assert_eq!(sibling_closes, 1);
            pair.cleanup().await;
        }
    }
}

// Regression #1076: retain immediate send/close while the receiver cannot progress.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn immediate_send_then_close_with_remote_reactor_paused() {
    for initiator_sends in [true, false] {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            for acknowledged in [false, true] {
                let (sender_tx, sender_events) = mpsc::unbounded_channel();
                let (receiver_tx, receiver_events) = mpsc::unbounded_channel();
                let settings = EngineSettings {
                    disable_mdns: true,
                    ..EngineSettings::default()
                };
                let remote_runtime = PausableReactor::new();
                let mut sender = Engine::new(settings, sender_tx).unwrap();
                let mut receiver = Engine::new(settings, receiver_tx).unwrap();
                if initiator_sends {
                    receiver.runtime = remote_runtime.clone();
                } else {
                    sender.runtime = remote_runtime.clone();
                }
                let mut pair = LivePair {
                    sender,
                    receiver,
                    sender_events,
                    receiver_events,
                    sender_id: PlayerId::from_u128(0x826),
                    receiver_id: PlayerId::from_u128(0x827),
                };
                pair.negotiate().await;
                if !initiator_sends {
                    std::mem::swap(&mut pair.sender, &mut pair.receiver);
                    std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                    std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                }
                let channel = pair.sender.channel(pair.receiver_id, label).unwrap();
                if !acknowledged {
                    remote_runtime.pause().await;
                }
                channel.send_text("final-before-reset").await.unwrap();
                if acknowledged {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while channel.outstanding_bytes().await.unwrap() != 0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("control reaches transport acknowledgment before closing");
                    remote_runtime.pause().await;
                }
                assert_eq!(
                    channel.outstanding_bytes().await.unwrap(),
                    if acknowledged { 0 } else { 18 },
                    "paused receiver cannot acknowledge pending payload"
                );
                channel.close().await.unwrap();
                channel.close().await.unwrap();
                assert!(channel.send_text("after-close").await.is_err());
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let EngineEvent::ChannelClosed { label: closed, .. } =
                            pair.sender_events.recv().await.unwrap()
                        {
                            assert_eq!(closed, label);
                            break;
                        }
                    }
                })
                .await
                .expect("local closing notification does not wait for the receiver");
                remote_runtime.resume();
                tokio::time::timeout(Duration::from_secs(5), async {
                    let mut received = 0;
                    loop {
                        match pair.receiver_events.recv().await.unwrap() {
                            EngineEvent::ChannelMessage { text, label: received_label, .. } => {
                                assert_eq!(received_label, label);
                                assert_eq!(text, "final-before-reset");
                                received += 1;
                            }
                            EngineEvent::ChannelClosed { label: closed, .. } if closed == label => {
                                assert_eq!(received, 1, "initiator={initiator_sends}, label={label}, acked={acknowledged}: accepted input precedes close");
                                break;
                            }
                            _ => {}
                        }
                    }
                }).await.expect("remote reset follows accepted input");
                let reports = pair.receiver.peers[&pair.sender_id]
                    .pc
                    .get_stats(std::time::Instant::now(), StatsSelector::None)
                    .await;
                let summary = reports
                    .iter()
                    .find_map(|entry| match entry {
                        RTCStatsReportEntry::DataChannel(channel) if channel.label == label => {
                            Some(channel)
                        }
                        _ => None,
                    })
                    .expect("closed summary remains available");
                assert_eq!(summary.messages_received, 1);
                assert_eq!(summary.bytes_received, 18);
                assert_eq!(
                    summary.state,
                    rtc::data_channel::RTCDataChannelState::Closed
                );
                pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                pair.receiver.remove_peer(pair.sender_id).await.unwrap();
                pair.negotiate().await;
                for fresh_label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                    pair.sender
                        .channel(pair.receiver_id, fresh_label)
                        .unwrap()
                        .send_text("fresh-after-close")
                        .await
                        .unwrap();
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if let EngineEvent::ChannelMessage { label, text, .. } =
                                pair.receiver_events.recv().await.unwrap()
                            {
                                assert_eq!(label, fresh_label);
                                assert_eq!(text, "fresh-after-close");
                                break;
                            }
                        }
                    })
                    .await
                    .expect("fresh link recovers both required channel modes");
                }
                pair.cleanup().await;
            }
        }
    }
}

// Investigations #853/#889/#826: qualify native SCTP fragmentation through reset.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fragmented_gameplay_preserves_receipts_through_same_stream_reset() {
    for initiator_sends in [true, false] {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            for binary in [false, true] {
                for acknowledged in [false, true] {
                    let context = format!(
                        "initiator={initiator_sends}, label={label}, binary={binary}, acked={acknowledged}"
                    );
                    let (mut pair, remote_runtime) = backpressure_pair(None, initiator_sends).await;
                    let generation = pair.receiver.peers[&pair.sender_id].generation;
                    let channel = pair.sender.channel(pair.receiver_id, label).unwrap();
                    // Distinct, valid UTF-8 bodies at the exact engine byte limit.
                    // The multibyte sequence crosses SCTP fragment boundaries.
                    let bodies: Vec<_> = (0..3)
                        .map(|index| format!("{index}:{}ab", "界".repeat(21_844)))
                        .collect();
                    assert!(bodies.iter().all(|body| body.len() == 65_536));
                    if !acknowledged {
                        remote_runtime.pause().await;
                    }
                    tokio::time::timeout(Duration::from_secs(5), async {
                        for body in &bodies {
                            if binary {
                                channel.send(body.as_bytes().into()).await.unwrap();
                            } else {
                                channel.send_text(body).await.unwrap();
                            }
                        }
                    })
                    .await
                    .expect("the full fragmented burst is accepted before reset");
                    let mut received = Vec::new();
                    if acknowledged {
                        // Application receipt, rather than SCTP acknowledgement,
                        // makes this control mandatory even for no-retransmit DATA.
                        tokio::time::timeout(Duration::from_secs(5), async {
                            while received.len() < bodies.len() {
                                match pair.receiver_events.recv().await.unwrap() {
                                    EngineEvent::ChannelMessage {
                                        peer,
                                        generation: event_generation,
                                        label: received_label,
                                        text,
                                        ..
                                    } => {
                                        assert_eq!(peer, pair.sender_id, "{context}");
                                        assert_eq!(event_generation, generation, "{context}");
                                        assert_eq!(received_label, label, "{context}");
                                        received.push(text);
                                    }
                                    EngineEvent::ChannelReceiveRejected { .. }
                                    | EngineEvent::ChannelClosed { .. } => {
                                        panic!(
                                            "{context}: live fragmented input must remain usable"
                                        )
                                    }
                                    _ => {}
                                }
                            }
                            while channel.outstanding_bytes().await.unwrap() != 0 {
                                tokio::task::yield_now().await;
                            }
                        })
                        .await
                        .expect("all application receipts precede control reset");
                        remote_runtime.pause().await;
                    }
                    assert_eq!(
                        channel.outstanding_bytes().await.unwrap(),
                        if acknowledged { 0 } else { 3 * 65_536 },
                        "{context}"
                    );
                    channel.close().await.unwrap();
                    remote_runtime.resume();
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            match pair.receiver_events.recv().await.unwrap() {
                                EngineEvent::ChannelMessage {
                                    peer,
                                    generation: event_generation,
                                    label: received_label,
                                    text,
                                    ..
                                } => {
                                    assert_eq!(peer, pair.sender_id, "{context}");
                                    assert_eq!(event_generation, generation, "{context}");
                                    assert_eq!(received_label, label, "{context}");
                                    received.push(text);
                                }
                                EngineEvent::ChannelClosed {
                                    peer,
                                    generation: event_generation,
                                    label: closed,
                                } => {
                                    assert_eq!(peer, pair.sender_id, "{context}");
                                    assert_eq!(event_generation, generation, "{context}");
                                    assert_eq!(closed, label, "{context}");
                                    break;
                                }
                                EngineEvent::ChannelReceiveRejected { .. } => {
                                    panic!(
                                        "{context}: fragmentation must not cause input rejection"
                                    )
                                }
                                _ => {}
                            }
                        }
                        while !matches!(
                            channel.outstanding_bytes().await,
                            Ok(0) | Err(webrtc::error::Error::ErrDataChannelClosed)
                        ) {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("fragmented input and reciprocal reset finish");
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while !pair.receiver.peers[&pair.sender_id]
                            .channel_tasks
                            .iter()
                            .any(|task| task.is_finished())
                        {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("the closed channel producer stops");
                    loop {
                        let event = match pair.receiver_events.try_recv() {
                            Ok(event) => event,
                            Err(mpsc::error::TryRecvError::Empty) => break,
                            Err(mpsc::error::TryRecvError::Disconnected) => {
                                panic!("{context}: the engine still owns its event sender")
                            }
                        };
                        assert!(
                            !matches!(
                                event,
                                EngineEvent::ChannelMessage { .. }
                                    | EngineEvent::ChannelClosed { .. }
                                    | EngineEvent::ChannelReceiveRejected { .. }
                            ),
                            "{context}: no late input or duplicate close after the producer stops"
                        );
                    }
                    let mut indices = BTreeSet::new();
                    for text in &received {
                        let index = bodies
                            .iter()
                            .position(|body| body == text)
                            .unwrap_or_else(|| panic!("{context}: corrupt or unexpected body"));
                        assert!(
                            indices.insert(index),
                            "{context}: duplicate complete receipt"
                        );
                    }
                    if label == RELIABLE_LABEL || acknowledged {
                        assert_eq!(received.len(), bodies.len(), "{context}: required receipts");
                    }
                    if label == RELIABLE_LABEL {
                        assert!(received == bodies, "{context}: reliable send order");
                    }
                    let snapshot = pair.receiver.diagnostic_snapshot(pair.sender_id);
                    assert_eq!(
                        snapshot["channels"][label]["receipts"],
                        serde_json::json!({
                            "raw": received.len(), "admitted": received.len(),
                            "rejected": 0, "discarded": 0,
                        }),
                        "{context}: complete driver input must admit once: {snapshot}"
                    );
                    assert_eq!(
                        pair.receiver.message_budget.messages.available_permits(),
                        256
                    );
                    assert_eq!(
                        pair.receiver.message_budget.bytes.available_permits(),
                        1024 * 1024
                    );
                    let reports = pair.receiver.peers[&pair.sender_id]
                        .pc
                        .get_stats(std::time::Instant::now(), StatsSelector::None)
                        .await;
                    let summary = reports
                        .iter()
                        .find_map(|entry| match entry {
                            RTCStatsReportEntry::DataChannel(channel) if channel.label == label => {
                                Some(channel)
                            }
                            _ => None,
                        })
                        .expect("closed fragmented channel retains statistics");
                    assert_eq!(
                        summary.messages_received,
                        received.len() as u32,
                        "{context}"
                    );
                    assert_eq!(
                        summary.bytes_received,
                        (received.len() * 65_536) as u64,
                        "{context}"
                    );
                    assert_eq!(
                        summary.state,
                        rtc::data_channel::RTCDataChannelState::Closed
                    );
                    eprintln!(
                        "fragmented-reset {context}: receipts={}, bytes={}",
                        received.len(),
                        summary.bytes_received
                    );
                    pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                    pair.receiver.remove_peer(pair.sender_id).await.unwrap();
                    pair.negotiate().await;
                    assert_ne!(pair.receiver.peers[&pair.sender_id].generation, generation);
                    pair.sender
                        .channel(pair.receiver_id, label)
                        .unwrap()
                        .send_text("fresh-after-fragmented-reset")
                        .await
                        .unwrap();
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            match pair.receiver_events.recv().await.unwrap() {
                                EngineEvent::ChannelMessage {
                                    label: received_label,
                                    text,
                                    ..
                                } => {
                                    assert_eq!(received_label, label, "{context}");
                                    assert_eq!(text, "fresh-after-fragmented-reset", "{context}");
                                    break;
                                }
                                EngineEvent::ChannelClosed { .. }
                                | EngineEvent::ChannelReceiveRejected { .. } => {
                                    panic!("{context}: a fresh link must recover")
                                }
                                _ => {}
                            }
                        }
                    })
                    .await
                    .expect("fresh link delivers after fragmented reset");
                    pair.cleanup().await;
                }
            }
        }
    }
}

async fn backpressure_pair(
    limit: Option<usize>,
    initiator_sends: bool,
) -> (LivePair, Arc<PausableReactor>) {
    let (pair, remote, _) = backpressure_pair_with_runtimes(limit, initiator_sends).await;
    (pair, remote)
}

async fn backpressure_pair_with_runtimes(
    limit: Option<usize>,
    initiator_sends: bool,
) -> (LivePair, Arc<PausableReactor>, Arc<PausableReactor>) {
    backpressure_profile_pair(limit, initiator_sends, ChannelProfile::Reference).await
}

fn swap_pair_endpoints(pair: &mut LivePair) {
    std::mem::swap(&mut pair.sender, &mut pair.receiver);
    std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
    std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
}

async fn backpressure_profile_pair(
    limit: Option<usize>,
    initiator_sends: bool,
    channel_profile: ChannelProfile,
) -> (LivePair, Arc<PausableReactor>, Arc<PausableReactor>) {
    let (sender_tx, sender_events) = mpsc::unbounded_channel();
    let (receiver_tx, receiver_events) = mpsc::unbounded_channel();
    let settings = EngineSettings {
        disable_mdns: true,
        channel_profile,
        ..EngineSettings::default()
    };
    let remote_runtime = PausableReactor::new();
    let local_runtime = PausableReactor::with_dedicated_reactor(false);
    let mut sender = Engine::new(settings, sender_tx).unwrap();
    let mut receiver = Engine::new(settings, receiver_tx).unwrap();
    sender.send_buffer_limit = limit;
    receiver.send_buffer_limit = limit;
    if initiator_sends {
        receiver.runtime = remote_runtime.clone();
        sender.runtime = local_runtime.clone();
    } else {
        sender.runtime = remote_runtime.clone();
        receiver.runtime = local_runtime.clone();
    }
    let mut pair = LivePair {
        sender,
        receiver,
        sender_events,
        receiver_events,
        sender_id: PlayerId::from_u128(0x1088),
        receiver_id: PlayerId::from_u128(0x1089),
    };
    pair.negotiate().await;
    if !initiator_sends {
        swap_pair_endpoints(&mut pair);
    }
    (pair, remote_runtime, local_runtime)
}

fn assert_channel_closed(result: webrtc::error::Result<()>) {
    assert!(
        matches!(result, Err(webrtc::error::Error::ErrDataChannelClosed)),
        "terminal channel must return typed Closed, got {result:?}"
    );
}

// Regression #1088: a full channel must release every kind of sender on local close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_channel_local_close_releases_senders_before_remote_reset() {
    for initiator_sends in [true, false] {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            for limit in [1, 8] {
                let context =
                    format!("initiator_sends={initiator_sends}, label={label}, limit={limit}");
                let (mut pair, remote_runtime, local_runtime) =
                    backpressure_pair_with_runtimes(Some(limit), initiator_sends).await;
                let channel = pair.sender.channel(pair.receiver_id, label).unwrap();
                remote_runtime.pause().await;
                local_runtime.pause().await;
                let pause_started = std::time::Instant::now();
                let local_before = local_runtime.progress.snapshot();
                let accepted = "a".repeat(limit);
                channel.send_text(&accepted).await.unwrap();
                assert_eq!(channel.outstanding_bytes().await.unwrap(), limit);
                assert!(matches!(
                    channel.try_send_text("x").await,
                    Err(webrtc::error::Error::ErrSendBufferFull)
                ));
                assert!(matches!(
                    channel.try_send(b"x".as_slice().into()).await,
                    Err(webrtc::error::Error::ErrSendBufferFull)
                ));
                let writable = channel.writable();
                let binary = channel.send(b"blocked-binary".as_slice().into());
                let text = channel.send_text("blocked-text");
                tokio::pin!(writable, binary, text);
                /*
                 * Hold the driver while arming these deliberately suspended
                 * futures. Pending must mean buffer pressure, not a core-lock
                 * waiter whose notification the test then leaves unpolled.
                 */
                assert!(futures_util::poll!(&mut writable).is_pending());
                assert!(futures_util::poll!(&mut binary).is_pending());
                assert!(futures_util::poll!(&mut text).is_pending());
                assert_eq!(
                    local_runtime.progress.snapshot()["reactor_polls"],
                    local_before["reactor_polls"],
                    "the local driver stays held while the callers reach capacity waits"
                );
                local_runtime.resume();
                channel.close().await.unwrap();
                assert_eq!(
                    channel.ready_state().await.unwrap(),
                    rtc::data_channel::RTCDataChannelState::Closing
                );
                assert_eq!(
                    channel.outstanding_bytes().await.unwrap(),
                    limit,
                    "close preserves already accepted payload until reciprocal reset"
                );
                tokio::time::timeout(Duration::from_secs(2), async {
                    let (ready, binary, text) = tokio::join!(&mut writable, &mut binary, &mut text);
                    for result in [ready, binary, text] {
                        assert_channel_closed(result);
                    }
                })
                .await
                .expect("local closing releases senders while the remote reactor remains paused");
                assert_channel_closed(channel.try_send_text("x").await);
                assert_channel_closed(channel.try_send(b"x".as_slice().into()).await);
                let sibling_label = if label == RELIABLE_LABEL {
                    UNRELIABLE_LABEL
                } else {
                    RELIABLE_LABEL
                };
                let sibling = pair
                    .sender
                    .channel(pair.receiver_id, sibling_label)
                    .unwrap();
                assert_eq!(
                    sibling.ready_state().await.unwrap(),
                    rtc::data_channel::RTCDataChannelState::Open
                );
                eprintln!("local-close resume: {context}; paused_ms={}; outstanding={:?}; local_before={local_before}; local_after={}; remote_packets={}", pause_started.elapsed().as_millis(), channel.outstanding_bytes().await, local_runtime.progress.snapshot(), remote_runtime.progress.snapshot());
                remote_runtime.resume();
                tokio::time::timeout(Duration::from_secs(5), async {
                    let mut received = 0;
                    loop {
                        match pair.receiver_events.recv().await.unwrap() {
                            EngineEvent::ChannelMessage {
                                label: received_label,
                                text,
                                ..
                            } if received_label == label => {
                                assert_eq!(text, accepted);
                                received += 1;
                            }
                            EngineEvent::ChannelClosed { label: closed, .. } if closed == label => {
                                assert_eq!(received, 1, "{context}: accepted input arrives once before close; elapsed_ms={}; local_before={local_before}; local_after={}; remote_packets={}; sender={}; receiver={}", pause_started.elapsed().as_millis(), local_runtime.progress.snapshot(), remote_runtime.progress.snapshot(), pair.sender.diagnostic_snapshot(pair.receiver_id), pair.receiver.diagnostic_snapshot(pair.sender_id));
                                break;
                            }
                            _ => {}
                        }
                    }
                    while !matches!(
                        channel.outstanding_bytes().await,
                        Ok(0) | Err(webrtc::error::Error::ErrDataChannelClosed)
                    ) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("reset preserves accepted input and releases outstanding bytes");
                /*
                    #1126: a sibling observer can retire the pair before this channel's
                    poller drains. Probe its transport only after the own-close FIFO.
                */
                sibling.try_send_text("sibling").await.unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let reports = pair.receiver.peers[&pair.sender_id]
                            .pc
                            .get_stats(std::time::Instant::now(), StatsSelector::None)
                            .await;
                        if reports.iter().any(|entry| {
                            matches!(entry,
                            RTCStatsReportEntry::DataChannel(stats)
                            if stats.label == sibling_label && stats.messages_received == 1
                                && stats.bytes_received == 7
                                && stats.state == rtc::data_channel::RTCDataChannelState::Open)
                        }) {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("closing one transport channel leaves its sibling able to deliver");
                assert_channel_closed(channel.writable().await);
                pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                pair.receiver.remove_peer(pair.sender_id).await.unwrap();
                pair.negotiate().await;
                let fresh = pair.sender.channel(pair.receiver_id, label).unwrap();
                fresh.try_send_text("fresh").await.unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let EngineEvent::ChannelMessage {
                            label: received_label,
                            text,
                            ..
                        } = pair.receiver_events.recv().await.unwrap()
                        {
                            assert_eq!((received_label.as_str(), text.as_str()), (label, "fresh"));
                            break;
                        }
                    }
                })
                .await
                .expect("fresh link delivers after terminal backpressure");
                pair.cleanup().await;
            }
        }
    }
}

// Regression #1126: physical receipt does not bypass observed pair-loss admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sibling_loss_fences_queued_target_input_before_own_poller_resumes() {
    let mut pair = LivePair::connect().await;
    let peer = pair.sender_id;
    let link = &pair.receiver.peers[&peer];
    let admission = link.receive_admission.clone();
    let generation = link.generation;
    for task in &link.channel_tasks {
        task.abort();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while !link.channel_tasks.iter().all(|task| task.is_finished()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let target = pair
        .sender
        .channel(pair.receiver_id, RELIABLE_LABEL)
        .unwrap();
    target.send_text("accepted-before-own-close").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while pair
            .receiver
            .channel_received_messages(peer, RELIABLE_LABEL)
            .await
            != 1
            || target.outstanding_bytes().await.unwrap() != 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("target input is physically received and acknowledged before reset");
    target.close().await.unwrap();
    let local_target = pair.receiver.channel(peer, RELIABLE_LABEL).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while matches!(
            local_target.ready_state().await,
            Ok(rtc::data_channel::RTCDataChannelState::Open)
        ) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pair.sender
        .channel(pair.receiver_id, UNRELIABLE_LABEL)
        .unwrap()
        .send_text("sibling-probe")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while pair
            .receiver
            .channel_received_messages(peer, UNRELIABLE_LABEL)
            .await
            != 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let sibling_task = spawn_channel_event_loop(
        &pair.receiver.runtime,
        &pair.receiver.events,
        peer,
        generation,
        UNRELIABLE_LABEL.into(),
        pair.receiver.channel(peer, UNRELIABLE_LABEL).unwrap(),
        admission.clone(),
    );
    pair.receiver
        .peers
        .get_mut(&peer)
        .unwrap()
        .channel_tasks
        .push(sibling_task);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !admission.terminal.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("sibling observes physical target loss before target poller resumes");
    let target_task = spawn_channel_event_loop(
        &pair.receiver.runtime,
        &pair.receiver.events,
        peer,
        generation,
        RELIABLE_LABEL.into(),
        local_target,
        admission,
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !target_task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pair.receiver
        .peers
        .get_mut(&peer)
        .unwrap()
        .channel_tasks
        .push(target_task);
    let mut target_closed = 0;
    loop {
        let event = match pair.receiver_events.try_recv() {
            Ok(event) => event,
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                panic!("live receiver engine retains its event sender")
            }
        };
        match event {
            EngineEvent::ChannelMessage { .. } => {
                panic!("observed pair loss fences all unadmitted input")
            }
            EngineEvent::ChannelClosed { label, .. } if label == RELIABLE_LABEL => {
                target_closed += 1
            }
            _ => {}
        }
    }
    assert_eq!(
        target_closed, 1,
        "sibling and target observers publish loss once"
    );
    let snapshot = pair.receiver.diagnostic_snapshot(peer);
    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        assert_eq!(
            snapshot["channels"][label]["receipts"],
            serde_json::json!({
                "raw": 1, "admitted": 0, "rejected": 0, "discarded": 1,
            }),
            "{label}: physically received input stays fenced after observed loss"
        );
    }
    pair.cleanup().await;
}

// Regression #1088: default and zero (unlimited) limits still obey channel state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlimited_channel_writable_rejects_local_closing_and_remote_closed() {
    for limit in [None, Some(0), Some(usize::MAX)] {
        for initiator_sends in [true, false] {
            let (pair, remote_runtime) = backpressure_pair(limit, initiator_sends).await;
            let channel = pair
                .sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap();
            let remote = pair
                .receiver
                .channel(pair.sender_id, RELIABLE_LABEL)
                .unwrap();
            channel.writable().await.unwrap();
            remote_runtime.pause().await;
            channel.close().await.unwrap();
            assert_eq!(
                channel.ready_state().await.unwrap(),
                rtc::data_channel::RTCDataChannelState::Closing
            );
            assert_channel_closed(channel.writable().await);
            remote_runtime.resume();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(
                    remote.ready_state().await,
                    Ok(rtc::data_channel::RTCDataChannelState::Closed)
                        | Err(webrtc::error::Error::ErrDataChannelClosed)
                ) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("remote observes channel reset");
            assert_channel_closed(remote.writable().await);
            pair.cleanup().await;
        }
    }
}

// Regression #1124: T3 abandonment releases public send capacity without peer input.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timer_abandonment_releases_public_channel_writable_without_peer_input() {
    for initiator_sends in [true, false] {
        for binary in [false, true] {
            let (pair, remote_runtime, local_runtime) =
                backpressure_pair_with_runtimes(Some(1), initiator_sends).await;
            let channel = pair
                .sender
                .channel(pair.receiver_id, UNRELIABLE_LABEL)
                .unwrap();
            remote_runtime.pause().await;
            local_runtime.pause().await;
            let remote_before = remote_runtime.progress.snapshot();
            if binary {
                channel.send(b"x".as_slice().into()).await.unwrap();
            } else {
                channel.send_text("x").await.unwrap();
            }
            assert_eq!(channel.outstanding_bytes().await.unwrap(), 1);
            let writable = channel.writable();
            tokio::pin!(writable);
            assert!(futures_util::poll!(&mut writable).is_pending());
            local_runtime.resume();
            tokio::time::timeout(Duration::from_secs(5), &mut writable)
                .await
                .expect("local T3 abandonment wakes public writable without peer input")
                .unwrap();
            assert_eq!(channel.outstanding_bytes().await.unwrap(), 0);
            assert_eq!(
                channel.ready_state().await.unwrap(),
                rtc::data_channel::RTCDataChannelState::Open
            );
            assert_eq!(
                remote_runtime.progress.snapshot()["sent"],
                remote_before["sent"],
                "the paused peer emits no ACK or reset while local credit is released"
            );
            local_runtime.pause().await;
            if binary {
                channel.try_send(b"y".as_slice().into()).await.unwrap();
            } else {
                channel.try_send_text("y").await.unwrap();
            }
            assert_eq!(channel.outstanding_bytes().await.unwrap(), 1);
            local_runtime.resume();
            remote_runtime.resume();
            pair.cleanup().await;
        }
    }
}

// Regression #1088: reciprocal reset must release senders blocked on the other peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_channel_remote_reset_releases_senders() {
    remote_reset_with_packet_history(false, false, false).await;
}

// Investigation #1120: keep the full-channel history under one application transmit loss.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_channel_remote_reset_releases_senders_after_remote_application_transmit_loss() {
    remote_reset_with_packet_history(true, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_channel_remote_reset_releases_senders_after_local_application_transmit_loss() {
    remote_reset_with_packet_history(false, true, false).await;
    remote_reset_with_packet_history(true, true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_channel_remote_reset_starts_before_accepted_data_reaches_udp() {
    remote_reset_with_packet_history(false, false, true).await;
}

async fn remote_reset_with_packet_history(
    drop_remote_application_transmit: bool,
    drop_local_application_transmit: bool,
    hold_accepted_packet: bool,
) {
    for initiator_sends in [true, false] {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            for limit in [1, 8] {
                if (drop_local_application_transmit || hold_accepted_packet)
                    && (label != UNRELIABLE_LABEL || limit != 1)
                {
                    continue;
                }
                let context =
                    format!("initiator_sends={initiator_sends}, label={label}, limit={limit}, drop_remote_application_transmit={drop_remote_application_transmit}, drop_local_application_transmit={drop_local_application_transmit}, hold_accepted_packet={hold_accepted_packet}");
                eprintln!("remote-reset start: {context}");
                let (pair, remote_runtime, local_runtime) =
                    backpressure_pair_with_runtimes(Some(limit), initiator_sends).await;
                let channel = pair.sender.channel(pair.receiver_id, label).unwrap();
                let remote = pair.receiver.channel(pair.sender_id, label).unwrap();
                remote_runtime.pause().await;
                /*
                 * These callers are intentionally not polled again until reset.
                 * Hold the local driver so each Pending is a capacity wait. A
                 * suspended core-lock waiter can consume the mutex wake that
                 * the driver needs, invalidating the intended packet history.
                 */
                local_runtime.pause().await;
                let before_local_send = local_runtime.progress.snapshot();
                local_runtime
                    .progress
                    .drop_next_application
                    .store(drop_local_application_transmit, Ordering::SeqCst);
                channel.send_text(&"a".repeat(limit)).await.unwrap();
                assert_eq!(channel.outstanding_bytes().await.unwrap(), limit);
                let writable = channel.writable();
                let binary = channel.send(b"blocked-binary".as_slice().into());
                let text = channel.send_text("blocked-text");
                tokio::pin!(writable, binary, text);
                assert!(futures_util::poll!(&mut writable).is_pending());
                assert!(futures_util::poll!(&mut binary).is_pending());
                assert!(futures_util::poll!(&mut text).is_pending());
                assert_eq!(
                    local_runtime.progress.snapshot()["reactor_polls"],
                    before_local_send["reactor_polls"],
                    "the local driver stays held while the callers reach capacity waits"
                );
                if !hold_accepted_packet {
                    local_runtime.resume();
                }
                let before_reset = remote_runtime.progress.snapshot();
                remote_runtime
                    .progress
                    .drop_next_application
                    .store(drop_remote_application_transmit, Ordering::SeqCst);
                remote.close().await.unwrap();
                assert_eq!(
                    remote.ready_state().await.unwrap(),
                    rtc::data_channel::RTCDataChannelState::Closing
                );
                remote_runtime.resume();
                if hold_accepted_packet {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while remote_runtime
                            .progress
                            .application_sent
                            .load(Ordering::SeqCst)
                            <= before_reset["application_sent"].as_u64().unwrap()
                        {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("remote SCTP output reaches UDP before local DATA gathering resumes");
                    assert_eq!(
                        local_runtime.progress.snapshot()["application_sent"],
                        before_local_send["application_sent"],
                        "local accepted DATA stays before UDP while the remote reset starts"
                    );
                    local_runtime.resume();
                }
                /*
                 * An ACK can free capacity before reset arrives. Keep the
                 * caller suspended until the public state proves terminal reset,
                 * then re-poll its already pending operations.
                 */
                let reset = tokio::time::timeout(Duration::from_secs(5), async {
                    while !matches!(
                        channel.ready_state().await,
                        Ok(rtc::data_channel::RTCDataChannelState::Closed)
                            | Err(webrtc::error::Error::ErrDataChannelClosed)
                    ) {
                        tokio::task::yield_now().await;
                    }
                })
                .await;
                if reset.is_err() {
                    // Driver/core lock failure must not hang post-deadline evidence.
                    let states = tokio::time::timeout(Duration::from_secs(1), async {
                        format!(
                            "local_state={:?}; local_outstanding={:?}; remote_state={:?}; remote_outstanding={:?}",
                            channel.ready_state().await,
                            channel.outstanding_bytes().await,
                            remote.ready_state().await,
                            remote.outstanding_bytes().await,
                        )
                    }).await;
                    panic!(
                        "remote reset reaches the local public channel state: {context}; states={states:?}; remote_packets_before={before_reset}; remote_packets_after={}; local_packets_before={before_local_send}; local_packets_after={}; sender={}; receiver={}",
                        remote_runtime.progress.snapshot(),
                        local_runtime.progress.snapshot(),
                        pair.sender.diagnostic_snapshot(pair.receiver_id),
                        pair.receiver.diagnostic_snapshot(pair.sender_id),
                    );
                }
                let after_reset = remote_runtime.progress.snapshot();
                for (endpoint, progress, inject_loss) in [
                    (
                        "local",
                        local_runtime.progress.snapshot(),
                        drop_local_application_transmit,
                    ),
                    (
                        "remote",
                        after_reset.clone(),
                        drop_remote_application_transmit,
                    ),
                ] {
                    assert_eq!(
                        progress["dropped_transmits"],
                        u64::from(inject_loss),
                        "{context}: {endpoint} loses exactly the requested application transmit"
                    );
                    assert_eq!(
                        progress["dropped_datagrams"].as_u64().unwrap() > 0,
                        inject_loss,
                        "{context}: {endpoint} reports all datagrams lost in that transmit"
                    );
                }
                assert!(
                    after_reset["reactor_polls"].as_u64().unwrap()
                        > before_reset["reactor_polls"].as_u64().unwrap(),
                    "{context}: remote reactor resumes"
                );
                eprintln!("remote-reset complete: {context}; remote_packets_before={before_reset}; remote_packets_after={after_reset}; local_packets_before={before_local_send}; local_packets_after={}", local_runtime.progress.snapshot());

                tokio::time::timeout(Duration::from_secs(5), async {
                    let (ready, binary, text) = tokio::join!(&mut writable, &mut binary, &mut text);
                    for result in [ready, binary, text] {
                        assert_channel_closed(result);
                    }
                })
                .await
                .expect("remote channel reset releases all blocked senders");
                assert!(matches!(
                    channel.ready_state().await,
                    Ok(rtc::data_channel::RTCDataChannelState::Closed)
                        | Err(webrtc::error::Error::ErrDataChannelClosed)
                ));
                assert!(
                    matches!(
                        channel.outstanding_bytes().await,
                        Ok(0) | Err(webrtc::error::Error::ErrDataChannelClosed)
                    ),
                    "reset releases byte accounting or removes the retired channel"
                );
                assert_channel_closed(channel.try_send_text("x").await);
                assert_channel_closed(channel.try_send(b"x".as_slice().into()).await);
                pair.cleanup().await;
            }
        }
    }
}

// Regression #1088: buffer checks preserve Connecting and peer close/Drop semantics.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_capacity_checks_preserve_connecting_and_peer_shutdown() {
    struct NoopHandler;
    #[async_trait]
    impl PeerConnectionEventHandler for NoopHandler {}
    for limit in [0, 1, usize::MAX] {
        for drop_peer in [false, true] {
            let pc = PeerConnectionBuilder::new()
                .with_udp_addrs(vec!["127.0.0.1:0"])
                .with_handler(Arc::new(NoopHandler))
                .with_data_channel_send_buffer_limit(limit)
                .build()
                .await
                .unwrap();
            let channel = pc.create_data_channel(RELIABLE_LABEL, None).await.unwrap();
            assert_eq!(
                channel.ready_state().await.unwrap(),
                rtc::data_channel::RTCDataChannelState::Connecting
            );
            channel.writable().await.unwrap();
            for result in [
                channel.send_text("before-open").await,
                channel.send(b"before-open".as_slice().into()).await,
                channel.try_send_text("before-open").await,
                channel.try_send(b"before-open".as_slice().into()).await,
            ] {
                assert!(
                    matches!(result, Err(webrtc::error::Error::ErrDataChannelNotOpen)),
                    "Connecting sends preserve typed NotOpen: {result:?}"
                );
            }
            if !drop_peer {
                pc.close().await.unwrap();
            }
            drop(pc);
            tokio::time::timeout(Duration::from_secs(2), async {
                assert_channel_closed(channel.writable().await);
                assert_channel_closed(channel.send_text("after-shutdown").await);
                assert_channel_closed(channel.send(b"after-shutdown".as_slice().into()).await);
                assert_channel_closed(channel.try_send_text("after-shutdown").await);
                assert_channel_closed(channel.try_send(b"after-shutdown".as_slice().into()).await);
            })
            .await
            .expect("peer close and Drop terminate all capacity operations");
        }
    }
}

// Regression #1260: empty packets must consume and release send capacity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_channel_sends_respect_backpressure_and_resume_exact_receipts() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for initiator_sends in [true, false] {
            for binary in [true, false] {
                let (mut pair, mut remote, mut local) =
                    backpressure_profile_pair(Some(1), initiator_sends, profile).await;
                for generation in 0..2 {
                    let channel = pair
                        .sender
                        .channel(pair.receiver_id, RELIABLE_LABEL)
                        .unwrap();
                    remote.pause().await;
                    local.pause().await;
                    if binary {
                        channel.send(Default::default()).await.unwrap();
                    } else {
                        channel.send_text("").await.unwrap();
                    }
                    assert_eq!(channel.outstanding_bytes().await.unwrap(), 1,
                        "profile={profile:?}, initiator_sends={initiator_sends}, binary={binary}, generation={generation}");
                    for result in [
                        channel.try_send(Default::default()).await,
                        channel.try_send_text("").await,
                    ] {
                        assert!(
                            matches!(result, Err(webrtc::error::Error::ErrSendBufferFull)),
                            "{result:?}"
                        );
                    }
                    {
                        let empty_binary = channel.send(Default::default());
                        let empty_text = channel.send_text("");
                        tokio::pin!(empty_binary, empty_text);
                        assert!(futures_util::poll!(&mut empty_binary).is_pending());
                        assert!(futures_util::poll!(&mut empty_text).is_pending());
                    }
                    let writable = channel.writable();
                    let pending = channel.send_text("tail 鱼");
                    tokio::pin!(writable, pending);
                    assert!(futures_util::poll!(&mut writable).is_pending());
                    assert!(futures_util::poll!(&mut pending).is_pending());
                    local.resume();
                    remote.resume();
                    tokio::time::timeout(Duration::from_secs(5), async {
                        writable.await.unwrap();
                        pending.await.unwrap();
                        let mut bodies = Vec::new();
                        while bodies.len() < 2 {
                            match pair.receiver_events.recv().await.unwrap() {
                                EngineEvent::ChannelMessage { label, text, .. } => {
                                    assert_eq!(label, RELIABLE_LABEL);
                                    assert!(
                                        profile == ChannelProfile::Reference
                                            || !binary
                                            || !bodies.is_empty()
                                    );
                                    bodies.push(text.into_bytes());
                                }
                                EngineEvent::ChannelBinaryMessage { label, data, .. } => {
                                    assert_eq!(label, RELIABLE_LABEL);
                                    assert_eq!(profile, ChannelProfile::Godot);
                                    assert!(binary && bodies.is_empty());
                                    bodies.push(data);
                                }
                                EngineEvent::ChannelClosed { .. }
                                | EngineEvent::ChannelReceiveRejected { .. } => {
                                    panic!("healthy backpressure must preserve the channel")
                                }
                                _ => {}
                            }
                        }
                        assert_eq!(bodies, vec![Vec::new(), "tail 鱼".as_bytes().to_vec()]);
                        while channel.outstanding_bytes().await.unwrap() != 0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("resuming both reactors drains the empty packet and exact successor");
                    for (engine, peer, sending) in [
                        (&pair.sender, pair.receiver_id, true),
                        (&pair.receiver, pair.sender_id, false),
                    ] {
                        let reports = engine.peers[&peer]
                            .pc
                            .get_stats(std::time::Instant::now(), StatsSelector::None)
                            .await;
                        let stats = reports
                            .iter()
                            .find_map(|entry| match entry {
                                RTCStatsReportEntry::DataChannel(stats)
                                    if stats.label == RELIABLE_LABEL =>
                                {
                                    Some(stats)
                                }
                                _ => None,
                            })
                            .unwrap();
                        let actual = if sending {
                            (stats.messages_sent, stats.bytes_sent)
                        } else {
                            (stats.messages_received, stats.bytes_received)
                        };
                        assert_eq!(
                            actual,
                            (2, u64::try_from("tail 鱼".len()).unwrap()),
                            "empty-message buffer credit must not become application traffic"
                        );
                    }
                    channel.close().await.unwrap();
                    assert_channel_closed(channel.send(Default::default()).await);
                    assert_channel_closed(channel.try_send_text("").await);
                    let old = pair.sender.peers[&pair.receiver_id].generation;
                    pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                    pair.receiver.remove_peer(pair.sender_id).await.unwrap();
                    if generation == 0 {
                        remote = PausableReactor::new();
                        local = PausableReactor::with_dedicated_reactor(false);
                        pair.sender.runtime = local.clone();
                        pair.receiver.runtime = remote.clone();
                        if !initiator_sends {
                            swap_pair_endpoints(&mut pair);
                        }
                        pair.negotiate().await;
                        if !initiator_sends {
                            swap_pair_endpoints(&mut pair);
                        }
                        assert_ne!(pair.sender.peers[&pair.receiver_id].generation, old);
                    }
                }
                pair.cleanup().await;
            }
        }
    }
}
