use super::{receive_tests::LivePair, *};

fn unconnected_negotiated_pair() -> LivePair {
    let (sender_tx, sender_events) = mpsc::unbounded_channel();
    let (receiver_tx, receiver_events) = mpsc::unbounded_channel();
    let settings = EngineSettings {
        disable_mdns: true,
        channel_profile: ChannelProfile::Godot,
        ..EngineSettings::default()
    };
    LivePair {
        sender: Engine::new(settings, sender_tx).unwrap(),
        receiver: Engine::new(settings, receiver_tx).unwrap(),
        sender_events,
        receiver_events,
        sender_id: PlayerId::from_u128(0x1240),
        receiver_id: PlayerId::from_u128(0x1241),
    }
}

async fn negotiated_pair() -> LivePair {
    let mut pair = unconnected_negotiated_pair();
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

async fn exchange(pair: &mut LivePair) -> HashMap<(bool, &'static str), usize> {
    let mut receipts = HashMap::new();
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
                .map(|seq| {
                    let mut bytes = format!("{label}: exact 鱼 bytes\0 {seq}").into_bytes();
                    bytes.extend_from_slice(&[0xff, 0x80, 0]);
                    bytes.resize(if label == RELIABLE_LABEL { 4096 } else { 128 }, 0xff);
                    bytes
                })
                .collect();
            for payload in &payloads {
                channel.send(payload.as_slice().into()).await.unwrap();
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
                    EngineEvent::ChannelBinaryMessage {
                        label: actual,
                        data,
                        ..
                    } => {
                        assert_eq!(actual, label);
                        let seq = payloads
                            .iter()
                            .position(|payload| payload == &data)
                            .expect("every received payload retains its exact offered bytes");
                        if label != "unreliable" {
                            assert!(
                                seen.last().is_none_or(|previous| *previous < seq),
                                "ordered gameplay must not reverse arrival order"
                            );
                        }
                        assert!(seen.insert(seq), "no duplicate gameplay receipt");
                    }
                    EngineEvent::ChannelMessage { .. } => {
                        panic!("binary traffic must not become a text event")
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
            }
            receipts.insert((sender_offers, label), seen.len());
        }
    }
    receipts
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn godot_profile_exchanges_and_retires_every_required_channel_before_fresh_pair() {
    let mut receipts = HashMap::<_, usize>::new();
    for closer_offers in [true, false] {
        for label in ["reliable", "ordered", "unreliable"] {
            let mut pair = negotiated_pair().await;
            for (key, count) in exchange(&mut pair).await {
                *receipts.entry((false, key)).or_default() += count;
            }
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
                .send(b"queued-before-loss\xff".as_slice().into())
                .await
                .unwrap();
            let queued = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let event @ EngineEvent::ChannelBinaryMessage { .. } =
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
            for (key, count) in exchange(&mut pair).await {
                *receipts.entry((true, key)).or_default() += count;
            }
            pair.cleanup().await;
        }
    }
    for fresh in [false, true] {
        for sender_offers in [false, true] {
            for label in ["reliable", "ordered", "unreliable"] {
                let received = receipts[&(fresh, (sender_offers, label))];
                println!("GODOT_PROFILE_TOTAL fresh={fresh} offerer_sends={sender_offers} label={label} offered=48 received={received}");
                assert!(received > 0, "fresh={fresh}, offerer_sends={sender_offers}, label={label}: the complete control history must prove exact delivery in each mode");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn godot_profile_preserves_non_utf8_gameplay_bytes() {
    let mut pair = negotiated_pair().await;
    for sender_offers in [true, false] {
        let (sender, peer, receiver, events) = if sender_offers {
            (
                &pair.sender,
                pair.receiver_id,
                &pair.receiver,
                &mut pair.receiver_events,
            )
        } else {
            (
                &pair.receiver,
                pair.sender_id,
                &pair.sender,
                &mut pair.sender_events,
            )
        };
        for payload in [
            vec![],
            "binary UTF-8 鱼\0".as_bytes().to_vec(),
            vec![0, 0xff, 0x80, 1, 0xc0, 0xaf, 0, 0xfe],
            (0..=255).collect(),
            vec![0xff; MAX_CHANNEL_MESSAGE_BYTES],
        ] {
            sender
                .channel(peer, RELIABLE_LABEL)
                .unwrap()
                .send(payload.as_slice().into())
                .await
                .unwrap();
            let receipt = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match events.recv().await.unwrap() {
                        event @ EngineEvent::ChannelBinaryMessage { .. } => break event,
                        EngineEvent::ChannelReceiveRejected { .. } => {
                            panic!("an admitted binary packet must retain raw bytes")
                        }
                        EngineEvent::ChannelMessage { .. } => {
                            panic!("binary packets must not be converted to text")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            assert!(receiver.is_current_event(&receipt));
            let EngineEvent::ChannelBinaryMessage {
                data, queue_permit, ..
            } = receipt
            else {
                unreachable!()
            };
            assert_eq!(data, payload);
            assert_eq!(
                receiver.message_budget.messages.available_permits(),
                MAX_PENDING_CHANNEL_MESSAGES - 1
            );
            assert_eq!(
                receiver.message_budget.bytes.available_permits(),
                MAX_PENDING_CHANNEL_BYTES - payload.len()
            );
            drop(queue_permit);
            assert_eq!(
                receiver.message_budget.bytes.available_permits(),
                MAX_PENDING_CHANNEL_BYTES
            );
            sender
                .channel(peer, RELIABLE_LABEL)
                .unwrap()
                .send_text("text after binary")
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let EngineEvent::ChannelMessage { text, .. } = events.recv().await.unwrap() {
                        assert_eq!(text, "text after binary");
                        break;
                    }
                }
            })
            .await
            .unwrap();
        }
    }
    pair.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn godot_transport_diagnostics_distinguish_reassembly_from_held_callback_polling() {
    for receiver_offers in [false, true] {
        let mut pair = unconnected_negotiated_pair();
        assert!(pair
            .receiver
            .transport_diagnostic_snapshot(pair.sender_id)
            .await
            .is_null());
        let mut previous_transport = None;
        for generation in 0u8..2 {
            pair.negotiate().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            let peer = pair.sender_id;
            let link = &pair.receiver.peers[&peer];
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
            let admission = link.receive_admission.clone();
            let physical_generation = link.generation;
            let initial = pair.receiver.transport_diagnostic_snapshot(peer).await;
            assert_eq!(initial["sctp"]["data_chunks_received"], 0);
            for field in [
                "data_chunks_accepted",
                "data_chunks_redundant",
                "data_chunks_refused",
            ] {
                assert_eq!(initial["sctp"][field], 0);
            }
            let initial_transport = initial["transports"].as_array().unwrap();
            assert_eq!(initial_transport.len(), 1);
            let initial_transport = &initial_transport[0];
            let transport_id = initial_transport["id"].as_str().unwrap();
            assert!(!transport_id.is_empty());
            let identity = (physical_generation, transport_id.to_owned());
            assert_ne!(previous_transport.as_ref(), Some(&identity));
            previous_transport = Some(identity);
            assert!(matches!(
                initial_transport["ice_state"].as_str(),
                Some("connected" | "completed")
            ));
            assert_eq!(initial_transport["dtls_state"], "connected");
            let initial_packets = initial_transport["packets_received"].as_u64().unwrap();
            let initial_bytes = initial_transport["bytes_received"].as_u64().unwrap();
            assert!(initial_packets > 0);
            assert!(initial_bytes > 0);
            let initial_stats = initial["transport_channels"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["label"] == RELIABLE_LABEL)
                .unwrap();
            assert_eq!(initial_stats["messages_received"], 0);
            assert_eq!(initial_stats["bytes_received"], 0);
            let channel = pair
                .sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap();
            let packets: Vec<Vec<u8>> = (0u8..4)
                .map(|sequence| {
                    let mut packet = vec![0xff; 4096];
                    packet[..3].copy_from_slice(&[generation, sequence, 0]);
                    packet
                })
                .collect();
            for packet in &packets {
                channel.send(packet.as_slice().into()).await.unwrap();
            }
            let held = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let snapshot = pair.receiver.transport_diagnostic_snapshot(peer).await;
                    let stats = snapshot["transport_channels"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|entry| entry["label"] == RELIABLE_LABEL)
                        .unwrap();
                    if stats["messages_received"] == 4 {
                        assert_eq!(stats["bytes_received"], 16384);
                        break snapshot;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("RTC receives the whole snapshot while callback polling is held");
            assert_eq!(held["generation"], physical_generation);
            assert!(held["sctp"]["data_chunks_received"].as_u64().unwrap() > 4);
            let accepted = held["sctp"]["data_chunks_accepted"].as_u64().unwrap();
            let redundant = held["sctp"]["data_chunks_redundant"].as_u64().unwrap();
            let refused = held["sctp"]["data_chunks_refused"].as_u64().unwrap();
            assert!(
                accepted > 4,
                "four fragmented messages require more than four admitted chunks"
            );
            assert_eq!(
                held["sctp"]["data_chunks_received"],
                accepted + redundant + refused
            );
            let held_transport = &held["transports"][0];
            assert_eq!(held_transport["id"], initial_transport["id"]);
            assert!(held_transport["packets_received"].as_u64().unwrap() > initial_packets);
            assert!(held_transport["bytes_received"].as_u64().unwrap() >= initial_bytes + 16384);
            assert_eq!(
                held["channels"][RELIABLE_LABEL]["receipts"],
                serde_json::json!({"raw":0,"admitted":0,"rejected":0,"discarded":0})
            );
            let resumed = spawn_channel_event_loop(
                &pair.receiver.runtime,
                &pair.receiver.events,
                peer,
                physical_generation,
                RELIABLE_LABEL.into(),
                pair.receiver.channel(peer, RELIABLE_LABEL).unwrap(),
                admission,
            );
            pair.receiver
                .peers
                .get_mut(&peer)
                .unwrap()
                .channel_tasks
                .push(resumed);
            tokio::time::timeout(Duration::from_secs(5), async {
                let mut received = 0;
                while received < packets.len() {
                    let event = pair.receiver_events.recv().await.unwrap();
                    if !pair.receiver.is_current_event(&event) {
                        continue;
                    }
                    match event {
                        EngineEvent::ChannelBinaryMessage { label, data, .. } => {
                            assert_eq!(label, RELIABLE_LABEL);
                            assert_eq!(data, packets[received]);
                            received += 1;
                        }
                        EngineEvent::ChannelReceiveRejected { .. }
                        | EngineEvent::ChannelClosed { .. } => {
                            panic!("held polling must retain reliable input")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            let drained = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let snapshot = pair.receiver.transport_diagnostic_snapshot(peer).await;
                    if snapshot["channels"][RELIABLE_LABEL]["receipts"]["admitted"] == 4 {
                        break snapshot;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("receipt counters settle after callback publication");
            assert_eq!(
                drained["channels"][RELIABLE_LABEL]["receipts"],
                serde_json::json!({"raw":4,"admitted":4,"rejected":0,"discarded":0})
            );
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            pair.receiver.remove_peer(peer).await.unwrap();
            assert!(pair
                .receiver
                .transport_diagnostic_snapshot(peer)
                .await
                .is_null());
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn godot_profile_preserves_startup_snapshot_prefix_in_both_fresh_generations() {
    let mut pair = unconnected_negotiated_pair();
    for generation in 0u8..2 {
        let offer = pair
            .sender
            .pair_with(pair.receiver_id, true, &[])
            .await
            .unwrap()
            .unwrap();
        pair.receiver
            .pair_with(pair.sender_id, false, &[])
            .await
            .unwrap();
        let answer = pair
            .receiver
            .handle_offer(pair.sender_id, offer)
            .await
            .unwrap();
        pair.sender
            .handle_answer(pair.receiver_id, answer)
            .await
            .unwrap();
        let payloads: Vec<Vec<u8>> = (0u8..128)
            .map(|sequence| {
                let mut packet = vec![0xff; 4096];
                packet[..3].copy_from_slice(&[generation, sequence, 0]);
                packet
            })
            .collect();
        let mut received = [0usize; 2];
        let mut sent = [false; 2];
        tokio::time::timeout(Duration::from_secs(15), async {
            while received != [128, 128] {
                let (seat, event) = tokio::select! {
                    event = pair.sender_events.recv() => (0, event.unwrap()),
                    event = pair.receiver_events.recv() => (1, event.unwrap()),
                };
                let (engine, peer) = if seat == 0 {
                    (&mut pair.sender, pair.receiver_id)
                } else {
                    (&mut pair.receiver, pair.sender_id)
                };
                if !engine.is_current_event(&event) { continue; }
                match event {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        let (remote, source) = if seat == 0 {
                            (&mut pair.receiver, pair.sender_id)
                        } else {
                            (&mut pair.sender, pair.receiver_id)
                        };
                        remote.handle_remote_candidate(source, &candidate_json).await.unwrap();
                    }
                    EngineEvent::ChannelOpen { label, .. } if label == RELIABLE_LABEL => {
                        assert!(!sent[seat], "one startup burst per physical generation");
                        sent[seat] = true;
                        let channel = engine.channel(peer, RELIABLE_LABEL).unwrap();
                        for packet in &payloads {
                            channel.send(packet.as_slice().into()).await.unwrap();
                        }
                    }
                    EngineEvent::ChannelBinaryMessage { label, data, .. } => {
                        assert_eq!(label, RELIABLE_LABEL);
                        assert_eq!(data, payloads[received[seat]], "generation={generation} seat={seat}: startup must retain the reliable prefix");
                        received[seat] += 1;
                    }
                    EngineEvent::ChannelReceiveRejected { .. } | EngineEvent::ChannelClosed { .. } => panic!("startup must keep every admitted reliable packet"),
                    _ => {}
                }
            }
        }).await.expect("both startup snapshot bursts arrive in full");
        assert_eq!(sent, [true, true]);
        pair.sender.remove_peer(pair.receiver_id).await.unwrap();
        pair.receiver.remove_peer(pair.sender_id).await.unwrap();
    }
}
