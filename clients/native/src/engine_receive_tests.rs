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
        budget: engine.message_budget.clone(),
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
    let mut pair = LivePair::connect().await;
    let (_, replacement) = mpsc::unbounded_channel();
    drop(std::mem::replace(&mut pair.receiver_events, replacement));
    pair.sender
        .channel(pair.receiver_id, RELIABLE_LABEL)
        .unwrap()
        .send_text("no-receiver")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let EngineEvent::ChannelClosed { label, .. } =
                pair.sender_events.recv().await.unwrap()
            {
                if label == RELIABLE_LABEL {
                    break;
                }
            }
        }
    })
    .await
    .expect("a dropped application receiver closes the channel producer");
    pair.cleanup().await;
}
