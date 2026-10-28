use super::*;

struct ReceiveMesh {
    center: Engine,
    senders: [Engine; 2],
    center_events: mpsc::UnboundedReceiver<EngineEvent>,
    sender_events: [mpsc::UnboundedReceiver<EngineEvent>; 2],
    center_id: PlayerId,
    sender_ids: [PlayerId; 2],
}

impl ReceiveMesh {
    fn new() -> Self {
        let settings = EngineSettings {
            disable_mdns: true,
            channel_profile: ChannelProfile::Godot,
            ..EngineSettings::default()
        };
        let (center_tx, center_events) = mpsc::unbounded_channel();
        let (first_tx, first_events) = mpsc::unbounded_channel();
        let (second_tx, second_events) = mpsc::unbounded_channel();
        Self {
            center: Engine::new(settings, center_tx).unwrap(),
            senders: [
                Engine::new(settings, first_tx).unwrap(),
                Engine::new(settings, second_tx).unwrap(),
            ],
            center_events,
            sender_events: [first_events, second_events],
            center_id: PlayerId::from_u128(0x1264),
            sender_ids: [PlayerId::from_u128(0x1240), PlayerId::from_u128(0x1244)],
        }
    }

    async fn pair(&mut self, index: usize, center_offers: bool) {
        let peer = self.sender_ids[index];
        if center_offers {
            let offer = self
                .center
                .pair_with(peer, true, &[])
                .await
                .unwrap()
                .unwrap();
            self.senders[index]
                .pair_with(self.center_id, false, &[])
                .await
                .unwrap();
            let answer = self.senders[index]
                .handle_offer(self.center_id, offer)
                .await
                .unwrap();
            self.center.handle_answer(peer, answer).await.unwrap();
        } else {
            let offer = self.senders[index]
                .pair_with(self.center_id, true, &[])
                .await
                .unwrap()
                .unwrap();
            self.center.pair_with(peer, false, &[]).await.unwrap();
            let answer = self.center.handle_offer(peer, offer).await.unwrap();
            self.senders[index]
                .handle_answer(self.center_id, answer)
                .await
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut center_open = false;
            let mut sender_open = false;
            while !center_open || !sender_open {
                tokio::select! {
                    event = self.center_events.recv() => {
                        let event = event.unwrap();
                        if !self.center.is_current_event(&event) { continue; }
                        match event {
                            EngineEvent::LocalCandidate { peer: target, candidate_json, .. } => {
                                let target_index = self.sender_ids.iter().position(|id| *id == target).unwrap();
                                self.senders[target_index].handle_remote_candidate(self.center_id, &candidate_json).await.unwrap();
                            }
                            EngineEvent::ChannelOpen { peer: target, label, .. } if target == peer => {
                                center_open |= self.center.note_channel_open(peer, &label).await;
                            }
                            EngineEvent::ChannelBinaryMessage { .. } | EngineEvent::ChannelMessage { .. } => panic!("pairing must not discard gameplay"),
                            _ => {}
                        }
                    }
                    event = self.sender_events[index].recv() => {
                        let event = event.unwrap();
                        if !self.senders[index].is_current_event(&event) { continue; }
                        match event {
                            EngineEvent::LocalCandidate { candidate_json, .. } => {
                                self.center.handle_remote_candidate(peer, &candidate_json).await.unwrap();
                            }
                            EngineEvent::ChannelOpen { label, .. } => {
                                sender_open |= self.senders[index].note_channel_open(self.center_id, &label).await;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }).await.expect("both mesh links open all three negotiated channels");
    }

    async fn send(&mut self, index: usize, sequence: usize, bytes: usize) -> EngineEvent {
        let mut expected = vec![0xff; bytes];
        expected[..8].copy_from_slice(&u64::try_from(sequence).unwrap().to_be_bytes());
        expected[8] = u8::try_from(index).unwrap();
        self.senders[index]
            .channel(self.center_id, RELIABLE_LABEL)
            .unwrap()
            .send(expected.as_slice().into())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = self.center_events.recv().await.unwrap();
                if !self.center.is_current_event(&event) {
                    continue;
                }
                match &event {
                    EngineEvent::ChannelBinaryMessage {
                        peer, label, data, ..
                    } => {
                        assert_eq!(*peer, self.sender_ids[index]);
                        assert_eq!(label, RELIABLE_LABEL);
                        assert_eq!(
                            data, &expected,
                            "every reliable body must retain its sender and sequence"
                        );
                        return event;
                    }
                    EngineEvent::ChannelReceiveRejected { .. }
                    | EngineEvent::ChannelClosed { .. } => {
                        panic!("mesh receipt cannot be replaced by terminal input")
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("exact mesh reliable receipt")
    }

    fn assert_pending(&self, index: usize, messages: usize, bytes: usize) {
        let snapshot = self.center.diagnostic_snapshot(self.sender_ids[index]);
        assert_eq!(
            snapshot["channels"][RELIABLE_LABEL]["pending_application"],
            serde_json::json!({
                "messages": messages, "bytes": bytes,
            })
        );
        for label in [ORDERED_LABEL, UNRELIABLE_LABEL] {
            assert_eq!(
                snapshot["channels"][label]["pending_application"],
                serde_json::json!({"messages":0,"bytes":0})
            );
        }
    }

    async fn remove(&mut self, index: usize) {
        self.senders[index]
            .remove_peer(self.center_id)
            .await
            .unwrap();
        self.center
            .remove_peer(self.sender_ids[index])
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn godot_mesh_receive_owners_survive_shared_limit_refusal_and_fresh_links() {
    for center_offers in [false, true] {
        for (bytes, per_sender, reason) in [
            (16, 128, ReceiveRejection::PendingMessages),
            (8192, 64, ReceiveRejection::PendingBytes),
        ] {
            let mut mesh = ReceiveMesh::new();
            for index in 0..2 {
                mesh.pair(index, center_offers).await;
            }
            assert_eq!(mesh.center.connected_pair_count(), 2);
            let mut retained = Vec::new();
            for index in 0..2 {
                for sequence in 0..per_sender {
                    retained.push(mesh.send(index, sequence, bytes).await);
                }
                mesh.assert_pending(index, per_sender, bytes * per_sender);
            }
            mesh.senders[1]
                .channel(mesh.center_id, RELIABLE_LABEL)
                .unwrap()
                .send(b"refused".as_slice().into())
                .await
                .unwrap();
            let failure = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = mesh.center_events.recv().await.unwrap();
                    if !mesh.center.is_current_event(&event) {
                        continue;
                    }
                    match event {
                        EngineEvent::ChannelReceiveRejected {
                            peer,
                            label,
                            failure,
                            ..
                        } => {
                            assert_eq!(peer, mesh.sender_ids[1]);
                            assert_eq!(label, RELIABLE_LABEL);
                            break failure;
                        }
                        EngineEvent::ChannelBinaryMessage { .. }
                        | EngineEvent::ChannelClosed { .. } => {
                            panic!("shared limit must produce the typed refusal")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("shared receive limit refusal");
            assert_eq!(failure.reason, reason);
            assert_eq!(
                mesh.center.connected_pair_count(),
                1,
                "receive refusal must exclude only the terminal physical link"
            );
            assert_eq!(failure.pending_messages, 2 * per_sender);
            assert_eq!(failure.pending_bytes, 2 * per_sender * bytes);
            for index in 0..2 {
                mesh.assert_pending(index, per_sender, bytes * per_sender);
            }
            for label in [RELIABLE_LABEL, ORDERED_LABEL, UNRELIABLE_LABEL] {
                assert!(mesh
                    .center
                    .send_channel(mesh.sender_ids[1], label)
                    .await
                    .is_none());
                assert!(mesh
                    .center
                    .send_channel(mesh.sender_ids[0], label)
                    .await
                    .is_some());
            }
            drop(retained.remove(0));
            mesh.assert_pending(0, per_sender - 1, bytes * (per_sender - 1));
            retained.push(mesh.send(0, per_sender, bytes).await);
            mesh.assert_pending(0, per_sender, bytes * per_sender);
            let old_generations = mesh
                .sender_ids
                .map(|peer| mesh.center.peers[&peer].generation);
            for index in 0..2 {
                mesh.remove(index).await;
            }
            for (index, old_generation) in old_generations.into_iter().enumerate() {
                mesh.pair(index, center_offers).await;
                assert_ne!(
                    mesh.center.peers[&mesh.sender_ids[index]].generation,
                    old_generation
                );
                mesh.assert_pending(index, 0, 0);
                let snapshot = mesh.center.diagnostic_snapshot(mesh.sender_ids[index]);
                assert_eq!(
                    snapshot["receive_budget"]["pending_messages"],
                    2 * per_sender
                );
                assert_eq!(
                    snapshot["receive_budget"]["pending_bytes"],
                    2 * per_sender * bytes
                );
                assert!(snapshot["receive_failure"].is_null());
            }
            assert_eq!(mesh.center.connected_pair_count(), 2);
            assert!(retained
                .iter()
                .all(|event| !mesh.center.is_current_event(event)));
            drop(retained);
            for index in 0..2 {
                for sequence in 0..4 {
                    drop(mesh.send(index, sequence, bytes).await);
                }
                mesh.assert_pending(index, 0, 0);
                let snapshot = mesh.center.diagnostic_snapshot(mesh.sender_ids[index]);
                assert_eq!(snapshot["receive_budget"]["pending_messages"], 0);
                assert_eq!(snapshot["receive_budget"]["pending_bytes"], 0);
                let reverse = format!("reverse-{center_offers}-{bytes}-{index}");
                mesh.center
                    .channel(mesh.sender_ids[index], RELIABLE_LABEL)
                    .unwrap()
                    .send_text(&reverse)
                    .await
                    .unwrap();
                let reverse_event = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let event = mesh.sender_events[index].recv().await.unwrap();
                        if !mesh.senders[index].is_current_event(&event) {
                            continue;
                        }
                        match &event {
                            EngineEvent::ChannelMessage {
                                peer, label, text, ..
                            } => {
                                assert_eq!(*peer, mesh.center_id);
                                assert_eq!(label, RELIABLE_LABEL);
                                assert_eq!(text, &reverse);
                                break event;
                            }
                            EngineEvent::ChannelReceiveRejected { .. }
                            | EngineEvent::ChannelClosed { .. } => {
                                panic!("fresh reverse gameplay must arrive")
                            }
                            _ => {}
                        }
                    }
                })
                .await
                .expect("fresh mesh links carry exact reverse gameplay");
                let reverse_snapshot = mesh.senders[index].diagnostic_snapshot(mesh.center_id);
                assert_eq!(
                    reverse_snapshot["channels"][RELIABLE_LABEL]["pending_application"],
                    serde_json::json!({"messages":1,"bytes":reverse.len()})
                );
                drop(reverse_event);
                let reverse_snapshot = mesh.senders[index].diagnostic_snapshot(mesh.center_id);
                assert_eq!(
                    reverse_snapshot["channels"][RELIABLE_LABEL]["pending_application"],
                    serde_json::json!({"messages":0,"bytes":0})
                );
                println!("GODOT_MESH_RECOVERY center_offers={center_offers} bytes={bytes} peer={} snapshot={snapshot}", mesh.sender_ids[index]);
            }
            for index in 0..2 {
                mesh.remove(index).await;
            }
        }
    }
}
