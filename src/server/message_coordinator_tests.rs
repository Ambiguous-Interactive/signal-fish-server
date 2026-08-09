use super::{ConditionalDeliveryReservation, ConnectionManager, InMemoryMessageCoordinator};
use crate::coordination::{
    ClientDeliveryHandle, CloseReason, ConnectionCloseSignal, DeliveryOutcome, MessageCoordinator,
    RoomMessageTransactionOutcome, RoomRecipientMessages,
};
use crate::metrics::ServerMetrics;
use crate::protocol::{
    DeliveryClass, GameDataEncoding, LobbyState, PlayerId, RoomId, ServerMessage,
    SpectatorJoinedPayload,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tokio::sync::{mpsc, watch, Notify};
use tokio::time::Duration;

#[derive(Clone, Copy, Debug)]
enum ControlCapacityWait {
    InitialTransition,
    ConditionalDelivery,
    ConditionalReservation,
}

#[derive(Clone, Copy, Debug)]
enum ControlQueueKind {
    Legacy,
    Classified,
}

#[derive(Clone, Copy, Debug)]
enum DeadlineBoundary {
    Exact,
    Post,
}

impl DeadlineBoundary {
    fn elapsed(self) -> Duration {
        match self {
            Self::Exact => Duration::from_secs(1),
            Self::Post => Duration::from_millis(1_001),
        }
    }
}

enum ControlReceiver {
    Legacy(mpsc::Receiver<Arc<ServerMessage>>),
    Classified(crate::coordination::outbound_queue::OutboundReceiver),
}

impl ControlReceiver {
    fn close(&mut self) {
        match self {
            Self::Legacy(receiver) => receiver.close(),
            Self::Classified(receiver) => receiver.close(),
        }
    }

    fn pop_message(&mut self, context: &str) -> Arc<ServerMessage> {
        match self {
            Self::Legacy(receiver) => receiver.try_recv().expect(context),
            Self::Classified(receiver) => {
                let queued = receiver.try_recv().expect(context);
                match queued.payload {
                    crate::coordination::outbound_queue::OutboundPayload::Message(message) => {
                        message
                    }
                    payload => panic!("{context}: expected control message, got {payload:?}"),
                }
            }
        }
    }

    fn assert_empty(&mut self, context: &str) {
        match self {
            Self::Legacy(receiver) => assert!(
                matches!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "{context}: legacy queue must remain open and empty"
            ),
            Self::Classified(receiver) => assert!(
                matches!(
                    receiver.try_recv(),
                    Err(crate::coordination::outbound_queue::TryReceiveError::Empty)
                ),
                "{context}: classified queue must remain open and empty"
            ),
        }
    }

    fn assert_disconnected(&mut self, context: &str) {
        match self {
            Self::Legacy(receiver) => assert!(
                matches!(
                    receiver.try_recv(),
                    Err(mpsc::error::TryRecvError::Disconnected)
                ),
                "{context}: legacy queue must be disconnected"
            ),
            Self::Classified(receiver) => assert!(
                matches!(
                    receiver.try_recv(),
                    Err(crate::coordination::outbound_queue::TryReceiveError::Disconnected)
                ),
                "{context}: classified queue must be disconnected"
            ),
        }
    }
}

fn full_control_queue(
    kind: ControlQueueKind,
) -> (
    ClientDeliveryHandle,
    crate::coordination::ConnectionCloseListener,
    ControlReceiver,
) {
    let (close, close_listener) = ConnectionCloseSignal::channel();
    match kind {
        ControlQueueKind::Legacy => {
            let (sender, receiver) = mpsc::channel(1);
            sender
                .try_send(Arc::new(ServerMessage::Pong))
                .expect("prefill must occupy the legacy control queue");
            (
                ClientDeliveryHandle::new(sender, close),
                close_listener,
                ControlReceiver::Legacy(receiver),
            )
        }
        ControlQueueKind::Classified => {
            let (sender, receiver) = crate::coordination::outbound_queue::channel(1, 1);
            sender.set_protocol_version(3);
            sender
                .try_enqueue_control_scoped(Arc::new(ServerMessage::Pong), None, 0)
                .expect("prefill must occupy the classified control lane");
            (
                ClientDeliveryHandle::classified(sender, close),
                close_listener,
                ControlReceiver::Classified(receiver),
            )
        }
    }
}

fn start_control_capacity_wait(
    case: ControlCapacityWait,
    coordinator: Arc<InMemoryMessageCoordinator>,
    player_id: PlayerId,
    handle: ClientDeliveryHandle,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = DeliveryOutcome> + Send>> {
    match case {
        ControlCapacityWait::InitialTransition => Box::pin(async move {
            match coordinator
                .reserve_initial_transition(player_id, &handle)
                .await
            {
                Ok(_permit) => DeliveryOutcome::Delivered,
                Err(outcome) => outcome,
            }
        }),
        ControlCapacityWait::ConditionalDelivery => {
            let (drain_tx, drain) = watch::channel(false);
            Box::pin(async move {
                let _drain_tx = drain_tx;
                coordinator
                    .deliver_to_one_if(
                        player_id,
                        handle,
                        Arc::new(ServerMessage::Pong),
                        &|| true,
                        drain,
                    )
                    .await
                    .unwrap_or(DeliveryOutcome::Canceled)
            })
        }
        ControlCapacityWait::ConditionalReservation => {
            let (drain_tx, drain) = watch::channel(false);
            Box::pin(async move {
                let _drain_tx = drain_tx;
                match coordinator
                    .reserve_one_if(player_id, handle, &|| true, drain, None)
                    .await
                {
                    ConditionalDeliveryReservation::SlowConsumer { .. } => {
                        DeliveryOutcome::SlowConsumer
                    }
                    ConditionalDeliveryReservation::ChannelClosed { .. } => {
                        DeliveryOutcome::ChannelClosed
                    }
                    ConditionalDeliveryReservation::Canceled => DeliveryOutcome::Canceled,
                    ConditionalDeliveryReservation::Reserved { .. } => DeliveryOutcome::Delivered,
                }
            })
        }
    }
}

async fn wait_for_counter(context: &str, max_yields: usize, mut condition: impl FnMut() -> bool) {
    for _ in 0..max_yields {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("{context}: counter condition never held");
}

#[tokio::test(start_paused = true)]
async fn expired_control_capacity_waits_cannot_revive_after_capacity_returns() {
    let cases = [
        ControlCapacityWait::InitialTransition,
        ControlCapacityWait::ConditionalDelivery,
        ControlCapacityWait::ConditionalReservation,
    ];
    let queue_kinds = [ControlQueueKind::Legacy, ControlQueueKind::Classified];
    let boundaries = [DeadlineBoundary::Exact, DeadlineBoundary::Post];

    for (case_index, case) in cases.into_iter().enumerate() {
        for (kind_index, kind) in queue_kinds.into_iter().enumerate() {
            for (boundary_index, boundary) in boundaries.into_iter().enumerate() {
                let metrics = Arc::new(ServerMetrics::new());
                let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
                    Duration::from_secs(1),
                    Arc::clone(&metrics),
                ));
                let player_id = PlayerId::from_u128(
                    0x660B_70BA_DA11_4CE1_8168_DA1A_D311_1000
                        + (case_index * 100 + kind_index * 10 + boundary_index) as u128,
                );
                let (handle, close_listener, mut receiver) = full_control_queue(kind);
                let mut wait = start_control_capacity_wait(
                    case,
                    Arc::clone(&coordinator),
                    player_id,
                    handle.clone(),
                );

                assert!(
                    futures_util::poll!(wait.as_mut()).is_pending(),
                    "{case:?}/{kind:?}/{boundary:?} must wait while the control queue is full"
                );
                tokio::time::advance(boundary.elapsed()).await;
                let prefill = receiver.pop_message("capacity must return at the test boundary");
                assert!(matches!(prefill.as_ref(), ServerMessage::Pong));

                assert_eq!(
                    wait.await,
                    DeliveryOutcome::SlowConsumer,
                    "{case:?}/{kind:?}/{boundary:?} must not use capacity returned at or after its deadline"
                );
                receiver.assert_empty(&format!("{case:?}/{kind:?}/{boundary:?}"));
                assert_eq!(
                    close_listener.requested_reason(),
                    Some(CloseReason::SlowConsumer),
                    "{case:?}/{kind:?}/{boundary:?} must expose the slow-consumer close reason"
                );
                assert_eq!(
                    metrics
                        .websocket_slow_consumer_disconnects
                        .load(Ordering::Relaxed),
                    1,
                    "{case:?}/{kind:?}/{boundary:?} must request exactly one slow-consumer close"
                );
                assert_eq!(
                    metrics.websocket_messages_dropped.load(Ordering::Relaxed),
                    1,
                    "{case:?}/{kind:?}/{boundary:?} must account for exactly one expired delivery"
                );
                assert_eq!(
                    metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
                    1,
                    "{case:?}/{kind:?}/{boundary:?} must account for one logical delivery attempt"
                );
                assert_eq!(
                    metrics
                        .websocket_backpressure_events
                        .load(Ordering::Relaxed),
                    1,
                    "{case:?}/{kind:?}/{boundary:?} must account for one full-queue wait"
                );
                assert_eq!(
                    metrics
                        .websocket_deliveries_enqueued
                        .load(Ordering::Relaxed),
                    0,
                    "{case:?}/{kind:?}/{boundary:?} must not account an expired delivery as enqueued"
                );
            }
        }
    }
}

/// Regression for #290's deadline-arbitration class across every classified
/// control-capacity wait: a writer release strictly before expiry remains
/// valid even if the producer is not scheduled again until after expiry.
#[tokio::test(start_paused = true)]
async fn predeadline_control_capacity_release_survives_delayed_producer_poll() {
    let cases = [
        ControlCapacityWait::InitialTransition,
        ControlCapacityWait::ConditionalDelivery,
        ControlCapacityWait::ConditionalReservation,
    ];

    for (index, case) in cases.into_iter().enumerate() {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(1),
            Arc::clone(&metrics),
        ));
        let player_id =
            PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_1800 + index as u128);
        let (handle, close_listener, mut receiver) =
            full_control_queue(ControlQueueKind::Classified);
        let mut wait = start_control_capacity_wait(case, coordinator, player_id, handle);

        assert!(
            futures_util::poll!(wait.as_mut()).is_pending(),
            "{case:?} must register its classified full-queue wait"
        );
        tokio::time::advance(Duration::from_millis(500)).await;
        let prefill = receiver.pop_message("release classified capacity before the deadline");
        assert!(matches!(prefill.as_ref(), ServerMessage::Pong));

        tokio::time::advance(Duration::from_millis(501)).await;
        assert_eq!(
            wait.await,
            DeliveryOutcome::Delivered,
            "{case:?} must retain capacity released before its deadline"
        );
        assert_eq!(
            close_listener.requested_reason(),
            None,
            "{case:?} must not request a slow-consumer close after timely progress"
        );
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0,
            "{case:?} must not count a timely writer as a slow consumer"
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            0,
            "{case:?} must not abandon a delivery after timely progress"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn predeadline_permit_release_survives_delayed_producer_poll() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(1),
        Arc::clone(&metrics),
    ));
    let player_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_1900);
    let (sender, _receiver) = crate::coordination::outbound_queue::channel(1, 1);
    sender.set_protocol_version(3);
    let held_permit = sender
        .try_reserve_control_scoped(0, None)
        .expect("occupy the only classified control slot with a permit");
    let (close, close_listener) = ConnectionCloseSignal::channel();
    let handle = ClientDeliveryHandle::classified(sender, close);
    let mut wait = start_control_capacity_wait(
        ControlCapacityWait::InitialTransition,
        coordinator,
        player_id,
        handle,
    );

    assert!(
        futures_util::poll!(wait.as_mut()).is_pending(),
        "initial transition must wait behind the held permit"
    );
    tokio::time::advance(Duration::from_millis(500)).await;
    drop(held_permit);
    tokio::time::advance(Duration::from_millis(501)).await;

    assert_eq!(wait.await, DeliveryOutcome::Delivered);
    assert_eq!(close_listener.requested_reason(), None);
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0
    );
}

/// Deadline evidence and the reservation must be one atomic queue-state
/// operation. Otherwise a pre-deadline drain followed by a refill can leave a
/// stale boolean that incorrectly admits capacity released only after expiry.
#[tokio::test(start_paused = true)]
async fn refilled_control_capacity_released_after_deadline_cannot_be_claimed() {
    let cases = [
        ControlCapacityWait::InitialTransition,
        ControlCapacityWait::ConditionalDelivery,
        ControlCapacityWait::ConditionalReservation,
    ];

    for (index, case) in cases.into_iter().enumerate() {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(1),
            Arc::clone(&metrics),
        ));
        let player_id =
            PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_1A00 + index as u128);
        let (handle, close_listener, mut receiver) =
            full_control_queue(ControlQueueKind::Classified);
        let mut wait = start_control_capacity_wait(case, coordinator, player_id, handle.clone());
        assert!(futures_util::poll!(wait.as_mut()).is_pending());

        tokio::time::advance(Duration::from_millis(500)).await;
        drop(receiver.pop_message("briefly return control capacity before the deadline"));
        handle
            .sender
            .try_send(Arc::new(ServerMessage::Pong), None)
            .expect("another producer refills the control lane");
        tokio::time::advance(Duration::from_millis(501)).await;
        drop(receiver.pop_message("refilled control capacity returns after the deadline"));

        assert_eq!(wait.await, DeliveryOutcome::SlowConsumer);
        assert_eq!(
            close_listener.requested_reason(),
            Some(CloseReason::SlowConsumer)
        );
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            1
        );
    }
}

#[tokio::test(start_paused = true)]
async fn queue_closure_at_or_after_the_deadline_precedes_slow_consumer_expiry() {
    let cases = [
        ControlCapacityWait::InitialTransition,
        ControlCapacityWait::ConditionalDelivery,
        ControlCapacityWait::ConditionalReservation,
    ];
    let queue_kinds = [ControlQueueKind::Legacy, ControlQueueKind::Classified];
    let boundaries = [DeadlineBoundary::Exact, DeadlineBoundary::Post];

    for case in cases {
        for kind in queue_kinds {
            for boundary in boundaries {
                let metrics = Arc::new(ServerMetrics::new());
                let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
                    Duration::from_secs(1),
                    Arc::clone(&metrics),
                ));
                let player_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_2000);
                let (handle, close_listener, mut receiver) = full_control_queue(kind);
                let mut wait = start_control_capacity_wait(case, coordinator, player_id, handle);

                assert!(
                    futures_util::poll!(wait.as_mut()).is_pending(),
                    "{case:?}/{kind:?}/{boundary:?} must enter backpressure"
                );
                tokio::time::advance(boundary.elapsed()).await;
                receiver.close();

                assert_eq!(
                    wait.await,
                    DeliveryOutcome::ChannelClosed,
                    "{case:?}/{kind:?}/{boundary:?} must preserve closure at or after the deadline"
                );
                assert_eq!(
                    close_listener.requested_reason(),
                    None,
                    "{case:?}/{kind:?}/{boundary:?} must not misclassify closure as slow consumption"
                );
                assert_eq!(
                    metrics
                        .websocket_deliveries_channel_closed
                        .load(Ordering::Relaxed),
                    1
                );
                assert_eq!(
                    metrics
                        .websocket_slow_consumer_disconnects
                        .load(Ordering::Relaxed),
                    0
                );
                assert_eq!(
                    metrics.websocket_messages_dropped.load(Ordering::Relaxed),
                    0
                );
                let prefill = receiver.pop_message("closed queue retains its existing item");
                assert!(matches!(prefill.as_ref(), ServerMessage::Pong));
                receiver.assert_disconnected(&format!("{case:?}/{kind:?}/{boundary:?}"));
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn classified_generation_cancellation_at_the_deadline_precedes_slow_consumer_expiry() {
    let cases = [
        ControlCapacityWait::InitialTransition,
        ControlCapacityWait::ConditionalDelivery,
        ControlCapacityWait::ConditionalReservation,
    ];

    for (index, case) in cases.into_iter().enumerate() {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(1),
            Arc::clone(&metrics),
        ));
        let player_id =
            PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_3000 + index as u128);
        let (handle, close_listener, mut receiver) =
            full_control_queue(ControlQueueKind::Classified);
        let mut wait =
            start_control_capacity_wait(case, Arc::clone(&coordinator), player_id, handle.clone());

        assert!(
            futures_util::poll!(wait.as_mut()).is_pending(),
            "{case:?} must enter classified backpressure"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        let prefill = receiver.pop_message("release the classified control lane");
        assert!(matches!(prefill.as_ref(), ServerMessage::Pong));

        // Generation zero may legitimately reserve generation one's
        // transition. Move initial-transition waits two generations ahead so
        // every case is unambiguously stale at the exact deadline.
        let generation_advances = if matches!(case, ControlCapacityWait::InitialTransition) {
            2
        } else {
            1
        };
        let mut current_sender = handle.sender.clone();
        for advance in 0..generation_advances {
            current_sender = current_sender.next_generation();
            current_sender
                .try_send(Arc::new(ServerMessage::RoomLeft), None)
                .expect("advance classified queue generation at the deadline");
            if advance + 1 < generation_advances {
                let transition =
                    receiver.pop_message("release capacity for the next generation transition");
                assert!(matches!(transition.as_ref(), ServerMessage::RoomLeft));
            }
        }

        assert_eq!(
            wait.await,
            DeliveryOutcome::Canceled,
            "{case:?} must preserve the classified generation fence at the deadline"
        );
        assert_eq!(
            close_listener.requested_reason(),
            None,
            "{case:?} must not close a stale classified generation as a slow consumer"
        );
        assert_eq!(
            metrics
                .websocket_deliveries_canceled
                .load(Ordering::Relaxed),
            1
        );
        assert_eq!(
            metrics
                .websocket_slow_consumer_disconnects
                .load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            metrics.websocket_messages_dropped.load(Ordering::Relaxed),
            0
        );
        assert_eq!(
            metrics
                .websocket_deliveries_enqueued
                .load(Ordering::Relaxed),
            0,
            "{case:?} must not account any late coordinator enqueue"
        );
        let transition = receiver.pop_message("only the explicit generation transition remains");
        assert!(matches!(transition.as_ref(), ServerMessage::RoomLeft));
        receiver.assert_empty(&format!("{case:?} classified generation cancellation"));
    }
}

#[tokio::test]
async fn missing_sender_stamp_cancels_broadcast_before_any_delivery_attempt() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(1),
        Arc::clone(&metrics),
    );
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0010);
    let sender_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0011);
    let recipient_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0012);
    let (sender, mut receiver) = mpsc::channel(1);

    coordinator
        .register_local_client(
            recipient_id,
            Some(room_id),
            ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("register recipient");

    let mut build_message = || None;
    coordinator
        .broadcast_to_room_except_with_borrowed_message(&room_id, &sender_id, &mut build_message)
        .await
        .expect("missing stamp cancels cleanly");

    let unexpected = receiver.try_recv();
    assert!(
        matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)),
        "an unregistered sender must not emit unstamped data"
    );
    assert_eq!(
        metrics.websocket_delivery_attempts.load(Ordering::Relaxed),
        0,
        "cancellation happens before recipient delivery accounting"
    );
}

#[tokio::test(start_paused = true)]
async fn builder_broadcast_backpressure_releases_routing_locks_and_keeps_snapshot() {
    for builder_kind in ["boxed", "borrowed", "borrowed-owned"] {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(5),
            Arc::clone(&metrics),
        ));
        let room_id = RoomId::new_v4();
        let sender_id = PlayerId::new_v4();
        let recipient_id = PlayerId::new_v4();
        let healthy_id = PlayerId::new_v4();
        let late_joiner_id = PlayerId::new_v4();
        let mut classified_receivers = Vec::new();
        for player_id in [recipient_id, healthy_id] {
            let (sender, mut receiver) = crate::coordination::outbound_queue::channel(1, 1);
            sender.set_protocol_version(3);
            sender.set_game_data_format(GameDataEncoding::Json);
            assert!(sender.delivery_classes_enabled());
            let mut handle =
                ClientDeliveryHandle::classified(sender, ConnectionCloseSignal::detached());
            handle.sender = handle.sender.next_generation();
            assert_eq!(
                handle.sender.relay_projection(),
                Some((true, GameDataEncoding::Json))
            );
            let transition = Arc::new(ServerMessage::SpectatorJoined(Box::new(
                SpectatorJoinedPayload {
                    room_id,
                    room_code: "CACHE1".to_string(),
                    spectator_id: player_id,
                    game_name: "relay-cache-test".to_string(),
                    current_players: Vec::new(),
                    current_spectators: Vec::new(),
                    lobby_state: LobbyState::Waiting,
                    reason: None,
                },
            )));
            let transition_outcome = handle
                .sender
                .try_send(transition, Some(room_id))
                .expect("establish the classified recipient's room scope");
            assert!(transition_outcome.enqueued);
            let transition = receiver
                .try_recv()
                .expect("drain the classified room-scope transition");
            assert!(matches!(
                transition.payload,
                crate::coordination::outbound_queue::OutboundPayload::Message(_)
            ));
            if player_id == recipient_id {
                let prefill_outcome = handle
                    .sender
                    .try_send(
                        Arc::new(ServerMessage::GameData {
                            from_player: sender_id,
                            data: serde_json::json!({"seq": 1}),
                            seq: Some(1),
                            epoch: Some(1),
                            class: Some(DeliveryClass::Reliable),
                            key: None,
                        }),
                        Some(room_id),
                    )
                    .expect("prefill the selected recipient's classified data queue");
                assert!(prefill_outcome.enqueued);
            }
            coordinator
                .register_local_client(player_id, Some(room_id), handle)
                .await
                .expect("register a classified relay recipient");
            classified_receivers.push((player_id, receiver));
        }

        let mut broadcast = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move {
                if builder_kind == "borrowed" {
                    let mut build_message = || {
                        Some(Arc::new(ServerMessage::GameData {
                            from_player: sender_id,
                            data: serde_json::json!({"seq": 2}),
                            seq: Some(2),
                            epoch: Some(1),
                            class: Some(DeliveryClass::Reliable),
                            key: None,
                        }))
                    };
                    coordinator
                        .broadcast_to_room_except_with_borrowed_message(
                            &room_id,
                            &sender_id,
                            &mut build_message,
                        )
                        .await
                } else if builder_kind == "borrowed-owned" {
                    let mut build_message = || {
                        Some(ServerMessage::GameData {
                            from_player: sender_id,
                            data: serde_json::json!({"seq": 2}),
                            seq: Some(2),
                            epoch: Some(1),
                            class: Some(DeliveryClass::Reliable),
                            key: None,
                        })
                    };
                    coordinator
                        .broadcast_to_room_except_with_borrowed_owned_message(
                            &room_id,
                            &sender_id,
                            &mut build_message,
                        )
                        .await
                } else {
                    coordinator
                        .broadcast_to_room_except_with_message(
                            &room_id,
                            &sender_id,
                            Box::new(|| {
                                Some(Arc::new(ServerMessage::GameData {
                                    from_player: sender_id,
                                    data: serde_json::json!({"seq": 2}),
                                    seq: Some(2),
                                    epoch: Some(1),
                                    class: Some(DeliveryClass::Reliable),
                                    key: None,
                                }))
                            }),
                        )
                        .await
                }
            })
        };
        wait_for_counter("builder broadcast reached backpressure", 10_000, || {
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                == 1
        })
        .await;

        let (late_joiner_tx, mut late_joiner_rx) = mpsc::channel(1);
        tokio::time::timeout(
            Duration::from_millis(10),
            coordinator.register_local_client(
                late_joiner_id,
                Some(room_id),
                ClientDeliveryHandle::new(late_joiner_tx, ConnectionCloseSignal::detached()),
            ),
        )
        .await
        .unwrap_or_else(|_| {
            panic!("{builder_kind} builder held routing locks across its capacity wait")
        })
        .expect("late joiner registration must succeed during capacity wait");
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut broadcast)
                .await
                .is_err(),
            "builder broadcast must still be waiting for the original recipient"
        );

        let (_, recipient_rx) = classified_receivers
            .iter_mut()
            .find(|(player_id, _)| *player_id == recipient_id)
            .expect("backpressured recipient receiver must exist");
        let prefill = recipient_rx
            .try_recv()
            .expect("original recipient prefill must still be queued");
        assert_eq!(prefill.class(), Some(DeliveryClass::Reliable));
        assert!(
            matches!(
                prefill.payload,
                crate::coordination::outbound_queue::OutboundPayload::Message(message)
                    if matches!(message.as_ref(), ServerMessage::GameData { seq: Some(1), .. })
            ),
            "original recipient prefill changed while the relay waited"
        );
        broadcast
            .await
            .expect("builder broadcast task must not panic")
            .expect("builder broadcast must complete after capacity returns");
        let mut observed_delivery = None;
        for (player_id, receiver) in &mut classified_receivers {
            let relayed = receiver
                .try_recv()
                .unwrap_or_else(|error| panic!("recipient {player_id} lost relay: {error:?}"));
            let crate::coordination::outbound_queue::OutboundPayload::Data(delivery) =
                relayed.payload
            else {
                panic!("{builder_kind} relay lost its shared carrier");
            };
            assert!(matches!(
                delivery.message(),
                ServerMessage::GameData { seq: Some(2), .. }
            ));
            if let Some(expected) = &observed_delivery {
                assert!(
                    delivery.shares_relay_carrier_with(expected),
                    "{builder_kind} relay recipients must share one carrier through retry"
                );
            } else {
                observed_delivery = Some(delivery);
            }
        }
        let unexpected = late_joiner_rx.try_recv();
        assert!(
            matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)),
            "late joiner must not enter the already-started routing snapshot"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn terminal_unroute_captures_every_stamp_allocated_before_player_left() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(1),
        Arc::clone(&metrics),
    ));
    let connection_manager = Arc::new(ConnectionManager::new(
        8,
        metrics,
        coordinator.clone(),
        false,
    ));
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0013);
    let sender_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0014);
    let recipient_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0015);
    let addr = "127.0.0.1:41000".parse().expect("test address");
    let (sender_tx, _sender_rx) = mpsc::channel(4);
    connection_manager
        .connect_test_client(sender_id, sender_tx, addr)
        .await;
    connection_manager
        .assign_client_to_room(&sender_id, room_id)
        .await;
    let (recipient_tx, mut recipient_rx) = mpsc::channel(4);
    connection_manager
        .connect_test_client(recipient_id, recipient_tx, addr)
        .await;
    connection_manager
        .assign_client_to_room(&recipient_id, room_id)
        .await;

    let (stamp_allocated_tx, stamp_allocated_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let relay_task = {
        let coordinator = Arc::clone(&coordinator);
        let connection_manager = Arc::clone(&connection_manager);
        let release = Arc::clone(&release);
        tokio::spawn(async move {
            let mut build_once = Some(move || {
                let stamp = connection_manager
                    .next_relay_stamp_in_room(&sender_id, &room_id)
                    .expect("sender remains routed during allocation");
                stamp_allocated_tx
                    .send(())
                    .expect("test waits for stamp allocation");
                let (released, wake) = &*release;
                let mut released = released.lock().expect("release lock");
                while !*released {
                    released = wake.wait(released).expect("release wait");
                }
                Some(Arc::new(ServerMessage::GameData {
                    from_player: sender_id,
                    data: serde_json::json!({"n": stamp.seq}),
                    seq: Some(stamp.seq),
                    epoch: Some(stamp.epoch),
                    class: None,
                    key: None,
                }))
            });
            let mut build_message = move || build_once.take().and_then(|build| build());
            coordinator
                .broadcast_to_room_except_with_borrowed_message(
                    &room_id,
                    &sender_id,
                    &mut build_message,
                )
                .await
        })
    };
    stamp_allocated_rx
        .await
        .expect("relay reaches allocation boundary");

    let mut unroute_task = {
        let coordinator = Arc::clone(&coordinator);
        let connection_manager = Arc::clone(&connection_manager);
        tokio::spawn(async move {
            coordinator
                .unroute_local_client_with_tail(
                    sender_id,
                    room_id,
                    Box::new(move || {
                        connection_manager
                            .clear_room_assignment_with_tail(&sender_id)
                            .map(|(delivery, stamp)| (delivery, stamp.epoch, stamp.seq))
                    }),
                )
                .await
        })
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut unroute_task)
            .await
            .is_err(),
        "terminal unroute must wait for a relay that already owns the routing snapshot"
    );

    {
        let (released, wake) = &*release;
        *released.lock().expect("release lock") = true;
        wake.notify_all();
    }
    relay_task
        .await
        .expect("relay task should not panic")
        .expect("relay should commit");
    assert_eq!(
        unroute_task
            .await
            .expect("unroute task should not panic")
            .expect("unroute should succeed"),
        Some((1, 1)),
        "PlayerLeft tail covers the last relay allocated before unroute"
    );
    assert!(matches!(
        recipient_rx.recv().await.as_deref(),
        Some(ServerMessage::GameData {
            seq: Some(1),
            epoch: Some(1),
            ..
        })
    ));
    assert_eq!(
        connection_manager.next_relay_stamp_in_room(&sender_id, &room_id),
        None,
        "no old-room stamp can be allocated after terminal unroute"
    );
}

#[tokio::test]
async fn registration_replaces_the_players_room_routing_scope() {
    let coordinator = InMemoryMessageCoordinator::new();
    let player_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0020);
    let room_a = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0021);
    let room_b = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0022);
    let (sender, mut receiver) = mpsc::channel(4);
    let delivery = ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached());

    coordinator
        .register_local_client(player_id, Some(room_a), delivery.clone())
        .await
        .expect("register player in first room");
    coordinator
        .register_local_client(player_id, None, delivery.clone())
        .await
        .expect("clear player room routing");
    coordinator
        .broadcast_to_room(&room_a, Arc::new(ServerMessage::Pong))
        .await
        .expect("broadcast to former room");

    let unexpected = receiver.try_recv();
    assert!(
        matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)),
        "clearing room routing must remove the player from the former room"
    );

    coordinator
        .register_local_client(player_id, Some(room_b), delivery)
        .await
        .expect("register player in replacement room");
    coordinator
        .broadcast_to_room(&room_a, Arc::new(ServerMessage::Pong))
        .await
        .expect("broadcast to stale room after replacement");
    coordinator
        .broadcast_to_room(&room_b, Arc::new(ServerMessage::Pong))
        .await
        .expect("broadcast to replacement room");

    let delivered = receiver.recv().await.expect("replacement room delivery");
    assert!(matches!(*delivered, ServerMessage::Pong));
    let unexpected = receiver.try_recv();
    assert!(
        matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)),
        "the stale room must not contribute a second delivery"
    );
}

#[tokio::test(start_paused = true)]
async fn initial_room_transitions_wait_for_capacity_before_taking_routing_locks() {
    // This test owns the deadline boundary. Full-suite Miri execution can
    // consume a real one-second timeout while the queue is intentionally full.
    for async_builder in [false, true] {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(1),
            Arc::clone(&metrics),
        ));
        let room_id = RoomId::new_v4();
        let stable_id = PlayerId::new_v4();
        let joining_id = PlayerId::new_v4();
        let (stable_sender, mut stable_receiver) = mpsc::channel(4);
        coordinator
            .register_local_client(
                stable_id,
                Some(room_id),
                ClientDeliveryHandle::new(stable_sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("register stable room member");

        let (joining_sender, mut joining_receiver) = mpsc::channel(1);
        joining_sender
            .try_send(Arc::new(ServerMessage::Pong))
            .expect("fill the one-slot transition queue");
        let joining_delivery =
            ClientDeliveryHandle::new(joining_sender, ConnectionCloseSignal::detached());
        let mut registration = {
            let coordinator = Arc::clone(&coordinator);
            tokio::spawn(async move {
                if async_builder {
                    coordinator
                        .register_local_client_with_initial_message_async(
                            joining_id,
                            room_id,
                            joining_delivery,
                            Box::new(|_| Box::pin(async { Ok(Arc::new(ServerMessage::Pong)) })),
                        )
                        .await
                } else {
                    coordinator
                        .register_local_client_with_initial_message(
                            joining_id,
                            room_id,
                            joining_delivery,
                            Box::new(|| Arc::new(ServerMessage::Pong)),
                        )
                        .await
                }
            })
        };
        wait_for_counter(
            "initial transition reached queue backpressure",
            10_000,
            || {
                metrics
                    .websocket_backpressure_events
                    .load(Ordering::Relaxed)
                    == 1
            },
        )
        .await;

        coordinator
            .broadcast_to_room(&room_id, Arc::new(ServerMessage::Pong))
            .await
            .expect("existing member broadcast is not blocked by baseline capacity");
        assert!(matches!(
            stable_receiver.recv().await.as_deref(),
            Some(ServerMessage::Pong)
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut registration)
                .await
                .is_err(),
            "registration still waits for its reserved initial-frame capacity"
        );

        assert!(matches!(
            joining_receiver.recv().await.as_deref(),
            Some(ServerMessage::Pong)
        ));
        assert_eq!(
            registration
                .await
                .expect("registration task should not panic")
                .expect("registration should succeed"),
            crate::coordination::DeliveryOutcome::Delivered
        );
        assert!(matches!(
            joining_receiver.recv().await.as_deref(),
            Some(ServerMessage::Pong)
        ));

        coordinator
            .broadcast_to_room(&room_id, Arc::new(ServerMessage::Pong))
            .await
            .expect("new member becomes routable after its baseline commits");
        assert!(matches!(
            stable_receiver.recv().await.as_deref(),
            Some(ServerMessage::Pong)
        ));
        assert!(matches!(
            joining_receiver.recv().await.as_deref(),
            Some(ServerMessage::Pong)
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn slow_consumer_timeout_does_not_remove_a_replacement_connection() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(5),
        Arc::clone(&metrics),
    ));
    let player_id = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0023);

    let (old_sender, _old_receiver) = mpsc::channel(1);
    old_sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("pre-fill replaced connection");
    let (old_close, mut old_close_listener) = ConnectionCloseSignal::channel();
    coordinator
        .register_local_client(
            player_id,
            None,
            ClientDeliveryHandle::new(old_sender, old_close),
        )
        .await
        .expect("register old connection");

    let timed_out_send = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move {
            coordinator
                .send_to_player(&player_id, Arc::new(ServerMessage::Pong))
                .await
        })
    };
    wait_for_counter("old connection reached backpressure", 10_000, || {
        metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            == 1
    })
    .await;

    let (replacement_sender, mut replacement_receiver) = mpsc::channel(2);
    coordinator
        .register_local_client(
            player_id,
            None,
            ClientDeliveryHandle::new(replacement_sender, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("register replacement connection");

    tokio::time::advance(Duration::from_secs(6)).await;
    timed_out_send
        .await
        .expect("timed-out send task should not panic")
        .expect("timed-out send should finish cleanly");
    assert_eq!(
        old_close_listener.closed().await,
        Some(crate::coordination::CloseReason::SlowConsumer)
    );

    coordinator
        .send_to_player(&player_id, Arc::new(ServerMessage::Pong))
        .await
        .expect("send through replacement connection");
    assert!(matches!(
        replacement_receiver.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
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
        .register_local_client(
            player_id,
            None,
            ClientDeliveryHandle {
                sender: sender.into(),
                close,
            },
        )
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
                sender: healthy_sender.into(),
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
                sender: slow_sender.into(),
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

#[tokio::test]
async fn rerouted_recipient_retries_broadcast_for_stable_peer_before_replay_hook() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(30),
        Arc::clone(&metrics),
    ));
    let room_a = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0200);
    let room_b = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0201);
    let source = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0202);
    let stable = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0203);
    let rerouted = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0204);

    let (stable_sender, mut stable_receiver) = mpsc::channel(2);
    coordinator
        .register_local_client(
            stable,
            Some(room_a),
            ClientDeliveryHandle::new(stable_sender, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("register stable recipient");

    let (queue_sender, _queue_receiver) = crate::coordination::outbound_queue::channel(1, 2);
    let stale_delivery =
        ClientDeliveryHandle::classified(queue_sender, ConnectionCloseSignal::detached());
    let mut current_delivery = stale_delivery.clone();
    current_delivery.sender = current_delivery.sender.next_generation();
    current_delivery
        .sender
        .try_send(Arc::new(ServerMessage::RoomLeft), None)
        .expect("advance the physical queue to the replacement generation");
    coordinator
        .register_local_client(rerouted, Some(room_a), stale_delivery)
        .await
        .expect("register stale routing snapshot");

    let hook_calls = Arc::new(AtomicUsize::new(0));
    let broadcast_task = {
        let coordinator = Arc::clone(&coordinator);
        let hook_calls = Arc::clone(&hook_calls);
        tokio::spawn(async move {
            let (_drain_tx, drain_rx) = watch::channel(false);
            let should_send = || true;
            coordinator
                .broadcast_to_room_except_if_with_hook(
                    &room_a,
                    &source,
                    Arc::new(ServerMessage::Pong),
                    &should_send,
                    drain_rx,
                    Box::new(move || {
                        Box::pin(async move {
                            hook_calls.fetch_add(1, Ordering::Relaxed);
                        })
                    }),
                )
                .await
        })
    };

    wait_for_counter(
        "stale recipient canceled the first snapshot",
        10_000,
        || {
            metrics
                .websocket_deliveries_canceled
                .load(Ordering::Relaxed)
                >= 2
        },
    )
    .await;
    coordinator
        .register_local_client(rerouted, Some(room_b), current_delivery)
        .await
        .expect("reroute replacement generation");

    assert!(
        tokio::time::timeout(Duration::from_secs(1), broadcast_task)
            .await
            .expect("broadcast retry should finish")
            .expect("broadcast task should not panic")
            .expect("broadcast retry should not error"),
        "the stable peer must receive the retried room event"
    );
    assert_eq!(
        hook_calls.load(Ordering::Relaxed),
        1,
        "replay records only the committed recipient snapshot"
    );
    assert!(matches!(
        stable_receiver.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    let unexpected = stable_receiver.try_recv();
    assert!(matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)));
}

#[tokio::test(start_paused = true)]
async fn replay_hook_and_live_broadcast_are_atomic_against_reconnect_registration() {
    let coordinator = Arc::new(InMemoryMessageCoordinator::new());
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0300);
    let existing = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0301);
    let reconnecting = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0302);
    let departed = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0303);

    let (existing_sender, mut existing_receiver) = mpsc::channel(4);
    coordinator
        .register_local_client(
            existing,
            Some(room_id),
            ClientDeliveryHandle::new(existing_sender, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("register existing room member");

    let replay_recorded = Arc::new(AtomicBool::new(false));
    let hook_entered = Arc::new(Notify::new());
    let release_hook = Arc::new(Notify::new());
    let broadcast_task = {
        let coordinator = Arc::clone(&coordinator);
        let replay_recorded = Arc::clone(&replay_recorded);
        let hook_entered = Arc::clone(&hook_entered);
        let release_hook = Arc::clone(&release_hook);
        tokio::spawn(async move {
            coordinator
                .broadcast_to_room_with_hook(
                    &room_id,
                    Arc::new(ServerMessage::PlayerLeft {
                        player_id: departed,
                        epoch: None,
                        final_seq: None,
                    }),
                    Box::new(move || {
                        Box::pin(async move {
                            replay_recorded.store(true, Ordering::Release);
                            hook_entered.notify_one();
                            release_hook.notified().await;
                        })
                    }),
                )
                .await
        })
    };
    hook_entered.notified().await;

    let (reconnecting_sender, mut reconnecting_receiver) = mpsc::channel(4);
    let mut registration_task = {
        let coordinator = Arc::clone(&coordinator);
        let replay_recorded = Arc::clone(&replay_recorded);
        tokio::spawn(async move {
            coordinator
                .register_local_client_with_initial_message_async(
                    reconnecting,
                    room_id,
                    ClientDeliveryHandle::new(
                        reconnecting_sender,
                        ConnectionCloseSignal::detached(),
                    ),
                    Box::new(move |_| {
                        Box::pin(async move {
                            Ok(Arc::new(ServerMessage::Error {
                                message: if replay_recorded.load(Ordering::Acquire) {
                                    "replay contains PlayerLeft".to_string()
                                } else {
                                    "replay missed PlayerLeft".to_string()
                                },
                                error_code: None,
                            }))
                        })
                    }),
                )
                .await
        })
    };

    assert!(
        tokio::time::timeout(Duration::from_millis(1), &mut registration_task)
            .await
            .is_err(),
        "reconnect registration must wait while replay and live delivery share the routing snapshot"
    );
    release_hook.notify_one();

    assert!(broadcast_task
        .await
        .expect("broadcast task should not panic")
        .expect("broadcast should succeed"));
    assert_eq!(
        registration_task
            .await
            .expect("registration task should not panic")
            .expect("registration should succeed"),
        crate::coordination::DeliveryOutcome::Delivered
    );

    assert!(matches!(
        existing_receiver.recv().await.as_deref(),
        Some(ServerMessage::PlayerLeft { player_id, .. }) if *player_id == departed
    ));
    match reconnecting_receiver
        .recv()
        .await
        .expect("reconnector receives its baseline")
        .as_ref()
    {
        ServerMessage::Error { message, .. } => {
            assert_eq!(message, "replay contains PlayerLeft");
        }
        other => panic!("unexpected reconnect baseline marker: {other:?}"),
    }
    let unexpected = reconnecting_receiver.try_recv();
    assert!(
        matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)),
        "the reconnector must not receive PlayerLeft live as well as through replay"
    );
}

fn two_frame_batch(player_id: PlayerId) -> RoomRecipientMessages {
    RoomRecipientMessages {
        player_id,
        first_phase: 0,
        messages: vec![
            Arc::new(ServerMessage::Pong),
            Arc::new(ServerMessage::Error {
                message: "tailored plan marker".to_string(),
                error_code: None,
            }),
        ],
    }
}

#[tokio::test]
async fn two_frame_transaction_progresses_at_minimum_control_capacity() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(1),
        Arc::clone(&metrics),
    ));
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0428);
    let player = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0429);
    let (sender, mut receiver) = mpsc::channel(2);
    sender
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("occupy one of the minimum two control slots");
    coordinator
        .register_local_client(
            player,
            Some(room_id),
            ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("register minimum-capacity recipient");
    let transaction = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move {
            coordinator
                .commit_room_messages_if_members_with_hook(
                    &room_id,
                    &[player],
                    vec![two_frame_batch(player)],
                    Box::new(|| Box::pin(async { Ok(true) })),
                    Box::new(|_| true),
                )
                .await
        })
    };
    wait_for_counter("second reservation reached backpressure", 10_000, || {
        metrics
            .websocket_backpressure_events
            .load(Ordering::Relaxed)
            == 1
    })
    .await;
    assert!(matches!(
        receiver.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    assert_eq!(
        transaction
            .await
            .expect("transaction task must not panic")
            .expect("minimum-capacity transaction succeeds"),
        RoomMessageTransactionOutcome::Committed
    );
    assert!(matches!(
        receiver.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    assert!(matches!(
        receiver.recv().await.as_deref(),
        Some(ServerMessage::Error { message, .. }) if message == "tailored plan marker"
    ));
}

#[tokio::test]
async fn room_transaction_commits_every_phase_zero_frame_before_phase_one() {
    let coordinator = InMemoryMessageCoordinator::new();
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0430);
    let joiner = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0431);
    let incumbent = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0432);
    let (joiner_tx, mut joiner_rx) = mpsc::channel(1);
    let (incumbent_tx, mut incumbent_rx) = mpsc::channel(1);
    for (player, sender) in [(joiner, joiner_tx), (incumbent, incumbent_tx)] {
        coordinator
            .register_local_client(
                player,
                Some(room_id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("register phased transaction member");
    }

    let outcome = coordinator
        .commit_room_messages_if_members_with_hook(
            &room_id,
            &[joiner, incumbent],
            vec![
                RoomRecipientMessages::in_order(joiner, vec![Arc::new(ServerMessage::Pong)]),
                RoomRecipientMessages::from_first_phase(
                    incumbent,
                    1,
                    vec![Arc::new(ServerMessage::Error {
                        message: "phase one".to_string(),
                        error_code: None,
                    })],
                ),
            ],
            Box::new(|| Box::pin(async { Ok(true) })),
            Box::new(|failed_phase_zero| {
                assert_eq!(failed_phase_zero, 0);
                let joiner_phase = joiner_rx.try_recv();
                assert!(matches!(joiner_phase.as_deref(), Ok(ServerMessage::Pong)));
                let incumbent_phase = incumbent_rx.try_recv();
                assert!(matches!(
                    incumbent_phase,
                    Err(mpsc::error::TryRecvError::Empty)
                ));
                true
            }),
        )
        .await
        .expect("phased transaction succeeds");

    assert_eq!(outcome, RoomMessageTransactionOutcome::Committed);
    assert!(matches!(
        incumbent_rx.recv().await.as_deref(),
        Some(ServerMessage::Error { message, .. }) if message == "phase one"
    ));
}

#[tokio::test]
async fn failed_phase_zero_can_cancel_every_dependent_phase_one_frame() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(1),
        Arc::clone(&metrics),
    );
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0440);
    let joiner = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0441);
    let incumbent = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0442);
    let (joiner_tx, mut joiner_rx) = crate::coordination::outbound_queue::channel(1, 1);
    joiner_tx.set_protocol_version(3);
    joiner_tx
        .try_enqueue_transition(
            Arc::new(ServerMessage::SpectatorJoined(Box::new(
                SpectatorJoinedPayload {
                    room_id,
                    room_code: "phase-zero-close".to_string(),
                    spectator_id: joiner,
                    game_name: "phase-zero-close".to_string(),
                    current_players: Vec::new(),
                    current_spectators: Vec::new(),
                    lobby_state: LobbyState::Lobby,
                    reason: None,
                },
            ))),
            1,
        )
        .expect("initialize classified actor generation");
    joiner_rx
        .recv()
        .await
        .expect("classified actor receiver remains open")
        .expect("classified actor transition is queued");
    let mut joiner_delivery =
        ClientDeliveryHandle::classified(joiner_tx, ConnectionCloseSignal::detached());
    joiner_delivery.sender = joiner_delivery.sender.next_generation();
    coordinator
        .register_local_client(joiner, Some(room_id), joiner_delivery)
        .await
        .expect("register classified actor");
    let (incumbent_tx, mut incumbent_rx) = mpsc::channel(1);
    coordinator
        .register_local_client(
            incumbent,
            Some(room_id),
            ClientDeliveryHandle::new(incumbent_tx, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("register dependent incumbent");
    let observed_failures = Arc::new(AtomicUsize::new(0));
    let callback_observation = Arc::clone(&observed_failures);

    let outcome = coordinator
        .commit_room_messages_if_members_with_hook(
            &room_id,
            &[joiner, incumbent],
            vec![
                RoomRecipientMessages::in_order(joiner, vec![Arc::new(ServerMessage::Pong)]),
                RoomRecipientMessages::from_first_phase(
                    incumbent,
                    1,
                    vec![Arc::new(ServerMessage::Pong)],
                ),
            ],
            Box::new(move || {
                Box::pin(async move {
                    joiner_rx.close();
                    Ok(true)
                })
            }),
            Box::new(move |failed_phase_zero| {
                callback_observation.store(failed_phase_zero, Ordering::Release);
                false
            }),
        )
        .await
        .expect("post-commit closure is reported as degraded delivery");

    assert_eq!(
        outcome,
        RoomMessageTransactionOutcome::CommittedDegraded { failed_frames: 2 }
    );
    assert_eq!(observed_failures.load(Ordering::Acquire), 1);
    let unexpected = incumbent_rx.try_recv();
    assert!(matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)));
    assert_eq!(
        metrics
            .websocket_deliveries_channel_closed
            .load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        metrics
            .websocket_deliveries_canceled
            .load(Ordering::Relaxed),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn room_transaction_reserves_slow_recipients_in_one_timeout_window() {
    let metrics = Arc::new(ServerMetrics::new());
    let timeout_window = Duration::from_secs(5);
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        timeout_window,
        Arc::clone(&metrics),
    ));
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0450);
    let alice = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0451);
    let bob = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0452);
    let (alice_tx, _alice_rx) = mpsc::channel(1);
    let (bob_tx, _bob_rx) = mpsc::channel(1);
    for (player, sender) in [(alice, alice_tx), (bob, bob_tx)] {
        sender
            .try_send(Arc::new(ServerMessage::Pong))
            .expect("fill recipient queue");
        coordinator
            .register_local_client(
                player,
                Some(room_id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("register slow transaction member");
    }

    let transaction = {
        let coordinator = Arc::clone(&coordinator);
        tokio::spawn(async move {
            coordinator
                .commit_room_messages_if_members_with_hook(
                    &room_id,
                    &[alice, bob],
                    vec![
                        RoomRecipientMessages::in_order(alice, vec![Arc::new(ServerMessage::Pong)]),
                        RoomRecipientMessages::in_order(bob, vec![Arc::new(ServerMessage::Pong)]),
                    ],
                    Box::new(|| Box::pin(async { Ok(true) })),
                    Box::new(|_| true),
                )
                .await
        })
    };
    wait_for_counter(
        "both recipient reservations reached backpressure",
        10_000,
        || {
            metrics
                .websocket_backpressure_events
                .load(Ordering::Relaxed)
                == 2
        },
    )
    .await;

    tokio::time::advance(timeout_window - Duration::from_millis(1)).await;
    assert_eq!(
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed),
        0
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    wait_for_counter("both slow recipients expired together", 10_000, || {
        metrics
            .websocket_slow_consumer_disconnects
            .load(Ordering::Relaxed)
            == 2
    })
    .await;

    assert_eq!(
        transaction
            .await
            .expect("transaction task must not panic")
            .expect("backpressure is a routing outcome"),
        RoomMessageTransactionOutcome::RoutingChanged
    );
}

#[tokio::test]
async fn room_transaction_hook_error_delivers_no_partial_frames() {
    let coordinator = InMemoryMessageCoordinator::new();
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0400);
    let alice = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0401);
    let bob = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0402);
    let (alice_tx, mut alice_rx) = mpsc::channel(2);
    let (bob_tx, mut bob_rx) = mpsc::channel(2);
    for (player, sender) in [(alice, alice_tx), (bob, bob_tx)] {
        coordinator
            .register_local_client(
                player,
                Some(room_id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("register transaction member");
    }

    let result = coordinator
        .commit_room_messages_if_members_with_hook(
            &room_id,
            &[alice, bob],
            vec![two_frame_batch(alice), two_frame_batch(bob)],
            Box::new(|| Box::pin(async { anyhow::bail!("injected finalize failure") })),
            Box::new(|_| true),
        )
        .await;

    assert!(result.is_err(), "fallible commit hook error must propagate");
    for receiver in [&mut alice_rx, &mut bob_rx] {
        let unexpected = receiver.try_recv();
        assert!(matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)));
    }
}

#[tokio::test]
async fn receiver_close_during_commit_hook_degrades_without_suppressing_healthy_phases() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(1),
        Arc::clone(&metrics),
    );
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0403);
    let healthy = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0404);
    let closing = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0405);
    let (healthy_tx, mut healthy_rx) = mpsc::channel(2);
    coordinator
        .register_local_client(
            healthy,
            Some(room_id),
            ClientDeliveryHandle::new(healthy_tx, ConnectionCloseSignal::detached()),
        )
        .await
        .expect("register healthy member");
    let (closing_tx, mut closing_rx) = crate::coordination::outbound_queue::channel(1, 2);
    closing_tx.set_protocol_version(3);
    closing_tx
        .try_enqueue_transition(
            Arc::new(ServerMessage::SpectatorJoined(Box::new(
                SpectatorJoinedPayload {
                    room_id,
                    room_code: "transaction-close".to_string(),
                    spectator_id: closing,
                    game_name: "transaction-close".to_string(),
                    current_players: Vec::new(),
                    current_spectators: Vec::new(),
                    lobby_state: LobbyState::Lobby,
                    reason: None,
                },
            ))),
            1,
        )
        .expect("advance the classified queue into the transaction room");
    closing_rx
        .recv()
        .await
        .expect("transition queue remains accountable")
        .expect("room transition is queued");
    let mut closing_delivery =
        ClientDeliveryHandle::classified(closing_tx, ConnectionCloseSignal::detached());
    closing_delivery.sender = closing_delivery.sender.next_generation();
    coordinator
        .register_local_client(closing, Some(room_id), closing_delivery)
        .await
        .expect("register closing member");
    let after_first_phase_calls = Arc::new(AtomicUsize::new(0));
    let callback_calls = Arc::clone(&after_first_phase_calls);

    let outcome = coordinator
        .commit_room_messages_if_members_with_hook(
            &room_id,
            &[healthy, closing],
            vec![two_frame_batch(healthy), two_frame_batch(closing)],
            Box::new(move || {
                Box::pin(async move {
                    closing_rx.close();
                    Ok(true)
                })
            }),
            Box::new(move |_| {
                callback_calls.fetch_add(1, Ordering::AcqRel);
                true
            }),
        )
        .await
        .expect("post-commit close is a degraded outcome, not an infrastructure error");

    assert_eq!(
        outcome,
        RoomMessageTransactionOutcome::CommittedDegraded { failed_frames: 2 }
    );
    assert_eq!(after_first_phase_calls.load(Ordering::Acquire), 1);
    assert!(matches!(
        healthy_rx.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));
    assert!(matches!(
        healthy_rx.recv().await.as_deref(),
        Some(ServerMessage::Error { message, .. }) if message == "tailored plan marker"
    ));
    assert_eq!(
        metrics
            .websocket_deliveries_channel_closed
            .load(Ordering::Relaxed),
        2,
        "every failed reserved frame is accounted after the durable commit"
    );
}

#[tokio::test]
async fn member_leave_while_second_frame_waits_aborts_whole_room_transaction() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_secs(1),
        Arc::clone(&metrics),
    ));
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0410);
    let stable = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0411);
    let leaving = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0412);
    let (stable_tx, mut stable_rx) = mpsc::channel(2);
    let (leaving_tx, mut leaving_rx) = mpsc::channel(2);
    leaving_tx
        .try_send(Arc::new(ServerMessage::Pong))
        .expect("occupy one slot so the second transaction frame waits");
    for (player, sender) in [(stable, stable_tx), (leaving, leaving_tx)] {
        coordinator
            .register_local_client(
                player,
                Some(room_id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("register transaction member");
    }

    let hook_called = Arc::new(AtomicBool::new(false));
    let transaction = {
        let coordinator = Arc::clone(&coordinator);
        let hook_called = Arc::clone(&hook_called);
        tokio::spawn(async move {
            coordinator
                .commit_room_messages_if_members_with_hook(
                    &room_id,
                    &[stable, leaving],
                    vec![two_frame_batch(stable), two_frame_batch(leaving)],
                    Box::new(move || {
                        Box::pin(async move {
                            hook_called.store(true, Ordering::Release);
                            Ok(true)
                        })
                    }),
                    Box::new(|_| true),
                )
                .await
        })
    };

    wait_for_counter("every frame reservation was attempted", 10_000, || {
        metrics.websocket_delivery_attempts.load(Ordering::Relaxed) >= 4
    })
    .await;
    coordinator
        .unregister_local_client(&leaving)
        .await
        .expect("publish the member leave");
    assert!(matches!(
        leaving_rx.recv().await.as_deref(),
        Some(ServerMessage::Pong)
    ));

    assert_eq!(
        transaction
            .await
            .expect("transaction task must not panic")
            .expect("routing cancellation is not an infrastructure error"),
        RoomMessageTransactionOutcome::RoutingChanged
    );
    assert!(!hook_called.load(Ordering::Acquire));
    let unexpected_stable = stable_rx.try_recv();
    assert!(matches!(
        unexpected_stable,
        Err(mpsc::error::TryRecvError::Empty)
    ));
    let unexpected_leaving = leaving_rx.try_recv();
    assert!(matches!(
        unexpected_leaving,
        Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected)
    ));
}

#[tokio::test]
async fn full_recipient_queue_aborts_room_transaction_before_commit_hook() {
    let coordinator = InMemoryMessageCoordinator::with_delivery_policy(
        Duration::from_millis(10),
        Arc::new(ServerMetrics::new()),
    );
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0420);
    let stable = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0421);
    let blocked = PlayerId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_0422);
    let (stable_tx, mut stable_rx) = mpsc::channel(2);
    let (blocked_tx, _blocked_rx) = mpsc::channel(2);
    blocked_tx.try_send(Arc::new(ServerMessage::Pong)).unwrap();
    blocked_tx.try_send(Arc::new(ServerMessage::Pong)).unwrap();
    for (player, sender) in [(stable, stable_tx), (blocked, blocked_tx)] {
        coordinator
            .register_local_client(
                player,
                Some(room_id),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .expect("register transaction member");
    }
    let hook_called = Arc::new(AtomicBool::new(false));
    let hook_marker = Arc::clone(&hook_called);

    let outcome = coordinator
        .commit_room_messages_if_members_with_hook(
            &room_id,
            &[stable, blocked],
            vec![two_frame_batch(stable), two_frame_batch(blocked)],
            Box::new(move || {
                Box::pin(async move {
                    hook_marker.store(true, Ordering::Release);
                    Ok(true)
                })
            }),
            Box::new(|_| true),
        )
        .await
        .expect("backpressure cancellation is not an infrastructure error");

    assert_eq!(outcome, RoomMessageTransactionOutcome::RoutingChanged);
    assert!(!hook_called.load(Ordering::Acquire));
    let unexpected = stable_rx.try_recv();
    assert!(matches!(unexpected, Err(mpsc::error::TryRecvError::Empty)));
}
