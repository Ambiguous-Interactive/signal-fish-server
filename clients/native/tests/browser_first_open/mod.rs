//! #853/#889/#826: max-size fragmented gameplay at the first usable callback,
//! through both real engines and both Chromium/native offerer roles.
use std::collections::BTreeSet;
use std::process::Stdio;
use std::time::Duration;

use futures_util::FutureExt;
use serde_json::{json, Value};
use signal_fish_reference_native::engine::{Engine, EngineEvent, EngineSettings};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use uuid::Uuid;

fn payload(sender: &str, label: &str, binary: bool) -> String {
    let prefix = format!(
        "{sender}:{label}:{}:",
        if binary { "binary" } else { "text" }
    );
    let remaining = 65_536 - prefix.len();
    format!(
        "{prefix}{}{}",
        "界".repeat(remaining / 3),
        "x".repeat(remaining % 3)
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fragmented_first_open_crosses_chromium_native_in_both_roles() {
    let _serial = super::acquire_serial().await;
    for (browser_initiates, binary_mode) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../browser/scripts/first-open-peer.mjs");
        let mut child = tokio::process::Command::new("node")
            .arg(script)
            .arg(browser_initiates.to_string())
            .arg(binary_mode.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start real Chromium engine helper");
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        let (events, mut receiver) = mpsc::unbounded_channel();
        let mut native = Engine::new(
            EngineSettings {
                disable_mdns: true,
                ..Default::default()
            },
            events,
        )
        .unwrap();
        let peer = Uuid::from_u128(0xb);
        let offer = native
            .pair_with(peer, !browser_initiates, &[])
            .await
            .unwrap();
        let mut ledger = Vec::new();
        // Catch failures to close stdin and reap Chromium before reporting them.
        let result = std::panic::AssertUnwindSafe(tokio::time::timeout(Duration::from_secs(30), async {
            let mut native_opens = BTreeSet::new();
            let mut browser_opens = BTreeSet::new();
            let mut native_receipts = BTreeSet::new();
            let mut browser_receipts = BTreeSet::new();
            let mut pairs = 0;
            let mut snapshot_requested = false;
            loop {
                let command = tokio::select! {
                    line = output.next_line() => {
                        let event: Value = serde_json::from_str(&line.unwrap().expect("browser helper stays live")).unwrap();
                        ledger.push(format!("browser:{}:{}", event["type"], event["label"]));
                        match event["type"].as_str().unwrap() {
                            "ready" => Some(json!({"type": "offer", "sdp": offer.as_ref().unwrap()})),
                            "offer" => {
                                let sdp = native.handle_offer(peer, event["sdp"].as_str().unwrap().to_owned()).await.unwrap();
                                Some(json!({"type": "answer", "sdp": sdp}))
                            }
                            "answer" => {
                                native.handle_answer(peer, event["sdp"].as_str().unwrap().to_owned()).await.unwrap();
                                None
                            }
                            "candidate" => {
                                native.handle_remote_candidate(peer, event["candidate"].as_str().unwrap()).await.unwrap();
                                None
                            }
                            "open" => {
                                assert!(browser_opens.insert(event["label"].as_str().unwrap().to_owned()));
                                None
                            }
                            "receipt" => {
                                assert_eq!(event["bytes"], 65_536);
                                assert_eq!(event["binary"], binary_mode);
                                assert!(browser_receipts.insert((event["label"].as_str().unwrap().to_owned(), event["binary"].as_bool().unwrap())));
                                None
                            }
                            "snapshot" => {
                                assert_eq!(event["pairs"], 1);
                                assert_eq!(event["opens"].as_array().unwrap().len(), 2);
                                assert_eq!(event["receipts"].as_array().unwrap().len(), 2);
                                for label in ["reliable", "unreliable"] {
                                    assert_eq!(event["link"]["channels"][label]["receipts"], json!({"raw": 1, "admitted": 1, "rejected": 0, "discarded": 0}));
                                    // Admission is counted after event publication. Keep
                                    // this wait inside the original exchange deadline.
                                    while native.diagnostic_snapshot(peer)["channels"][label]["receipts"]["admitted"] != 1 {
                                        tokio::task::yield_now().await;
                                    }
                                    assert_eq!(native.diagnostic_snapshot(peer)["channels"][label]["receipts"], json!({"raw": 1, "admitted": 1, "rejected": 0, "discarded": 0}));
                                }
                                break;
                            }
                            _ => panic!("unexpected browser event: {event}"),
                        }
                    }
                    event = receiver.recv() => {
                        let event = event.expect("native engine stays live");
                        let (event_peer, generation) = event.peer_generation();
                        assert_eq!(event_peer, peer, "first-open callback belongs to the remote peer");
                        assert!(native.is_current_generation(event_peer, generation), "first-open callback belongs to the current physical link");
                        match event {
                            EngineEvent::LocalCandidate { candidate_json, .. } => Some(json!({"type": "candidate", "candidate": candidate_json})),
                            EngineEvent::RemoteChannel { label, channel, .. } => {
                                native.store_remote_channel(peer, label, channel).await;
                                None
                            }
                            EngineEvent::ChannelOpen { label, .. } => {
                                ledger.push(format!("native:open:{label}"));
                                assert!(native_opens.insert(label.clone()));
                                let channel = native.channel(peer, &label).unwrap();
                                let body = payload("native", &label, binary_mode);
                                if binary_mode { channel.send(body.as_bytes().into()).await.unwrap(); }
                                else { channel.send_text(&body).await.unwrap(); }
                                pairs += usize::from(native.note_channel_open(peer, &label).await);
                                None
                            }
                            EngineEvent::ChannelMessage { label, text, .. } => {
                                ledger.push(format!("native:receipt:{label}:{}", text.len()));
                                let binary = text.starts_with(&format!("browser:{label}:binary:"));
                                assert_eq!(text.len(), 65_536);
                                assert!(text == payload("browser", &label, binary), "exact UTF-8 content: browser_initiates={browser_initiates}, label={label}, binary={binary}");
                                assert_eq!(binary, binary_mode, "expected first-send encoding");
                                assert!(native_receipts.insert((label, binary)));
                                None
                            }
                            EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => panic!("first-open gameplay must keep the pair live"),
                            EngineEvent::PcState { state, .. } => {
                                assert!(!matches!(state, webrtc::peer_connection::RTCPeerConnectionState::Failed | webrtc::peer_connection::RTCPeerConnectionState::Closed));
                                None
                            }
                            EngineEvent::IceGatheringComplete { .. } => None,
                        }
                    }
                };
                if let Some(command) = command {
                    input.write_all(format!("{command}\n").as_bytes()).await.unwrap();
                }
                if !snapshot_requested && native_receipts.len() == 2 && browser_receipts.len() == 2 && native_opens.len() == 2 && browser_opens.len() == 2 {
                    assert_eq!(pairs, 1);
                    let expected_labels = BTreeSet::from(["reliable".to_owned(), "unreliable".to_owned()]);
                    assert_eq!(native_opens, expected_labels);
                    assert_eq!(browser_opens, expected_labels);
                    input.write_all(b"{\"type\":\"snapshot\"}\n").await.unwrap();
                    snapshot_requested = true;
                }
            }
        })).catch_unwind().await;
        if !matches!(&result, Ok(Ok(()))) {
            eprintln!(
                "browser_initiates={browser_initiates}; binary_mode={binary_mode}; events={ledger:?}; native={}",
                native.diagnostic_snapshot(peer)
            );
            if input.write_all(b"{\"type\":\"snapshot\"}\n").await.is_ok() {
                let snapshot = tokio::time::timeout(Duration::from_secs(2), async {
                    while let Ok(Some(line)) = output.next_line().await {
                        let event: Value = serde_json::from_str(&line).unwrap();
                        if event["type"] == "snapshot" {
                            return event;
                        }
                    }
                    Value::Null
                })
                .await;
                eprintln!("browser failure snapshot: {snapshot:?}");
            }
        }
        drop(input);
        let status = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
            Ok(status) => status.unwrap(),
            Err(_) => {
                child.kill().await.expect("kill hung Chromium helper");
                child.wait().await.expect("reap hung Chromium helper");
                panic!("helper did not close Chromium on input EOF");
            }
        };
        native.remove_peer(peer).await.unwrap();
        match result {
            Ok(outcome) => outcome.expect("first-open fragmented exchange completes"),
            Err(panic) => std::panic::resume_unwind(panic),
        }
        assert!(status.success(), "Chromium helper exits successfully");
        eprintln!("first-open browser_initiates={browser_initiates}: binary_mode={binary_mode}: both labels deliver exact 65536-byte first sends in both directions");
    }
}
