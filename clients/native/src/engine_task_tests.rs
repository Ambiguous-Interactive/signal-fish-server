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
                            .create_data_channel(label, None)
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
