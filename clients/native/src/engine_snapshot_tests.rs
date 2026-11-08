use super::{receive_tests::LivePair, *};
use std::future::Future;
use std::pin::Pin;
use webrtc::runtime::{AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket};

type RuntimeFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug)]
pub(crate) struct HeldClock {
    inner: Arc<dyn Runtime>,
    probe_tasks: std::sync::Mutex<Option<Arc<AtomicUsize>>>,
    hold: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
}

struct ProbeTaskGuard(Arc<AtomicUsize>);

impl Drop for ProbeTaskGuard {
    /// Record capture release when the observed query exits or is cancelled.
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Runtime for HeldClock {
    /// Hold one real RTC send inside its core lock until the test releases it.
    fn now(&self) -> std::time::Instant {
        if std::thread::current().name() == Some("snapshot-held-send") {
            if let Some((started, release)) = self.hold.lock().unwrap().take() {
                let _ = started.send(());
                release.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        }
        self.inner.now()
    }

    /// Observe only the next query task so teardown tasks cannot affect its count.
    fn spawn(&self, future: RuntimeFuture<'static, ()>) -> Box<dyn JoinHandle> {
        let observed = self.probe_tasks.lock().unwrap().take();
        if let Some(observed) = observed {
            observed.fetch_add(1, Ordering::SeqCst);
            let guard = ProbeTaskGuard(observed);
            self.inner.spawn(Box::pin(async move {
                let _guard = guard;
                future.await;
            }))
        } else {
            self.inner.spawn(future)
        }
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

/// Construct both channel profiles with an observable receiver runtime.
pub(crate) fn unconnected_pair(profile: ChannelProfile) -> (LivePair, Arc<HeldClock>) {
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
        probe_tasks: Default::default(),
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

/// Regression #1507: retire a blocked query, then qualify the fresh link and route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retired_links_cancel_blocked_selected_pair_queries_before_fresh_gameplay() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for receiver_offers in [false, true] {
            let (mut pair, clock) = unconnected_pair(profile);
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
            let old_generation = pair.receiver.peers[&peer].generation;
            let live_probes = Arc::new(AtomicUsize::new(0));
            *clock.probe_tasks.lock().unwrap() = Some(live_probes.clone());
            let (entered, waiting) = tokio::sync::oneshot::channel();
            let (release, released) = std::sync::mpsc::channel();
            *clock.hold.lock().unwrap() = Some((entered, released));
            let channel = pair.receiver.channel(peer, RELIABLE_LABEL).unwrap();
            let writer = std::thread::Builder::new()
                .name("snapshot-held-send".into())
                .spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap()
                        .block_on(channel.send_text("held-before-retirement"))
                })
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), waiting)
                .await
                .unwrap()
                .unwrap();
            let (results, mut received) = mpsc::unbounded_channel();
            assert!(pair
                .receiver
                .start_selected_candidate_pair_probe(peer, results));
            assert_eq!(live_probes.load(Ordering::SeqCst), 1);
            let early = received.try_recv();
            assert!(
                matches!(early, Err(mpsc::error::TryRecvError::Empty)),
                "actual RTC core keeps query pending"
            );
            let (duplicate_results, mut duplicate_received) = mpsc::unbounded_channel();
            *clock.probe_tasks.lock().unwrap() = Some(live_probes.clone());
            assert!(pair
                .receiver
                .start_selected_candidate_pair_probe(peer, duplicate_results));
            *clock.probe_tasks.lock().unwrap() = None;
            let duplicate_bounded = live_probes.load(Ordering::SeqCst) == 1;
            let duplicate_result = duplicate_received.try_recv();
            pair.receiver.remove_peer(peer).await.unwrap();
            let cancelled = tokio::time::timeout(Duration::from_secs(1), async {
                while live_probes.load(Ordering::SeqCst) != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            release.send(()).unwrap();
            let _ = tokio::task::spawn_blocking(move || writer.join().unwrap())
                .await
                .unwrap();
            cancelled.expect("retiring the physical link must cancel its blocked query");
            assert!(
                duplicate_bounded,
                "a running link keeps one transport query"
            );
            assert!(
                matches!(
                    duplicate_result,
                    Err(mpsc::error::TryRecvError::Disconnected)
                ),
                "coalesced calls keep the first result sender"
            );
            let after_cancel = received.try_recv();
            assert!(
                matches!(after_cancel, Err(mpsc::error::TryRecvError::Disconnected)),
                "cancelled query cannot publish stale completion"
            );
            *clock.probe_tasks.lock().unwrap() = None;
            pair.sender.remove_peer(pair.receiver_id).await.unwrap();
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
            assert_ne!(pair.receiver.peers[&peer].generation, old_generation);
            assert_fresh_selected_pair(&mut pair.receiver, peer).await;
            assert_fresh_selected_pair(&mut pair.sender, pair.receiver_id).await;
            let expected = vec![b'F'; 4096];
            pair.sender
                .channel(pair.receiver_id, RELIABLE_LABEL)
                .unwrap()
                .send(expected.as_slice().into())
                .await
                .unwrap();
            drop(next_exact_receipt(&mut pair.receiver_events, &expected, profile).await);
            let reverse = vec![b'R'; 4096];
            pair.receiver
                .channel(peer, RELIABLE_LABEL)
                .unwrap()
                .send(reverse.as_slice().into())
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = pair.sender_events.recv().await.unwrap();
                    if !pair.sender.is_current_event(&event) {
                        continue;
                    }
                    match event {
                        EngineEvent::ChannelMessage { text, .. } => {
                            assert_eq!(profile, ChannelProfile::Reference);
                            assert_eq!(text.as_bytes(), reverse);
                            break;
                        }
                        EngineEvent::ChannelBinaryMessage { data, .. } => {
                            assert_eq!(profile, ChannelProfile::Godot);
                            assert_eq!(data, reverse);
                            break;
                        }
                        EngineEvent::ChannelClosed { .. }
                        | EngineEvent::ChannelReceiveRejected { .. } => {
                            panic!("fresh reverse traffic cannot lose its physical link");
                        }
                        _ => {}
                    }
                }
            })
            .await
            .expect("fresh physical generation preserves exact reverse reliable traffic");
            pair.cleanup().await;
        }
    }
}

/// Engine destruction cancels pending work; replacement keeps published evidence.
#[tokio::test]
async fn dropped_engine_cancels_its_pending_query_and_replacement_preserves_published_rows() {
    for drop_engine in [false, true] {
        let (events, _received_events) = mpsc::unbounded_channel();
        let mut engine = Engine::new(
            EngineSettings {
                crippled: true,
                ..EngineSettings::default()
            },
            events,
        )
        .unwrap();
        let peer = PlayerId::from_u128(0x1507);
        engine.pair_with(peer, false, &[]).await.unwrap();
        let generation = engine.peers[&peer].generation;
        let retained = Arc::new(());
        let weak = Arc::downgrade(&retained);
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let (results, mut rows) = mpsc::unbounded_channel();
        let query = spawn_selected_pair_probe(
            engine.runtime.clone(),
            results,
            peer,
            generation,
            async move {
                let _retained = retained;
                started.send(()).unwrap();
                let _ = released.await;
                None
            },
        );
        engine.peers.get_mut(&peer).unwrap().selected_pair_probe = Some(query);
        tokio::time::timeout(Duration::from_secs(1), entered)
            .await
            .unwrap()
            .unwrap();
        if drop_engine {
            drop(engine);
            tokio::time::timeout(Duration::from_secs(1), async {
                while weak.strong_count() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("dropping the Engine releases pending query captures");
            assert!(
                release.send(()).is_err(),
                "the cancelled query cannot resume"
            );
            let row = rows.try_recv();
            assert!(matches!(row, Err(mpsc::error::TryRecvError::Disconnected)));
        } else {
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                while !*engine.peers[&peer]
                    .selected_pair_probe
                    .as_ref()
                    .unwrap()
                    .completed
                    .lock()
                    .unwrap()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("completed query publishes before replacement");
            let (next_results, mut next_rows) = mpsc::unbounded_channel();
            assert!(engine.start_selected_candidate_pair_probe(peer, next_results));
            let next = tokio::time::timeout(Duration::from_secs(1), next_rows.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(next.peer, peer);
            assert_eq!(next.generation, generation);
            engine.remove_peer(peer).await.unwrap();
            let published = rows
                .try_recv()
                .expect("replacing the query and retiring its link preserves its published row");
            assert_eq!(published.peer, peer);
            assert_eq!(published.generation, generation);
            assert!(published.completed_at <= next.completed_at);
            assert!(published.selected.is_none());
        }
    }
}

/// Both fresh endpoints must publish their actual nominated route and generation.
async fn assert_fresh_selected_pair(engine: &mut Engine, peer: PlayerId) {
    let generation = engine.peers[&peer].generation;
    let (results, mut rows) = mpsc::unbounded_channel();
    let selected = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            assert!(engine.start_selected_candidate_pair_probe(peer, results.clone()));
            let row = rows.recv().await.expect("fresh query publishes its result");
            assert_eq!(row.peer, peer);
            assert_eq!(row.generation, generation);
            assert!(engine.is_current_generation(row.peer, row.generation));
            if let Some(selected) = row.selected {
                break selected;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fresh physical link publishes nominated transport evidence");
    assert_eq!(selected.local_candidate_type, "host");
    assert!(matches!(
        selected.remote_candidate_type.as_str(),
        "host" | "prflx"
    ));
    for address in [
        selected.local_candidate_address,
        selected.remote_candidate_address,
    ] {
        let address: IpAddr = address
            .expect("nominated candidate keeps its actual address")
            .parse()
            .expect("nominated candidate has a concrete IP address");
        assert!(!address.is_unspecified());
        assert!(!address.is_multicast());
    }
}

/// Hold a real send under the RTC core lock until explicit release.
pub(crate) struct HeldCoreSend {
    release: Option<std::sync::mpsc::Sender<()>>,
    writer: Option<std::thread::JoinHandle<webrtc::error::Result<()>>>,
}

impl HeldCoreSend {
    pub(crate) async fn start(clock: &HeldClock, channel: Arc<dyn DataChannel>) -> Self {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        *clock.hold.lock().unwrap() = Some((entered, released));
        let writer = std::thread::Builder::new()
            .name("snapshot-held-send".into())
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(channel.send_text("held-readiness"))
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
        Self {
            release: Some(release),
            writer: Some(writer),
        }
    }

    pub(crate) async fn finish(mut self) {
        self.release.take().unwrap().send(()).unwrap();
        let writer = self.writer.take().unwrap();
        let _ = tokio::task::spawn_blocking(move || writer.join().unwrap())
            .await
            .unwrap();
    }
}

impl Drop for HeldCoreSend {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

impl HeldClock {
    pub(crate) fn observe_next_task(&self) -> Arc<AtomicUsize> {
        let live = Arc::new(AtomicUsize::new(0));
        *self.probe_tasks.lock().unwrap() = Some(live.clone());
        live
    }
}

/// #1510/#1264: pending readiness work must stop before the old core is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retired_links_cancel_pending_readiness_before_core_release() {
    for profile in [ChannelProfile::Reference, ChannelProfile::Godot] {
        for receiver_offers in [false, true] {
            for drop_engine in [false, true] {
                let (mut pair, clock, opens) =
                    startup_pair_with_held_open_events(profile, receiver_offers).await;
                let peer = pair.sender_id;
                let generation = pair.receiver.peers[&peer].generation;
                let held = HeldCoreSend::start(
                    &clock,
                    pair.receiver.channel(peer, RELIABLE_LABEL).unwrap(),
                )
                .await;
                let live = clock.observe_next_task();
                for event in opens {
                    let EngineEvent::ChannelOpen { label, .. } = event else {
                        unreachable!()
                    };
                    assert!(!pair.receiver.request_channel_open_check(peer, &label).await);
                }
                assert_eq!(live.load(Ordering::SeqCst), 1);
                assert!(!pair.receiver.accept_pair_ready(peer, generation));
                if drop_engine {
                    let (events, _) = mpsc::unbounded_channel();
                    drop(std::mem::replace(
                        &mut pair.receiver,
                        Engine::new(pair.sender.settings, events).unwrap(),
                    ));
                } else {
                    pair.receiver.remove_peer(peer).await.unwrap();
                }
                tokio::time::timeout(Duration::from_secs(1), async {
                    while live.load(Ordering::SeqCst) != 0 {
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("readiness query cancellation must not wait for the retired core");
                loop {
                    match pair.receiver_events.try_recv() {
                        Ok(event) => assert!(
                            !matches!(event, EngineEvent::PairReady { generation: old, .. } if old == generation),
                            "pending readiness cannot publish completion after retirement"
                        ),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            assert!(drop_engine, "retained Engine must keep its event sender");
                            break;
                        }
                    }
                }
                held.finish().await;
                assert!(!pair.receiver.accept_pair_ready(peer, generation));
                pair.sender.remove_peer(pair.receiver_id).await.unwrap();
                pair.receiver.remove_peer(peer).await.unwrap();
            }
        }
    }
}

/// Complete real transport startup while retaining the receiver's actual open callbacks.
pub(crate) async fn startup_pair_with_held_open_events(
    profile: ChannelProfile,
    receiver_offers: bool,
) -> (LivePair, Arc<HeldClock>, Vec<EngineEvent>) {
    let (mut pair, clock) = unconnected_pair(profile);
    if receiver_offers {
        let offer = pair
            .receiver
            .pair_with(pair.sender_id, true, &[])
            .await
            .unwrap()
            .unwrap();
        pair.sender
            .pair_with(pair.receiver_id, false, &[])
            .await
            .unwrap();
        let answer = pair
            .sender
            .handle_offer(pair.receiver_id, offer)
            .await
            .unwrap();
        pair.receiver
            .handle_answer(pair.sender_id, answer)
            .await
            .unwrap();
    } else {
        let offer = pair
            .sender
            .pair_with(pair.receiver_id, true, &[])
            .await
            .unwrap()
            .unwrap();
        pair.receiver
            .pair_with(pair.sender_id, false, &[])
            .await
            .unwrap();
        let answer = pair
            .receiver
            .handle_offer(pair.sender_id, offer)
            .await
            .unwrap();
        pair.sender
            .handle_answer(pair.receiver_id, answer)
            .await
            .unwrap();
    }
    let opens = tokio::time::timeout(Duration::from_secs(15), async {
        let mut sender_ready = false;
        let mut opens = Vec::new();
        while !sender_ready || opens.len() < profile.labels().len() {
            tokio::select! {
                event = pair.sender_events.recv() => match event.unwrap() {
                    EngineEvent::LocalCandidate { candidate_json, .. } => pair.receiver.handle_remote_candidate(pair.sender_id, &candidate_json).await.unwrap(),
                    EngineEvent::RemoteChannel { label, channel, .. } => pair.sender.store_remote_channel(pair.receiver_id, label, channel).await,
                    EngineEvent::ChannelOpen { label, .. } => sender_ready |= pair.sender.note_channel_open(pair.receiver_id, &label).await,
                    EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => panic!("startup sender must remain healthy"),
                    _ => {}
                },
                event = pair.receiver_events.recv() => match event.unwrap() {
                    EngineEvent::LocalCandidate { candidate_json, .. } => pair.sender.handle_remote_candidate(pair.receiver_id, &candidate_json).await.unwrap(),
                    EngineEvent::RemoteChannel { label, channel, .. } => pair.receiver.store_remote_channel(pair.sender_id, label, channel).await,
                    event @ EngineEvent::ChannelOpen { .. } => opens.push(event),
                    EngineEvent::ChannelClosed { .. } | EngineEvent::ChannelReceiveRejected { .. } => panic!("startup receiver must remain healthy"),
                    _ => {}
                },
            }
        }
        opens
    }).await.expect("all actual required-channel opens arrive before owner admission");
    assert_eq!(pair.receiver.connected_pair_count(), 0);
    (pair, clock, opens)
}
