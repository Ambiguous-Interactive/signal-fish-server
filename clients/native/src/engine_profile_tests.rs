use super::{receive_tests::LivePair, *};
use crate::socket_tests::TRANSPORT_OBSERVATION_POLL_INTERVAL;

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
            assert_eq!(pair.receiver.connected_pair_count(), 1);
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
            assert_eq!(pair.receiver.connected_pair_count(), 0);
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
            assert_eq!(pair.receiver.connected_pair_count(), 1);
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
/// Separate SCTP reassembly, retained callbacks, and exact application admission in both roles.
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
                "data_chunks_emitted",
                "data_chunks_retransmitted",
                "messages_abandoned",
            ] {
                assert_eq!(initial["sctp"][field], 0);
            }
            for field in [
                "last_enqueued_message",
                "first_abandoned_message",
                "last_abandoned_message",
            ] {
                assert!(initial["sctp"][field].is_null());
            }
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
                    tokio::time::sleep(TRANSPORT_OBSERVATION_POLL_INTERVAL).await;
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
                    tokio::time::sleep(TRANSPORT_OBSERVATION_POLL_INTERVAL).await;
                }
            })
            .await
            .expect("receipt counters settle after callback publication");
            assert_eq!(
                drained["channels"][RELIABLE_LABEL]["receipts"],
                serde_json::json!({"raw":4,"admitted":4,"rejected":0,"discarded":0})
            );
            let sender_snapshot = pair
                .sender
                .transport_diagnostic_snapshot(pair.receiver_id)
                .await;
            assert_eq!(sender_snapshot["sctp"]["messages_abandoned"], 0);
            assert!(sender_snapshot["sctp"]["first_abandoned_message"].is_null());
            assert_eq!(
                sender_snapshot["sctp"]["last_enqueued_message"]["stream_id"],
                1
            );
            assert_eq!(
                sender_snapshot["sctp"]["last_enqueued_message"]["reliability"],
                "reliable"
            );
            assert!(
                sender_snapshot["sctp"]["data_chunks_emitted"]
                    .as_u64()
                    .unwrap()
                    >= packets.len() as u64
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Drain exact reliable packets after held callbacks and SCTP backpressure in both roles.
async fn godot_reliable_prefix_survives_saturated_callback_queues_and_fresh_pair() {
    const PACKETS: u32 = 2048;
    const PACKET_BYTES: usize = 4096;
    for receiver_offers in [false, true] {
        let mut pair = unconnected_negotiated_pair();
        for generation in 0u8..2 {
            pair.negotiate().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            let peer = pair.sender_id;
            let link = pair.receiver.peers.get_mut(&peer).unwrap();
            hold_channel_callbacks(&link.channel_tasks).await;
            let channel = pair
                .sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap();
            let mut payload = vec![0xff; PACKET_BYTES];
            payload[4] = generation;
            let mut sent = 0u32;
            let send_result = tokio::time::timeout(Duration::from_secs(15), async {
                for sequence in 0..PACKETS {
                    payload[..4].copy_from_slice(&sequence.to_be_bytes());
                    channel.send(payload.as_slice().into()).await.unwrap();
                    sent += 1;
                }
            })
            .await;
            if let Err(error) = send_result {
                let timeout_snapshot = pair.receiver.transport_diagnostic_snapshot(peer).await;
                panic!("unbounded send admission retains the complete offered burst: {error}; receiver_offers={receiver_offers} payload_generation={generation} sent={sent}/{PACKETS} packet_bytes={PACKET_BYTES} timeout_snapshot={timeout_snapshot}");
            }
            let mut last_snapshot = serde_json::Value::Null;
            let mut polls = 0;
            let wait_started = std::time::Instant::now();
            let held = match tokio::time::timeout(
                Duration::from_secs(15),
                wait_for_held_sctp_refusal(
                    || pair.receiver.transport_diagnostic_snapshot(peer),
                    &mut last_snapshot,
                    &mut polls,
                ),
            )
            .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    let wait_us = wait_started.elapsed().as_micros();
                    let sender_snapshot = pair
                        .sender
                        .transport_diagnostic_snapshot(pair.receiver_id)
                        .await;
                    panic!("held application polling fills downstream queues and exhausts SCTP receive credit: {error}; receiver_offers={receiver_offers} payload_generation={generation} sent={PACKETS} packet_bytes={PACKET_BYTES} polls={polls} wait_us={wait_us} last_snapshot={last_snapshot} sender_snapshot={sender_snapshot}");
                }
            };
            println!("GODOT_HELD_REFUSAL receiver_offers={receiver_offers} payload_generation={generation} polls={polls} wait_us={}", wait_started.elapsed().as_micros());
            let received = held["transport_channels"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["label"] == RELIABLE_LABEL)
                .unwrap()["messages_received"]
                .as_u64()
                .unwrap();
            assert!(
                (256..u64::from(PACKETS)).contains(&received),
                "the reliable callback queue is full while a tail stays upstream: {held}"
            );
            assert!(channel.outstanding_bytes().await.unwrap() > 0);
            assert_eq!(held["channels"][RELIABLE_LABEL]["receipts"]["raw"], 0);
            let receiver = pair.receiver.channel(peer, RELIABLE_LABEL).unwrap();
            tokio::time::timeout(Duration::from_secs(15), async {
                let mut received = 0;
                while received < PACKETS {
                    match receiver.poll().await.expect("the channel remains readable") {
                        DataChannelEvent::OnMessage(message) => {
                            assert!(!message.is_string);
                            payload[..4].copy_from_slice(&received.to_be_bytes());
                            assert_eq!(message.data.as_ref(), payload.as_slice(), "generation={generation}, receiver_offers={receiver_offers}, sequence={received}");
                            received += 1;
                        }
                        DataChannelEvent::OnError | DataChannelEvent::OnClosing | DataChannelEvent::OnClose => panic!("resumed reliable transport must drain without loss"),
                        _ => {}
                    }
                }
                while channel.outstanding_bytes().await.unwrap() != 0 {
                    tokio::time::sleep(TRANSPORT_OBSERVATION_POLL_INTERVAL).await;
                }
            }).await.expect("resuming channel polling drains every exact reliable packet and sender credit");
            assert_eq!(
                pair.receiver
                    .channel_received_messages(peer, RELIABLE_LABEL)
                    .await,
                PACKETS
            );
            assert_eq!(
                receiver.ready_state().await.unwrap(),
                rtc::data_channel::RTCDataChannelState::Open
            );
            println!("GODOT_CALLBACK_SATURATION receiver_offers={receiver_offers} generation={generation} packets={PACKETS} bytes={} reassembled_while_held={received} refused_chunks={}", u64::from(PACKETS) * u64::try_from(PACKET_BYTES).unwrap(), held["sctp"]["data_chunks_refused"]);
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            pair.receiver.remove_peer(peer).await.unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Verify exact byte and message admission limits, permit release, and fresh-link recovery.
async fn godot_receive_limits_distinguish_byte_and_message_credit_before_fresh_recovery() {
    for receiver_offers in [false, true] {
        for (packet_bytes, retained_packets) in [(8192, 128), (2048, 256)] {
            let mut pair = unconnected_negotiated_pair();
            let receiver_budget = if receiver_offers {
                pair.sender.message_budget.clone()
            } else {
                pair.receiver.message_budget.clone()
            };
            let mut previous_generation = None;
            for release_one in [false, true] {
                pair.negotiate().await;
                if receiver_offers {
                    std::mem::swap(&mut pair.sender, &mut pair.receiver);
                    std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                    std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                }
                assert!(Arc::ptr_eq(&receiver_budget, &pair.receiver.message_budget));
                let peer = pair.sender_id;
                let generation = pair.receiver.peers[&peer].generation;
                assert_ne!(previous_generation, Some(generation));
                previous_generation = Some(generation);
                let channel = pair
                    .sender
                    .channel(pair.receiver_id, RELIABLE_LABEL)
                    .unwrap();
                let mut payload = vec![0xff; packet_bytes];
                payload[4] = u8::from(release_one);
                let mut retained = Vec::new();
                tokio::time::timeout(Duration::from_secs(15), async {
                    for sequence in 0..retained_packets {
                        payload[..4]
                            .copy_from_slice(&u32::try_from(sequence).unwrap().to_be_bytes());
                        channel.send(payload.as_slice().into()).await.unwrap();
                        loop {
                            let event = pair.receiver_events.recv().await.unwrap();
                            if !pair.receiver.is_current_event(&event) {
                                continue;
                            }
                            match event {
                                event @ EngineEvent::ChannelBinaryMessage { .. } => {
                                    let EngineEvent::ChannelBinaryMessage { label, data, .. } =
                                        &event
                                    else {
                                        unreachable!()
                                    };
                                    assert_eq!(label, RELIABLE_LABEL);
                                    assert_eq!(data, &payload);
                                    retained.push(event);
                                    break;
                                }
                                EngineEvent::ChannelReceiveRejected { .. }
                                | EngineEvent::ChannelClosed { .. } => panic!(
                                    "all packets within both receive limits must remain admitted"
                                ),
                                _ => {}
                            }
                        }
                    }
                })
                .await
                .expect("exact binary receipts fill one receive-credit dimension");
                assert_eq!(retained.len(), retained_packets);
                assert_eq!(
                    pair.receiver.message_budget.messages.available_permits(),
                    MAX_PENDING_CHANNEL_MESSAGES - retained_packets
                );
                assert_eq!(
                    pair.receiver.message_budget.bytes.available_permits(),
                    MAX_PENDING_CHANNEL_BYTES - packet_bytes * retained_packets
                );
                if release_one {
                    drop(retained.pop().unwrap());
                }
                payload[..4]
                    .copy_from_slice(&u32::try_from(retained_packets).unwrap().to_be_bytes());
                tokio::time::timeout(
                    Duration::from_secs(5),
                    channel.send(payload.as_slice().into()),
                )
                .await
                .unwrap()
                .unwrap();
                tokio::time::timeout(Duration::from_secs(15), async {
                    loop {
                        let event = pair.receiver_events.recv().await.unwrap();
                        if !pair.receiver.is_current_event(&event) {
                            continue;
                        }
                        match event {
                            event @ EngineEvent::ChannelBinaryMessage { .. } => {
                                assert!(
                                    release_one,
                                    "an exhausted application budget cannot admit another receipt"
                                );
                                let EngineEvent::ChannelBinaryMessage { label, data, .. } = &event
                                else {
                                    unreachable!()
                                };
                                assert_eq!(label, RELIABLE_LABEL);
                                assert_eq!(data, &payload);
                                retained.push(event);
                                break;
                            }
                            EngineEvent::ChannelReceiveRejected { label, .. } => {
                                assert!(
                                    !release_one,
                                    "releasing one real receipt restores capacity"
                                );
                                assert_eq!(label, RELIABLE_LABEL);
                                break;
                            }
                            EngineEvent::ChannelClosed { label, .. } => {
                                panic!("a physical close is not a receive-limit verdict: {label}")
                            }
                            _ => {}
                        }
                    }
                })
                .await
                .expect("the next receipt has an exact admission or rejection verdict");
                let admitted = retained_packets + usize::from(release_one);
                let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let snapshot = pair.receiver.diagnostic_snapshot(peer);
                        if snapshot["channels"][RELIABLE_LABEL]["receipts"]["admitted"] == admitted
                        {
                            break snapshot;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("published receipt counters settle");
                assert_eq!(
                    snapshot["receive_rejection"],
                    if release_one {
                        serde_json::Value::Null
                    } else {
                        serde_json::json!(if packet_bytes == 8192 {
                            "pending_bytes"
                        } else {
                            "pending_messages"
                        })
                    }
                );
                assert_eq!(
                    snapshot["channels"][RELIABLE_LABEL]["receipts"],
                    serde_json::json!({
                        "raw": retained_packets + 1, "admitted": admitted,
                        "rejected": usize::from(!release_one), "discarded": 0,
                    })
                );
                assert_eq!(
                    *pair.receiver.peers[&peer]
                        .receive_admission
                        .rejected
                        .lock()
                        .await,
                    !release_one
                );
                assert_eq!(
                    pair.receiver.peers[&peer]
                        .receive_admission
                        .terminal
                        .load(Ordering::Relaxed),
                    !release_one
                );
                assert_eq!(retained.len(), retained_packets);
                assert_eq!(
                    pair.receiver.message_budget.messages.available_permits(),
                    MAX_PENDING_CHANNEL_MESSAGES - retained_packets
                );
                assert_eq!(
                    pair.receiver.message_budget.bytes.available_permits(),
                    MAX_PENDING_CHANNEL_BYTES - packet_bytes * retained_packets
                );
                if release_one {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while channel.outstanding_bytes().await.unwrap() != 0 {
                            tokio::time::sleep(TRANSPORT_OBSERVATION_POLL_INTERVAL).await;
                        }
                    })
                    .await
                    .expect("all accepted reliable packets release sender credit");
                    assert_eq!(
                        channel.ready_state().await.unwrap(),
                        rtc::data_channel::RTCDataChannelState::Open
                    );
                    assert_eq!(pair.receiver.peers[&peer].generation, generation);
                } else {
                    assert!(pair
                        .receiver
                        .send_channel(peer, RELIABLE_LABEL)
                        .await
                        .is_none());
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            if let EngineEvent::ChannelClosed { peer, label, .. } =
                                pair.sender_events.recv().await.unwrap()
                            {
                                assert_eq!(peer, pair.receiver_id);
                                assert_eq!(label, RELIABLE_LABEL);
                                break;
                            }
                        }
                    })
                    .await
                    .expect("intentional admission refusal closes the remote reliable channel");
                }
                drop(retained);
                assert_eq!(
                    pair.receiver.message_budget.messages.available_permits(),
                    MAX_PENDING_CHANNEL_MESSAGES
                );
                assert_eq!(
                    pair.receiver.message_budget.bytes.available_permits(),
                    MAX_PENDING_CHANNEL_BYTES
                );
                pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                pair.receiver.remove_peer(peer).await.unwrap();
                if receiver_offers {
                    std::mem::swap(&mut pair.sender, &mut pair.receiver);
                    std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                    std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                }
                println!("GODOT_RECEIVE_CREDIT receiver_offers={receiver_offers} bytes={packet_bytes} retained={retained_packets} release_one={release_one} admitted={admitted} rejected={}", usize::from(!release_one));
            }
        }
    }
}

/// Stop every callback poller before offering a held-queue workload.
async fn hold_channel_callbacks(tasks: &[Box<dyn JoinHandle>]) {
    for task in tasks {
        task.abort();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while !tasks.iter().all(|task| task.is_finished()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all callback pollers finish cancellation before sending");
}

/// #1319: abort requests do not prove task completion or a held queue.
#[tokio::test]
async fn held_callbacks_wait_for_every_cooperative_cancellation_before_sending() {
    struct HeldTask {
        aborted: Arc<std::sync::atomic::AtomicBool>,
        finished: Arc<std::sync::atomic::AtomicBool>,
    }
    impl JoinHandle for HeldTask {
        fn detach(&self) {}
        fn abort(&self) {
            self.aborted.store(true, Ordering::SeqCst);
        }
        fn is_finished(&self) -> bool {
            self.finished.load(Ordering::SeqCst)
        }
    }
    let states: Vec<_> = (0..2)
        .map(|_| {
            (
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
        })
        .collect();
    let tasks: Vec<Box<dyn JoinHandle>> = states
        .iter()
        .map(|(aborted, finished)| {
            Box::new(HeldTask {
                aborted: aborted.clone(),
                finished: finished.clone(),
            }) as Box<dyn JoinHandle>
        })
        .collect();
    let held = hold_channel_callbacks(&tasks);
    tokio::pin!(held);
    assert!(
        futures_util::poll!(&mut held).is_pending(),
        "an abort request does not establish a held callback queue"
    );
    assert!(states
        .iter()
        .all(|(aborted, _)| aborted.load(Ordering::SeqCst)));
    states[0].1.store(true, Ordering::SeqCst);
    assert!(
        futures_util::poll!(&mut held).is_pending(),
        "every poller must finish before the burst starts"
    );
    states[1].1.store(true, Ordering::SeqCst);
    held.await;
}

/// Sample held transport state without repeatedly taking its driver's shared core lock.
/// The caller owns the deadline and retains the last failed sample.
async fn wait_for_held_sctp_refusal<Sample, Snapshot>(
    mut sample: Sample,
    last_snapshot: &mut serde_json::Value,
    polls: &mut u64,
) -> serde_json::Value
where
    Sample: FnMut() -> Snapshot,
    Snapshot: std::future::Future<Output = serde_json::Value>,
{
    loop {
        let snapshot = sample().await;
        *polls = polls.saturating_add(1);
        if snapshot["sctp"]["data_chunks_refused"]
            .as_u64()
            .is_some_and(|count| count > 0)
        {
            return snapshot;
        }
        *last_snapshot = snapshot;
        tokio::time::sleep(TRANSPORT_OBSERVATION_POLL_INTERVAL).await;
    }
}

/// A failed observer must leave the shared core available to an already-woken worker.
#[tokio::test(start_paused = true)]
async fn held_refusal_observation_allows_transport_progress_between_failed_snapshots() {
    let core = webrtc::runtime::Mutex::new(0_u64);
    let samples = std::cell::Cell::new(0_u32);
    let mut last_snapshot = serde_json::Value::Null;
    let mut polls = 0;
    let waiting = wait_for_held_sctp_refusal(
        || async {
            let refused = *core.lock().await;
            samples.set(samples.get() + 1);
            serde_json::json!({"sctp": {"data_chunks_refused": refused}})
        },
        &mut last_snapshot,
        &mut polls,
    );
    tokio::pin!(waiting);
    assert!(futures_util::poll!(&mut waiting).is_pending());
    assert_eq!(samples.get(), 1);
    let guard = core.lock().await;
    let progress = async {
        *core.lock().await = 1;
    };
    tokio::pin!(progress);
    assert!(futures_util::poll!(&mut progress).is_pending());
    drop(guard);
    assert!(futures_util::poll!(&mut waiting).is_pending());
    assert_eq!(
        samples.get(),
        1,
        "the observer cannot immediately retake the core"
    );
    assert!(futures_util::poll!(&mut progress).is_ready());
    tokio::time::advance(Duration::from_millis(10)).await;
    let snapshot = tokio::time::timeout(Duration::from_millis(20), waiting)
        .await
        .expect("the worker supplies an observed refusal before the test deadline");
    assert_eq!(snapshot["sctp"]["data_chunks_refused"], 1);
    assert_eq!(samples.get(), 2);
}

/// Preserve incomplete snapshots and wait until a positive integer refusal is observed.
#[tokio::test(start_paused = true)]
async fn held_refusal_observation_preserves_incomplete_evidence_until_positive_count() {
    for initial in [
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!({"sctp": null}),
        serde_json::json!({"sctp": {}}),
        serde_json::json!({"sctp": {"data_chunks_refused": null}}),
        serde_json::json!({"sctp": {"data_chunks_refused": 0}}),
        serde_json::json!({"sctp": {"data_chunks_refused": -1}}),
        serde_json::json!({"sctp": {"data_chunks_refused": 1.0}}),
        serde_json::json!({"sctp": {"data_chunks_refused": "1"}}),
        serde_json::json!({"sctp": {"data_chunks_refused": true}}),
    ] {
        let samples = std::cell::Cell::new(0_u32);
        let mut last_snapshot = serde_json::Value::Null;
        let mut polls = 0;
        let positive = serde_json::json!({"sctp": {"data_chunks_refused": u64::MAX}});
        let observed = {
            let waiting = wait_for_held_sctp_refusal(
                || {
                    let snapshot = if samples.replace(samples.get() + 1) == 0 {
                        initial.clone()
                    } else {
                        positive.clone()
                    };
                    std::future::ready(snapshot)
                },
                &mut last_snapshot,
                &mut polls,
            );
            tokio::pin!(waiting);
            assert!(futures_util::poll!(&mut waiting).is_pending(), "{initial}");
            assert!(futures_util::poll!(&mut waiting).is_pending(), "{initial}");
            assert_eq!(samples.get(), 1, "{initial}");
            tokio::time::advance(TRANSPORT_OBSERVATION_POLL_INTERVAL).await;
            tokio::time::timeout(Duration::from_millis(20), waiting)
                .await
                .expect("a later positive refusal ends the wait")
        };
        assert_eq!(observed, positive);
        assert_eq!(last_snapshot, initial);
        assert_eq!(polls, 2);
    }
}

/// Offer the fixed burst after pollers stop and wait for observed SCTP refusal.
async fn saturate_held_reliable_callbacks(
    pair: &mut LivePair,
    packet_bytes: usize,
    payload_generation: u8,
) -> (Arc<dyn DataChannel>, serde_json::Value) {
    let peer = pair.sender_id;
    let link = pair.receiver.peers.get_mut(&peer).unwrap();
    hold_channel_callbacks(&link.channel_tasks).await;
    let channel = pair
        .sender
        .channel(pair.receiver_id, RELIABLE_LABEL)
        .unwrap();
    let mut payload = vec![0xff; packet_bytes];
    payload[4] = payload_generation;
    let mut sent = 0u32;
    let mut last_snapshot = serde_json::Value::Null;
    let mut polls = 0;
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        for sequence in 0u32..2048 {
            payload[..4].copy_from_slice(&sequence.to_be_bytes());
            channel.send(payload.as_slice().into()).await.unwrap();
            sent += 1;
        }
        let snapshot = wait_for_held_sctp_refusal(
            || pair.receiver.transport_diagnostic_snapshot(peer),
            &mut last_snapshot,
            &mut polls,
        )
        .await;
        assert_eq!(snapshot["channels"][RELIABLE_LABEL]["receipts"]["raw"], 0);
        assert_eq!(snapshot["receive_budget"]["pending_messages"], 0);
        assert_eq!(snapshot["receive_budget"]["pending_bytes"], 0);
        snapshot
    })
    .await;
    let snapshot = match result {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let elapsed_us = started.elapsed().as_micros();
            let timeout_snapshot = pair.receiver.transport_diagnostic_snapshot(peer).await;
            let sender_snapshot = pair
                .sender
                .transport_diagnostic_snapshot(pair.receiver_id)
                .await;
            panic!("held callbacks exhaust actual SCTP receive credit: {error}; payload_generation={payload_generation} packet_bytes={packet_bytes} sent={sent}/2048 polls={polls} elapsed_us={elapsed_us} last_snapshot={last_snapshot} timeout_snapshot={timeout_snapshot} sender_snapshot={sender_snapshot}");
        }
    };
    println!("GODOT_HELD_REFUSAL payload_generation={payload_generation} packet_bytes={packet_bytes} polls={polls} elapsed_us={}", started.elapsed().as_micros());
    (channel, snapshot)
}

fn resume_reliable_callbacks(pair: &mut LivePair) {
    let peer = pair.sender_id;
    let link = &pair.receiver.peers[&peer];
    let resumed = spawn_channel_event_loop(
        &pair.receiver.runtime,
        &pair.receiver.events,
        peer,
        link.generation,
        RELIABLE_LABEL.into(),
        pair.receiver.channel(peer, RELIABLE_LABEL).unwrap(),
        link.receive_admission.clone(),
    );
    pair.receiver
        .peers
        .get_mut(&peer)
        .unwrap()
        .channel_tasks
        .push(resumed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn godot_saturated_engine_callbacks_report_exact_limit_and_recover_fresh_links() {
    for receiver_offers in [false, true] {
        for (packet_bytes, admitted_packets, reason) in [
            (4096, 256, "pending_messages"),
            (8192, 128, "pending_bytes"),
        ] {
            let mut pair = unconnected_negotiated_pair();
            pair.negotiate().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            assert_eq!(
                pair.receiver.peers[&pair.sender_id]
                    .pc
                    .local_description()
                    .await
                    .unwrap()
                    .sdp_type,
                if receiver_offers {
                    rtc::peer_connection::sdp::RTCSdpType::Offer
                } else {
                    rtc::peer_connection::sdp::RTCSdpType::Answer
                },
                "the receiver keeps its selected offer role"
            );
            let peer = pair.sender_id;
            let generation = pair.receiver.peers[&peer].generation;
            let (channel, held) =
                saturate_held_reliable_callbacks(&mut pair, packet_bytes, 0).await;
            println!("GODOT_ENGINE_HELD receiver_offers={receiver_offers} packet_bytes={packet_bytes} snapshot={held}");
            let mut payload = vec![0xff; packet_bytes];
            payload[4] = 0;
            resume_reliable_callbacks(&mut pair);
            tokio::time::timeout(Duration::from_secs(15), async {
                while pair.receiver.diagnostic_snapshot(peer)["receive_rejection"] != reason {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("held application events reach the unchanged receive limit");
            let snapshot = pair.receiver.transport_diagnostic_snapshot(peer).await;
            assert_eq!(
                snapshot["receive_budget"]["pending_messages"],
                admitted_packets
            );
            assert_eq!(snapshot["receive_budget"]["pending_bytes"], 1024 * 1024);
            assert_eq!(
                snapshot["receive_failure"],
                serde_json::json!({
                    "reason": reason,
                    "message_bytes": packet_bytes,
                    "pending_messages": admitted_packets,
                    "pending_bytes": 1024 * 1024,
                }),
                "rejection evidence must survive later backlog changes"
            );
            assert_eq!(
                snapshot["channels"][RELIABLE_LABEL]["receipts"],
                serde_json::json!({
                    "raw": admitted_packets + 1, "admitted": admitted_packets, "rejected": 1, "discarded": 0,
                })
            );
            let mut retained = Vec::new();
            let failure = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = pair.receiver_events.recv().await.unwrap();
                    if !pair.receiver.is_current_event(&event) {
                        continue;
                    }
                    match event {
                        event @ EngineEvent::ChannelBinaryMessage { .. } => {
                            let EngineEvent::ChannelBinaryMessage { label, data, .. } = &event
                            else {
                                unreachable!()
                            };
                            payload[..4].copy_from_slice(
                                &u32::try_from(retained.len()).unwrap().to_be_bytes(),
                            );
                            assert_eq!(label, RELIABLE_LABEL);
                            assert_eq!(
                                data, &payload,
                                "no reliable prefix may be silently skipped"
                            );
                            retained.push(event);
                        }
                        EngineEvent::ChannelReceiveRejected { label, failure, .. } => {
                            assert_eq!(label, RELIABLE_LABEL);
                            assert_eq!(retained.len(), admitted_packets);
                            assert_eq!(
                                serde_json::to_value(failure).unwrap(),
                                snapshot["receive_failure"]
                            );
                            break failure;
                        }
                        EngineEvent::ChannelClosed { label, .. } => {
                            panic!("closure cannot replace the limit verdict: {label}; {snapshot}")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            for label in [RELIABLE_LABEL, ORDERED_LABEL, UNRELIABLE_LABEL] {
                assert!(pair.receiver.send_channel(peer, label).await.is_none());
            }
            println!("GODOT_ENGINE_REJECTION receiver_offers={receiver_offers} packet_bytes={packet_bytes} sender_debt={} snapshot={snapshot}", channel.outstanding_bytes().await.unwrap());
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            drop(retained.pop());
            let drained = pair.receiver.diagnostic_snapshot(peer);
            assert_eq!(drained["receive_failure"], snapshot["receive_failure"]);
            assert_eq!(
                drained["receive_budget"]["pending_messages"],
                admitted_packets - 1
            );
            assert_eq!(
                serde_json::to_value(failure).unwrap(),
                drained["receive_failure"]
            );
            pair.receiver.remove_peer(peer).await.unwrap();
            assert!(pair.receiver.diagnostic_snapshot(peer).is_null());
            assert_eq!(
                serde_json::to_value(failure).unwrap(),
                snapshot["receive_failure"]
            );
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            pair.negotiate().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            assert_eq!(
                pair.receiver.peers[&pair.sender_id]
                    .pc
                    .local_description()
                    .await
                    .unwrap()
                    .sdp_type,
                if receiver_offers {
                    rtc::peer_connection::sdp::RTCSdpType::Offer
                } else {
                    rtc::peer_connection::sdp::RTCSdpType::Answer
                },
                "the receiver keeps its selected offer role"
            );
            assert_ne!(pair.receiver.peers[&peer].generation, generation);
            assert!(retained
                .iter()
                .all(|event| !pair.receiver.is_current_event(event)));
            let fresh = pair.receiver.diagnostic_snapshot(peer);
            assert!(fresh["receive_rejection"].is_null());
            assert_eq!(
                fresh["receive_budget"]["pending_messages"],
                admitted_packets - 1
            );
            assert_eq!(
                fresh["receive_budget"]["pending_bytes"],
                1024 * 1024 - packet_bytes
            );
            drop(retained);
            let fresh = pair.receiver.diagnostic_snapshot(peer);
            assert_eq!(fresh["receive_budget"]["pending_messages"], 0);
            assert_eq!(fresh["receive_budget"]["pending_bytes"], 0);
            exchange(&mut pair).await;
            pair.cleanup().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
/// Require exact reliable recovery or an explicit admission refusal across roles and generations.
async fn godot_active_consumer_classifies_saturated_recovery_without_silent_prefix_loss() {
    const PACKETS: u32 = 2048;
    const PACKET_BYTES: usize = 4096;
    for receiver_offers in [false, true] {
        let mut pair = unconnected_negotiated_pair();
        let mut previous_generation = None;
        for payload_generation in 0u8..2 {
            pair.negotiate().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
            let peer = pair.sender_id;
            let link = &pair.receiver.peers[&peer];
            let generation = link.generation;
            assert_ne!(previous_generation, Some(generation));
            previous_generation = Some(generation);
            assert_eq!(
                link.pc.local_description().await.unwrap().sdp_type,
                if receiver_offers {
                    rtc::peer_connection::sdp::RTCSdpType::Offer
                } else {
                    rtc::peer_connection::sdp::RTCSdpType::Answer
                }
            );
            let (channel, held) =
                saturate_held_reliable_callbacks(&mut pair, PACKET_BYTES, payload_generation).await;
            println!("GODOT_ACTIVE_HELD receiver_offers={receiver_offers} generation={generation} snapshot={held}");
            resume_reliable_callbacks(&mut pair);
            let mut received = 0u32;
            let mut expected = vec![0xff; PACKET_BYTES];
            expected[4] = payload_generation;
            let failure = tokio::time::timeout(Duration::from_secs(15), async {
                while received < PACKETS {
                    let event = pair.receiver_events.recv().await.unwrap();
                    if !pair.receiver.is_current_event(&event) { continue; }
                    match event {
                        EngineEvent::ChannelBinaryMessage { label, data, queue_permit, .. } => {
                            assert_eq!(label, RELIABLE_LABEL);
                            expected[..4].copy_from_slice(&received.to_be_bytes());
                            assert_eq!(data, expected, "receiver_offers={receiver_offers} generation={generation} sequence={received}");
                            received += 1;
                            drop(queue_permit);
                        }
                        EngineEvent::ChannelReceiveRejected { label, failure, .. } => {
                            assert_eq!(label, RELIABLE_LABEL);
                            assert_eq!(failure.message_bytes, PACKET_BYTES);
                            assert_eq!(failure.reason, ReceiveRejection::PendingMessages);
                            return Some(failure);
                        }
                        EngineEvent::ChannelClosed { label, .. } => {
                            panic!("active resume closed {label} without a receive verdict: {}", pair.receiver.diagnostic_snapshot(peer));
                        }
                        _ => {}
                    }
                }
                None
            }).await.unwrap_or_else(|error| panic!("active resume stalled at {received}/{PACKETS}: {error}; {}", pair.receiver.diagnostic_snapshot(peer)));
            tokio::time::timeout(Duration::from_secs(5), async {
                while pair.receiver.peers[&peer]
                    .receive_admission
                    .receipts(RELIABLE_LABEL)
                    .unwrap()
                    .admitted
                    .load(Ordering::Relaxed)
                    != u64::from(received)
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the producer classifies the last published receipt");
            let snapshot = pair.receiver.transport_diagnostic_snapshot(peer).await;
            let receipts = &snapshot["channels"][RELIABLE_LABEL]["receipts"];
            assert_eq!(receipts["admitted"], received);
            assert_eq!(receipts["raw"], received + u32::from(failure.is_some()));
            assert_eq!(receipts["rejected"], u32::from(failure.is_some()));
            assert_eq!(receipts["discarded"], 0);
            assert_eq!(
                snapshot["receive_failure"],
                serde_json::to_value(failure).unwrap()
            );
            assert_eq!(snapshot["receive_budget"]["pending_messages"], 0);
            assert_eq!(snapshot["receive_budget"]["pending_bytes"], 0);
            if failure.is_some() {
                for label in [RELIABLE_LABEL, ORDERED_LABEL, UNRELIABLE_LABEL] {
                    assert!(pair.receiver.send_channel(peer, label).await.is_none());
                }
            } else {
                tokio::time::timeout(Duration::from_secs(15), async {
                    while channel.outstanding_bytes().await.unwrap() != 0 {
                        tokio::time::sleep(TRANSPORT_OBSERVATION_POLL_INTERVAL).await;
                    }
                })
                .await
                .expect("complete reliable delivery clears sender debt");
                assert!(pair
                    .receiver
                    .send_channel(peer, RELIABLE_LABEL)
                    .await
                    .is_some());
            }
            println!("GODOT_ACTIVE_RESUME receiver_offers={receiver_offers} generation={generation} received={received} offered={PACKETS} failure={} sender_debt={} snapshot={snapshot}", serde_json::to_value(failure).unwrap(), channel.outstanding_bytes().await.unwrap());
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            pair.receiver.remove_peer(peer).await.unwrap();
            assert!(pair.receiver.diagnostic_snapshot(peer).is_null());
            if let Some(failure) = failure {
                assert_eq!(
                    serde_json::to_value(failure).unwrap(),
                    snapshot["receive_failure"],
                    "event evidence survives peer removal"
                );
            }
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
            }
        }
        pair.negotiate().await;
        exchange(&mut pair).await;
        pair.cleanup().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_pending_application_counts_retained_empty_binary_and_projected_text() {
    for godot in [false, true] {
        let mut pair = if godot {
            negotiated_pair().await
        } else {
            LivePair::connect().await
        };
        let mut retained = Vec::new();
        let mut pending_bytes = 0;
        for raw in [Vec::new(), vec![0xff; 16], "雪".as_bytes().to_vec()] {
            pair.sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap()
                .send(raw.as_slice().into())
                .await
                .unwrap();
            let event = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = pair.receiver_events.recv().await.unwrap();
                    if !pair.receiver.is_current_event(&event) {
                        continue;
                    }
                    match &event {
                        EngineEvent::ChannelBinaryMessage { data, label, .. } => {
                            assert!(godot);
                            assert_eq!(label, RELIABLE_LABEL);
                            assert_eq!(data, &raw);
                            pending_bytes += raw.len();
                            break event;
                        }
                        EngineEvent::ChannelMessage { text, label, .. } => {
                            assert!(!godot);
                            assert_eq!(label, RELIABLE_LABEL);
                            assert_eq!(text, &String::from_utf8_lossy(&raw));
                            pending_bytes += text.len();
                            break event;
                        }
                        EngineEvent::ChannelReceiveRejected { .. }
                        | EngineEvent::ChannelClosed { .. } => {
                            panic!("small retained messages must remain admitted")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("empty and non-UTF8 application receipts");
            retained.push(event);
            let snapshot = pair.receiver.diagnostic_snapshot(pair.sender_id);
            assert_eq!(
                snapshot["channels"][RELIABLE_LABEL]["pending_application"],
                serde_json::json!({
                    "messages": retained.len(), "bytes": pending_bytes,
                })
            );
            assert_eq!(
                snapshot["receive_budget"]["pending_messages"],
                retained.len()
            );
            assert_eq!(snapshot["receive_budget"]["pending_bytes"], pending_bytes);
        }
        drop(retained.remove(0));
        let snapshot = pair.receiver.diagnostic_snapshot(pair.sender_id);
        assert_eq!(
            snapshot["channels"][RELIABLE_LABEL]["pending_application"],
            serde_json::json!({"messages":2,"bytes":pending_bytes})
        );
        drop(retained);
        let snapshot = pair.receiver.diagnostic_snapshot(pair.sender_id);
        assert_eq!(
            snapshot["channels"][RELIABLE_LABEL]["pending_application"],
            serde_json::json!({"messages":0,"bytes":0})
        );
        assert_eq!(snapshot["receive_budget"]["pending_messages"], 0);
        assert_eq!(snapshot["receive_budget"]["pending_bytes"], 0);
        pair.cleanup().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closed_application_receiver_releases_channel_pending_usage() {
    let mut pair = negotiated_pair().await;
    pair.receiver_events.close();
    pair.sender
        .channel(pair.receiver_id, RELIABLE_LABEL)
        .unwrap()
        .send(b"cannot-publish".as_slice().into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = pair.receiver.diagnostic_snapshot(pair.sender_id);
            if snapshot["channels"][RELIABLE_LABEL]["receipts"]["discarded"] == 1 {
                assert_eq!(
                    snapshot["channels"][RELIABLE_LABEL]["pending_application"],
                    serde_json::json!({"messages":0,"bytes":0})
                );
                assert_eq!(snapshot["receive_budget"]["pending_messages"], 0);
                assert_eq!(snapshot["receive_budget"]["pending_bytes"], 0);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed publication returns all receive credit");
    pair.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn godot_required_close_keeps_first_observation_until_fresh_pair() {
    struct HeldOutput {
        started: Option<std::sync::mpsc::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
        bytes: Arc<std::sync::Mutex<Vec<u8>>>,
    }
    impl std::io::Write for HeldOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
                self.release.recv().unwrap();
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for receiver_offers in [false, true] {
        for label in [RELIABLE_LABEL, ORDERED_LABEL, UNRELIABLE_LABEL] {
            let mut pair = unconnected_negotiated_pair();
            pair.negotiate().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
            }
            let peer = pair.sender_id;
            let generation = pair.receiver.peers[&peer].generation;
            assert!(pair.receiver.diagnostic_snapshot(peer)["channel_closure"].is_null());
            let payload = vec![0xff; 4096];
            pair.sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap()
                .send(payload.as_slice().into())
                .await
                .unwrap();
            let (data, permit) = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let EngineEvent::ChannelBinaryMessage {
                        data, queue_permit, ..
                    } = pair.receiver_events.recv().await.unwrap()
                    {
                        break (data, queue_permit);
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(data, payload);
            let (started, waiting) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel();
            let output_bytes = Arc::new(std::sync::Mutex::new(Vec::new()));
            let output = crate::event_output::EventOutput::new(HeldOutput {
                started: Some(started),
                release: released,
                bytes: output_bytes.clone(),
            });
            output.emit_with_permit(
                &crate::events::Event::ChannelBinaryMessage {
                    peer,
                    generation,
                    label: RELIABLE_LABEL.into(),
                    data_base64: base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &data,
                    ),
                },
                permit,
            );
            waiting.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(
                pair.receiver.diagnostic_snapshot(peer)["channels"][RELIABLE_LABEL]
                    ["pending_application"],
                serde_json::json!({"messages":1,"bytes":4096})
            );
            pair.receiver
                .channel(peer, label)
                .unwrap()
                .close()
                .await
                .unwrap();
            let closed = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = pair.receiver_events.recv().await.unwrap();
                    if matches!(event, EngineEvent::ChannelClosed { .. }) {
                        break event;
                    }
                }
            })
            .await
            .expect("required channel closure is published");
            let EngineEvent::ChannelClosed {
                reason,
                label: closed_label,
                ..
            } = &closed
            else {
                unreachable!()
            };
            assert_eq!(closed_label, label);
            assert!(matches!(
                reason,
                ChannelCloseReason::CallbackClosing
                    | ChannelCloseReason::CallbackClosed
                    | ChannelCloseReason::ObservedClosing
                    | ChannelCloseReason::ObservedClosed
                    | ChannelCloseReason::StateUnavailable
            ));
            let snapshot = pair.receiver.diagnostic_snapshot(peer);
            assert_eq!(snapshot["channel_closure"]["label"], label);
            assert_eq!(
                snapshot["channel_closure"]["reason"],
                serde_json::to_value(reason).unwrap()
            );
            assert!(
                snapshot["receive_failure"].is_null(),
                "physical closure is not receive refusal"
            );
            for sibling in [RELIABLE_LABEL, ORDERED_LABEL, UNRELIABLE_LABEL] {
                assert!(pair.receiver.send_channel(peer, sibling).await.is_none());
            }
            assert_eq!(
                pair.receiver.diagnostic_snapshot(peer)["channel_closure"],
                snapshot["channel_closure"]
            );
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            pair.receiver.remove_peer(peer).await.unwrap();
            assert_eq!(closed.peer_generation(), (peer, generation));
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                snapshot["channel_closure"]["reason"],
                "queued terminal evidence survives removal"
            );
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
            }
            pair.negotiate().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
            }
            assert!(!pair.receiver.is_current_event(&closed));
            let fresh = pair.receiver.diagnostic_snapshot(peer);
            assert!(fresh["channel_closure"].is_null());
            assert_eq!(
                fresh["channels"][RELIABLE_LABEL]["pending_application"],
                serde_json::json!({"messages":0,"bytes":0})
            );
            assert_eq!(fresh["receive_budget"]["pending_messages"], 1);
            assert_eq!(fresh["receive_budget"]["pending_bytes"], 4096);
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), output.drained())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                pair.receiver.diagnostic_snapshot(peer)["receive_budget"]["pending_bytes"],
                0
            );
            let row: serde_json::Value =
                serde_json::from_slice(&output_bytes.lock().unwrap()).unwrap();
            assert_eq!(
                row["generation"], generation,
                "flushed gameplay must retain the old physical identity after rebuilding"
            );
            assert_ne!(row["generation"], fresh["generation"]);
            assert_eq!(row["peer"], peer.to_string());
            assert_eq!(
                row["data_base64"],
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &payload)
            );
            let payload = format!("fresh:{receiver_offers}:{label}:鱼\0").into_bytes();
            pair.sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap()
                .send(payload.as_slice().into())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match pair.receiver_events.recv().await.unwrap() {
                        EngineEvent::ChannelBinaryMessage { data, .. } => {
                            assert_eq!(data, payload);
                            break;
                        }
                        event
                            if pair.receiver.is_current_event(&event)
                                && matches!(
                                    event,
                                    EngineEvent::ChannelClosed { .. }
                                        | EngineEvent::ChannelReceiveRejected { .. }
                                ) =>
                        {
                            panic!("fresh link must deliver exact reliable gameplay")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            pair.cleanup().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transport_snapshot_preserves_actual_channel_contracts_in_both_profiles() {
    for godot in [false, true] {
        let pair = if godot {
            negotiated_pair().await
        } else {
            LivePair::connect().await
        };
        for (engine, peer) in [
            (&pair.sender, pair.receiver_id),
            (&pair.receiver, pair.sender_id),
        ] {
            let snapshot = engine.transport_diagnostic_snapshot(peer).await;
            let properties = snapshot["channel_properties"]
                .as_object()
                .expect("retain actual registered channel properties");
            assert_eq!(properties.len(), if godot { 3 } else { 2 });
            for (label, channel) in &engine.peers[&peer].channels {
                let label = label.as_str();
                let actual = &properties[label];
                assert_eq!(actual["internal_id"], channel.id());
                assert_eq!(actual["label"], label);
                assert_eq!(actual["protocol"], "");
                assert_eq!(actual["negotiated"], godot);
                assert_eq!(actual["ordered"], label != UNRELIABLE_LABEL);
                assert_eq!(
                    actual["max_packet_life_time"],
                    if godot && label != RELIABLE_LABEL {
                        serde_json::json!(1)
                    } else {
                        serde_json::Value::Null
                    }
                );
                assert_eq!(
                    actual["max_retransmits"],
                    if !godot && label == UNRELIABLE_LABEL {
                        serde_json::json!(0)
                    } else {
                        serde_json::Value::Null
                    }
                );
                assert!(actual.get("error").is_none());
                let stats = snapshot["transport_channels"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["label"] == label)
                    .unwrap();
                if godot {
                    assert_eq!(
                        stats["stream_id"],
                        match label {
                            RELIABLE_LABEL => 1,
                            ORDERED_LABEL => 2,
                            _ => 3,
                        }
                    );
                }
            }
        }
        pair.cleanup().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn godot_terminal_connection_callback_keeps_first_state_until_fresh_pair() {
    for first in [
        RTCPeerConnectionState::Failed,
        RTCPeerConnectionState::Closed,
    ] {
        for receiver_offers in [false, true] {
            let mut pair = negotiated_pair().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
            }
            let peer = pair.sender_id;
            let link = &pair.receiver.peers[&peer];
            let generation = link.generation;
            let handler = PeerHandler {
                peer,
                generation,
                crippled: false,
                events: pair.receiver.events.clone(),
                channel_admission: link.channel_admission.clone(),
                receive_admission: link.receive_admission.clone(),
            };
            handler
                .on_connection_state_change(RTCPeerConnectionState::Disconnected)
                .await;
            assert!(pair.receiver.diagnostic_snapshot(peer)["connection_terminal"].is_null());
            assert_eq!(pair.receiver.connected_pair_count(), 1);
            handler.on_connection_state_change(first).await;
            handler
                .on_connection_state_change(RTCPeerConnectionState::Closed)
                .await;
            handler
                .on_connection_state_change(RTCPeerConnectionState::Failed)
                .await;
            let snapshot = pair.receiver.diagnostic_snapshot(peer);
            assert_eq!(snapshot["connection_terminal"], first.to_string());
            assert!(
                snapshot["channel_closure"].is_null(),
                "a PC callback does not invent a channel closure"
            );
            assert!(snapshot["receive_failure"].is_null());
            assert_eq!(pair.receiver.connected_pair_count(), 0);
            for label in [RELIABLE_LABEL, ORDERED_LABEL, UNRELIABLE_LABEL] {
                assert!(pair.receiver.send_channel(peer, label).await.is_none());
            }
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            pair.receiver.remove_peer(peer).await.unwrap();
            pair.negotiate().await;
            assert_ne!(pair.receiver.peers[&peer].generation, generation);
            assert!(pair.receiver.diagnostic_snapshot(peer)["connection_terminal"].is_null());
            handler.on_connection_state_change(first).await;
            assert!(
                pair.receiver.diagnostic_snapshot(peer)["connection_terminal"].is_null(),
                "retired callbacks cannot alter fresh-link evidence"
            );
            assert_eq!(pair.receiver.connected_pair_count(), 1);
            exchange(&mut pair).await;
            pair.cleanup().await;
        }
    }
}

#[tokio::test]
async fn transport_snapshot_preserves_retained_channel_contracts_after_driver_close() {
    let (events, _receiver) = mpsc::unbounded_channel();
    let mut engine = Engine::new(
        EngineSettings {
            crippled: true,
            channel_profile: ChannelProfile::Godot,
            ..EngineSettings::default()
        },
        events,
    )
    .unwrap();
    let peer = PlayerId::from_u128(0x570);
    assert!(engine.transport_diagnostic_snapshot(peer).await.is_null());
    engine.pair_with(peer, false, &[]).await.unwrap();
    let before = engine.transport_diagnostic_snapshot(peer).await;
    assert_eq!(before["channel_properties"].as_object().unwrap().len(), 3);
    assert_eq!(
        before["channel_properties"][RELIABLE_LABEL]["negotiated"],
        true
    );
    engine.peers[&peer].pc.close().await.unwrap();
    let after = tokio::time::timeout(
        Duration::from_secs(5),
        engine.transport_diagnostic_snapshot(peer),
    )
    .await
    .unwrap();
    assert_eq!(
        after["channel_properties"], before["channel_properties"],
        "closed channel objects still report their actual immutable contract"
    );
    for label in [RELIABLE_LABEL, ORDERED_LABEL, UNRELIABLE_LABEL] {
        let stats = after["transport_channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["label"] == label)
            .unwrap();
        assert_eq!(stats["state"], "closed");
    }
    engine.remove_peer(peer).await.unwrap();
    assert!(engine.transport_diagnostic_snapshot(peer).await.is_null());
}
