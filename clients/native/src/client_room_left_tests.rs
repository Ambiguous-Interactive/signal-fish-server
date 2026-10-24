use super::*;
use clap::Parser;
use futures_util::StreamExt;

#[tokio::test]
async fn room_left_retires_links_and_work_without_forgiving_exchange_debt() {
    for mode in ["connecting", "open", "retired"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(tcp).await.unwrap()
        });
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let mut server_ws = server.await.unwrap();
        let release_directory = tempfile::tempdir().unwrap();
        let mut cli = Cli::parse_from([
            "native",
            "--server-url",
            &url,
            "--create-room",
            "--exchange",
            "--disable-mdns",
        ]);
        cli.p2p_release_file = Some(release_directory.path().join("pairing"));
        cli.p2p_rebuild_release_file = Some(release_directory.path().join("rebuild"));
        let peer = PlayerId::from_u128(2);
        let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
        let offer = state.engine.pair_with(peer, true, &[]).await.unwrap();
        let old_channel = state.engine.channel(peer, RELIABLE_LABEL).unwrap();
        state.expected_peers.insert(peer);
        state.present.insert(peer);
        state.connected_pairs.insert(peer);
        state.pair_roles.insert(peer, true);
        let _remote = if mode == "open" {
            Some(
                Box::pin(super::retry_tests::open_physical_pair_with_events(
                    &mut state,
                    &mut server_ws,
                    peer,
                    offer,
                ))
                .await,
            )
        } else {
            None
        };
        state.exchange_ledger.note_connected(peer);
        state.exchange_ledger.note_sent(peer, RELIABLE_LABEL);
        state.queue_exchange(peer, &[UNRELIABLE_LABEL]);
        state.webrtc_plan_seen = true;
        state.in_lobby = true;
        state.game_started = true;
        state.ready_sent = true;
        state.exchange_ready_reported = true;
        state.transport_status = Some(true);
        state.p2p_deadline = Some(Deadline::after(Instant::now(), Duration::from_secs(1)));
        state.p2p_retry_at = Some(Instant::now());
        state.relay_send_at = Some(Instant::now());
        state.reconnect_poll_at = Some(Instant::now());
        state.reconnection_token = Some("old-room".into());
        let mut generation = state.engine.diagnostic_snapshot(peer)["generation"]
            .as_u64()
            .unwrap();
        if mode == "retired" {
            state.remove_pair_obligation(peer).await;
            // Transport loss can retain admitted receipts after its plan obligation ends.
            state.engine.pair_with(peer, false, &[]).await.unwrap();
            generation = state.engine.diagnostic_snapshot(peer)["generation"]
                .as_u64()
                .unwrap();
            state.engine.remove_peer(peer).await.unwrap();
        }
        state
            .handle_server_message(ServerMessage::RoomLeft)
            .await
            .unwrap();
        assert!(
            !state.engine.is_paired(peer),
            "leave must close the physical link"
        );
        assert_eq!(
            old_channel.ready_state().await.unwrap(),
            rtc::data_channel::RTCDataChannelState::Closed
        );
        assert!(!state.engine.owns_pending_receipt(peer, generation));
        assert!(state.expected_peers.is_empty());
        assert!(state.connected_pairs.is_empty());
        assert!(state.present.is_empty());
        assert!(state.deferred_exchange.is_empty());
        assert!(state.p2p_deadline.is_none());
        assert!(state.p2p_retry_at.is_none());
        assert!(state.relay_send_at.is_none());
        assert!(state.reconnect_poll_at.is_none());
        assert!(!state.in_lobby && !state.game_started && !state.ready_sent);
        assert_eq!(state.transport_status, None);
        let debt = state.exchange_ledger.unmet_criteria();
        assert!(!debt.is_empty());
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            state
                .handle_engine_event(EngineEvent::ChannelMessage {
                    peer,
                    generation,
                    label: label.into(),
                    text: json!({"from":peer,"channel":label,"seq":0}).to_string(),
                    queue_permit: crate::engine::MessageQueuePermit::for_test(),
                    received_at: Instant::now(),
                })
                .await
                .unwrap();
        }
        assert_eq!(state.exchange_ledger.unmet_criteria(), debt);
        state.next_ping_at = Instant::now();
        state.process_timers().await.unwrap();
        assert!(
            state.p2p_release_poll_at.is_none(),
            "leave must not rearm a held pairing gate"
        );
        assert!(
            state.p2p_rebuild_release_poll_at.is_none(),
            "leave must not rearm a held rebuild gate"
        );
        let frame = tokio::time::timeout(Duration::from_secs(1), server_ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<ClientMessage>(frame.to_text().unwrap()).unwrap(),
            ClientMessage::Ping
        ));
        let after_keepalive =
            tokio::time::timeout(Duration::from_millis(20), server_ws.next()).await;
        assert!(
            after_keepalive.is_err(),
            "leave must not write old-room status or gameplay after keepalive"
        );
        state.engine.pair_with(peer, false, &[]).await.unwrap();
        assert!(
            state.engine.is_paired(peer),
            "fresh generation can form after leave"
        );
        state.engine.remove_peer(peer).await.unwrap();
    }
}
