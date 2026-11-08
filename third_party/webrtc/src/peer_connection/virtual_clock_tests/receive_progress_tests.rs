use super::*;
use crate::data_channel::DataChannelEvent;
use futures::FutureExt;

/// Build a peer whose small SCTP window makes held callbacks close receive credit.
async fn small_window_peer(network: &Arc<MockUDPNetwork>) -> Peer {
    let rt = Arc::new(MockRuntime::with_network(network.clone()));
    rt.clock().advance(CLOCK_OFFSET);
    let rec = Arc::new(StateRecorder::default());
    let pc = PeerConnectionBuilder::new()
        .with_setting_engine(
            SettingEngineBuilder::new()
                .with_multicast_dns_mode(rtc::ice::mdns::MulticastDnsMode::Disabled)
                .with_sctp_max_receive_buffer_size(2048)
                .build(),
        )
        .with_runtime(rt.clone())
        .with_handler(rec.clone())
        .with_udp_addrs(vec!["127.0.0.1:0"])
        .build()
        .await
        .unwrap();
    Peer {
        pc: Box::new(pc),
        rt,
        rec,
    }
}

/// Regression #1508: accepted input must not wait for a new packet or timer.
#[test]
fn resumed_receive_window_sends_credit_without_an_unrelated_timer() {
    let runtime = MockRuntime::new();
    runtime.block_on(Box::pin(async {
        for receiver_offers in [false, true] {
            for _ in 0..2 {
                resumed_receive_window_case(receiver_offers).await;
            }
        }
    }));
}

/// Drain accepted input with frozen clocks, then recover refused input normally.
async fn resumed_receive_window_case(receiver_offers: bool) {
    let network = Arc::new(MockUDPNetwork::new());
    let a = small_window_peer(&network).await;
    let b = small_window_peer(&network).await;
    let mut channels = Vec::new();
    for peer in [&a, &b] {
        channels.push(
            peer.pc
                .create_data_channel(
                    "reliable",
                    Some(rtc::data_channel::RTCDataChannelInit {
                        negotiated: Some(42),
                        ..Default::default()
                    }),
                )
                .await
                .unwrap(),
        );
    }
    let offer = a.pc.create_offer(None).await.unwrap();
    a.pc.set_local_description(offer.clone()).await.unwrap();
    b.pc.set_remote_description(offer).await.unwrap();
    let answer = b.pc.create_answer(None).await.unwrap();
    b.pc.set_local_description(answer.clone()).await.unwrap();
    a.pc.set_remote_description(answer).await.unwrap();
    let mut opened = [false; 2];
    for _ in 0..400 {
        for candidate in a.rec.take_candidates() {
            b.pc.add_ice_candidate(candidate).await.unwrap();
        }
        for candidate in b.rec.take_candidates() {
            a.pc.add_ice_candidate(candidate).await.unwrap();
        }
        for (side, channel) in channels.iter().enumerate() {
            while let Some(Some(event)) = channel.poll().now_or_never() {
                if matches!(event, DataChannelEvent::OnOpen) {
                    opened[side] = true;
                }
            }
        }
        if opened == [true; 2] {
            break;
        }
        advance_both(&a.rt, &b.rt, Duration::from_millis(25));
    }
    assert_eq!(opened, [true; 2]);
    let source = usize::from(receiver_offers);
    let destination = 1 - source;
    let sender_transport = if receiver_offers { &b } else { &a }
        .pc
        .sctp()
        .await
        .unwrap();
    // Negotiated SID 42 sends no DCEP DATA. Each 1024-byte payload fits one
    // default-MTU DATA chunk, so accepted DATA counts name an exact prefix.
    const COUNT: u32 = 640;
    for sequence in 0..COUNT {
        let mut body = vec![0x5a; 1024];
        body[..4].copy_from_slice(&sequence.to_be_bytes());
        channels[source]
            .send(bytes::BytesMut::from(body.as_slice()))
            .await
            .unwrap();
    }
    let transport = if receiver_offers { &a } else { &b }
        .pc
        .sctp()
        .await
        .unwrap();
    let mut full = None;
    for _ in 0..1000 {
        let stats = transport.association_stats().await.unwrap().unwrap();
        if stats.flow_control.as_ref().unwrap().receive_window == 0 && stats.data_chunks_refused > 0
        {
            full = Some(stats);
            break;
        }
        advance_both(&a.rt, &b.rt, Duration::from_millis(25));
    }
    let full = full
        .expect("held callback queue must close the actual SCTP receive window and refuse a probe");
    let initial_sacks = sender_transport
        .association_stats()
        .await
        .unwrap()
        .unwrap()
        .sack_chunks_received;
    let frozen = [a.rt.now(), b.rt.now()];
    let mut received = 0u32;
    for _ in 0..1000 {
        while let Some(Some(event)) = channels[destination].poll().now_or_never() {
            match event {
                DataChannelEvent::OnMessage(message) => {
                    let mut expected = vec![0x5a; 1024];
                    expected[..4].copy_from_slice(&received.to_be_bytes());
                    assert_eq!(message.data.as_ref(), expected.as_slice());
                    received += 1;
                }
                DataChannelEvent::OnClose
                | DataChannelEvent::OnClosing
                | DataChannelEvent::OnError => panic!("reliable channel terminated while draining"),
                _ => {}
            }
        }
        if u64::from(received) >= full.data_chunks_accepted {
            break;
        }
        settle();
    }
    assert_eq!(
        [a.rt.now(), b.rt.now()],
        frozen,
        "no recovery timer or consent packet may hide a missed credit update"
    );
    let sender = sender_transport.association_stats().await.unwrap();
    let receiver = transport.association_stats().await.unwrap();
    assert!(
        u64::from(received) >= full.data_chunks_accepted,
        "all already accepted reliable messages must drain with protocol timers frozen: received={received} initial={full:?} sender={sender:?} receiver={receiver:?}"
    );
    for _ in 0..100 {
        if sender_transport
            .association_stats()
            .await
            .unwrap()
            .unwrap()
            .sack_chunks_received
            > initial_sacks
        {
            break;
        }
        settle();
    }
    assert!(
        sender_transport
            .association_stats()
            .await
            .unwrap()
            .unwrap()
            .sack_chunks_received
            > initial_sacks,
        "the reopened receive window must be advertised before an unrelated timer"
    );
    // The refused probe was never accepted. Normal DATA recovery may now
    // retransmit it; frozen timers above cannot be relaxed for accepted DATA.
    for _ in 0..1000 {
        while let Some(Some(event)) = channels[destination].poll().now_or_never() {
            if let DataChannelEvent::OnMessage(message) = event {
                let mut expected = vec![0x5a; 1024];
                expected[..4].copy_from_slice(&received.to_be_bytes());
                assert_eq!(message.data.as_ref(), expected.as_slice());
                received += 1;
            }
        }
        if received == COUNT {
            break;
        }
        advance_both(&a.rt, &b.rt, Duration::from_millis(25));
    }
    assert_eq!(
        received, COUNT,
        "normal retransmission must recover the refused probe and exact suffix"
    );
    a.pc.close().await.unwrap();
    b.pc.close().await.unwrap();
}
