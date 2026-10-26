use super::{receive_tests::LivePair, *};

async fn negotiated_pair() -> LivePair {
    let (sender_tx, sender_events) = mpsc::unbounded_channel();
    let (receiver_tx, receiver_events) = mpsc::unbounded_channel();
    let settings = EngineSettings {
        disable_mdns: true,
        channel_profile: ChannelProfile::Godot,
        ..EngineSettings::default()
    };
    let mut pair = LivePair {
        sender: Engine::new(settings, sender_tx).unwrap(),
        receiver: Engine::new(settings, receiver_tx).unwrap(),
        sender_events,
        receiver_events,
        sender_id: PlayerId::from_u128(0x1240),
        receiver_id: PlayerId::from_u128(0x1241),
    };
    pair.negotiate().await;
    pair
}

#[tokio::test]
async fn godot_profile_registers_exact_reserved_contract_before_both_roles_negotiate() {
    for initiate in [false, true] {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(
            EngineSettings {
                crippled: true,
                channel_profile: ChannelProfile::Godot,
                ..EngineSettings::default()
            },
            tx,
        )
        .unwrap();
        let peer = PlayerId::from_u128(0x1240);
        engine.pair_with(peer, initiate, &[]).await.unwrap();
        assert_eq!(engine.peers[&peer].channels.len(), 3);
        assert_eq!(engine.peers[&peer].channel_tasks.len(), 3);
        for (label, id, ordered, lifetime) in [
            ("reliable", 1, true, None),
            ("ordered", 2, true, Some(1)),
            ("unreliable", 3, false, Some(1)),
        ] {
            let channel = engine.channel(peer, label).unwrap();
            assert_eq!(
                ChannelProfile::Godot.channel_init(label).negotiated,
                Some(id)
            );
            assert!(channel.negotiated().await.unwrap());
            assert_eq!(channel.ordered().await.unwrap(), ordered);
            assert_eq!(channel.max_packet_life_time().await.unwrap(), lifetime);
            assert_eq!(channel.max_retransmits().await.unwrap(), None);
        }
        engine.remove_peer(peer).await.unwrap();
    }
    assert_eq!(
        EngineSettings::default().channel_profile,
        ChannelProfile::Reference
    );
}

async fn exchange(pair: &mut LivePair) {
    for sender_offers in [true, false] {
        let (sender, peer, events) = if sender_offers {
            (&pair.sender, pair.receiver_id, &mut pair.receiver_events)
        } else {
            (&pair.receiver, pair.sender_id, &mut pair.sender_events)
        };
        let reports = sender.peers[&peer]
            .pc
            .get_stats(std::time::Instant::now(), StatsSelector::None)
            .await;
        for (label, id) in [("reliable", 1), ("ordered", 2), ("unreliable", 3)] {
            assert!(reports.iter().any(|entry| matches!(entry,
                RTCStatsReportEntry::DataChannel(stats)
                    if stats.label == label && stats.data_channel_identifier == id)));
            let channel = sender.channel(peer, label).unwrap();
            let payloads: Vec<_> = (0..8)
                .map(|seq| format!("{label}: exact 鱼 bytes\0 {seq}"))
                .collect();
            for payload in &payloads {
                channel.send(payload.as_bytes().into()).await.unwrap();
                tokio::task::yield_now().await;
            }
            let drain = if label == "reliable" {
                Duration::from_secs(5)
            } else {
                Duration::from_millis(250)
            };
            let deadline = tokio::time::Instant::now() + drain;
            let mut seen = BTreeSet::new();
            while seen.len() < payloads.len() {
                let Ok(event) = tokio::time::timeout_at(deadline, events.recv()).await else {
                    break;
                };
                match event.unwrap() {
                    EngineEvent::ChannelMessage {
                        label: actual,
                        text,
                        ..
                    } => {
                        assert_eq!(actual, label);
                        let seq = payloads
                            .iter()
                            .position(|payload| payload.as_bytes() == text.as_bytes())
                            .expect("every received payload retains its exact offered bytes");
                        if label != "unreliable" {
                            assert!(
                                seen.last().is_none_or(|previous| *previous < seq),
                                "ordered gameplay must not reverse arrival order"
                            );
                        }
                        assert!(seen.insert(seq), "no duplicate gameplay receipt");
                    }
                    EngineEvent::ChannelClosed { label, .. } => {
                        panic!("unexpected channel loss: {label}")
                    }
                    _ => {}
                }
            }
            println!("GODOT_PROFILE_RECEIPTS offerer_sends={sender_offers} label={label} offered=8 received={}", seen.len());
            if label == "reliable" {
                assert_eq!(seen.len(), 8, "reliable gameplay must arrive in full");
            } else {
                assert!(
                    !seen.is_empty(),
                    "lifetime-limited gameplay must produce an exact receipt"
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn godot_profile_exchanges_and_retires_every_required_channel_before_fresh_pair() {
    for closer_offers in [true, false] {
        for label in ["reliable", "ordered", "unreliable"] {
            let mut pair = negotiated_pair().await;
            exchange(&mut pair).await;
            for (engine, peer) in [
                (&mut pair.sender, pair.receiver_id),
                (&mut pair.receiver, pair.sender_id),
            ] {
                let link = engine.peers.get_mut(&peer).unwrap();
                link.open_labels.clear();
                link.pair_connected = false;
                assert!(!engine.note_channel_open(peer, "reliable").await);
                assert!(!engine.note_channel_open(peer, "unreliable").await);
                assert!(engine.note_channel_open(peer, "ordered").await);
                assert!(!engine.note_channel_open(peer, "ordered").await);
            }
            pair.sender
                .channel(pair.receiver_id, "reliable")
                .unwrap()
                .send_text("queued-before-loss")
                .await
                .unwrap();
            let queued = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let event @ EngineEvent::ChannelMessage { .. } =
                        pair.receiver_events.recv().await.unwrap()
                    {
                        break event;
                    }
                }
            })
            .await
            .expect("retain one admitted callback across physical-link replacement");
            assert!(pair.receiver.is_current_event(&queued));
            let generation = pair.receiver.peers[&pair.sender_id].generation;
            let channel = if closer_offers {
                pair.sender.channel(pair.receiver_id, label)
            } else {
                pair.receiver.channel(pair.sender_id, label)
            }
            .unwrap();
            channel.close().await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let EngineEvent::ChannelClosed { label: actual, .. } =
                        pair.receiver_events.recv().await.unwrap()
                    {
                        assert_eq!(actual, label);
                        break;
                    }
                }
            })
            .await
            .expect("every reserved channel loss retires the pair");
            for sibling in ["reliable", "ordered", "unreliable"] {
                assert!(pair
                    .receiver
                    .send_channel(pair.sender_id, sibling)
                    .await
                    .is_none());
            }
            assert!(
                !pair
                    .receiver
                    .note_channel_open(pair.sender_id, "reliable")
                    .await
            );
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            pair.receiver.remove_peer(pair.sender_id).await.unwrap();
            pair.negotiate().await;
            assert_ne!(pair.receiver.peers[&pair.sender_id].generation, generation);
            assert!(!pair.receiver.is_current_event(&queued));
            drop(queued);
            exchange(&mut pair).await;
            pair.cleanup().await;
        }
    }
}
