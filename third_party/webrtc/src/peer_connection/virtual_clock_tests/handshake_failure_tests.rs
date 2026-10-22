use super::*;
use crate::runtime::{AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle};
use futures::FutureExt;
use quinn_udp::{RecvMeta, Transmit};
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

// Test-only network fault: preserve STUN consent and DTLS handshake records,
// but lose encrypted application records, which carry the SCTP handshake.
#[derive(Debug)]
struct LostSctpRuntime {
    runtime: Arc<MockRuntime>,
    dropped: Arc<AtomicUsize>,
}
#[derive(Debug)]
struct LostSctpSocket {
    socket: Arc<dyn AsyncUdpSocket>,
    dropped: Arc<AtomicUsize>,
}
impl AsyncUdpSocket for LostSctpSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
    fn poll_send(&self, cx: &mut Context<'_>, packet: &Transmit<'_>) -> Poll<io::Result<usize>> {
        if packet.contents.first() == Some(&23) {
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
        self.socket.poll_recv(cx, bufs, meta)
    }
}
impl Runtime for LostSctpRuntime {
    fn now(&self) -> Instant {
        self.runtime.now()
    }
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        self.runtime.spawn(future)
    }
    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Ok(Arc::new(LostSctpSocket {
            socket: self.runtime.wrap_udp_socket(socket)?,
            dropped: self.dropped.clone(),
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
    let rt = Arc::new(MockRuntime::with_network(network.clone()));
    rt.clock().advance(CLOCK_OFFSET);
    let rec = Arc::new(StateRecorder::default());
    let pc = PeerConnectionBuilder::new()
        .with_setting_engine(
            SettingEngineBuilder::new()
                .with_multicast_dns_mode(rtc::ice::mdns::MulticastDnsMode::Disabled)
                .build(),
        )
        .with_runtime(Arc::new(LostSctpRuntime {
            runtime: rt.clone(),
            dropped: dropped.clone(),
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
        // New public peers use the same native driver and an intact network.
        let (fresh_a, fresh_b) = connect_pair(&Arc::new(MockUDPNetwork::new())).await;
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
    }));
}
