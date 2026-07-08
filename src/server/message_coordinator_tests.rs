use super::InMemoryMessageCoordinator;
use crate::coordination::{ClientDeliveryHandle, ConnectionCloseSignal, MessageCoordinator};
use crate::metrics::ServerMetrics;
use crate::protocol::{PlayerId, RoomId, ServerMessage};
use std::sync::atomic::{AtomicUsize, Ordering};
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

#[tokio::test]
async fn slow_consumer_retry_cancels_already_reserved_broadcast_permits() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_millis(1),
        Arc::clone(&metrics),
    ));
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0100);
    let sender_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0101);
    let healthy_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0102);
    let slow_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0103);

    let (healthy_sender, mut healthy_receiver) = mpsc::channel(1);
    let (healthy_close, _healthy_close_listener) = ConnectionCloseSignal::channel();
    coordinator
        .register_local_client(
            healthy_id,
            Some(room_id),
            ClientDeliveryHandle {
                sender: healthy_sender,
                close: healthy_close,
            },
        )
        .await
        .expect("register healthy recipient");

    let (slow_sender, slow_receiver) = mpsc::channel(1);
    slow_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("pre-fill slow recipient queue");
    let (slow_close, _slow_close_listener) = ConnectionCloseSignal::channel();
    coordinator
        .register_local_client(
            slow_id,
            Some(room_id),
            ClientDeliveryHandle {
                sender: slow_sender,
                close: slow_close,
            },
        )
        .await
        .expect("register slow recipient");

    let (drain_tx, drain_rx) = watch::channel(false);
    let should_send = || true;
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let before_send = {
        let hook_calls = Arc::clone(&hook_calls);
        Box::new(move || {
            let hook_calls = Arc::clone(&hook_calls);
            Box::pin(async move {
                hook_calls.fetch_add(1, Ordering::Relaxed);
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        })
    };

    let delivered = coordinator
        .broadcast_to_room_except_if_with_hook(
            &room_id,
            &sender_id,
            Arc::new(ServerMessage::Pong),
            &should_send,
            drain_rx,
            before_send,
        )
        .await
        .expect("broadcast succeeds");

    assert!(
        delivered,
        "broadcast should retry after pruning the slow recipient and reach the healthy recipient"
    );
    assert_eq!(
        hook_calls.load(Ordering::Relaxed),
        1,
        "replay hook must only run for the committed retry"
    );
    let delivered_message = tokio::time::timeout(Duration::from_secs(1), healthy_receiver.recv())
        .await
        .expect("healthy recipient should receive committed retry")
        .expect("healthy recipient channel remains open");
    assert!(
        matches!(*delivered_message, ServerMessage::Pong),
        "healthy recipient should receive the broadcast payload"
    );

    assert_eq!(
        metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
        3,
        "first pass reserves healthy and drops slow; retry reserves healthy again"
    );
    assert_eq!(
        metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed),
        1,
        "only the committed retry is enqueued"
    );
    assert_eq!(
        metrics
            .websocket_deliveries_canceled
            .load(Ordering::Relaxed),
        1,
        "healthy first-pass reservation must be canceled before the retry"
    );
    assert_eq!(
        metrics.websocket_messages_dropped.load(Ordering::Relaxed),
        1,
        "slow recipient's timed-out first-pass message is the only true drop"
    );
    assert_eq!(
        metrics
            .websocket_deliveries_channel_closed
            .load(Ordering::Relaxed),
        0,
        "no recipient channel closed during this scenario"
    );
    assert_eq!(
        metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
        metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed)
            + metrics
                .websocket_deliveries_canceled
                .load(Ordering::Relaxed)
            + metrics.websocket_messages_dropped.load(Ordering::Relaxed),
        "each attempted delivery should resolve as enqueued, canceled, or dropped"
    );

    drop(drain_tx);
    drop(slow_receiver);
}
