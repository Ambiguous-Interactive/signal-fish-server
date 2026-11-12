//! Signal readers share a room fence; room publications keep exclusive ordering.
use super::*;

#[tokio::test]
async fn independent_signals_share_one_lane_and_block_mutations() {
    let sequencer = Arc::new(RoomEventSequencer::default());
    let room = RoomId::new_v4();
    let first = sequencer.lock_signal(room).await;
    let mut second = Box::pin(sequencer.lock_signal(room));
    let second = match futures_util::poll!(&mut second) {
        std::task::Poll::Ready(guard) => guard,
        std::task::Poll::Pending => panic!("independent signals must share admission"),
    };
    let mut mutation = Box::pin(sequencer.lock(room));
    assert!(futures_util::poll!(&mut mutation).is_pending());
    drop(first);
    assert!(futures_util::poll!(&mut mutation).is_pending());
    drop(second);
    drop(mutation.await);
    assert!(sequencer.lanes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn queued_mutation_precedes_later_signals_and_blocks_new_readers() {
    let sequencer = Arc::new(RoomEventSequencer::default());
    let room = RoomId::new_v4();
    let first = sequencer.lock_signal(room).await;
    let mut mutation = Box::pin(sequencer.lock(room));
    assert!(futures_util::poll!(&mut mutation).is_pending());
    let mut later = Box::pin(sequencer.lock_signal(room));
    assert!(futures_util::poll!(&mut later).is_pending());
    drop(first);
    let mutation = mutation.await;
    assert!(futures_util::poll!(&mut later).is_pending());
    drop(mutation);
    drop(later.await);
    assert!(sequencer.lanes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn canceled_signal_admission_releases_lane_without_stranding_mutation() {
    let sequencer = Arc::new(RoomEventSequencer::default());
    let room = RoomId::new_v4();
    let mutation = sequencer.lock(room).await;
    let mut signal = Box::pin(sequencer.lock_signal(room));
    assert!(futures_util::poll!(&mut signal).is_pending());
    drop(signal);
    drop(mutation);
    assert!(sequencer.lanes.lock().unwrap().is_empty());
    drop(sequencer.lock(room).await);
    assert!(sequencer.lanes.lock().unwrap().is_empty());
}
