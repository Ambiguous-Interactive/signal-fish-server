use super::*;

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
