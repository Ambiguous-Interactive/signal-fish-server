//! Allocation baseline for the relay fan-out and classified outbound queue.
//!
//! Run explicitly:
//!
//! ```text
//! cargo bench --bench relay_allocations --features allocation-tracking
//! ```
//!
//! Counts are deterministic properties of the exercised code path; elapsed
//! time is intentionally absent because the instrumented allocator adds a
//! sequentially consistent atomic operation to every allocation.

use signal_fish_server::coordination::allocation_benchmark::{
    channel_with_metrics, DataDeliveryMetadata, OutboundData, OutboundPayload, OutboundReceiver,
    OutboundSender,
};
use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, MessageCoordinator,
};
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::{
    DeliveryClass, LobbyState, PlayerId, RoomId, ServerMessage, SpectatorJoinedPayload,
};
use signal_fish_server::server::InMemoryMessageCoordinator;
use stats_alloc::{Region, Stats, StatsAlloc, INSTRUMENTED_SYSTEM};
use std::alloc::System;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const RELAYS_PER_SAMPLE: usize = 4_096;
const REPEATS: usize = 5;
const ROOM_SIZES: [usize; 3] = [2, 8, 16];
const DATA_CAPACITY: usize = 32;
const CONTROL_CAPACITY: usize = 8;
const ROOM_ID: RoomId = RoomId::from_u128(0x66);
const SENDER_ID: PlayerId = PlayerId::from_u128(0x6600);
const QUEUE_RECIPIENT_ID: PlayerId = PlayerId::from_u128(0x6601);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sample {
    stats: Stats,
    deliveries: usize,
}

struct FanoutFixture {
    runtime: Runtime,
    coordinator: Arc<InMemoryMessageCoordinator>,
    metrics: Arc<ServerMetrics>,
    receivers: Vec<(PlayerId, OutboundReceiver)>,
    message: Arc<ServerMessage>,
}

impl FanoutFixture {
    fn new(room_size: usize) -> Self {
        assert!(
            room_size >= 2,
            "fan-out fixture needs a sender and recipient"
        );

        let runtime = Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread allocation runtime must build");
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator = Arc::new(InMemoryMessageCoordinator::with_delivery_policy(
            Duration::from_secs(1),
            Arc::clone(&metrics),
        ));
        let mut receivers = Vec::with_capacity(room_size);

        runtime.block_on(async {
            for index in 0..room_size {
                let player_id = if index == 0 {
                    SENDER_ID
                } else {
                    PlayerId::from_u128(0x6600 + index as u128)
                };
                let (sender, receiver) = classified_room_queue(
                    Arc::clone(&metrics),
                    player_id,
                    ROOM_ID,
                    DATA_CAPACITY,
                    CONTROL_CAPACITY,
                );
                let handle = ClientDeliveryHandle::classified_for_allocation_benchmark(
                    sender,
                    1,
                    ConnectionCloseSignal::detached(),
                );
                coordinator
                    .register_local_client(player_id, Some(ROOM_ID), handle)
                    .await
                    .expect("allocation fixture route must register");
                receivers.push((player_id, receiver));
            }
        });

        let message = game_data_message(SENDER_ID, 1);
        let mut fixture = Self {
            runtime,
            coordinator,
            metrics,
            receivers,
            message,
        };

        // Warm Tokio and every classified queue's backing storage before
        // taking a steady-state allocator snapshot. Recipient and join_all
        // storage is intentionally rebuilt inside each measured fan-out.
        let expected = room_size - 1;
        let warmed = fixture.relay_batch(1);
        assert_eq!(
            warmed, expected,
            "fan-out warm-up must reach every non-sender recipient"
        );
        fixture
    }

    fn relay_batch(&mut self, relays: usize) -> usize {
        let coordinator = Arc::clone(&self.coordinator);
        let message = Arc::clone(&self.message);
        let receivers = &mut self.receivers;

        self.runtime.block_on(async move {
            let mut deliveries = 0;
            for _ in 0..relays {
                let relay = Arc::clone(&message);
                coordinator
                    .broadcast_to_room_except_with_message(
                        &ROOM_ID,
                        &SENDER_ID,
                        Box::new(move || Some(relay)),
                    )
                    .await
                    .expect("fan-out broadcast must succeed");
                deliveries += drain_recipients(receivers, SENDER_ID);
            }
            deliveries
        })
    }

    fn measure(&mut self) -> Sample {
        let attempts_before = self
            .metrics
            .websocket_delivery_attempts
            .load(Ordering::Relaxed);
        let enqueued_before = self
            .metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed);
        let region = Region::new(GLOBAL);
        let deliveries = self.relay_batch(RELAYS_PER_SAMPLE);
        let stats = region.change();
        let attempts = self
            .metrics
            .websocket_delivery_attempts
            .load(Ordering::Relaxed)
            - attempts_before;
        let enqueued = self
            .metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed)
            - enqueued_before;

        let expected = RELAYS_PER_SAMPLE * (self.receivers.len() - 1);
        assert_eq!(
            deliveries, expected,
            "allocation baseline is vacuous: receiver drain count disagrees"
        );
        assert_eq!(
            attempts, expected as u64,
            "allocation baseline is vacuous: delivery attempts disagree"
        );
        assert_eq!(
            enqueued, expected as u64,
            "allocation baseline is vacuous: successful enqueues disagree"
        );

        Sample { stats, deliveries }
    }
}

struct QueueFixture {
    sender: OutboundSender,
    receiver: OutboundReceiver,
    message: Arc<ServerMessage>,
    metadata: DataDeliveryMetadata,
}

impl QueueFixture {
    fn new() -> Self {
        let metrics = Arc::new(ServerMetrics::new());
        let (sender, receiver) = classified_room_queue(
            metrics,
            QUEUE_RECIPIENT_ID,
            ROOM_ID,
            DATA_CAPACITY,
            CONTROL_CAPACITY,
        );
        let message = game_data_message(SENDER_ID, 1);
        let metadata = DataDeliveryMetadata {
            class: DeliveryClass::Reliable,
            key: None,
            from_player: SENDER_ID,
            room_id: ROOM_ID,
            epoch: 1,
            seq: 1,
        };
        let mut fixture = Self {
            sender,
            receiver,
            message,
            metadata,
        };
        assert_eq!(
            fixture.enqueue_and_drain(1),
            1,
            "classified queue warm-up must deliver one item"
        );
        fixture
    }

    fn enqueue_and_drain(&mut self, relays: usize) -> usize {
        let mut deliveries = 0;
        for _ in 0..relays {
            let outcome = self
                .sender
                .try_enqueue_data_scoped(
                    OutboundData::new(Arc::clone(&self.message), self.metadata),
                    1,
                )
                .expect("warmed reliable queue enqueue must succeed");
            assert!(outcome.enqueued, "reliable queue item must remain queued");
            assert_eq!(outcome.losses, 0, "reliable queue cannot report loss");
            let queued = self
                .receiver
                .try_recv()
                .expect("warmed reliable queue item must be available");
            assert!(
                matches!(queued.payload, OutboundPayload::Message(_)),
                "classified reliable queue must return the submitted data frame"
            );
            deliveries += 1;
        }
        deliveries
    }

    fn measure(&mut self) -> Sample {
        let region = Region::new(GLOBAL);
        let deliveries = self.enqueue_and_drain(RELAYS_PER_SAMPLE);
        let stats = region.change();
        assert_eq!(
            deliveries, RELAYS_PER_SAMPLE,
            "classified queue baseline is vacuous"
        );
        Sample { stats, deliveries }
    }
}

fn classified_room_queue(
    metrics: Arc<ServerMetrics>,
    player_id: PlayerId,
    room_id: RoomId,
    data_capacity: usize,
    control_capacity: usize,
) -> (OutboundSender, OutboundReceiver) {
    let (sender, mut receiver) = channel_with_metrics(data_capacity, control_capacity, metrics);
    sender.set_protocol_version(3);
    let transition = Arc::new(ServerMessage::SpectatorJoined(Box::new(
        SpectatorJoinedPayload {
            room_id,
            room_code: "ALLOC1".to_string(),
            spectator_id: player_id,
            game_name: "allocation-baseline".to_string(),
            current_players: Vec::new(),
            current_spectators: Vec::new(),
            lobby_state: LobbyState::Waiting,
            reason: None,
        },
    )));
    let outcome = sender
        .try_enqueue_transition(transition, 1)
        .expect("room transition must fit the fresh control queue");
    assert!(outcome.enqueued, "room transition must be queued");
    let barrier = receiver
        .try_recv()
        .expect("room transition barrier must be available");
    assert!(
        matches!(barrier.payload, OutboundPayload::Message(_)),
        "room transition must materialize as a control message"
    );
    (sender, receiver)
}

fn game_data_message(from_player: PlayerId, seq: u64) -> Arc<ServerMessage> {
    Arc::new(ServerMessage::GameData {
        from_player,
        data: serde_json::Value::Null,
        seq: Some(seq),
        epoch: Some(1),
        class: Some(DeliveryClass::Reliable),
        key: None,
    })
}

fn drain_recipients(receivers: &mut [(PlayerId, OutboundReceiver)], sender_id: PlayerId) -> usize {
    receivers
        .iter_mut()
        .filter(|(player_id, _)| *player_id != sender_id)
        .map(|(_, receiver)| {
            let queued = receiver
                .try_recv()
                .expect("every non-sender recipient must have one queued relay");
            assert!(
                matches!(queued.payload, OutboundPayload::Message(_)),
                "fan-out must enqueue a data message"
            );
            1
        })
        .sum()
}

fn repeated_samples(mut measure: impl FnMut() -> Sample) -> Sample {
    let first = measure();
    for repeat in 1..REPEATS {
        let observed = measure();
        assert_eq!(
            observed, first,
            "allocation sample {repeat} drifted; the baseline is contaminated by \
             setup, background work, or an un-warmed collection"
        );
    }
    first
}

fn print_sample(scope: &str, room_size: usize, relays: usize, sample: Sample) {
    let recipients = if scope == "fanout" { room_size - 1 } else { 1 };
    let allocation_operations = sample.stats.allocations + sample.stats.reallocations;
    println!(
        "{scope},{room_size},{recipients},{relays},{},{},{},{},{},{:.4},{:.2},{:.4},{:.2}",
        sample.deliveries,
        sample.stats.allocations,
        sample.stats.reallocations,
        sample.stats.deallocations,
        sample.stats.bytes_allocated,
        allocation_operations as f64 / relays as f64,
        sample.stats.bytes_allocated as f64 / relays as f64,
        allocation_operations as f64 / sample.deliveries as f64,
        sample.stats.bytes_allocated as f64 / sample.deliveries as f64,
    );
}

fn main() {
    println!(
        "scope,room_size,recipients,relays,deliveries,allocations,reallocations,\
         deallocations,bytes_allocated,allocation_ops_per_relay,bytes_per_relay,\
         allocation_ops_per_delivery,bytes_per_delivery"
    );

    for room_size in ROOM_SIZES {
        let mut fixture = FanoutFixture::new(room_size);
        let sample = repeated_samples(|| fixture.measure());
        print_sample("fanout", room_size, RELAYS_PER_SAMPLE, sample);
    }

    let mut fixture = QueueFixture::new();
    let sample = repeated_samples(|| fixture.measure());
    print_sample("classified_queue", 2, RELAYS_PER_SAMPLE, sample);
}
