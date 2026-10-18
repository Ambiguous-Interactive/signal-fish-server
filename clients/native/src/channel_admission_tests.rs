use super::*;
use rtc::data_channel::RTCDataChannelState;

// Regression #911: a required label cannot weaken delivery guarantees.
#[tokio::test]
async fn required_remote_channels_reject_wrong_delivery_modes() {
    let cases = [
        (RELIABLE_LABEL, false, None, None),
        (RELIABLE_LABEL, true, Some(0), None),
        (RELIABLE_LABEL, true, Some(3), None),
        (RELIABLE_LABEL, true, None, Some(0)),
        (RELIABLE_LABEL, true, None, Some(1)),
        (UNRELIABLE_LABEL, true, Some(0), None),
        (UNRELIABLE_LABEL, false, None, None),
        (UNRELIABLE_LABEL, false, Some(3), None),
        (UNRELIABLE_LABEL, false, None, Some(0)),
        (UNRELIABLE_LABEL, false, None, Some(1)),
    ];
    for through_callback in [true, false] {
        for (label, ordered, max_retransmits, max_packet_life_time) in cases {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let mut engine = Engine::new(
                EngineSettings {
                    crippled: true,
                    disable_mdns: true,
                    ..EngineSettings::default()
                },
                tx.clone(),
            )
            .unwrap();
            let peer = PlayerId::from_u128(0x918);
            engine.pair_with(peer, false, &[]).await.unwrap();
            let link = &engine.peers[&peer];
            let pc = link.pc.clone();
            let handler = PeerHandler {
                peer,
                generation: link.generation,
                crippled: true,
                events: tx,
                channel_admission: link.channel_admission.clone(),
                receive_admission: link.receive_admission.clone(),
            };
            let invalid = pc
                .create_data_channel(
                    label,
                    Some(RTCDataChannelInit {
                        ordered,
                        max_retransmits,
                        max_packet_life_time,
                        ..RTCDataChannelInit::default()
                    }),
                )
                .await
                .unwrap();
            if through_callback {
                handler.on_data_channel(invalid.clone()).await;
            } else {
                engine
                    .store_remote_channel(peer, label.into(), invalid.clone())
                    .await;
            }
            assert!(matches!(invalid.ready_state().await.unwrap(), RTCDataChannelState::Closing | RTCDataChannelState::Closed),
                "wrong mode admitted: callback={through_callback}, label={label}, ordered={ordered}, retries={max_retransmits:?}, lifetime={max_packet_life_time:?}");
            assert!(rx.is_empty(), "rejected channel reached the event queue");
            assert!(engine.peers[&peer].channels.is_empty());
            assert!(engine.peers[&peer].channel_tasks.is_empty());
            let valid = pc
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
            handler.on_data_channel(valid.clone()).await;
            let EngineEvent::RemoteChannel {
                label: announced,
                channel,
                ..
            } = rx.recv().await.unwrap()
            else {
                panic!("valid replacement must announce");
            };
            engine.store_remote_channel(peer, announced, channel).await;
            assert!(
                Arc::ptr_eq(&valid, &engine.channel(peer, label).unwrap()),
                "invalid first announcement reserved the label"
            );
            assert_eq!(engine.peers[&peer].channel_tasks.len(), 1);
            engine.remove_peer(peer).await.unwrap();
        }
    }
}

// Exercise DCEP-decoded properties, rather than only local channel handles.
#[tokio::test]
async fn live_remote_delivery_modes_preserve_required_channels() {
    use futures_util::{stream::FuturesUnordered, StreamExt};
    let (a_tx, mut a_rx) = mpsc::unbounded_channel();
    let (b_tx, mut b_rx) = mpsc::unbounded_channel();
    let settings = EngineSettings {
        disable_mdns: true,
        ..EngineSettings::default()
    };
    let mut a = Engine::new(settings, a_tx).unwrap();
    let mut b = Engine::new(settings, b_tx).unwrap();
    let a_id = PlayerId::from_u128(0x911a);
    let b_id = PlayerId::from_u128(0x911b);
    a.pair_with(b_id, false, &[]).await.unwrap();
    b.pair_with(a_id, false, &[]).await.unwrap();
    let pc = a.peers[&b_id].pc.clone();
    // Offer every channel before any closure, avoiding the independent #872
    // stream-ID reuse defect while verifying this admission contract.
    let mut rejected = Vec::new();
    for (label, ordered, max_retransmits, max_packet_life_time) in [
        (RELIABLE_LABEL, false, None, None),
        (RELIABLE_LABEL, true, Some(0), None),
        (RELIABLE_LABEL, true, Some(3), None),
        (RELIABLE_LABEL, true, None, Some(0)),
        (RELIABLE_LABEL, true, None, Some(1)),
        (UNRELIABLE_LABEL, true, Some(0), None),
        (UNRELIABLE_LABEL, false, None, None),
        (UNRELIABLE_LABEL, false, Some(3), None),
        (UNRELIABLE_LABEL, false, None, Some(0)),
        (UNRELIABLE_LABEL, false, None, Some(1)),
    ] {
        let channel = pc
            .create_data_channel(
                label,
                Some(RTCDataChannelInit {
                    ordered,
                    max_retransmits,
                    max_packet_life_time,
                    ..RTCDataChannelInit::default()
                }),
            )
            .await
            .unwrap();
        rejected.push(channel);
    }
    let mut required = Vec::new();
    for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
        required.push((
            label,
            pc.create_data_channel(
                label,
                Some(RTCDataChannelInit {
                    ordered: label == RELIABLE_LABEL,
                    max_retransmits: (label == UNRELIABLE_LABEL).then_some(0),
                    ..RTCDataChannelInit::default()
                }),
            )
            .await
            .unwrap(),
        ));
    }
    let offer = pc.create_offer(None).await.unwrap();
    let sdp = offer.sdp.clone();
    pc.set_local_description(offer).await.unwrap();
    let answer = b.handle_offer(a_id, sdp).await.unwrap();
    a.handle_answer(b_id, answer).await.unwrap();
    let mut closures: FuturesUnordered<_> = rejected
        .into_iter()
        .enumerate()
        .map(|(index, channel)| async move {
            loop {
                if matches!(
                    channel.poll().await.expect("rejected channel emits close"),
                    DataChannelEvent::OnClose
                ) {
                    return index;
                }
            }
        })
        .collect();
    let mut closed = BTreeSet::new();
    let mut announced = BTreeSet::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !closures.is_empty() || b.connected_pair_count() != 1 {
            tokio::select! {
                index = closures.next(), if !closures.is_empty() => {
                    assert!(closed.insert(index.unwrap()), "duplicate close");
                }
                event = a_rx.recv() => match event.unwrap() {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        b.handle_remote_candidate(a_id, &candidate_json).await.unwrap();
                    }
                    EngineEvent::PcState { state: RTCPeerConnectionState::Failed, .. } => panic!("sender link failed"),
                    _ => {}
                },
                event = b_rx.recv() => match event.unwrap() {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        a.handle_remote_candidate(b_id, &candidate_json).await.unwrap();
                    }
                    EngineEvent::RemoteChannel { label, channel, .. } => {
                        assert!(announced.insert(label.clone()), "invalid mode reached application queue");
                        assert!(required_channel_delivery_mode(&label, channel.as_ref()).await, "remote DCEP mode lost");
                        b.store_remote_channel(a_id, label, channel).await;
                    }
                    EngineEvent::ChannelOpen { label, .. } => { b.note_channel_open(a_id, &label).await; }
                    EngineEvent::ChannelClosed { .. } => panic!("required channel closed"),
                    EngineEvent::PcState { state: RTCPeerConnectionState::Failed, .. } => panic!("receiver link failed"),
                    _ => {}
                },
            }
        }
    }).await.expect("real DCEP admission and refusal complete");
    assert_eq!(closed.len(), 10);
    assert_eq!(
        announced,
        BTreeSet::from([RELIABLE_LABEL.to_string(), UNRELIABLE_LABEL.to_string()])
    );
    for (label, channel) in required {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(channel.poll().await.unwrap(), DataChannelEvent::OnOpen) {
                    break;
                }
            }
            channel.send_text(label).await.unwrap();
            loop {
                match b_rx.recv().await.unwrap() {
                    EngineEvent::ChannelMessage {
                        label: received,
                        text,
                        ..
                    } => {
                        assert_eq!(received, label);
                        assert_eq!(text, label);
                        break;
                    }
                    EngineEvent::RemoteChannel { .. } | EngineEvent::ChannelClosed { .. } => {
                        panic!("rejected channel published callbacks")
                    }
                    _ => {}
                }
            }
            b.channel(a_id, label)
                .unwrap()
                .send_text("reply")
                .await
                .unwrap();
            loop {
                if let DataChannelEvent::OnMessage(message) = channel.poll().await.unwrap() {
                    assert_eq!(message.data.as_ref(), b"reply");
                    break;
                }
            }
        })
        .await
        .expect("both required modes carry bidirectional gameplay");
    }
    assert_eq!(b.peers[&a_id].channels.len(), 2);
    assert_eq!(b.peers[&a_id].channel_tasks.len(), 2);
    a.remove_peer(b_id).await.unwrap();
    b.remove_peer(a_id).await.unwrap();
    assert!(a.peers.is_empty() && b.peers.is_empty());
}
