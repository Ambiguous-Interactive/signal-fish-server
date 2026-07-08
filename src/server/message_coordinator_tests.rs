use super::InMemoryMessageCoordinator;
use crate::coordination::{ClientDeliveryHandle, ConnectionCloseSignal, MessageCoordinator};
use crate::metrics::ServerMetrics;
use crate::protocol::{PlayerId, ServerMessage};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tokio::time::Duration;

async fn wait_for_counter(context: &str, max_yields: usize, mut condition: impl FnMut() -> bool) {
    for _ in 0..max_yields {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{context}: counter condition never held");
}

#[tokio::test]
async fn drain_canceled_conditional_delivery_is_not_a_drop_or_relay_stat_loss() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(30),
        Arc::clone(&metrics),
    ));
    let player_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0001);
    let connection_stats = metrics.register_connection_delivery_stats(player_id);

    let (sender, _receiver) = mpsc::channel(1);
    sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("pre-fill outbound queue");
    let (close, _close_listener) = ConnectionCloseSignal::channel();

    coordinator
        .register_local_client(player_id, None, ClientDeliveryHandle { sender, close })
        .await
        .expect("register test client");

    let (drain_tx, drain_rx) = watch::channel(false);
    let send_task = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move {
            let should_send = || true;
            coordinator
                .send_to_player_if(
                    &player_id,
                    Arc::new(ServerMessage::Pong),
                    &should_send,
                    drain_rx,
                )
                .await
        })
    };

    wait_for_counter(
        "conditional send reached the full-queue wait",
        10_000,
        || {
            metrics.websocket_delivery_attempts.load(Ordering::Relaxed) == 1
                && metrics
                    .websocket_backpressure_events
                    .load(Ordering::Relaxed)
                    == 1
        },
    )
    .await;

    drain_tx.send(true).expect("start shutdown drain");
    let delivered = tokio::time::timeout(Duration::from_secs(1), send_task)
        .await
        .expect("conditional send should wake when drain starts")
        .expect("conditional send task should not panic")
        .expect("conditional send should not error");

    assert!(
        !delivered,
        "drain-canceled conditional send must report no delivery"
    );
    assert_eq!(
        metrics
            .websocket_deliveries_canceled
            .load(Ordering::Relaxed),
        1,
        "drain cancellation should resolve the delivery attempt as canceled"
    );
    assert_eq!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed),
        0,
        "canceled conditional delivery is not a dropped message"
    );
    assert_eq!(
        connection_stats.dropped_for_you.load(Ordering::Relaxed),
        0,
        "RelayStats dropped_for_you must only count true per-connection loss"
    );
    assert_eq!(
        connection_stats.sent_to_you.load(Ordering::Relaxed),
        0,
        "the canceled message was never enqueued"
    );
    assert_eq!(
        connection_stats.backpressure_events.load(Ordering::Relaxed),
        1,
        "the attempt did wait on the recipient's full outbound queue"
    );
}
