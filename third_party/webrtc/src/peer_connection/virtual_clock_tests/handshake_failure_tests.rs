use super::*;
use crate::runtime::{AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle};
use futures::FutureExt;
use quinn_udp::{RecvMeta, Transmit};
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};

#[derive(Debug, Default)]
struct RecvFault {
    enabled: AtomicBool,
    waker: futures::task::AtomicWaker,
}
impl RecvFault {
    fn trigger(&self) {
        self.enabled.store(true, Ordering::SeqCst);
        self.waker.wake();
    }
}

// Test-only network fault: preserve STUN consent while dropping one DTLS
// record type (handshake records, or application records carrying SCTP).
#[derive(Debug)]
struct LostRecordRuntime {
    runtime: Arc<MockRuntime>,
    dropped: Arc<AtomicUsize>,
    record_type: u8,
    enabled: Arc<AtomicBool>,
    fatal_recv: Arc<RecvFault>,
}
#[derive(Debug)]
struct LostRecordSocket {
    socket: Arc<dyn AsyncUdpSocket>,
    dropped: Arc<AtomicUsize>,
    record_type: u8,
    enabled: Arc<AtomicBool>,
    fatal_recv: Arc<RecvFault>,
}
impl AsyncUdpSocket for LostRecordSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
    fn poll_send(&self, cx: &mut Context<'_>, packet: &Transmit<'_>) -> Poll<io::Result<usize>> {
        if self.enabled.load(Ordering::SeqCst) && packet.contents.first() == Some(&self.record_type)
        {
            self.dropped.fetch_add(1, Ordering::SeqCst);
            Poll::Ready(Ok(packet.contents.len()))
        } else {
            self.socket.poll_send(cx, packet)
        }
    }
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        self.fatal_recv.waker.register(cx.waker());
        if self.fatal_recv.enabled.swap(false, Ordering::SeqCst) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "test receive failure",
            )));
        }
        self.socket.poll_recv(cx, bufs, meta)
    }
}
impl Runtime for LostRecordRuntime {
    fn now(&self) -> Instant {
        self.runtime.now()
    }
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        self.runtime.spawn(future)
    }
    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Ok(Arc::new(LostRecordSocket {
            socket: self.runtime.wrap_udp_socket(socket)?,
            dropped: self.dropped.clone(),
            record_type: self.record_type,
            enabled: self.enabled.clone(),
            fatal_recv: self.fatal_recv.clone(),
        }))
    }
    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> io::Result<Arc<dyn AsyncTcpListener>> {
        self.runtime.wrap_tcp_listener(listener)
    }
    fn connect_tcp<'a>(
        &'a self,
        addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>> {
        self.runtime.connect_tcp(addr)
    }
    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        self.runtime.resolve_host(host)
    }
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.runtime.sleep(duration)
    }
    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        self.runtime.interval(period)
    }
    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        self.runtime.block_on(future)
    }
}

async fn loss_peer(network: &Arc<MockUDPNetwork>, dropped: &Arc<AtomicUsize>) -> Peer {
    loss_record_peer(network, dropped, 23).await
}

async fn loss_record_peer(
    network: &Arc<MockUDPNetwork>,
    dropped: &Arc<AtomicUsize>,
    record_type: u8,
) -> Peer {
    loss_record_peer_with_faults(
        network,
        dropped,
        record_type,
        Arc::new(AtomicBool::new(true)),
        Arc::new(RecvFault::default()),
    )
    .await
}

async fn loss_record_peer_with_faults(
    network: &Arc<MockUDPNetwork>,
    dropped: &Arc<AtomicUsize>,
    record_type: u8,
    enabled: Arc<AtomicBool>,
    fatal_recv: Arc<RecvFault>,
) -> Peer {
    let rt = Arc::new(MockRuntime::with_network(network.clone()));
    rt.clock().advance(CLOCK_OFFSET);
    let rec = Arc::new(StateRecorder::default());
    let pc = PeerConnectionBuilder::new()
        .with_setting_engine(
            SettingEngineBuilder::new()
                .with_multicast_dns_mode(rtc::ice::mdns::MulticastDnsMode::Disabled)
                .build(),
        )
        .with_data_channel_send_buffer_limit(1)
        .with_runtime(Arc::new(LostRecordRuntime {
            runtime: rt.clone(),
            dropped: dropped.clone(),
            record_type,
            enabled,
            fatal_recv,
        }))
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

// #1128: actual native handles must observe SCTP's terminal handshake failure.
#[test]
fn sctp_handshake_loss_closes_public_pending_handles_and_fresh_peer_recovers() {
    let runtime = MockRuntime::new();
    runtime.block_on(Box::pin(async {
        let network = Arc::new(MockUDPNetwork::new());
        let dropped = Arc::new(AtomicUsize::new(0));
        let a = loss_peer(&network, &dropped).await;
        let b = loss_peer(&network, &dropped).await;
        let mut channels = Vec::new();
        for peer in [&a, &b] {
            channels.push(vec![
                peer.pc.create_data_channel("pending", None).await.unwrap(),
                peer.pc
                    .create_data_channel(
                        "negotiated",
                        Some(rtc::data_channel::RTCDataChannelInit {
                            negotiated: Some(42),
                            ..Default::default()
                        }),
                    )
                    .await
                    .unwrap(),
            ]);
        }
        let offer = a.pc.create_offer(None).await.unwrap();
        a.pc.set_local_description(offer.clone()).await.unwrap();
        b.pc.set_remote_description(offer).await.unwrap();
        let answer = b.pc.create_answer(None).await.unwrap();
        b.pc.set_local_description(answer.clone()).await.unwrap();
        a.pc.set_remote_description(answer).await.unwrap();
        let peers = [&a, &b];
        let mut failed = None;
        // DTLS and ICE use short steps before SCTP's longer retransmit deadlines.
        for tick in 0..1200 {
            for candidate in a.rec.take_candidates() {
                b.pc.add_ice_candidate(candidate).await.unwrap();
            }
            for candidate in b.rec.take_candidates() {
                a.pc.add_ice_candidate(candidate).await.unwrap();
            }
            for (index, peer) in peers.iter().enumerate() {
                if let Some(sctp) = peer.pc.sctp().await
                    && sctp.state().await.unwrap()
                        == rtc::peer_connection::transport::RTCSctpTransportState::Closed
                {
                    failed = Some(index);
                }
            }
            if failed.is_some() {
                break;
            }
            advance_both(
                &a.rt,
                &b.rt,
                if tick < 200 {
                    Duration::from_millis(25)
                } else {
                    Duration::from_secs(1)
                },
            );
        }
        let failed = failed.expect("lost SCTP handshake must reach its terminal retry limit");
        assert!(
            peers[failed]
                .rec
                .saw_conn(RTCPeerConnectionState::Connected),
            "DTLS connects before SCTP fails"
        );
        assert!(
            !peers[failed].rec.saw_ice(RTCIceConnectionState::Failed),
            "the SCTP fault preserves ICE consent"
        );
        for channel in &channels[1 - failed] {
            assert!(
                channel.poll().now_or_never().is_none(),
                "another peer's pending channels do not close with this failure"
            );
        }
        assert!(
            dropped.load(Ordering::SeqCst) > 1,
            "SCTP retries crossed the fault boundary"
        );
        for channel in &channels[failed] {
            let mut closed = false;
            for _ in 0..200 {
                match channel.poll().now_or_never() {
                    Some(Some(crate::data_channel::DataChannelEvent::OnClose)) => {
                        closed = true;
                        break;
                    }
                    Some(Some(event)) => panic!("pending channel received {event:?}"),
                    Some(None) => panic!("channel ended without OnClose"),
                    None => settle(),
                }
            }
            assert!(closed, "pending handle must receive OnClose");
            assert!(matches!(
                channel.send_text("late-gameplay").await,
                Err(Error::ErrDataChannelClosed)
            ));
            assert!(matches!(
                channel.writable().await,
                Err(Error::ErrDataChannelClosed)
            ));
            assert!(
                !matches!(channel.poll().now_or_never(), Some(Some(_))),
                "close fires once"
            );
        }
        assert!(matches!(
            peers[failed].pc.create_data_channel("late", None).await,
            Err(Error::ErrSCTPNotEstablished)
        ));
        a.pc.close().await.unwrap();
        b.pc.close().await.unwrap();
        assert_fresh_gameplay(&Arc::new(MockUDPNetwork::new())).await;
    }));
}

// #1131: DTLS failure must reach the app before any remote channel exists.
#[test]
fn dtls_handshake_loss_notifies_responder_without_remote_channels() {
    let runtime = MockRuntime::new();
    runtime.block_on(Box::pin(async {
        for local_channels in [false, true] {
            let network = Arc::new(MockUDPNetwork::new());
            let dropped = Arc::new(AtomicUsize::new(0));
            let a = loss_record_peer(&network, &dropped, 22).await;
            let b = loss_record_peer(&network, &dropped, 22).await;
            let _pending = a.pc.create_data_channel("pending-gameplay", None).await.unwrap();
            let mut pending = Vec::new();
            if local_channels {
                pending.push(b.pc.create_data_channel("pending", None).await.unwrap());
                pending.push(b.pc.create_data_channel("negotiated", Some(rtc::data_channel::RTCDataChannelInit { negotiated: Some(42), ..Default::default() })).await.unwrap());
            }
            let offer = a.pc.create_offer(None).await.unwrap();
            a.pc.set_local_description(offer.clone()).await.unwrap();
            b.pc.set_remote_description(offer).await.unwrap();
            let answer = b.pc.create_answer(None).await.unwrap();
            b.pc.set_local_description(answer.clone()).await.unwrap();
            a.pc.set_remote_description(answer).await.unwrap();
            for tick in 0..600 {
                for candidate in a.rec.take_candidates() { b.pc.add_ice_candidate(candidate).await.unwrap(); }
                for candidate in b.rec.take_candidates() { a.pc.add_ice_candidate(candidate).await.unwrap(); }
                if b.rec.saw_conn(RTCPeerConnectionState::Failed) { break; }
                advance_both(&a.rt, &b.rt, if tick < 100 { Duration::from_millis(25) } else { Duration::from_secs(1) });
            }
            assert!(dropped.load(Ordering::SeqCst) >= 8, "real handshake retries reach the loss boundary");
            assert!(!b.rec.saw_ice(RTCIceConnectionState::Failed), "handshake loss preserves ICE consent");
            assert!(b.rec.saw_conn(RTCPeerConnectionState::Failed), "a responder with no remote channels must receive a terminal handshake failure callback: {:?}", b.rec.conn_states());
            assert_eq!(b.pc.sctp().await.unwrap().transport().state().await.unwrap(), rtc::peer_connection::transport::RTCDtlsTransportState::Failed);
            for channel in pending {
                let mut closed = false;
                for _ in 0..200 {
                    match channel.poll().now_or_never() {
                        Some(Some(crate::data_channel::DataChannelEvent::OnClose)) => { closed = true; break; }
                        Some(Some(event)) => panic!("pending channel received {event:?}"),
                        Some(None) => panic!("pending channel ended without OnClose"),
                        None => settle(),
                    }
                }
                assert!(closed, "DTLS failure closes pending public handles");
                assert!(matches!(channel.send_text("late-gameplay").await, Err(Error::ErrDataChannelClosed)));
                assert!(matches!(channel.writable().await, Err(Error::ErrDataChannelClosed)));
                assert!(!matches!(channel.poll().now_or_never(), Some(Some(_))), "close fires once");
            }
            assert!(matches!(b.pc.create_data_channel("late", None).await, Err(Error::ErrSCTPNotEstablished)));
            assert!(!a.rec.saw_conn(RTCPeerConnectionState::Failed), "a peer with no received DTLS flight has no failed retry timer");
            assert_fresh_gameplay(&network).await;
            assert_eq!(b.rec.conn_states().iter().filter(|state| **state == RTCPeerConnectionState::Failed).count(), 1);
            a.pc.close().await.unwrap();
            b.pc.close().await.unwrap();
        }
    }));
}

async fn assert_fresh_gameplay(network: &Arc<MockUDPNetwork>) {
    let (fresh_a, fresh_b) = connect_pair(network).await;
    let mut fresh_channels = Vec::new();
    for peer in [&fresh_a, &fresh_b] {
        fresh_channels.push(
            peer.pc
                .create_data_channel(
                    "fresh-gameplay",
                    Some(rtc::data_channel::RTCDataChannelInit {
                        negotiated: Some(42),
                        ..Default::default()
                    }),
                )
                .await
                .unwrap(),
        );
    }
    let mut opened = [false; 2];
    for _ in 0..400 {
        for (index, channel) in fresh_channels.iter().enumerate() {
            if let Some(Some(crate::data_channel::DataChannelEvent::OnOpen)) =
                channel.poll().now_or_never()
            {
                opened[index] = true;
            }
        }
        if opened == [true, true] {
            break;
        }
        advance_both(&fresh_a.rt, &fresh_b.rt, Duration::from_millis(25));
    }
    assert_eq!(opened, [true, true], "fresh public channels open");
    fresh_channels[0].send_text("fresh-gameplay").await.unwrap();
    let mut delivered = false;
    for _ in 0..200 {
        if let Some(Some(crate::data_channel::DataChannelEvent::OnMessage(message))) =
            fresh_channels[1].poll().now_or_never()
        {
            assert_eq!(&message.data[..], b"fresh-gameplay");
            delivered = true;
            break;
        }
        advance_both(&fresh_a.rt, &fresh_b.rt, Duration::from_millis(25));
    }
    assert!(delivered, "fresh native gameplay reaches its peer");
    fresh_a.pc.close().await.unwrap();
    fresh_b.pc.close().await.unwrap();
}

// Issue #1134: a stopped native reactor must retire startup and notify its owner.
#[test]
fn fatal_socket_loss_retires_startup_before_any_remote_channel_exists() {
    let runtime = MockRuntime::new();
    runtime.block_on(Box::pin(async {
        for local_channels in [false, true] {
            let network = Arc::new(MockUDPNetwork::new());
            let fatal_recv = Arc::new(RecvFault::default());
            let peer = loss_record_peer_with_faults(
                &network,
                &Arc::new(AtomicUsize::new(0)),
                22,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                fatal_recv.clone(),
            )
            .await;
            let mut pending = Vec::new();
            if local_channels {
                for unreliable in [false, true] {
                    pending.push(
                        peer.pc
                            .create_data_channel(
                                "pending-gameplay",
                                Some(rtc::data_channel::RTCDataChannelInit {
                                    ordered: !unreliable,
                                    max_retransmits: unreliable.then_some(0),
                                    ..Default::default()
                                }),
                            )
                            .await
                            .unwrap(),
                    );
                }
            }
            fatal_recv.trigger();
            for _ in 0..100 {
                if peer.rec.saw_conn(RTCPeerConnectionState::Closed) {
                    break;
                }
                peer.rt.clock().advance(Duration::from_secs(1));
                settle();
            }
            assert!(
                !fatal_recv.enabled.load(Ordering::SeqCst),
                "the driver must reach the injected socket error"
            );
            assert!(
                peer.rec.saw_conn(RTCPeerConnectionState::Closed),
                "loss of the last socket must notify a responder that has no remote channels: {:?}",
                peer.rec.conn_states()
            );
            assert!(
                matches!(
                    peer.pc.create_data_channel("dead-driver", None).await,
                    Err(Error::ErrConnectionClosed)
                ),
                "a stopped driver must not create usable-looking handles"
            );
            for channel in pending {
                assert!(matches!(
                    channel.writable().await,
                    Err(Error::ErrDataChannelClosed)
                ));
                assert!(matches!(
                    channel.send_text("late-gameplay").await,
                    Err(Error::ErrDataChannelClosed)
                ));
                assert!(
                    matches!(channel.poll().now_or_never(), Some(None)),
                    "driver stop ends each pending channel stream"
                );
            }
            peer.pc.close().await.unwrap();
            assert_eq!(
                peer.rec
                    .conn_states()
                    .iter()
                    .filter(|state| **state == RTCPeerConnectionState::Closed)
                    .count(),
                1
            );
            assert_fresh_gameplay(&network).await;
        }
    }));
}

// #1133: a lost reset exhausts its existing retry budget and
// retires sibling gameplay handles instead of leaving a timerless Closing SID.
#[test]
fn sctp_reset_loss_retires_public_channels_and_blocked_senders() {
    let runtime = MockRuntime::new();
    runtime.block_on(Box::pin(async {
        let network = Arc::new(MockUDPNetwork::new());
        let dropped = Arc::new(AtomicUsize::new(0));
        let enabled = Arc::new(AtomicBool::new(false));
        let a = loss_record_peer_with_faults(
            &network,
            &dropped,
            23,
            enabled.clone(),
            Arc::new(RecvFault::default()),
        )
        .await;
        let b = loss_record_peer_with_faults(
            &network,
            &dropped,
            23,
            enabled.clone(),
            Arc::new(RecvFault::default()),
        )
        .await;
        let mut channels = Vec::new();
        for peer in [&a, &b] {
            let mut own = Vec::new();
            for sid in [42, 43] {
                own.push(
                    peer.pc
                        .create_data_channel(
                            "gameplay",
                            Some(rtc::data_channel::RTCDataChannelInit {
                                negotiated: Some(sid),
                                ..Default::default()
                            }),
                        )
                        .await
                        .unwrap(),
                );
            }
            channels.push(own);
        }
        let offer = a.pc.create_offer(None).await.unwrap();
        a.pc.set_local_description(offer.clone()).await.unwrap();
        b.pc.set_remote_description(offer).await.unwrap();
        let answer = b.pc.create_answer(None).await.unwrap();
        b.pc.set_local_description(answer.clone()).await.unwrap();
        a.pc.set_remote_description(answer).await.unwrap();
        let mut opened = [[false; 2]; 2];
        for _ in 0..400 {
            for candidate in a.rec.take_candidates() {
                b.pc.add_ice_candidate(candidate).await.unwrap();
            }
            for candidate in b.rec.take_candidates() {
                a.pc.add_ice_candidate(candidate).await.unwrap();
            }
            for (side, own) in channels.iter().enumerate() {
                for (index, channel) in own.iter().enumerate() {
                    if let Some(Some(crate::data_channel::DataChannelEvent::OnOpen)) =
                        channel.poll().now_or_never()
                    {
                        opened[side][index] = true;
                    }
                }
            }
            if opened == [[true; 2]; 2] {
                break;
            }
            advance_both(&a.rt, &b.rt, Duration::from_millis(25));
        }
        assert_eq!(opened, [[true; 2]; 2]);
        eprintln!("reset-loss: channels opened");
        enabled.store(true, Ordering::SeqCst);
        channels[1][1].send_text("x").await.unwrap();
        assert_eq!(channels[1][1].outstanding_bytes().await.unwrap(), 1);
        let binary = channels[1][1].send(bytes::BytesMut::from(&b"y"[..]));
        let text = channels[1][1].send_text("z");
        let writable = channels[1][1].writable();
        futures::pin_mut!(binary, text, writable);
        assert!(binary.as_mut().now_or_never().is_none());
        assert!(text.as_mut().now_or_never().is_none());
        assert!(writable.as_mut().now_or_never().is_none());
        let retire_transport = async {
            eprintln!("reset-loss: senders blocked, closing selected channel");
            channels[1][0].close().await.unwrap();
            eprintln!("reset-loss: selected channel closing");
            for _ in 0..300 {
                if b.pc.sctp().await.unwrap().state().await.unwrap()
                    == rtc::peer_connection::transport::RTCSctpTransportState::Closed
                {
                    break;
                }
                advance_both(&a.rt, &b.rt, Duration::from_secs(1));
                a.rt.yield_now().await;
            }
            assert_eq!(
                b.pc.sctp().await.unwrap().state().await.unwrap(),
                rtc::peer_connection::transport::RTCSctpTransportState::Closed,
                "lost RECONFIG must not leave an established transport after its retry limit"
            );
            assert!(
                !b.rec.saw_ice(RTCIceConnectionState::Failed),
                "SCTP-only loss preserves consent"
            );
            assert!(
                dropped.load(Ordering::SeqCst) >= 6,
                "reset retries crossed the actual UDP fault"
            );
        };
        // A once-polled writer can be waiting for the core mutex, not capacity.
        // Poll all writers while close and the driver need that same mutex.
        let ((binary_result, text_result, writable_result), ()) = futures::join!(
            async { futures::join!(binary, text, writable) },
            retire_transport,
        );
        eprintln!("reset-loss: terminal writers completed");
        for result in [binary_result, text_result, writable_result] {
            assert!(matches!(result, Err(Error::ErrDataChannelClosed)));
        }
        for (index, channel) in channels[1].iter().enumerate() {
            assert!(matches!(
                channel.outstanding_bytes().await,
                Err(Error::ErrDataChannelClosed)
            ));
            let mut closed = 0;
            let mut closing = 0;
            for _ in 0..200 {
                match channel.poll().now_or_never() {
                    Some(Some(crate::data_channel::DataChannelEvent::OnClose)) => closed += 1,
                    Some(Some(crate::data_channel::DataChannelEvent::OnClosing)) => closing += 1,
                    Some(Some(event)) => panic!("unexpected event after reset loss: {event:?}"),
                    Some(None) => break,
                    None if closed > 0 => break,
                    None => settle(),
                }
            }
            assert_eq!(closed, 1, "each public handle closes once");
            assert_eq!(
                closing,
                usize::from(index == 0),
                "only local close announces Closing"
            );
        }
        assert!(matches!(
            b.pc.create_data_channel("late", None).await,
            Err(Error::ErrSCTPNotEstablished)
        ));
        assert_eq!(
            channels[0][1].ready_state().await.unwrap(),
            rtc::data_channel::RTCDataChannelState::Open,
            "one endpoint's timeout must not invent remote receipt"
        );
        eprintln!("reset-loss: assertions complete, closing first peer");
        a.pc.close().await.unwrap();
        eprintln!("reset-loss: first peer closed");
        b.pc.close().await.unwrap();
        eprintln!("reset-loss: second peer closed, checking fresh gameplay");
        assert_fresh_gameplay(&network).await;
        eprintln!("reset-loss: fresh gameplay complete");
    }));
}

// Issue #1134: terminal socket loss also releases established gameplay writers.
#[test]
fn fatal_socket_loss_releases_established_gameplay_senders() {
    let runtime = MockRuntime::new();
    runtime.block_on(Box::pin(async {
        for unreliable in [false, true] {
            let network = Arc::new(MockUDPNetwork::new());
            let enabled = Arc::new(AtomicBool::new(false));
            let fatal_recv = Arc::new(RecvFault::default());
            let dropped = Arc::new(AtomicUsize::new(0));
            let a = loss_record_peer_with_faults(
                &network,
                &dropped,
                23,
                enabled.clone(),
                fatal_recv.clone(),
            )
            .await;
            let b = build_peer(&network).await;
            let init = rtc::data_channel::RTCDataChannelInit {
                negotiated: Some(42),
                ordered: !unreliable,
                max_retransmits: unreliable.then_some(0),
                ..Default::default()
            };
            let outgoing =
                a.pc.create_data_channel("gameplay", Some(init.clone()))
                    .await
                    .unwrap();
            let incoming =
                b.pc.create_data_channel("gameplay", Some(init))
                    .await
                    .unwrap();
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
                for (index, channel) in [&outgoing, &incoming].into_iter().enumerate() {
                    if let Some(Some(crate::data_channel::DataChannelEvent::OnOpen)) =
                        channel.poll().now_or_never()
                    {
                        opened[index] = true;
                    }
                }
                if opened == [true, true] {
                    break;
                }
                advance_both(&a.rt, &b.rt, Duration::from_millis(25));
            }
            assert_eq!(opened, [true, true]);
            enabled.store(true, Ordering::SeqCst);
            outgoing.send_text("x").await.unwrap();
            let binary = outgoing.send(bytes::BytesMut::from(&b"y"[..]));
            let text = outgoing.send_text("z");
            let writable = outgoing.writable();
            futures::pin_mut!(binary, text, writable);
            assert!(binary.as_mut().now_or_never().is_none());
            assert!(text.as_mut().now_or_never().is_none());
            assert!(writable.as_mut().now_or_never().is_none());
            let stop_driver = async {
                fatal_recv.trigger();
                for _ in 0..100 {
                    if a.rec.saw_conn(RTCPeerConnectionState::Closed) {
                        break;
                    }
                    advance_both(&a.rt, &b.rt, Duration::from_millis(1));
                    a.rt.yield_now().await;
                }
                assert!(
                    !fatal_recv.enabled.load(Ordering::SeqCst),
                    "the actual UDP receive path failed"
                );
                assert!(a.rec.saw_conn(RTCPeerConnectionState::Closed));
            };
            let ((binary_result, text_result, writable_result), ()) = futures::join!(
                async { futures::join!(binary, text, writable) },
                stop_driver,
            );
            for result in [binary_result, text_result, writable_result] {
                assert!(
                    matches!(result, Err(Error::ErrDataChannelClosed)),
                    "parked sends must finish when their driver stops"
                );
            }
            assert!(matches!(
                a.pc.create_data_channel("late", None).await,
                Err(Error::ErrConnectionClosed)
            ));
            assert_eq!(
                a.rec
                    .conn_states()
                    .iter()
                    .filter(|state| **state == RTCPeerConnectionState::Closed)
                    .count(),
                1
            );
            a.pc.close().await.unwrap();
            b.pc.close().await.unwrap();
        }
    }));
}
