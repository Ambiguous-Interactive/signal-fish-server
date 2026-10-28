use super::{receive_tests::LivePair, *};
use std::future::Future;
use std::pin::Pin;
use webrtc::runtime::{AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket};

type RuntimeFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug)]
struct HeldClock {
    inner: Arc<dyn Runtime>,
    hold: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
}

impl Runtime for HeldClock {
    fn now(&self) -> std::time::Instant {
        if std::thread::current().name() == Some("snapshot-held-send") {
            if let Some((started, release)) = self.hold.lock().unwrap().take() {
                let _ = started.send(());
                release.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        }
        self.inner.now()
    }

    fn spawn(&self, future: RuntimeFuture<'static, ()>) -> Box<dyn JoinHandle> {
        self.inner.spawn(future)
    }

    fn spawn_reactor(
        &self,
        pool: usize,
        future: RuntimeFuture<'static, ()>,
    ) -> Box<dyn JoinHandle> {
        self.inner.spawn_reactor(pool, future)
    }

    fn wrap_udp_socket(&self, socket: UdpSocket) -> std::io::Result<Arc<dyn AsyncUdpSocket>> {
        self.inner.wrap_udp_socket(socket)
    }

    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> std::io::Result<Arc<dyn AsyncTcpListener>> {
        self.inner.wrap_tcp_listener(listener)
    }

    fn connect_tcp<'a>(
        &'a self,
        address: SocketAddr,
    ) -> RuntimeFuture<'a, std::io::Result<Arc<dyn AsyncTcpStream>>> {
        self.inner.connect_tcp(address)
    }

    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> RuntimeFuture<'a, std::io::Result<Vec<SocketAddr>>> {
        self.inner.resolve_host(host)
    }

    fn sleep(&self, duration: Duration) -> RuntimeFuture<'static, ()> {
        self.inner.sleep(duration)
    }

    fn interval(&self, duration: Duration) -> Box<dyn AsyncInterval> {
        self.inner.interval(duration)
    }

    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        self.inner.block_on(future);
    }
}

async fn next_exact_receipt(
    events: &mut mpsc::UnboundedReceiver<EngineEvent>,
    expected: &[u8],
    profile: ChannelProfile,
) -> EngineEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap();
            match &event {
                EngineEvent::ChannelMessage { label, text, .. } => {
                    assert_eq!(label, RELIABLE_LABEL);
                    assert_eq!(text.as_bytes(), expected);
                    assert_eq!(profile, ChannelProfile::Reference);
                    return event;
                }
                EngineEvent::ChannelBinaryMessage { label, data, .. } => {
                    assert_eq!(label, RELIABLE_LABEL);
                    assert_eq!(data, expected);
                    assert_eq!(profile, ChannelProfile::Godot);
                    return event;
                }
                EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => {
                    panic!("snapshot sampling must not terminate gameplay");
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}

fn unconnected_pair(profile: ChannelProfile) -> (LivePair, Arc<HeldClock>) {
    let (sender_tx, sender_events) = mpsc::unbounded_channel();
    let (receiver_tx, receiver_events) = mpsc::unbounded_channel();
    let settings = EngineSettings {
        disable_mdns: true,
        channel_profile: profile,
        ..EngineSettings::default()
    };
    let clock = Arc::new(HeldClock {
        inner: default_runtime().unwrap(),
        hold: Default::default(),
    });
    let mut receiver = Engine::new(settings, receiver_tx).unwrap();
    receiver.runtime = clock.clone();
    let pair = LivePair {
        sender: Engine::new(settings, sender_tx).unwrap(),
        receiver,
        sender_events,
        receiver_events,
        sender_id: PlayerId::from_u128(0x5721),
        receiver_id: PlayerId::from_u128(0x5722),
    };
    (pair, clock)
}

async fn snapshot_during_send(
    engine: &Engine,
    peer: PlayerId,
    clock: &HeldClock,
    channel: Arc<dyn DataChannel>,
    outgoing: Vec<u8>,
) -> serde_json::Value {
    let (started, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    *clock.hold.lock().unwrap() = Some((started, released));
    let writer = std::thread::Builder::new()
        .name("snapshot-held-send".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(channel.send(outgoing.as_slice().into()))
                .unwrap();
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        engine.transport_diagnostic_snapshot(peer),
    )
    .await;
    release.send(()).unwrap();
    tokio::task::spawn_blocking(move || writer.join().unwrap())
        .await
        .unwrap();
    result.expect("snapshot must return while the actual RTC core lock is held")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_transport_snapshot_returns_local_evidence_and_preserves_reliable_gameplay() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for receiver_offers in [false, true] {
            let (mut pair, clock) = unconnected_pair(profile);
            let mut retained = Vec::new();
            let mut last_generation = None;
            for generation in 0..2 {
                if receiver_offers {
                    std::mem::swap(&mut pair.sender, &mut pair.receiver);
                    std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                    std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                }
                pair.negotiate().await;
                if receiver_offers {
                    std::mem::swap(&mut pair.sender, &mut pair.receiver);
                    std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                    std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
                }
                let peer = pair.sender_id;
                let fresh = pair.receiver.diagnostic_snapshot(peer);
                assert_ne!(last_generation, Some(fresh["generation"].clone()));
                last_generation = Some(fresh["generation"].clone());
                assert_eq!(
                    fresh["channels"][RELIABLE_LABEL]["pending_application"]["messages"],
                    0
                );
                assert_eq!(fresh["receive_budget"]["pending_messages"], generation);
                let payload = vec![b'A' + generation; 4096];
                pair.sender
                    .channel(pair.receiver_id, RELIABLE_LABEL)
                    .unwrap()
                    .send(payload.as_slice().into())
                    .await
                    .unwrap();
                retained
                    .push(next_exact_receipt(&mut pair.receiver_events, &payload, profile).await);
                tokio::time::timeout(Duration::from_secs(5), async {
                    while pair.receiver.diagnostic_snapshot(peer)["channels"][RELIABLE_LABEL]
                        ["receipts"]["admitted"]
                        != 1
                    {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("receipt publication precedes its final admission counter update");
                let before = pair.receiver.diagnostic_snapshot(peer);
                let expected = vec![b'X' + generation; 4096];
                let snapshot = snapshot_during_send(
                    &pair.receiver,
                    peer,
                    &clock,
                    pair.receiver.channel(peer, RELIABLE_LABEL).unwrap(),
                    expected.clone(),
                )
                .await;
                assert_eq!(snapshot["generation"], before["generation"]);
                assert_eq!(snapshot["channels"], before["channels"]);
                assert_eq!(
                    snapshot["receive_budget"]["pending_messages"],
                    generation + 1
                );
                assert_eq!(
                    snapshot["receive_budget"]["pending_bytes"],
                    u64::from(generation + 1) * 4096
                );
                assert_eq!(
                    snapshot["channels"][RELIABLE_LABEL]["pending_application"]["messages"],
                    1
                );
                assert_eq!(snapshot["transport_snapshot_error"], "deadline_elapsed");
                assert_eq!(snapshot["transport_snapshot_stage"], "rtc_stats");
                assert!(
                    snapshot.get("transport_channels").is_none(),
                    "uncollected evidence must not look empty"
                );
                drop(next_exact_receipt(&mut pair.sender_events, &expected, profile).await);
                let after = pair.receiver.transport_diagnostic_snapshot(peer).await;
                assert!(after.get("transport_snapshot_error").is_none());
                assert_eq!(
                    after["channel_properties"].as_object().unwrap().len(),
                    profile.labels().len()
                );
                assert_eq!(
                    after["send_channels"].as_object().unwrap().len(),
                    profile.labels().len()
                );
                assert_eq!(after["channels"][RELIABLE_LABEL]["receipts"]["raw"], 1);
                assert_eq!(after["channels"][RELIABLE_LABEL]["receipts"]["admitted"], 1);
                pair.receiver.remove_peer(peer).await.unwrap();
                pair.sender.remove_peer(pair.receiver_id).await.unwrap();
            }
            drop(retained);
            assert_eq!(
                pair.receiver.message_budget.messages.available_permits(),
                MAX_PENDING_CHANNEL_MESSAGES
            );
            assert_eq!(
                pair.receiver.message_budget.bytes.available_permits(),
                MAX_PENDING_CHANNEL_BYTES
            );
            for events in [&mut pair.receiver_events, &mut pair.sender_events] {
                loop {
                    match events.try_recv() {
                        Ok(event) => assert!(
                            !matches!(
                                event,
                                EngineEvent::ChannelMessage { .. }
                                    | EngineEvent::ChannelBinaryMessage { .. }
                            ),
                            "no extra gameplay receipts remain after both exact exchanges"
                        ),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            panic!("retained Engines must keep their event senders");
                        }
                    }
                }
            }
            pair.cleanup().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_transport_snapshot_keeps_real_closure_and_receive_refusal_distinct() {
    for receiver_offers in [false, true] {
        for receive_refused in [false, true] {
            let (mut pair, clock) = unconnected_pair(ChannelProfile::Godot);
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
            }
            pair.negotiate().await;
            if receiver_offers {
                std::mem::swap(&mut pair.sender, &mut pair.receiver);
                std::mem::swap(&mut pair.sender_events, &mut pair.receiver_events);
                std::mem::swap(&mut pair.sender_id, &mut pair.receiver_id);
            }
            let peer = pair.sender_id;
            let mut retained = Vec::new();
            if receive_refused {
                let sender = pair
                    .sender
                    .channel(pair.receiver_id, RELIABLE_LABEL)
                    .unwrap();
                for _ in 0..MAX_PENDING_CHANNEL_MESSAGES {
                    sender.send(Default::default()).await.unwrap();
                    retained.push(
                        next_exact_receipt(&mut pair.receiver_events, &[], ChannelProfile::Godot)
                            .await,
                    );
                }
                sender.send(Default::default()).await.unwrap();
            } else {
                pair.receiver
                    .channel(peer, UNRELIABLE_LABEL)
                    .unwrap()
                    .close()
                    .await
                    .unwrap();
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match pair.receiver_events.recv().await.unwrap() {
                        EngineEvent::ChannelReceiveRejected { failure, label, .. } => {
                            assert!(receive_refused);
                            assert_eq!(label, RELIABLE_LABEL);
                            assert_eq!(failure.reason, ReceiveRejection::PendingMessages);
                            assert_eq!(failure.pending_messages, MAX_PENDING_CHANNEL_MESSAGES);
                            assert_eq!(failure.pending_bytes, 0);
                            break;
                        }
                        EngineEvent::ChannelClosed { label, .. } if !receive_refused => {
                            assert_eq!(label, UNRELIABLE_LABEL);
                            break;
                        }
                        EngineEvent::ChannelMessage { .. }
                        | EngineEvent::ChannelBinaryMessage { .. } => {
                            panic!("refused message cannot publish a receipt");
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            for label in ChannelProfile::Godot.labels() {
                assert!(pair.receiver.send_channel(peer, label).await.is_none());
            }
            let before = pair.receiver.diagnostic_snapshot(peer);
            assert_eq!(before["receive_failure"].is_object(), receive_refused);
            assert_eq!(before["channel_closure"].is_object(), !receive_refused);
            let held_label = if receive_refused {
                UNRELIABLE_LABEL
            } else {
                RELIABLE_LABEL
            };
            let snapshot = snapshot_during_send(
                &pair.receiver,
                peer,
                &clock,
                pair.receiver.channel(peer, held_label).unwrap(),
                vec![0; 32],
            )
            .await;
            assert_eq!(snapshot["transport_snapshot_error"], "deadline_elapsed");
            for field in [
                "generation",
                "receive_failure",
                "channel_closure",
                "connection_terminal",
                "receive_budget",
            ] {
                assert_eq!(snapshot[field], before[field], "{field}: preserve the actual local terminal cause while transport sampling is blocked");
            }
            drop(retained);
            assert_eq!(
                pair.receiver.message_budget.messages.available_permits(),
                MAX_PENDING_CHANNEL_MESSAGES
            );
            assert_eq!(
                pair.receiver.diagnostic_snapshot(peer)["receive_failure"],
                before["receive_failure"]
            );
            pair.cleanup().await;
        }
    }
}
