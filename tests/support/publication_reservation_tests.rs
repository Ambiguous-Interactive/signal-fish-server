use crate::coordination::{ClientDeliveryHandle, ConnectionCloseSignal};
use crate::protocol::PlayerId;
use crate::server::{InMemoryMessageCoordinator, RoomBatchReservation};
use tokio::sync::mpsc;

fn sorted_covers(
    reservations: &[RoomBatchReservation],
    recipients: &[(PlayerId, ClientDeliveryHandle)],
    mut reserved_player: impl FnMut(&RoomBatchReservation) -> Option<PlayerId>,
) -> bool {
    recipients.iter().all(|(id, handle)| {
        InMemoryMessageCoordinator::batch_reservation_covers_recipient(
            reservations,
            *id,
            handle,
            &mut reserved_player,
        )
    })
}

fn original_covers(
    reservations: &[RoomBatchReservation],
    recipients: &[(PlayerId, ClientDeliveryHandle)],
    mut observe_comparison: impl FnMut(),
) -> bool {
    recipients.iter().all(|(id, handle)| {
        reservations.iter().any(|reservation| {
            observe_comparison();
            match reservation {
                RoomBatchReservation::Reserved {
                    player_id, sender, ..
                } => player_id == id && sender.same_channel(&handle.sender),
                _ => false,
            }
        })
    })
}

#[test]
fn sorted_reservation_validation_preserves_identity_and_channel_checks() {
    for count in [0_u128, 1, 2, 16, 255] {
        let mut reservations = Vec::new();
        let mut recipients = Vec::new();
        let mut receivers = Vec::new();
        for index in 1..=count {
            let id = PlayerId::from_u128(index);
            let (sender, receiver) = mpsc::channel(2);
            receivers.push(receiver);
            let handle = ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached());
            reservations.push(RoomBatchReservation::Reserved {
                player_id: id,
                sender: handle.sender.clone(),
                permits: [None, None],
                metrics: std::sync::Arc::new(crate::metrics::ServerMetrics::new()),
                stats: None,
            });
            recipients.push((id, handle));
        }
        reservations.reverse();
        reservations.push(RoomBatchReservation::Canceled);
        let (closed_sender, _closed_receiver) = mpsc::channel(2);
        let closed = ClientDeliveryHandle::new(closed_sender, ConnectionCloseSignal::detached());
        reservations.push(RoomBatchReservation::ChannelClosed {
            player_id: PlayerId::from_u128(1001),
            sender: closed.sender.clone(),
        });
        reservations.push(RoomBatchReservation::SlowConsumer {
            player_id: PlayerId::from_u128(1002),
            sender: closed.sender.clone(),
        });
        reservations.sort_unstable_by_key(RoomBatchReservation::reserved_player);
        recipients.reverse();
        assert!(sorted_covers(
            &reservations,
            &recipients,
            RoomBatchReservation::reserved_player,
        ));
        assert_eq!(
            original_covers(&reservations, &recipients, || {}),
            sorted_covers(
                &reservations,
                &recipients,
                RoomBatchReservation::reserved_player,
            )
        );
        if let Some((_, handle)) = recipients.first_mut() {
            let original = handle.clone();
            let (sender, _receiver) = mpsc::channel(2);
            *handle = ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached());
            assert!(!sorted_covers(
                &reservations,
                &recipients,
                RoomBatchReservation::reserved_player,
            ));
            let entry = recipients.first_mut().unwrap();
            entry.1 = original;
            entry.0 = PlayerId::from_u128(count + 1);
            assert!(!sorted_covers(
                &reservations,
                &recipients,
                RoomBatchReservation::reserved_player,
            ));
        }
    }
}

#[test]
fn sorted_reservation_validation_matches_original_large_roster() {
    let mut reservations = Vec::new();
    let mut recipients = Vec::new();
    let mut receivers = Vec::new();
    for index in 1..=255 {
        let id = PlayerId::from_u128(index);
        let (sender, receiver) = mpsc::channel(2);
        receivers.push(receiver);
        let handle = ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached());
        reservations.push(RoomBatchReservation::Reserved {
            player_id: id,
            sender: handle.sender.clone(),
            permits: [None, None],
            metrics: std::sync::Arc::new(crate::metrics::ServerMetrics::new()),
            stats: None,
        });
        recipients.push((id, handle));
    }
    let transactions = 100;
    let validations_per_transaction = 2;
    let members = recipients.len();
    let logarithmic_bound = usize::try_from(members.ilog2()).unwrap() + 2;
    let mut original_comparisons = 0;
    let mut original_elapsed = std::time::Duration::ZERO;
    for _ in 0..transactions {
        reservations.rotate_left(127);
        let original_started = tokio::time::Instant::now();
        for _ in 0..validations_per_transaction {
            assert!(original_covers(
                std::hint::black_box(&reservations),
                &recipients,
                || original_comparisons += 1
            ));
        }
        original_elapsed += original_started.elapsed();
    }
    assert_eq!(
        original_comparisons,
        transactions * validations_per_transaction * members * (members + 1) / 2
    );
    let mut sort_key_reads = 0;
    let mut lookup_comparisons = 0;
    let mut sort_elapsed = std::time::Duration::ZERO;
    let mut lookup_elapsed = std::time::Duration::ZERO;
    for _ in 0..transactions {
        reservations.rotate_left(127);
        let sort_started = tokio::time::Instant::now();
        reservations.sort_unstable_by_key(|reservation| {
            sort_key_reads += 1;
            reservation.reserved_player()
        });
        sort_elapsed += sort_started.elapsed();
        let lookup_started = tokio::time::Instant::now();
        for _ in 0..validations_per_transaction {
            assert!(sorted_covers(
                std::hint::black_box(&reservations),
                &recipients,
                |reservation| {
                    lookup_comparisons += 1;
                    reservation.reserved_player()
                }
            ));
        }
        lookup_elapsed += lookup_started.elapsed();
    }
    assert!(lookup_comparisons >= transactions * validations_per_transaction * members);
    assert!(
        lookup_comparisons
            <= transactions * validations_per_transaction * members * logarithmic_bound,
        "lookup returned to a full-roster scan: {lookup_comparisons}"
    );
    assert!(
        sort_key_reads <= transactions * 4 * members * logarithmic_bound,
        "reservation sort exceeded its comparison budget: {sort_key_reads}"
    );
    println!("RESERVATION_LOOKUP members={members} transactions={transactions} validations_per_transaction={validations_per_transaction} original_comparisons={original_comparisons} lookup_comparisons={lookup_comparisons} sort_key_reads={sort_key_reads} original={original_elapsed:?} lookup={lookup_elapsed:?} sort={sort_elapsed:?}");
    for receiver in &mut receivers {
        let trailing = receiver.try_recv();
        assert!(matches!(trailing, Err(mpsc::error::TryRecvError::Empty)));
    }
}

#[test]
fn observer_batch_preserves_precedence_for_reservation_and_commit() {
    use crate::coordination::RoomRecipientMessages;
    use crate::protocol::ServerMessage;
    use std::{collections::HashMap, sync::Arc};
    let id = PlayerId::from_u128(1);
    let seated_frame = Arc::new(ServerMessage::Pong);
    let observer_frame = Arc::new(ServerMessage::Error {
        message: "observer lifecycle".into(),
        error_code: None,
    });
    let seated = vec![RoomRecipientMessages::in_order(
        id,
        vec![seated_frame.clone(), seated_frame.clone()],
    )];
    let mut observers = HashMap::new();
    let batch = InMemoryMessageCoordinator::room_recipient_batch(&id, &seated, &observers).unwrap();
    assert_eq!(batch.messages.len(), 2);
    observers.insert(
        id,
        RoomRecipientMessages::in_order(id, vec![observer_frame.clone()]),
    );
    let batch = InMemoryMessageCoordinator::room_recipient_batch(&id, &seated, &observers).unwrap();
    assert_eq!(batch.messages.len(), 1);
    assert!(Arc::ptr_eq(&batch.messages[0], &observer_frame));
    assert!(InMemoryMessageCoordinator::room_recipient_batch(
        &PlayerId::nil(),
        &seated,
        &observers
    )
    .is_none());
}

/// #1446: sorted owned batches must still reject malformed coverage before hooks.
#[tokio::test]
async fn owned_publication_batches_reject_duplicate_missing_and_foreign_members() {
    use crate::coordination::{
        ClientDeliveryHandle, ConnectionCloseSignal, MessageCoordinator, RoomRecipientMessages,
    };
    use crate::protocol::{RoomId, ServerMessage};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let coordinator = InMemoryMessageCoordinator::new();
    let room = RoomId::new_v4();
    let first = PlayerId::from_u128(1);
    let second = PlayerId::from_u128(2);
    let foreign = PlayerId::from_u128(3);
    let mut receivers = Vec::new();
    for id in [first, second] {
        let (sender, receiver) = mpsc::channel(4);
        receivers.push(receiver);
        coordinator
            .register_local_client(
                id,
                Some(room),
                ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached()),
            )
            .await
            .unwrap();
    }
    for (name, expected, batches) in [
        ("empty expected", vec![], vec![first, second]),
        ("duplicate expected", vec![first, first], vec![first, first]),
        ("duplicate batch", vec![first, second], vec![second, second]),
        ("missing batch", vec![first, second], vec![first]),
        (
            "extra batch",
            vec![first, second],
            vec![first, second, foreign],
        ),
        ("foreign batch", vec![first, second], vec![first, foreign]),
    ] {
        let hooks = AtomicUsize::new(0);
        let result = coordinator
            .commit_room_messages_if_members_with_hook(
                &room,
                &expected,
                batches
                    .into_iter()
                    .map(|id| {
                        RoomRecipientMessages::in_order(id, vec![Arc::new(ServerMessage::Pong)])
                    })
                    .collect(),
                vec![],
                Box::new(|| {
                    Box::pin(async {
                        hooks.fetch_add(1, Ordering::Relaxed);
                        Ok(true)
                    })
                }),
                Box::new(|_| panic!("invalid membership reached phase callback")),
            )
            .await;
        assert!(result.is_err(), "{name}");
        assert_eq!(hooks.load(Ordering::Relaxed), 0, "{name}");
        for receiver in &mut receivers {
            let trailing = receiver.try_recv();
            assert!(
                matches!(trailing, Err(mpsc::error::TryRecvError::Empty)),
                "{name}"
            );
        }
    }
}
