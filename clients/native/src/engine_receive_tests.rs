use super::*;

// Regression #1053: required-channel loss must stop sibling input admission.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_pair_does_not_admit_later_sibling_messages() {
    for failure in [
        None,
        Some(RTCPeerConnectionState::Failed),
        Some(RTCPeerConnectionState::Closed),
        Some(RTCPeerConnectionState::Disconnected),
    ] {
        for initiator_receives in [false, true] {
            let mut pair = LivePair::connect().await;
            if initiator_receives {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            let reliable = pair
                .sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap();
            reliable.send_text("before-sibling-loss").await.unwrap();
            let early = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let event @ EngineEvent::ChannelMessage { .. } =
                        pair.receiver_events.recv().await.unwrap()
                    {
                        break event;
                    }
                }
            })
            .await
            .expect("the healthy pair admits early input");
            let terminal = failure != Some(RTCPeerConnectionState::Disconnected);
            if let Some(state) = failure {
                let link = &pair.receiver.peers[&pair.sender_id];
                let handler = PeerHandler {
                    peer: pair.sender_id,
                    generation: link.generation,
                    crippled: false,
                    events: pair.receiver.events.clone(),
                    channel_admission: link.channel_admission.clone(),
                    receive_admission: link.receive_admission.clone(),
                };
                // Exercise the production callback on real open channels. This
                // injects the observation; it does not change physical PC state.
                handler.on_connection_state_change(state).await;
            } else {
                pair.sender
                    .channel(pair.receiver_id, UNRELIABLE_LABEL)
                    .unwrap()
                    .close()
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let EngineEvent::ChannelClosed { label, .. } =
                            pair.receiver_events.recv().await.unwrap()
                        {
                            assert_eq!(label, UNRELIABLE_LABEL);
                            break;
                        }
                    }
                })
                .await
                .expect("the engine observes actual sibling loss before the new message");
            }
            assert_eq!(
                pair.receiver.peers[&pair.sender_id]
                    .receive_admission
                    .terminal
                    .load(Ordering::Relaxed),
                terminal
            );

            // Keep application-level loss handling held. Await SCTP acknowledgement
            // before reset. The driver preserves message/close FIFO per channel,
            // so the receiver's close event marks a completed message drain.
            let reliable = pair
                .sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap();
            reliable.send_text("after-sibling-loss").await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while reliable.outstanding_bytes().await.unwrap() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the reliable probe reaches SCTP acknowledgement before reset");
            reliable.close().await.unwrap();
            let mut late_messages = 0;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match pair.receiver_events.recv().await.unwrap() {
                        EngineEvent::ChannelMessage { text, .. } => {
                            assert!(
                                !terminal,
                                "a terminal pair must not publish new gameplay input"
                            );
                            assert_eq!(text, "after-sibling-loss");
                            late_messages += 1;
                        }
                        EngineEvent::ChannelReceiveRejected { .. } => {
                            panic!("terminal input must not consume or exhaust receive capacity")
                        }
                        EngineEvent::ChannelClosed { label, .. } => {
                            assert_eq!(label, RELIABLE_LABEL);
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("the surviving channel drains and closes");
            assert_eq!(late_messages, usize::from(!terminal));
            assert_eq!(
                pair.receiver.message_budget.messages.available_permits(),
                255
            );
            if let EngineEvent::ChannelMessage { text, .. } = &early {
                assert_eq!(
                    text, "before-sibling-loss",
                    "loss preserves already admitted input"
                );
            }
            let receipts = pair.receiver.diagnostic_snapshot(pair.sender_id);
            assert_eq!(
                receipts["channels"][RELIABLE_LABEL]["receipts"],
                serde_json::json!({
                    "raw": 2, "admitted": if terminal { 1 } else { 2 },
                    "rejected": 0, "discarded": usize::from(terminal),
                }),
                "both roles distinguish driver receipt from terminal-link discard: {receipts}"
            );
            drop(early);
            assert_eq!(
                pair.receiver.message_budget.messages.available_permits(),
                256
            );

            let old_generation = pair.receiver.peers[&pair.sender_id].generation;
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            pair.receiver.remove_peer(pair.sender_id).await.unwrap();
            pair.negotiate().await;
            assert_ne!(
                pair.receiver.peers[&pair.sender_id].generation,
                old_generation
            );
            let fresh = pair.receiver.diagnostic_snapshot(pair.sender_id);
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                assert_eq!(
                    fresh["channels"][label]["receipts"],
                    serde_json::json!({"raw": 0, "admitted": 0, "rejected": 0, "discarded": 0}),
                    "receipt observations belong to a physical generation: {fresh}"
                );
            }
            pair.sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap()
                .send_text("fresh-after-loss")
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match pair.receiver_events.recv().await.unwrap() {
                        EngineEvent::ChannelMessage { text, .. } => {
                            assert_eq!(text, "fresh-after-loss");
                            break;
                        }
                        EngineEvent::ChannelReceiveRejected { .. } => {
                            panic!("fresh input must be admitted")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("a fresh link admits gameplay input");
            pair.cleanup().await;
        }
    }
}

#[test]
fn receive_budget_is_shared_across_links_and_releases_when_payloads_drop() {
    let (events, _receiver) = mpsc::unbounded_channel();
    let engine = Engine::new(EngineSettings::default(), events).unwrap();
    let links = [0, 1].map(|_| ChannelReceiveAdmission {
        message_observed: std::sync::Mutex::new(None),
        receipts: Default::default(),
        budget: engine.message_budget.clone(),
        channels: Arc::new(Mutex::new(RequiredChannelAdmission::default())),
        closed_labels: AtomicU8::new(0),
        rejected: Mutex::new(false),
        terminal: AtomicBool::new(false),
    });
    // The default SCTP message cap prevents live raw-oversize delivery.
    assert!(links[0]
        .decode_and_reserve(&vec![b'x'; 64 * 1024 + 1])
        .is_none());
    let mut held = Vec::new();
    for index in 0..256 {
        // Empty messages still occupy one count slot across both peer links.
        held.push(links[index % 2].decode_and_reserve(b"").unwrap());
    }
    for link in &links {
        assert!(link.decode_and_reserve(b"").is_none());
    }
    assert_eq!(engine.message_budget.bytes.available_permits(), 1024 * 1024);
    drop(held);
    assert_eq!(engine.message_budget.messages.available_permits(), 256);

    let data = vec![b'x'; 64 * 1024];
    let held: Vec<_> = (0..16)
        .map(|index| links[index % 2].decode_and_reserve(&data).unwrap())
        .collect();
    for link in &links {
        assert!(link.decode_and_reserve(b"x").is_none());
    }
    assert_eq!(
        engine.message_budget.messages.available_permits(),
        240,
        "a failed byte reservation must return its count slot"
    );
    assert_eq!(engine.message_budget.bytes.available_permits(), 0);
    drop(held);
    assert_eq!(engine.message_budget.messages.available_permits(), 256);
    assert_eq!(engine.message_budget.bytes.available_permits(), 1024 * 1024);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_live_channels_publish_one_terminal_receive_rejection() {
    let mut pair = LivePair::connect().await;
    let held: Vec<_> = (0..255)
        .map(|_| pair.receiver.message_budget.reserve(0).unwrap())
        .collect();
    let reliable = pair
        .sender
        .channel(pair.receiver_id, RELIABLE_LABEL)
        .unwrap();
    let unreliable = pair
        .sender
        .channel(pair.receiver_id, UNRELIABLE_LABEL)
        .unwrap();
    tokio::join!(
        async {
            reliable.send_text("reliable-one").await.unwrap();
        },
        async {
            unreliable.send_text("unreliable-one").await.unwrap();
        }
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        while !*pair.receiver.peers[&pair.sender_id]
            .receive_admission
            .rejected
            .lock()
            .await
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("concurrent real messages reach the full receive queue");
    // Pin completed remote refusal before local cleanup. Overflow already
    // closes its channel, so that sender handle can be gone by cleanup time.
    let remotely_closed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let EngineEvent::ChannelClosed {
                peer,
                generation,
                label,
            } = pair.sender_events.recv().await.unwrap()
            {
                assert_eq!(peer, pair.receiver_id);
                assert_eq!(generation, pair.sender.peers[&pair.receiver_id].generation);
                assert!(matches!(label.as_str(), RELIABLE_LABEL | UNRELIABLE_LABEL));
                break label;
            }
        }
    })
    .await
    .expect("overflow closes its channel on the sender");
    for (label, channel) in [(RELIABLE_LABEL, reliable), (UNRELIABLE_LABEL, unreliable)] {
        if label == remotely_closed {
            assert!(
                matches!(
                    channel.ready_state().await,
                    Err(webrtc::error::Error::ErrDataChannelClosed)
                ),
                "the observed remote close has removed its sender handle"
            );
            assert!(
                matches!(
                    channel.close().await,
                    Err(webrtc::error::Error::ErrDataChannelClosed)
                ),
                "cleanup may encounter only the confirmed terminal closure"
            );
        } else {
            channel
                .close()
                .await
                .expect("the other channel accepts local cleanup");
        }
    }
    tokio::time::timeout(Duration::from_secs(15), async {
        while !pair.receiver.peers[&pair.sender_id]
            .channel_tasks
            .iter()
            .all(|task| task.is_finished())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both real channel producers terminate");
    let mut rejected = false;
    let mut retained = Vec::new();
    loop {
        let event = match pair.receiver_events.try_recv() {
            Ok(event) => event,
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                panic!("live engine must retain its event sender")
            }
        };
        match event {
            EngineEvent::ChannelMessage { queue_permit, .. } => {
                assert!(
                    !rejected,
                    "the other channel must not publish after rejection"
                );
                retained.push(queue_permit);
            }
            EngineEvent::ChannelReceiveRejected { .. } => {
                assert!(!rejected, "both channels share one terminal rejection");
                rejected = true;
            }
            _ => {}
        }
    }
    assert!(rejected);
    assert_eq!(retained.len(), 1);
    assert_eq!(pair.receiver.message_budget.messages.available_permits(), 0);
    drop(held);
    drop(retained);
    assert_eq!(
        pair.receiver.message_budget.messages.available_permits(),
        256
    );
    pair.cleanup().await;
}

pub(super) struct LivePair {
    pub(super) sender: Engine,
    pub(super) receiver: Engine,
    pub(super) sender_events: mpsc::UnboundedReceiver<EngineEvent>,
    pub(super) receiver_events: mpsc::UnboundedReceiver<EngineEvent>,
    pub(super) sender_id: PlayerId,
    pub(super) receiver_id: PlayerId,
}

impl LivePair {
    pub(super) async fn connect() -> Self {
        let (sender_tx, sender_events) = mpsc::unbounded_channel();
        let (receiver_tx, receiver_events) = mpsc::unbounded_channel();
        let settings = EngineSettings {
            disable_mdns: true,
            ..EngineSettings::default()
        };
        let mut pair = Self {
            sender: Engine::new(settings, sender_tx).unwrap(),
            receiver: Engine::new(settings, receiver_tx).unwrap(),
            sender_events,
            receiver_events,
            sender_id: PlayerId::from_u128(0x123),
            receiver_id: PlayerId::from_u128(0x456),
        };
        pair.negotiate().await;
        pair
    }

    pub(super) async fn negotiate(&mut self) {
        let offer = self
            .sender
            .pair_with(self.receiver_id, true, &[])
            .await
            .unwrap()
            .unwrap();
        self.receiver
            .pair_with(self.sender_id, false, &[])
            .await
            .unwrap();
        let answer = self
            .receiver
            .handle_offer(self.sender_id, offer)
            .await
            .unwrap();
        self.sender
            .handle_answer(self.receiver_id, answer)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut sender_connected = false;
            let mut receiver_connected = false;
            while !sender_connected || !receiver_connected {
                tokio::select! {
                    event = self.sender_events.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            self.receiver.handle_remote_candidate(self.sender_id, &candidate_json).await.unwrap();
                        }
                        EngineEvent::RemoteChannel { label, channel, .. } => {
                            self.sender.store_remote_channel(self.receiver_id, label, channel).await;
                        }
                        EngineEvent::ChannelOpen { label, .. } => {
                            sender_connected |= self.sender.note_channel_open(self.receiver_id, &label).await;
                        }
                        _ => {}
                    },
                    event = self.receiver_events.recv() => match event.unwrap() {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            self.sender.handle_remote_candidate(self.receiver_id, &candidate_json).await.unwrap();
                        }
                        EngineEvent::RemoteChannel { label, channel, .. } => {
                            self.receiver.store_remote_channel(self.sender_id, label, channel).await;
                        }
                        EngineEvent::ChannelOpen { label, .. } => {
                            receiver_connected |= self.receiver.note_channel_open(self.sender_id, &label).await;
                        }
                        _ => {}
                    },
                }
            }
        }).await.expect("local pair opens both required channels");
    }

    pub(super) async fn cleanup(mut self) {
        self.sender.remove_peer(self.receiver_id).await.unwrap();
        self.receiver.remove_peer(self.sender_id).await.unwrap();
    }
}

// Regression #878: unread application callbacks retain payloads without a bound.
#[tokio::test]
async fn unread_live_receive_queue_bounds_count_and_preserves_fifo() {
    let mut pair = LivePair::connect().await;
    let admitted = flood_until_rejected(&mut pair, 257, 16, false).await;
    assert_eq!(admitted.len(), 256);

    // Removing a link must not free payloads still owned by queued callbacks.
    let old_generation = pair.receiver.peers[&pair.sender_id].generation;
    pair.sender.remove_peer(pair.receiver_id).await.unwrap();
    pair.receiver.remove_peer(pair.sender_id).await.unwrap();
    pair.negotiate().await;
    assert_ne!(
        old_generation,
        pair.receiver.peers[&pair.sender_id].generation
    );
    let fresh = flood_until_rejected(&mut pair, 1, 16, false).await;
    assert!(
        fresh.is_empty(),
        "stale queued payloads still consume the engine budget"
    );
    drop(admitted);

    // Consuming the retained callbacks releases capacity for a fresh physical link.
    pair.sender.remove_peer(pair.receiver_id).await.unwrap();
    pair.receiver.remove_peer(pair.sender_id).await.unwrap();
    pair.negotiate().await;
    let channel = pair
        .sender
        .channel(pair.receiver_id, RELIABLE_LABEL)
        .unwrap();
    channel.send_text("healthy-after-drain").await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match pair.receiver_events.recv().await.unwrap() {
                EngineEvent::ChannelMessage { text, .. } => {
                    assert_eq!(text, "healthy-after-drain");
                    break;
                }
                EngineEvent::ChannelReceiveRejected { .. } => {
                    panic!("released capacity must allow healthy traffic")
                }
                _ => {}
            }
        }
    })
    .await
    .expect("healthy traffic resumes after queue drain");
    pair.cleanup().await;
}

fn payload(index: usize, bytes: usize) -> String {
    let mut text = format!("message-{index}");
    text.extend(std::iter::repeat_n('x', bytes - text.len()));
    text
}

async fn flood_until_rejected(
    pair: &mut LivePair,
    sent: usize,
    bytes: usize,
    binary: bool,
) -> Vec<MessageQueuePermit> {
    let channel = pair
        .sender
        .channel(pair.receiver_id, RELIABLE_LABEL)
        .unwrap();
    for index in 0..sent {
        let result = if binary {
            channel.send(vec![0xff; bytes].as_slice().into()).await
        } else {
            channel.send_text(&payload(index, bytes)).await
        };
        // A closed remote channel can refuse later sends in a long burst.
        if let Err(error) = result {
            assert!(
                index > 0,
                "first send failed: bytes={bytes}, binary={binary}, error={error}"
            );
            break;
        }
    }
    let mut permits = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match pair.receiver_events.recv().await.unwrap() {
                EngineEvent::ChannelMessage {
                    peer,
                    generation,
                    label,
                    text,
                    queue_permit,
                    ..
                } => {
                    assert_eq!(peer, pair.sender_id);
                    assert_eq!(generation, pair.receiver.peers[&pair.sender_id].generation);
                    assert_eq!(label, RELIABLE_LABEL);
                    if binary {
                        assert_eq!(text, "\u{fffd}".repeat(bytes));
                    } else {
                        assert_eq!(
                            text,
                            payload(permits.len(), bytes),
                            "admitted messages preserve FIFO"
                        );
                    }
                    permits.push(queue_permit);
                }
                EngineEvent::ChannelReceiveRejected {
                    peer,
                    generation,
                    label,
                } => {
                    assert_eq!(peer, pair.sender_id);
                    assert_eq!(generation, pair.receiver.peers[&pair.sender_id].generation);
                    assert_eq!(label, RELIABLE_LABEL);
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap_or_else(|error| panic!("receive overload missing: sent={sent}, bytes={bytes}, binary={binary}, admitted={}, error={error}", permits.len()));
    let sender_generation = pair.sender.peers[&pair.receiver_id].generation;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let EngineEvent::ChannelClosed {
                generation, label, ..
            } = pair.sender_events.recv().await.unwrap()
            {
                if generation == sender_generation && label == RELIABLE_LABEL {
                    break;
                }
            }
        }
    })
    .await
    .expect("receive rejection closes the remote reliable channel");
    // The rejection is the producer's terminal barrier, not a sleep-based oracle.
    loop {
        let event = match pair.receiver_events.try_recv() {
            Ok(event) => event,
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                panic!("live engine must retain its event sender")
            }
        };
        assert!(
            !matches!(
                event,
                EngineEvent::ChannelMessage { .. } | EngineEvent::ChannelReceiveRejected { .. }
            ),
            "the overloaded link must stop publishing after one rejection"
        );
    }
    let snapshot = pair.receiver.diagnostic_snapshot(pair.sender_id);
    assert_eq!(
        snapshot["channels"][RELIABLE_LABEL]["receipts"],
        serde_json::json!({
            "raw": permits.len() + 1, "admitted": permits.len(),
            "rejected": 1, "discarded": 0,
        }),
        "receive refusal differs from transport loss: {snapshot}"
    );
    assert_eq!(snapshot["channels"][UNRELIABLE_LABEL]["receipts"]["raw"], 0);
    permits
}

#[tokio::test]
async fn unread_live_receive_queue_bounds_decoded_bytes_and_single_message_size() {
    for (description, count, bytes, binary, expected) in [
        ("byte budget", 129, 8192, false, 128),
        ("exact single-message limit", 17, 64 * 1024, false, 16),
        ("lossy UTF-8 expansion", 1, 22000, true, 0),
    ] {
        let mut pair = LivePair::connect().await;
        let permits = flood_until_rejected(&mut pair, count, bytes, binary).await;
        assert_eq!(permits.len(), expected, "{description}");
        drop(permits);
        pair.cleanup().await;
    }
}

#[tokio::test]
async fn dropped_receive_event_queue_stops_live_channel_producer() {
    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        let mut pair = LivePair::connect().await;
        let (_, replacement) = mpsc::unbounded_channel();
        drop(std::mem::replace(&mut pair.receiver_events, replacement));
        pair.sender
            .channel(pair.receiver_id, label)
            .unwrap()
            .send_text("no-receiver")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let EngineEvent::ChannelClosed {
                    label: closed_label,
                    ..
                } = pair.sender_events.recv().await.unwrap()
                {
                    if closed_label == label {
                        break;
                    }
                }
            }
        })
        .await
        .expect("a dropped application receiver closes the channel producer");
        let snapshot = pair.receiver.diagnostic_snapshot(pair.sender_id);
        assert_eq!(
            snapshot["channels"][label]["receipts"],
            serde_json::json!({"raw": 1, "admitted": 0, "rejected": 0, "discarded": 1}),
            "driver input without a live application receiver is discarded: {snapshot}"
        );
        pair.cleanup().await;
    }
}

// Regression #1059: live channel loss can precede its queued lifecycle event.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_sibling_closure_fences_gameplay_before_its_poller_runs() {
    for observation in ["send", "receive", "open"] {
        for closed_label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            for initiator_receives in [false, true] {
                let mut pair = LivePair::connect().await;
                if initiator_receives {
                    std::mem::swap(&mut pair.sender, &mut pair.receiver);
                    std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                    std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                }
                let peer = pair.sender_id;
                let link = &pair.receiver.peers[&peer];
                let generation = link.generation;
                let admission = link.receive_admission.clone();
                for task in &link.channel_tasks {
                    task.abort();
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !link.channel_tasks.iter().all(|task| task.is_finished()) {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("hold lifecycle observation before real channel closure");
                let healthy_label = if closed_label == RELIABLE_LABEL {
                    UNRELIABLE_LABEL
                } else {
                    RELIABLE_LABEL
                };
                let healthy_task = spawn_channel_event_loop(
                    &pair.receiver.runtime,
                    &pair.receiver.events,
                    peer,
                    generation,
                    healthy_label.into(),
                    pair.receiver.channel(peer, healthy_label).unwrap(),
                    admission.clone(),
                );
                pair.receiver
                    .peers
                    .get_mut(&peer)
                    .unwrap()
                    .channel_tasks
                    .push(healthy_task);
                let remote_healthy = pair
                    .sender
                    .channel(pair.receiver_id, healthy_label)
                    .unwrap();
                remote_healthy.send_text("before-live-loss").await.unwrap();
                let early = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let event @ EngineEvent::ChannelMessage { .. } =
                            pair.receiver_events.recv().await.unwrap()
                        {
                            break event;
                        }
                    }
                })
                .await
                .expect("healthy input is admitted before channel loss");
                let closed = pair.receiver.channel(peer, closed_label).unwrap();
                closed.close().await.unwrap();
                assert!(matches!(
                    closed.ready_state().await,
                    Ok(rtc::data_channel::RTCDataChannelState::Closing
                        | rtc::data_channel::RTCDataChannelState::Closed)
                        | Err(webrtc::error::Error::ErrDataChannelClosed)
                ));
                assert!(
                    !admission.terminal.load(Ordering::Relaxed),
                    "closed-channel poller remains held"
                );
                match observation {
                    "send" => {
                        assert!(pair.receiver.send_channel(peer, healthy_label).await.is_none(),
                        "live sibling closure must prevent a new gameplay send before its callback")
                    }
                    "receive" => remote_healthy.send_text("after-live-loss").await.unwrap(),
                    "open" => {
                        let link = pair.receiver.peers.get_mut(&peer).unwrap();
                        link.open_labels.clear();
                        link.pair_connected = false;
                        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                            assert!(!pair.receiver.note_channel_open(peer, label).await);
                        }
                    }
                    _ => unreachable!(),
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        match pair.receiver_events.recv().await.unwrap() {
                            EngineEvent::ChannelMessage { .. } => {
                                panic!("{observation}: terminal sibling must not admit gameplay")
                            }
                            EngineEvent::ChannelClosed {
                                peer: event_peer,
                                generation: event_generation,
                                label,
                            } => {
                                assert_eq!(
                                    (event_peer, event_generation, label.as_str()),
                                    (peer, generation, closed_label)
                                );
                                break;
                            }
                            _ => {}
                        }
                    }
                })
                .await
                .expect("live state publishes loss before the held poller resumes");
                for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                    assert!(pair.receiver.send_channel(peer, label).await.is_none());
                }
                assert_eq!(
                    pair.receiver.message_budget.messages.available_permits(),
                    255
                );
                let EngineEvent::ChannelMessage { text, .. } = &early else {
                    unreachable!()
                };
                assert_eq!(text, "before-live-loss");
                drop(early);
                assert_eq!(
                    pair.receiver.message_budget.messages.available_permits(),
                    256
                );
                let resumed = spawn_channel_event_loop(
                    &pair.receiver.runtime,
                    &pair.receiver.events,
                    peer,
                    generation,
                    closed_label.into(),
                    closed,
                    admission,
                );
                pair.receiver
                    .peers
                    .get_mut(&peer)
                    .unwrap()
                    .channel_tasks
                    .push(resumed);
                remote_healthy.close().await.unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !pair.receiver.peers[&peer]
                        .channel_tasks
                        .iter()
                        .all(|task| task.is_finished())
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("both channel streams drain after the held poller resumes");
                let mut healthy_closed = 0;
                loop {
                    let event = match pair.receiver_events.try_recv() {
                        Ok(event) => event,
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            panic!("the live engine must retain its event sender")
                        }
                    };
                    match event {
                        EngineEvent::ChannelClosed { label, .. } => {
                            assert_eq!(
                                label, healthy_label,
                                "resumed closure must not duplicate the observed loss"
                            );
                            healthy_closed += 1;
                        }
                        EngineEvent::ChannelMessage { .. } => {
                            panic!("terminal input must stay fenced")
                        }
                        _ => {}
                    }
                }
                assert_eq!(healthy_closed, 1);
                pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                pair.receiver.remove_peer(peer).await.unwrap();
                pair.negotiate().await;
                assert_ne!(pair.receiver.peers[&peer].generation, generation);
                assert!(pair
                    .receiver
                    .send_channel(peer, healthy_label)
                    .await
                    .is_some());
                pair.sender
                    .channel(pair.receiver_id, RELIABLE_LABEL)
                    .unwrap()
                    .send_text("fresh-after-live-loss")
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let EngineEvent::ChannelMessage { text, .. } =
                            pair.receiver_events.recv().await.unwrap()
                        {
                            assert_eq!(text, "fresh-after-live-loss");
                            break;
                        }
                    }
                })
                .await
                .expect("fresh physical links admit gameplay again");
                pair.cleanup().await;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_reliable_input_drains_before_its_own_graceful_close() {
    for initiator_receives in [false, true] {
        let mut pair = LivePair::connect().await;
        if initiator_receives {
            std::mem::swap(&mut pair.sender, &mut pair.receiver);
            std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
            std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
        }
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
        let reliable = pair
            .sender
            .channel(pair.receiver_id, RELIABLE_LABEL)
            .unwrap();
        reliable.send_text("queued-before-own-close").await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while reliable.outstanding_bytes().await.unwrap() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("SCTP acknowledges the reliable input before reset");
        reliable.close().await.unwrap();
        let local = pair.receiver.channel(peer, RELIABLE_LABEL).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while matches!(
                local.ready_state().await,
                Ok(rtc::data_channel::RTCDataChannelState::Open)
            ) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("live channel state changes while its event poller stays held");
        assert!(!admission.terminal.load(Ordering::Relaxed));
        let resumed = spawn_channel_event_loop(
            &pair.receiver.runtime,
            &pair.receiver.events,
            peer,
            generation,
            RELIABLE_LABEL.into(),
            local,
            admission,
        );
        pair.receiver
            .peers
            .get_mut(&peer)
            .unwrap()
            .channel_tasks
            .push(resumed);
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut messages = 0;
            loop {
                match pair.receiver_events.recv().await.unwrap() {
                    EngineEvent::ChannelMessage { label, text, .. } => {
                        assert_eq!(label, RELIABLE_LABEL);
                        assert_eq!(text, "queued-before-own-close");
                        messages += 1;
                    }
                    EngineEvent::ChannelClosed { label, .. } => {
                        assert_eq!(label, RELIABLE_LABEL);
                        assert_eq!(
                            messages, 1,
                            "same-channel FIFO must retain input before its close"
                        );
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("queued reliable input drains before its close notification");
        pair.cleanup().await;
    }
}

impl Engine {
    /// Observe decoded transport input before a channel's application poller starts.
    pub(crate) async fn channel_received_messages(&self, peer: PlayerId, label: &str) -> u32 {
        self.peers[&peer]
            .pc
            .get_stats(std::time::Instant::now(), StatsSelector::None)
            .await
            .iter()
            .find_map(|entry| match entry {
                RTCStatsReportEntry::DataChannel(channel) if channel.label == label => {
                    Some(channel.messages_received)
                }
                _ => None,
            })
            .expect("required channel has transport statistics")
    }
}

// Regression #1074: channel-poll arrival precedes a suspended admission.
#[tokio::test]
async fn channel_receipt_time_precedes_admission_wait() {
    for initiator_receives in [false, true] {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            let mut pair = LivePair::connect().await;
            if initiator_receives {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            let admission = pair.receiver.peers[&pair.sender_id]
                .receive_admission
                .clone();
            let observed = Arc::new(tokio::sync::Notify::new());
            *admission.message_observed.lock().unwrap() = Some(observed.clone());
            let held = admission.rejected.lock().await;
            pair.sender
                .channel(pair.receiver_id, label)
                .unwrap()
                .send_text("timely-before-admission")
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), observed.notified())
                .await
                .expect("the real channel poll completes before the admission wait");
            let cutoff = tokio::time::Instant::now() + Duration::from_secs(1);
            // The actual receive-admission mutex holds publication beyond cutoff.
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(2)).await;
            drop(held);
            let received_at = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let EngineEvent::ChannelMessage {
                        text,
                        label: actual,
                        received_at,
                        ..
                    } = pair.receiver_events.recv().await.unwrap()
                    {
                        assert_eq!(text, "timely-before-admission");
                        assert_eq!(actual, label);
                        break received_at;
                    }
                }
            })
            .await
            .expect("the admitted gameplay receipt publishes after the wait");
            assert!(tokio::time::Instant::now() >= cutoff);
            assert!(
                received_at < cutoff,
                "role={initiator_receives} label={label}: publication cannot replace arrival time"
            );
            tokio::time::resume();
            pair.cleanup().await;
        }
    }
}
