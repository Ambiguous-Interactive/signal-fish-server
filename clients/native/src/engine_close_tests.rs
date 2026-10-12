use super::{receive_tests::LivePair, *};

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
        while let Ok(event) = events.try_recv() {
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
