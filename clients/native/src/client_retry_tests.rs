// Regression #923: dispatch retries through the real native engine.
use super::*;
use clap::Parser;
use futures_util::StreamExt;

#[tokio::test]
async fn formed_pair_accepts_remote_retry_after_window_expiry() {
    for initiate in [false, true] {
        for close_before_marker in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                tokio_tungstenite::accept_async(tcp).await.unwrap()
            });
            let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            let mut server_ws = server.await.unwrap();
            let cli = Cli::parse_from([
                "reference-native",
                "--server-url",
                &url,
                "--create-room",
                "--p2p-retry-count",
                "2",
                "--p2p-timeout-secs",
                "0",
            ]);
            let peer = PlayerId::from_u128(2);
            let generation = SessionGeneration::from_u128(3);
            let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
            state.current_session_generation = Some(generation);
            state.negotiated_version = 3;
            state.webrtc_plan_seen = true;
            state.establish_pair(peer, initiate).await.unwrap();
            // A due local attempt must not publish a marker without rebuilding.
            state.request_pair_retry(peer, 1).await.unwrap();
            state.send_message(&ClientMessage::Ping).await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let frame = server_ws.next().await.unwrap().unwrap();
                    if let tokio_tungstenite::tungstenite::Message::Text(text) = frame {
                        let message: serde_json::Value = serde_json::from_str(&text).unwrap();
                        assert!(
                            message["data"]["signal"]["PairRetry"].is_null(),
                            "an expired local retry cannot make only the remote endpoint rebuild"
                        );
                        if message["type"] == "Ping" {
                            break;
                        }
                    }
                }
            })
            .await
            .unwrap();
            // An expired, never-formed link cannot reopen its initial window.
            state.process_timers().await.unwrap();
            state
                .handle_signal(peer, generation, json!({"PairRetry": 1}))
                .await
                .unwrap();
            assert!(state.pair_retry_attempts.is_empty());
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                state
                    .handle_engine_event(EngineEvent::ChannelOpen {
                        peer,
                        generation: 1,
                        label: label.to_string(),
                    })
                    .await
                    .unwrap();
            }
            assert!(state.connected_pairs.contains(&peer));
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                state.exchange_ledger.note_sent(peer, label);
                state.exchange_ledger.note_received(peer, label.to_string());
            }
            state.process_timers().await.unwrap();
            assert!(state.p2p_deadline.is_none());
            if close_before_marker {
                state
                    .handle_engine_event(EngineEvent::ChannelClosed {
                        peer,
                        generation: 1,
                        label: RELIABLE_LABEL.to_string(),
                    })
                    .await
                    .unwrap();
            }
            state
                .handle_signal(peer, generation, json!({"PairRetry": 1}))
                .await
                .unwrap();
            assert_eq!(state.pair_retry_attempts.get(&peer), Some(&1),
                "initiate={initiate}, close_before_marker={close_before_marker}: formed peer must join remote rebuild without a local harness gate");
            assert!(state.engine.is_current_generation(peer, 2));
            assert!(state.retrying_pairs.contains(&peer));
            assert!(!state.connected_pairs.contains(&peer));
            assert_eq!(state.pair_roles.get(&peer), Some(&initiate));
            assert!(state.exchange_ledger.unmet_criteria().is_empty());
            assert!(state
                .unmet_criteria()
                .iter()
                .any(|reason| reason.contains("pair retry")));
            for (from, wire_generation, payload) in [
                (peer, generation, json!({"PairRetry": 1})),
                (peer, generation, json!({"PairRetry": 0})),
                (peer, generation, json!({"PairRetry": 3})),
                (peer, generation, json!({"PairRetry": "2"})),
                (
                    peer,
                    SessionGeneration::from_u128(4),
                    json!({"PairRetry": 2}),
                ),
                (PlayerId::from_u128(4), generation, json!({"PairRetry": 1})),
            ] {
                state
                    .handle_signal(from, wire_generation, payload)
                    .await
                    .unwrap();
                assert!(state.engine.is_current_generation(peer, 2));
            }
            // A live window stays fixed even when a previously formed peer retries.
            let deadline = Deadline::after(Instant::now(), Duration::from_secs(60));
            state.p2p_deadline = Some(deadline);
            state
                .handle_signal(peer, generation, json!({"PairRetry": 2}))
                .await
                .unwrap();
            assert_eq!(state.p2p_deadline.unwrap().finite(), deadline.finite());
            assert!(state.engine.is_current_generation(peer, 3));
            state
                .handle_signal(peer, generation, json!({"PairRetry": 1}))
                .await
                .unwrap();
            assert!(state.engine.is_current_generation(peer, 3));
            state.p2p_deadline = Some(Deadline::after(Instant::now(), Duration::ZERO));
            state.process_timers().await.unwrap();
            assert!(state.retrying_pairs.is_empty());
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                state
                    .handle_engine_event(EngineEvent::ChannelOpen {
                        peer,
                        generation: 3,
                        label: label.to_string(),
                    })
                    .await
                    .unwrap();
            }
            assert!(
                state.p2p_reconnected_pairs.contains(&peer),
                "replacement opening after wait expiry must still report recovery"
            );
            // A newer authoritative physical generation cannot inherit formation.
            state.prepare_retained_pair_replacement(peer).await;
            assert!(!state.formed_pairs.contains(&peer));
            state.p2p_deadline = None;
            state
                .handle_engine_event(EngineEvent::ChannelOpen {
                    peer,
                    generation: 3,
                    label: RELIABLE_LABEL.to_string(),
                })
                .await
                .unwrap();
            state
                .handle_signal(peer, generation, json!({"PairRetry": 1}))
                .await
                .unwrap();
            assert!(state.pair_retry_attempts.is_empty());
            assert!(!state.engine.is_paired(peer));
            state.remove_pair_obligation(peer).await;
            assert!(!state.formed_pairs.contains(&peer));
        }
    }
}
