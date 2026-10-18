use super::*;

// Regression #876: a stalled orchestrator must not retain every required-label
// announcement. These are real stack channel objects, before any network open.
#[tokio::test]
async fn required_channel_announcements_are_bounded_before_queue_drain() {
    let (engine_tx, _engine_rx) = mpsc::unbounded_channel();
    let settings = EngineSettings {
        crippled: true,
        ip_family: IpFamily::Ipv4,
        ..EngineSettings::default()
    };
    let mut engine = Engine::new(settings, engine_tx).unwrap();
    let peer = PlayerId::from_u128(0x870);
    for initiate in [false, true] {
        for _ in 0..3 {
            let (tx, mut rx) = mpsc::unbounded_channel();
            engine.pair_with(peer, initiate, &[]).await.unwrap();
            let pc = engine.peers[&peer].pc.clone();
            let handler = handler_for(&engine, peer, tx);
            let mut first = HashMap::new();
            if initiate {
                for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                    first.insert(label.to_string(), engine.channel(peer, label).unwrap());
                }
            }
            for index in 0..32 {
                for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                    let channel = pc
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
                    handler.on_data_channel(channel.clone()).await;
                    if !initiate && index == 0 {
                        first.insert(label.to_string(), channel);
                    } else {
                        assert_closed(&channel).await;
                    }
                }
            }
            assert_eq!(
                rx.len(),
                if initiate { 0 } else { 2 },
                "initiate={initiate}: only first remote channels may queue"
            );
            for channel in first.values() {
                // Repeating the exact selected object is an idempotent callback.
                handler.on_data_channel(channel.clone()).await;
                assert_eq!(
                    channel.ready_state().await.unwrap(),
                    rtc::data_channel::RTCDataChannelState::Connecting
                );
            }
            if !initiate {
                // A direct caller must not steal an unread callback's selection.
                for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                    let channel = pc
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
                    engine
                        .store_remote_channel(peer, label.into(), channel.clone())
                        .await;
                    assert_closed(&channel).await;
                    assert!(engine.peers[&peer].channels.is_empty());
                }
            }
            for _ in 0..if initiate { 0 } else { 2 } {
                let event = rx
                    .try_recv()
                    .expect("selected channel announcement is queued");
                let EngineEvent::RemoteChannel { label, channel, .. } = event else {
                    panic!("unexpected event before selected channel admission")
                };
                engine.store_remote_channel(peer, label, channel).await;
            }
            let remaining = rx.try_recv();
            assert!(matches!(remaining, Err(mpsc::error::TryRecvError::Empty)));
            assert_eq!(engine.peers[&peer].channels.len(), 2);
            assert_eq!(engine.peers[&peer].channel_tasks.len(), 2);
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                assert!(Arc::ptr_eq(
                    &first[label],
                    &engine.channel(peer, label).unwrap()
                ));
                first[label].close().await.unwrap();
                let duplicate = pc
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
                handler.on_data_channel(duplicate.clone()).await;
                assert_closed(&duplicate).await;
            }
            assert!(
                rx.is_empty(),
                "queue drain and closure must not renew admission"
            );
            let weak: Vec<_> = first.values().map(Arc::downgrade).collect();
            drop(first);
            engine.remove_peer(peer).await.unwrap();
            drop(pc);
            tokio::time::timeout(Duration::from_secs(1), async {
                while weak.iter().any(|channel| channel.strong_count() != 0) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("admission identities must not retain retired channels");
            // The old handler remains alive until here. Its weak slots cannot
            // retain channel objects or consume the next generation's budget.
            drop(handler);
        }
    }
}

#[tokio::test]
async fn concurrent_required_announcements_select_one_channel_per_label() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (engine_tx, _engine_rx) = mpsc::unbounded_channel();
    let mut engine = Engine::new(
        EngineSettings {
            crippled: true,
            ..EngineSettings::default()
        },
        engine_tx,
    )
    .unwrap();
    let peer = PlayerId::from_u128(0x876);
    engine.pair_with(peer, false, &[]).await.unwrap();
    let handler = handler_for(&engine, peer, tx);
    let mut channels = Vec::new();
    for _ in 0..32 {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            channels.push(
                engine.peers[&peer]
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
                    .unwrap(),
            );
        }
    }
    futures_util::future::join_all(
        channels
            .iter()
            .map(|channel| handler.on_data_channel(channel.clone())),
    )
    .await;
    assert_eq!(rx.len(), 2);
    let mut selected = HashMap::new();
    for _ in 0..2 {
        let event = rx
            .try_recv()
            .expect("selected channel announcement is queued");
        let EngineEvent::RemoteChannel { label, channel, .. } = event else {
            panic!("unexpected event from concurrent announcement callbacks")
        };
        assert!(selected.insert(label, channel).is_none());
    }
    let remaining = rx.try_recv();
    assert!(matches!(remaining, Err(mpsc::error::TryRecvError::Empty)));
    for channel in channels {
        let label = channel.label().await.unwrap();
        if !Arc::ptr_eq(&selected[&label], &channel) {
            assert_closed(&channel).await;
        }
    }
    for (label, channel) in selected {
        engine.store_remote_channel(peer, label, channel).await;
    }
    assert_eq!(engine.peers[&peer].channel_tasks.len(), 2);
    engine.remove_peer(peer).await.unwrap();
}

#[tokio::test]
async fn required_announcements_close_when_event_receiver_is_gone() {
    let (tx, rx) = mpsc::unbounded_channel();
    drop(rx);
    let (engine_tx, _engine_rx) = mpsc::unbounded_channel();
    let mut engine = Engine::new(
        EngineSettings {
            crippled: true,
            ..EngineSettings::default()
        },
        engine_tx,
    )
    .unwrap();
    let peer = PlayerId::from_u128(0x876);
    engine.pair_with(peer, false, &[]).await.unwrap();
    let handler = handler_for(&engine, peer, tx);
    for _ in 0..2 {
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            let channel = engine.peers[&peer]
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
            handler.on_data_channel(channel.clone()).await;
            assert_closed(&channel).await;
        }
    }
    assert!(engine.peers[&peer].channels.is_empty());
    assert!(engine.peers[&peer].channel_tasks.is_empty());
    engine.remove_peer(peer).await.unwrap();
}

fn handler_for(
    engine: &Engine,
    peer: PlayerId,
    events: mpsc::UnboundedSender<EngineEvent>,
) -> PeerHandler {
    let link = &engine.peers[&peer];
    PeerHandler {
        peer,
        generation: link.generation,
        crippled: true,
        events,
        channel_admission: link.channel_admission.clone(),
        receive_admission: link.receive_admission.clone(),
    }
}

async fn assert_closed(channel: &Arc<dyn DataChannel>) {
    assert!(
        matches!(
            channel.ready_state().await,
            Ok(rtc::data_channel::RTCDataChannelState::Closing
                | rtc::data_channel::RTCDataChannelState::Closed)
        ),
        "refused announcement must close its channel"
    );
}

#[tokio::test]
async fn timeout_snapshot_tracks_local_observations_and_replacement() {
    let (events, _receiver) = mpsc::unbounded_channel();
    let settings = EngineSettings {
        crippled: true,
        disable_mdns: true,
        ip_family: IpFamily::Ipv4,
        ..EngineSettings::default()
    };
    let mut engine = Engine::new(settings, events).unwrap();
    let peer = PlayerId::from_u128(12);
    assert!(engine.diagnostic_snapshot(peer).is_null());
    for initiate in [false, true] {
        let offer = engine.pair_with(peer, initiate, &[]).await.unwrap();
        let generation = engine.peers[&peer].generation;
        let snapshot = engine.diagnostic_snapshot(peer);
        assert_eq!(snapshot["generation"], generation);
        assert_eq!(snapshot["remote_description_set"], false);
        assert_eq!(snapshot["remote_candidate_count"], 0);
        assert_eq!(snapshot["pending_remote_candidates"], 0);
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            assert_eq!(snapshot["channels"][label]["registered"], initiate);
            assert_eq!(snapshot["channels"][label]["open_observed"], false);
        }
        let candidate = r#"{"candidate":"candidate:1 1 udp 2130706431 127.0.0.1 9 typ host","sdpMid":"0","sdpMLineIndex":0}"#;
        engine
            .handle_remote_candidate(peer, candidate)
            .await
            .unwrap();
        let snapshot = engine.diagnostic_snapshot(peer);
        assert_eq!(snapshot["remote_candidate_count"], 1);
        assert_eq!(snapshot["pending_remote_candidates"], 1);
        assert!(!snapshot.to_string().contains("127.0.0.1"));
        assert!(!engine.note_channel_open(peer, RELIABLE_LABEL).await);
        let snapshot = engine.diagnostic_snapshot(peer);
        assert_eq!(snapshot["channels"][RELIABLE_LABEL]["open_observed"], true);
        assert_eq!(snapshot["channels"][RELIABLE_LABEL]["registered"], initiate);
        assert_eq!(
            snapshot["channels"][UNRELIABLE_LABEL]["open_observed"],
            false
        );
        let (events, _receiver) = mpsc::unbounded_channel();
        let mut remote = Engine::new(settings, events).unwrap();
        let me = PlayerId::from_u128(13);
        let remote_offer = remote.pair_with(me, !initiate, &[]).await.unwrap();
        if initiate {
            let answer = remote.handle_offer(me, offer.unwrap()).await.unwrap();
            engine.handle_answer(peer, answer).await.unwrap();
        } else {
            let answer = engine
                .handle_offer(peer, remote_offer.unwrap())
                .await
                .unwrap();
            remote.handle_answer(me, answer).await.unwrap();
        }
        let snapshot = engine.diagnostic_snapshot(peer);
        assert_eq!(snapshot["remote_description_set"], true);
        assert_eq!(snapshot["remote_candidate_count"], 1);
        assert_eq!(snapshot["pending_remote_candidates"], 0);
        remote.remove_peer(me).await.unwrap();
        engine.remove_peer(peer).await.unwrap();
        assert!(engine.diagnostic_snapshot(peer).is_null());
    }
}
