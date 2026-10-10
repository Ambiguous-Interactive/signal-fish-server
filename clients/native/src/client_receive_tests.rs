use super::*;
use clap::Parser;
use futures_util::StreamExt;

#[tokio::test]
async fn receive_rejection_reports_fallback_and_preserves_a_newer_link() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(tcp).await.unwrap()
    });
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let mut server_ws = server.await.unwrap();
    let cli = Cli::parse_from(["reference-native", "--server-url", &url, "--create-room"]);
    let peer = PlayerId::from_u128(2);
    let mut state = super::tests::fixture_orchestrator(&cli, ws, PlayerId::from_u128(1));
    state.engine.pair_with(peer, false, &[]).await.unwrap();
    state.webrtc_plan_seen = true;
    state.expected_peers.insert(peer);
    state.connected_pairs.insert(peer);
    state.ice_gathering_complete.insert(peer);
    state.transport_status = Some(true);

    state
        .handle_engine_event(EngineEvent::ChannelReceiveRejected {
            peer,
            generation: 1,
            label: RELIABLE_LABEL.to_string(),
        })
        .await
        .unwrap();
    assert!(!state.engine.is_paired(peer));
    assert!(!state.connected_pairs.contains(&peer));
    assert!(!state.ice_gathering_complete.contains(&peer));
    assert!(state.expected_peers.contains(&peer));
    assert_eq!(state.transport_status, Some(false));
    let frame = tokio::time::timeout(Duration::from_secs(5), server_ws.next())
        .await
        .expect("fallback status reaches the signaling server")
        .unwrap()
        .unwrap();
    let frame: ClientMessage = serde_json::from_str(frame.to_text().unwrap()).unwrap();
    assert!(matches!(
        frame,
        ClientMessage::TransportStatus {
            transport: Transport::WebRtc,
            connected: false
        }
    ));

    state.engine.pair_with(peer, false, &[]).await.unwrap();
    state.connected_pairs.insert(peer);
    state.transport_status = Some(true);
    state
        .handle_engine_event(EngineEvent::ChannelReceiveRejected {
            peer,
            generation: 1,
            label: UNRELIABLE_LABEL.to_string(),
        })
        .await
        .unwrap();
    assert!(state.engine.is_current_generation(peer, 2));
    assert!(state.connected_pairs.contains(&peer));
    assert_eq!(state.transport_status, Some(true));
    state.engine.remove_peer(peer).await.unwrap();
}
