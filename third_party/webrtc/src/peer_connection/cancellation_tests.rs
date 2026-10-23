use super::*;
use std::sync::atomic::AtomicBool;

struct HeldHandler {
    entered: AtomicBool,
    held: bool,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for HeldHandler {
    async fn on_ice_gathering_state_change(&self, _: RTCIceGatheringState) {
        self.entered.store(true, Ordering::Release);
        if self.held {
            std::future::pending::<()>().await;
        }
    }
}

fn run(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

async fn released(handler: &std::sync::Weak<HeldHandler>) {
    tokio::time::timeout(Duration::from_millis(500), async {
        while handler.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retired driver must release its handler and transports");
}

async fn concrete_peer(
    handler: Arc<dyn PeerConnectionEventHandler>,
    pool_size: usize,
) -> Arc<PeerConnectionImpl> {
    let runtime = default_runtime().unwrap();
    let core = rtc::peer_connection::RTCPeerConnectionBuilder::new()
        .build(runtime.now())
        .unwrap();
    Arc::new(
        PeerConnectionImpl::new(
            core,
            runtime,
            handler,
            rtc::ice::mdns::MulticastDnsMode::Disabled,
            false,
            vec!["127.0.0.1:0"],
            Vec::<&str>::new(),
            pool_size,
            usize::MAX,
            None,
            rtc::crypto::default_provider().unwrap(),
        )
        .await
        .unwrap(),
    )
}

fn fill_driver_queue(inner: &PeerConnectionRef) {
    let mut queued = 0;
    loop {
        match inner
            .driver_event_tx
            .try_send(PeerConnectionDriverEvent::WriteNotify)
        {
            Ok(()) => queued += 1,
            Err(crate::runtime::TrySendError::Full(_)) => break,
            Err(crate::runtime::TrySendError::Disconnected(_)) => {
                panic!("the held driver must retain its input queue")
            }
        }
    }
    assert!(queued > 0, "the live driver's queue must become full");
}

// #1154: the bounded driver-stop wait must remain reachable behind a full queue.
#[test]
fn full_driver_queue_does_not_bypass_the_external_close_deadline() {
    run(async {
        for pool_size in [0, 1] {
            let handler = Arc::new(HeldHandler {
                entered: AtomicBool::new(false),
                held: true,
            });
            let pc = concrete_peer(handler.clone(), pool_size).await;
            let channel = pc.create_data_channel("pending", None).await.unwrap();
            let offer = pc.create_offer(None).await.unwrap();
            pc.set_local_description(offer).await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                while !handler.entered.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the real driver enters its held callback");
            fill_driver_queue(&pc.inner);

            tokio::time::timeout(Duration::from_millis(500), pc.close())
                .await
                .expect("a full queue cannot defeat the driver's bounded close wait")
                .unwrap();
            assert!(channel.poll().await.is_none());
            pc.close().await.unwrap();
        }
    });
}

struct SelfClosingHandler {
    peer: std::sync::Mutex<Option<std::sync::Weak<PeerConnectionImpl>>>,
    started: AtomicBool,
    closed: AtomicBool,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for SelfClosingHandler {
    async fn on_ice_gathering_state_change(&self, _state: RTCIceGatheringState) {
        if self.started.load(Ordering::Acquire) {
            return;
        }
        let Some(pc) = self
            .peer
            .lock()
            .unwrap()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
        else {
            return;
        };
        fill_driver_queue(&pc.inner);
        self.started.store(true, Ordering::Release);
        pc.close().await.unwrap();
        self.closed.store(true, Ordering::Release);
    }
}

// #1154: a public callback must never await its own driver's queue capacity.
#[test]
fn full_driver_queue_does_not_deadlock_callback_close() {
    run(async {
        for pool_size in [0, 1] {
            let handler = Arc::new(SelfClosingHandler {
                peer: std::sync::Mutex::new(None),
                started: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            });
            let pc = concrete_peer(handler.clone(), pool_size).await;
            *handler.peer.lock().unwrap() = Some(Arc::downgrade(&pc));
            let channel = pc.create_data_channel("pending", None).await.unwrap();
            let offer = pc.create_offer(None).await.unwrap();
            pc.set_local_description(offer).await.unwrap();
            let completed = tokio::time::timeout(Duration::from_millis(500), async {
                while !handler.closed.load(Ordering::Acquire) {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            assert!(
                handler.started.load(Ordering::Acquire),
                "the callback fills its driver's queue before close"
            );
            completed.expect("a real driver callback must be able to close its own peer");
            assert!(channel.poll().await.is_none());
            pc.close().await.unwrap();
        }
    });
}

#[test]
fn cancelled_pending_build_releases_the_spawned_driver() {
    run(async {
        let handler = Arc::new(HeldHandler {
            entered: AtomicBool::new(false),
            held: false,
        });
        let weak = Arc::downgrade(&handler);
        let mut build = Box::pin(
            PeerConnectionBuilder::new()
                .with_handler(handler.clone())
                .with_udp_addrs(vec!["127.0.0.1:0"])
                .build(),
        );
        assert!(
            futures::poll!(&mut build).is_pending(),
            "initialization waits for the spawned driver"
        );
        drop(build);
        drop(handler);
        released(&weak).await;
    });
}

#[test]
fn dropped_peer_releases_default_and_dedicated_drivers() {
    run(async {
        for pool_size in [0, 1] {
            let handler = Arc::new(HeldHandler {
                entered: AtomicBool::new(false),
                held: false,
            });
            let weak = Arc::downgrade(&handler);
            let pc = PeerConnectionBuilder::new()
                .with_handler(handler.clone())
                .with_dedicated_reactor_pool_size(pool_size)
                .with_udp_addrs(vec!["127.0.0.1:0"])
                .build()
                .await
                .unwrap();
            drop(handler);
            drop(pc);
            released(&weak).await;
        }
    });
}

#[test]
fn cancelled_close_retains_driver_cancellation_ownership() {
    run(async {
        let handler = Arc::new(HeldHandler {
            entered: AtomicBool::new(false),
            held: true,
        });
        let weak = Arc::downgrade(&handler);
        let pc = PeerConnectionBuilder::new()
            .with_handler(handler.clone())
            .with_udp_addrs(vec!["127.0.0.1:0"])
            .build()
            .await
            .unwrap();
        pc.create_data_channel("reliable", None).await.unwrap();
        let offer = pc.create_offer(None).await.unwrap();
        pc.set_local_description(offer).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !handler.entered.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("driver enters the held callback");
        let mut close = Box::pin(pc.close());
        assert!(
            futures::poll!(&mut close).is_pending(),
            "close waits for the held driver"
        );
        drop(close);
        drop(handler);
        drop(pc);
        released(&weak).await;
    });
}

#[test]
fn cancelled_close_stops_the_driver_while_peer_clones_remain() {
    run(async {
        let runtime = default_runtime().unwrap();
        let core = rtc::peer_connection::RTCPeerConnectionBuilder::new()
            .build(runtime.now())
            .unwrap();
        let pc = Arc::new(
            PeerConnectionImpl::new(
                core,
                runtime,
                Arc::new(HeldHandler {
                    entered: AtomicBool::new(false),
                    held: false,
                }),
                rtc::ice::mdns::MulticastDnsMode::Disabled,
                false,
                vec!["127.0.0.1:0"],
                Vec::<&str>::new(),
                0,
                usize::MAX,
                None,
                rtc::crypto::default_provider().unwrap(),
            )
            .await
            .unwrap(),
        );
        let retained = pc.clone();
        let mut stopped = pc.driver_stopped.lock().await.take().unwrap();
        let core_guard = pc.inner.core.lock().await;
        let mut close = Box::pin(pc.close());
        assert!(
            futures::poll!(&mut close).is_pending(),
            "close waits for the held core"
        );
        drop(close);
        tokio::time::timeout(Duration::from_millis(500), stopped.recv())
            .await
            .expect("canceling close must stop its driver while callers still retain the peer");
        assert!(retained.inner.closing.load(Ordering::Acquire));
        drop(core_guard);
        drop(retained);
        drop(pc);
    });
}

// #1156: cancellation cannot strand retained receivers after driver abort.
#[test]
fn cancelled_close_ends_retained_channel_streams_after_the_driver_stops() {
    run(async {
        for pool_size in [0, 1] {
            let pc = concrete_peer(
                Arc::new(HeldHandler {
                    entered: AtomicBool::new(false),
                    held: false,
                }),
                pool_size,
            )
            .await;
            let channel = pc.create_data_channel("retained", None).await.unwrap();
            let retained = pc.clone();
            let mut stopped = pc.driver_stopped.lock().await.take().unwrap();
            let core = pc.inner.core.lock().await;
            let mut close = Box::pin(pc.close());
            assert!(futures::poll!(&mut close).is_pending());
            drop(close);
            tokio::time::timeout(Duration::from_millis(500), stopped.recv())
                .await
                .expect("the canceled close stops the real driver");
            drop(core);
            let terminal = tokio::time::timeout(Duration::from_millis(500), channel.poll())
                .await
                .expect("a retained handle cannot wait for events from a stopped driver");
            assert!(terminal.is_none());
            assert!(retained.inner.is_closing());
            assert!(pc.create_data_channel("late", None).await.is_err());
            pc.close().await.unwrap();
        }
    });
}
