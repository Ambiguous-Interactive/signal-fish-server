use super::*;
use clap::Parser;
use std::io::Write;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(super) struct DiagnosticCapture(Arc<Mutex<Vec<u8>>>);

impl Write for DiagnosticCapture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl DiagnosticCapture {
    pub(super) fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + use<> {
        let capture = self.clone();
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_target(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || capture.clone())
            .finish()
    }

    pub(super) fn losses(&self) -> Vec<(String, serde_json::Value)> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .filter(|line| line.contains("P2P link lost"))
            .map(|line| {
                let (prefix, snapshot) = line.split_once("snapshot=").unwrap();
                (prefix.to_owned(), serde_json::from_str(snapshot).unwrap())
            })
            .collect()
    }
}

// Regression #1019: genuine failure observations must survive link removal.
#[tokio::test]
async fn native_peer_loss_retains_snapshot_before_removal() {
    for mode in [
        "failed",
        "closed",
        "reliable-close",
        "unreliable-close",
        "reliable-rejected",
        "unreliable-rejected",
        "expired",
        "pending",
        "ready-late",
        "replacement",
        "departure",
        "stale-event",
    ] {
        let capture = DiagnosticCapture::default();
        let _subscriber = tracing::subscriber::set_default(capture.subscriber());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let _server_ws = server.await.unwrap();
        let cli = Cli::parse_from(["native", "--server-url", &url, "--create-room"]);
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        state.engine.pair_with(peer, false, &[]).await.unwrap();
        state
            .engine
            .handle_remote_candidate(peer, "queued")
            .await
            .unwrap();
        let expected = state.engine.diagnostic_snapshot(peer);
        let generation = expected["generation"].as_u64().unwrap();
        assert_eq!(expected["pending_remote_candidates"], 1);
        let intentional = matches!(mode, "replacement" | "departure" | "stale-event");
        let reason = match mode {
            "failed" | "closed" | "stale-event" => {
                let failed = mode != "closed";
                state
                    .handle_engine_event(EngineEvent::PcState {
                        peer,
                        generation: if mode == "stale-event" {
                            generation + 1
                        } else {
                            generation
                        },
                        state: if failed {
                            webrtc::peer_connection::RTCPeerConnectionState::Failed
                        } else {
                            webrtc::peer_connection::RTCPeerConnectionState::Closed
                        },
                    })
                    .await
                    .unwrap();
                if failed {
                    "connection state failed"
                } else {
                    "connection state closed"
                }
            }
            "replacement" => {
                state.prepare_retained_pair_replacement(peer).await;
                ""
            }
            "departure" => {
                state.remove_pair_obligation(peer).await;
                ""
            }
            "expired" | "pending" | "ready-late" => {
                tokio::time::pause();
                let now = Instant::now();
                state.run_deadline = Deadline::after(now, Duration::from_secs(10));
                state.p2p_deadline = Some(Deadline::after(now, Duration::from_secs(1)));
                let entered = Arc::new(tokio::sync::Notify::new());
                let release = Arc::new(tokio::sync::Notify::new());
                state.engine.negotiation_hold = Some(crate::engine::NegotiationHold {
                    step: "candidate_complete",
                    entered: entered.clone(),
                    release: release.clone(),
                });
                if mode == "expired" {
                    state.run_deadline = Deadline::after(now, Duration::ZERO);
                }
                let mut operation =
                    Box::pin(state.negotiate(peer, Negotiation::Candidate("tail".into())));
                if mode != "expired" {
                    tokio::select! {
                        _ = entered.notified() => {}
                        result = &mut operation => panic!("{mode}: operation did not reach hold: {result:?}"),
                    }
                    tokio::time::advance(Duration::from_secs(1)).await;
                    if mode == "ready-late" {
                        release.notify_one();
                    }
                }
                assert!(operation.await.unwrap().is_err());
                tokio::time::resume();
                "negotiation deadline expired"
            }
            _ => {
                let label = if mode.starts_with("reliable-") {
                    RELIABLE_LABEL
                } else {
                    UNRELIABLE_LABEL
                };
                let event = if mode.ends_with("rejected") {
                    EngineEvent::ChannelReceiveRejected {
                        peer,
                        generation,
                        label: label.into(),
                    }
                } else {
                    EngineEvent::ChannelClosed {
                        peer,
                        generation,
                        label: label.into(),
                    }
                };
                state.handle_engine_event(event).await.unwrap();
                if mode.ends_with("rejected") {
                    "data-channel receive rejected"
                } else {
                    "required data channel closed"
                }
            }
        };
        let losses = capture.losses();
        assert_eq!(
            losses.len(),
            usize::from(!intentional),
            "mode={mode}: {losses:?}"
        );
        if !intentional {
            let (prefix, snapshot) = &losses[0];
            assert!(prefix.contains(reason), "mode={mode}: {prefix}");
            assert!(prefix.contains(&peer.to_string()), "{prefix}");
            assert_eq!(snapshot["generation"], generation);
            assert_eq!(snapshot["remote_description_set"], false);
            assert_eq!(
                snapshot["remote_candidate_count"],
                if matches!(mode, "pending" | "ready-late") {
                    2
                } else {
                    1
                }
            );
            assert_eq!(
                snapshot["pending_remote_candidates"],
                snapshot["remote_candidate_count"]
            );
            assert_eq!(snapshot["channels"], expected["channels"]);
            assert!(state.engine.diagnostic_snapshot(peer).is_null());
            state
                .handle_engine_event(EngineEvent::ChannelClosed {
                    peer,
                    generation,
                    label: RELIABLE_LABEL.into(),
                })
                .await
                .unwrap();
            assert_eq!(
                capture.losses().len(),
                1,
                "retired events cannot duplicate loss"
            );
        }
        state.engine.remove_peer(peer).await.unwrap();
        state.engine.pair_with(peer, false, &[]).await.unwrap();
        state
            .handle_engine_event(EngineEvent::ChannelClosed {
                peer,
                generation,
                label: UNRELIABLE_LABEL.into(),
            })
            .await
            .unwrap();
        assert!(
            state.engine.is_paired(peer),
            "old event cannot close replacement"
        );
        assert_eq!(
            capture.losses().len(),
            losses.len(),
            "old event cannot report replacement loss"
        );
        state.engine.remove_peer(peer).await.unwrap();
    }
}

// Regression #889: connected exchange debt must retain the live link at cutoff.
#[tokio::test]
async fn native_exchange_cutoff_retains_unpaid_peer_snapshots() {
    for missing in [
        &[RELIABLE_LABEL][..],
        &[UNRELIABLE_LABEL][..],
        &[RELIABLE_LABEL, UNRELIABLE_LABEL][..],
    ] {
        for mode in ["sent", "received", "both", "paid", "removed", "disabled"] {
            let capture = DiagnosticCapture::default();
            let _subscriber = tracing::subscriber::set_default(capture.subscriber());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                tokio_tungstenite::accept_async(tcp).await.unwrap()
            });
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let _server_ws = server.await.unwrap();
            let mut cli = Cli::parse_from([
                "native",
                "--server-url",
                &url,
                "--create-room",
                "--exchange",
            ]);
            cli.exchange = mode != "disabled";
            let peer = PlayerId::from_u128(2);
            let paid_peer = PlayerId::from_u128(3);
            let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
            state.engine.pair_with(peer, false, &[]).await.unwrap();
            state.connected_pairs.insert(peer);
            for member in [peer, paid_peer] {
                state.exchange_ledger.note_connected(member);
                for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                    if member == paid_peer
                        || !missing.contains(&label)
                        || !matches!(mode, "sent" | "both")
                    {
                        state.exchange_ledger.note_sent(member, label);
                    }
                    if member == paid_peer
                        || !missing.contains(&label)
                        || !matches!(mode, "received" | "both" | "removed" | "disabled")
                    {
                        state.exchange_ledger.note_received(member, label.into());
                    }
                }
            }
            let expected = state.engine.diagnostic_snapshot(peer);
            if mode == "removed" {
                state.engine.remove_peer(peer).await.unwrap();
                state.connected_pairs.remove(&peer);
            }
            state.run_deadline = Deadline::after(Instant::now(), Duration::ZERO);
            assert_eq!(
                state.process_timers().await.unwrap(),
                Some(EXIT_CRITERIA_UNMET)
            );
            let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
            let records: Vec<_> = text
                .lines()
                .filter(|line| line.contains("Exchange timed out"))
                .collect();
            if matches!(mode, "paid" | "disabled") {
                assert!(records.is_empty(), "{mode}: {text}");
            } else {
                assert_eq!(records.len(), 1, "{mode}: {text}");
                let (prefix, snapshot) = records[0].split_once("snapshot=").unwrap();
                assert!(prefix.contains(&peer.to_string()));
                let snapshot: serde_json::Value = serde_json::from_str(snapshot).unwrap();
                let missing_sent = if matches!(mode, "sent" | "both") {
                    missing.to_vec()
                } else {
                    vec![]
                };
                let missing_received = if mode == "sent" {
                    vec![]
                } else {
                    missing.to_vec()
                };
                assert_eq!(snapshot["missing_sent"], json!(missing_sent), "{mode}");
                assert_eq!(
                    snapshot["missing_received"],
                    json!(missing_received),
                    "{mode}"
                );
                assert_eq!(snapshot["connected"], mode != "removed");
                assert_eq!(
                    snapshot["link"],
                    if mode == "removed" {
                        serde_json::Value::Null
                    } else {
                        expected
                    }
                );
            }
            state.engine.remove_peer(peer).await.unwrap();
        }
    }
}
