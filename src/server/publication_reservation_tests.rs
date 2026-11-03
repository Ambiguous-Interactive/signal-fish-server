use super::{InMemoryMessageCoordinator, RoomBatchReservation};
use crate::coordination::{ClientDeliveryHandle, ConnectionCloseSignal};
use crate::protocol::PlayerId;
use tokio::sync::mpsc;

fn original_covers(
    reservations: &[RoomBatchReservation],
    recipients: &[(PlayerId, ClientDeliveryHandle)],
) -> bool {
    recipients.iter().all(|(id, handle)| {
        reservations.iter().any(|reservation| match reservation {
            RoomBatchReservation::Reserved {
                player_id, sender, ..
            } => player_id == id && sender.same_channel(&handle.sender),
            _ => false,
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
                permits: vec![],
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
        assert!(
            InMemoryMessageCoordinator::sorted_batch_reservations_cover_recipients(
                &reservations,
                &recipients
            )
        );
        assert_eq!(
            original_covers(&reservations, &recipients),
            InMemoryMessageCoordinator::sorted_batch_reservations_cover_recipients(
                &reservations,
                &recipients
            )
        );
        if let Some((_, handle)) = recipients.first_mut() {
            let original = handle.clone();
            let (sender, _receiver) = mpsc::channel(2);
            *handle = ClientDeliveryHandle::new(sender, ConnectionCloseSignal::detached());
            assert!(
                !InMemoryMessageCoordinator::sorted_batch_reservations_cover_recipients(
                    &reservations,
                    &recipients
                )
            );
            let entry = recipients.first_mut().unwrap();
            entry.1 = original;
            entry.0 = PlayerId::from_u128(count + 1);
            assert!(
                !InMemoryMessageCoordinator::sorted_batch_reservations_cover_recipients(
                    &reservations,
                    &recipients
                )
            );
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
            permits: vec![],
            stats: None,
        });
        recipients.push((id, handle));
    }
    let original_started = std::time::Instant::now();
    for _ in 0..100 {
        reservations.rotate_left(127);
        for _ in 0..2 {
            assert!(original_covers(
                std::hint::black_box(&reservations),
                &recipients
            ));
        }
    }
    let original_elapsed = original_started.elapsed();
    let sorted_started = std::time::Instant::now();
    for _ in 0..100 {
        reservations.rotate_left(127);
        reservations.sort_unstable_by_key(RoomBatchReservation::reserved_player);
        for _ in 0..2 {
            assert!(
                InMemoryMessageCoordinator::sorted_batch_reservations_cover_recipients(
                    std::hint::black_box(&reservations),
                    &recipients
                )
            );
        }
    }
    println!("RESERVATION_LOOKUP members=255 transactions=100 validations_per_transaction=2 original={original_elapsed:?} sorted={:?}", sorted_started.elapsed());
    for receiver in &mut receivers {
        let trailing = receiver.try_recv();
        assert!(matches!(trailing, Err(mpsc::error::TryRecvError::Empty)));
    }
}
