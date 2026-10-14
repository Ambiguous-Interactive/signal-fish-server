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
