use super::*;
use std::time::Duration;

struct ProbePair {
    engines: [Engine; 2],
    events: [mpsc::UnboundedReceiver<EngineEvent>; 2],
    peers: [PlayerId; 2],
}

impl ProbePair {
    fn new() -> Self {
        let (first, first_events) = mpsc::unbounded_channel();
        let (second, second_events) = mpsc::unbounded_channel();
        let settings = EngineSettings {
            disable_mdns: true,
            channel_profile: ChannelProfile::Godot,
            ..EngineSettings::default()
        };
        Self {
            engines: [
                Engine::new(settings, first).unwrap(),
                Engine::new(settings, second).unwrap(),
            ],
            events: [first_events, second_events],
            peers: [PlayerId::from_u128(0x5731), PlayerId::from_u128(0x5732)],
        }
    }

    async fn negotiate(&mut self, offerer: usize) {
        let answerer = 1 - offerer;
        let offer = self.engines[offerer]
            .pair_with(self.peers[offerer], true, &[])
            .await
            .unwrap()
            .unwrap();
        self.engines[answerer]
            .pair_with(self.peers[answerer], false, &[])
            .await
            .unwrap();
        let answer = self.engines[answerer]
            .handle_offer(self.peers[answerer], offer)
            .await
            .unwrap();
        self.engines[offerer]
            .handle_answer(self.peers[offerer], answer)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            let mut connected = [false; 2];
            while !connected.iter().all(|connected| *connected) {
                let [first, second] = &mut self.events;
                let (seat, event) = tokio::select! {
                    event = first.recv() => (0, event.unwrap()),
                    event = second.recv() => (1, event.unwrap()),
                };
                if !self.engines[seat].is_current_event(&event) {
                    continue;
                }
                match event {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        self.engines[1 - seat]
                            .handle_remote_candidate(self.peers[1 - seat], &candidate_json)
                            .await
                            .unwrap();
                    }
                    EngineEvent::ChannelOpen { label, .. } => {
                        connected[seat] |= self.engines[seat]
                            .note_channel_open(self.peers[seat], &label)
                            .await;
                    }
                    EngineEvent::ChannelClosed { .. }
                    | EngineEvent::ChannelReceiveRejected { .. } => panic!("fresh link must open"),
                    _ => {}
                }
            }
        })
        .await
        .expect("both negotiated peers open all required channels");
    }

    async fn remove(&mut self) {
        for seat in 0..2 {
            self.engines[seat]
                .remove_peer(self.peers[seat])
                .await
                .unwrap();
        }
    }

    async fn receipt(&mut self, receiver: usize, payload: &[u8]) -> EngineEvent {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = self.events[receiver].recv().await.unwrap();
                if !self.engines[receiver].is_current_event(&event) {
                    continue;
                }
                match &event {
                    EngineEvent::ChannelBinaryMessage { label, data, .. } => {
                        assert_eq!(label, "reliable");
                        assert_eq!(data, payload);
                        return event;
                    }
                    EngineEvent::ChannelMessage { .. }
                    | EngineEvent::ChannelClosed { .. }
                    | EngineEvent::ChannelReceiveRejected { .. } => {
                        panic!("exact reliable binary receipt required")
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("exact reliable probe payload reaches its peer")
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn packet_commands_refuse_terminal_siblings_and_recover_on_fresh_links() {
    for offerer in [0, 1] {
        for closed_label in ["reliable", "ordered", "unreliable"] {
            println!("GODOT_PROBE_CLOSE offerer={offerer} label={closed_label}");
            let mut pair = ProbePair::new();
            pair.negotiate(offerer).await;
            let (output, mut rows) = mpsc::unbounded_channel();
            let payload = [0, 0xff, 0x80, 1].repeat(1024);
            let packet = json!({"op":"send_binary", "label":"reliable", "data_base64":STANDARD.encode(&payload)});
            send_packet(&output, &pair.engines[0], pair.peers[0], &packet)
                .await
                .unwrap();
            let retained = pair.receipt(1, &payload).await;
            let old_generation = retained.peer_generation().1;
            assert_eq!(rows.try_recv().unwrap().0["event"], "sent");
            pair.engines[0]
                .channel(pair.peers[0], closed_label)
                .unwrap()
                .close()
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while pair.engines[0].diagnostic_snapshot(pair.peers[0])["channel_closure"]
                    .is_null()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let surviving_label = if closed_label == "reliable" {
                "ordered"
            } else {
                "reliable"
            };
            let before = pair.engines[0]
                .transport_diagnostic_snapshot(pair.peers[0])
                .await;
            for packet in [
                json!({"op":"send", "label":surviving_label, "text":"refused-after-sibling-close"}),
                json!({"op":"send_binary", "label":surviving_label, "data_base64":STANDARD.encode(&payload)}),
            ] {
                let result = send_packet(&output, &pair.engines[0], pair.peers[0], &packet).await;
                assert!(result.is_err(), "offerer={offerer} closed={closed_label}: probe must refuse terminal-link command: {result:?}");
                assert!(
                    matches!(rows.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                    "refused commands must not emit sent rows"
                );
            }
            assert!(
                send_startup_packet(&pair.engines[0], pair.peers[0], b"refused-startup")
                    .await
                    .is_err()
            );
            let after = pair.engines[0]
                .transport_diagnostic_snapshot(pair.peers[0])
                .await;
            assert!(
                before["sctp"]["last_enqueued_message"].is_object(),
                "live SCTP enqueue evidence is required: {before}"
            );
            assert!(
                after["sctp"]["last_enqueued_message"].is_object(),
                "live SCTP enqueue evidence is required: {after}"
            );
            assert_eq!(
                before["sctp"]["last_enqueued_message"],
                after["sctp"]["last_enqueued_message"]
            );
            pair.remove().await;
            pair.negotiate(offerer).await;
            assert_ne!(
                pair.engines[1].diagnostic_snapshot(pair.peers[1])["generation"],
                old_generation
            );
            assert_eq!(
                pair.engines[1].diagnostic_snapshot(pair.peers[1])["receive_budget"]
                    ["pending_bytes"],
                payload.len()
            );
            drop(retained);
            assert_eq!(
                pair.engines[1].diagnostic_snapshot(pair.peers[1])["receive_budget"]
                    ["pending_bytes"],
                0
            );
            send_packet(&output, &pair.engines[0], pair.peers[0], &packet)
                .await
                .unwrap();
            drop(pair.receipt(1, &payload).await);
            pair.remove().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn packet_commands_refuse_actual_receive_limits_before_terminal_event_drain() {
    for offerer in [0, 1] {
        for (packet_bytes, admitted, reason) in
            [(0, 256, "pending_messages"), (8192, 128, "pending_bytes")]
        {
            println!("GODOT_PROBE_REFUSAL offerer={offerer} bytes={packet_bytes} reason={reason}");
            let mut pair = ProbePair::new();
            pair.negotiate(offerer).await;
            let incoming = pair.engines[1].channel(pair.peers[1], "reliable").unwrap();
            let mut payload = vec![0xfe; packet_bytes];
            for sequence in 0..=admitted {
                if packet_bytes >= 4 {
                    payload[..4].copy_from_slice(&u32::try_from(sequence).unwrap().to_be_bytes());
                }
                incoming.send(payload.as_slice().into()).await.unwrap();
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                while pair.engines[0].diagnostic_snapshot(pair.peers[0])["receive_rejection"]
                    != reason
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("held Engine events exhaust the actual unchanged receive budget");
            let (output, mut rows) = mpsc::unbounded_channel();
            let snapshot = pair.engines[0].diagnostic_snapshot(pair.peers[0]);
            assert_eq!(snapshot["receive_failure"]["pending_messages"], admitted);
            assert_eq!(
                snapshot["receive_failure"]["pending_bytes"],
                packet_bytes * admitted
            );
            for packet in [
                json!({"op":"send", "label":"ordered", "text":"refused-after-actual-rejection"}),
                json!({"op":"send_binary", "label":"ordered", "data_base64":STANDARD.encode(b"refused")}),
            ] {
                assert!(
                    send_packet(&output, &pair.engines[0], pair.peers[0], &packet)
                        .await
                        .is_err()
                );
                assert!(matches!(
                    rows.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ));
            }
            assert!(
                send_startup_packet(&pair.engines[0], pair.peers[0], b"refused-startup")
                    .await
                    .is_err()
            );
            let mut retained = Vec::new();
            for sequence in 0..admitted {
                if packet_bytes >= 4 {
                    payload[..4].copy_from_slice(&u32::try_from(sequence).unwrap().to_be_bytes());
                }
                retained.push(pair.receipt(0, &payload).await);
            }
            let failure = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match pair.events[0].recv().await.unwrap() {
                        event @ EngineEvent::ChannelReceiveRejected { .. } => break event,
                        EngineEvent::ChannelBinaryMessage { .. } => {
                            panic!("the exact admitted prefix has already drained")
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            let EngineEvent::ChannelReceiveRejected { failure, .. } = failure else {
                unreachable!()
            };
            assert_eq!(
                serde_json::to_value(failure).unwrap(),
                snapshot["receive_failure"]
            );
            pair.remove().await;
            pair.negotiate(offerer).await;
            assert_eq!(
                pair.engines[0].diagnostic_snapshot(pair.peers[0])["receive_budget"]
                    ["pending_messages"],
                admitted
            );
            drop(retained);
            let fresh = [0, 0xff, 0x80, 1].repeat(1024);
            send_startup_packet(&pair.engines[1], pair.peers[1], &fresh)
                .await
                .unwrap();
            drop(pair.receipt(0, &fresh).await);
            pair.remove().await;
        }
    }
}
