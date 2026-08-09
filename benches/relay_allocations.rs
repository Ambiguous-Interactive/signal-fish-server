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
    DeliveryClass, GameDataEncoding, LobbyState, PlayerId, RoomId, ServerMessage,
    SpectatorJoinedPayload,
};
use signal_fish_server::server::allocation_benchmark::broadcast_game_data_with;
use signal_fish_server::server::InMemoryMessageCoordinator;
use stats_alloc::{Region, Stats, StatsAlloc, INSTRUMENTED_SYSTEM};
use std::alloc::System;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};

use bytes::Bytes;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const RELAYS_PER_SAMPLE: usize = 4_096;
// One stable current-thread runtime/block_on allocation belongs to the whole
// sample, not to any logical relay.
const SAMPLE_FIXED_ALLOCATION_OPERATIONS: usize = 1;
const SAMPLE_FIXED_ALLOCATED_BYTES: usize = 64;
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
    delivery_handle_clones: usize,
}

#[derive(Debug, Clone, Copy)]
enum IngressKind {
    Json,
    Binary,
}

impl IngressKind {
    const ALL: [Self; 2] = [Self::Json, Self::Binary];

    const fn name(self) -> &'static str {
        match self {
            Self::Json => "production_json_ingress",
            Self::Binary => "production_binary_ingress",
        }
    }
}

#[derive(Clone)]
enum IngressPayload {
    Json(serde_json::Value),
    Binary(Bytes),
}

struct FanoutFixture {
    runtime: Runtime,
    coordinator: Arc<InMemoryMessageCoordinator>,
    metrics: Arc<ServerMetrics>,
    receivers: Vec<(PlayerId, OutboundReceiver)>,
    clone_probes: Vec<OutboundSender>,
    payload: IngressPayload,
    handoff_message: Arc<ServerMessage>,
    ingress_kind: IngressKind,
}

impl FanoutFixture {
    fn new(room_size: usize, ingress_kind: IngressKind) -> Self {
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
        let mut clone_probes = Vec::with_capacity(room_size);

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
                    sender.clone(),
                    1,
                    ConnectionCloseSignal::detached(),
                );
                coordinator
                    .register_local_client(player_id, Some(ROOM_ID), handle)
                    .await
                    .expect("allocation fixture route must register");
                receivers.push((player_id, receiver));
                clone_probes.push(sender);
            }
        });

        // Construct caller-owned payloads before the measured region. The
        // production ingress cell still builds the ServerMessage and shared
        // carrier in the measured region, while the isolated handoff cell uses
        // the prebuilt minimal envelope retained by the historical baseline.
        let payload = ingress_payload(ingress_kind);
        let handoff_message = game_data_message(ingress_kind, SENDER_ID, 1);
        let mut fixture = Self {
            runtime,
            coordinator,
            metrics,
            receivers,
            clone_probes,
            payload,
            handoff_message,
            ingress_kind,
        };

        // Warm Tokio and every classified queue's backing storage before
        // taking a steady-state allocator snapshot. The healthy path walks
        // the guarded routing snapshot directly; exceptional backpressure
        // storage is intentionally not exercised by this baseline.
        let expected = room_size - 1;
        let warmed = fixture.relay_ingress_batch(vec![fixture.payload.clone()]);
        assert_eq!(
            warmed, expected,
            "fan-out warm-up must reach every non-sender recipient"
        );
        fixture
    }

    fn relay_ingress_batch(&mut self, payloads: Vec<IngressPayload>) -> usize {
        let coordinator = Arc::clone(&self.coordinator);
        let metrics = Arc::clone(&self.metrics);
        let receivers = &mut self.receivers;
        let ingress_kind = self.ingress_kind;

        self.runtime.block_on(async move {
            let mut deliveries = 0;
            for payload in payloads {
                broadcast_game_data_with(
                    coordinator.as_ref(),
                    metrics.as_ref(),
                    &SENDER_ID,
                    &ROOM_ID,
                    move || Some(game_data_from_payload(ingress_kind, payload, SENDER_ID, 1)),
                )
                .await
                .expect("production game-data handoff must succeed");
                deliveries += drain_recipients(receivers, SENDER_ID, Some(ingress_kind));
            }
            deliveries
        })
    }

    fn relay_handoff_batch(&mut self, relays: usize) -> usize {
        let coordinator = Arc::clone(&self.coordinator);
        let message = Arc::clone(&self.handoff_message);
        let receivers = &mut self.receivers;
        let ingress_kind = self.ingress_kind;
        self.runtime.block_on(async move {
            let mut deliveries = 0;
            for _ in 0..relays {
                let mut relay = Some(Arc::clone(&message));
                let mut build_message = move || relay.take();
                coordinator
                    .broadcast_to_room_except_with_borrowed_message(
                        &ROOM_ID,
                        &SENDER_ID,
                        &mut build_message,
                    )
                    .await
                    .expect("borrowed coordinator handoff must succeed");
                deliveries += drain_recipients(receivers, SENDER_ID, Some(ingress_kind));
            }
            deliveries
        })
    }

    fn measure_ingress(&mut self) -> Sample {
        let payloads = vec![self.payload.clone(); RELAYS_PER_SAMPLE];
        let delivery_handle_clones_before = self.delivery_handle_clone_operations();
        let attempts_before = self
            .metrics
            .websocket_delivery_attempts
            .load(Ordering::Relaxed);
        let enqueued_before = self
            .metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed);
        let messages_before = self.metrics.game_data_messages.load(Ordering::Relaxed);
        let region = Region::new(GLOBAL);
        let deliveries = self.relay_ingress_batch(payloads);
        let stats = region.change();
        let delivery_handle_clones = self
            .delivery_handle_clone_operations()
            .saturating_sub(delivery_handle_clones_before);
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
        let messages = self.metrics.game_data_messages.load(Ordering::Relaxed) - messages_before;

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
        assert_eq!(
            messages, RELAYS_PER_SAMPLE as u64,
            "allocation baseline is vacuous: production ingress ledger disagrees"
        );

        Sample {
            stats,
            deliveries,
            delivery_handle_clones,
        }
    }

    fn measure_handoff(&mut self) -> Sample {
        let delivery_handle_clones_before = self.delivery_handle_clone_operations();
        let region = Region::new(GLOBAL);
        let deliveries = self.relay_handoff_batch(RELAYS_PER_SAMPLE);
        let stats = region.change();
        let delivery_handle_clones = self
            .delivery_handle_clone_operations()
            .saturating_sub(delivery_handle_clones_before);
        assert_eq!(
            deliveries,
            RELAYS_PER_SAMPLE * (self.receivers.len() - 1),
            "borrowed handoff baseline is vacuous"
        );
        Sample {
            stats,
            deliveries,
            delivery_handle_clones,
        }
    }

    fn delivery_handle_clone_operations(&self) -> usize {
        self.clone_probes
            .iter()
            .map(OutboundSender::clone_operations_for_allocation_benchmark)
            .sum()
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
        let message = game_data_message(IngressKind::Json, SENDER_ID, 1);
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
                matches!(
                    queued.payload,
                    OutboundPayload::Message(_) | OutboundPayload::Data(_)
                ),
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
        Sample {
            stats,
            deliveries,
            delivery_handle_clones: 0,
        }
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

fn game_data_message(
    ingress_kind: IngressKind,
    from_player: PlayerId,
    seq: u64,
) -> Arc<ServerMessage> {
    match ingress_kind {
        IngressKind::Json => Arc::new(ServerMessage::GameData {
            from_player,
            data: serde_json::Value::Null,
            seq: Some(seq),
            epoch: Some(1),
            class: Some(DeliveryClass::Reliable),
            key: None,
        }),
        IngressKind::Binary => Arc::new(ServerMessage::GameDataBinary {
            from_player,
            encoding: GameDataEncoding::MessagePack,
            payload: Bytes::from_static(b"\x83\xa4tick\x07\xa1x\x01\xa1y\xff"),
            seq: Some(seq),
            epoch: Some(1),
        }),
    }
}

fn ingress_payload(ingress_kind: IngressKind) -> IngressPayload {
    match ingress_kind {
        IngressKind::Json => IngressPayload::Json(serde_json::json!({
            "tick": 7,
            "input": [1, -1, 0],
        })),
        IngressKind::Binary => {
            IngressPayload::Binary(Bytes::from_static(b"\x83\xa4tick\x07\xa1x\x01\xa1y\xff"))
        }
    }
}

fn game_data_from_payload(
    ingress_kind: IngressKind,
    payload: IngressPayload,
    from_player: PlayerId,
    seq: u64,
) -> ServerMessage {
    match (ingress_kind, payload) {
        (IngressKind::Json, IngressPayload::Json(data)) => ServerMessage::GameData {
            from_player,
            data,
            seq: Some(seq),
            epoch: Some(1),
            class: Some(DeliveryClass::Reliable),
            key: None,
        },
        (IngressKind::Binary, IngressPayload::Binary(payload)) => ServerMessage::GameDataBinary {
            from_player,
            encoding: GameDataEncoding::MessagePack,
            payload,
            seq: Some(seq),
            epoch: Some(1),
        },
        _ => panic!("ingress kind and payload must match"),
    }
}

fn drain_recipients(
    receivers: &mut [(PlayerId, OutboundReceiver)],
    sender_id: PlayerId,
    expected_kind: Option<IngressKind>,
) -> usize {
    receivers
        .iter_mut()
        .filter(|(player_id, _)| *player_id != sender_id)
        .map(|(_, receiver)| {
            let queued = receiver
                .try_recv()
                .expect("every non-sender recipient must have one queued relay");
            if let Some(expected_kind) = expected_kind {
                let message = match &queued.payload {
                    OutboundPayload::Message(message) => message.as_ref(),
                    OutboundPayload::Data(data) => data.message(),
                    OutboundPayload::DeliveryReport(_) => {
                        panic!("relay unexpectedly queued a delivery report")
                    }
                };
                assert!(
                    matches!(
                        (expected_kind, message),
                        (IngressKind::Json, ServerMessage::GameData { .. })
                            | (IngressKind::Binary, ServerMessage::GameDataBinary { .. })
                    ),
                    "queued relay variant disagrees with measured ingress"
                );
            }
            assert!(
                matches!(
                    queued.payload,
                    OutboundPayload::Message(_) | OutboundPayload::Data(_)
                ),
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

fn assert_healthy_fanout_uses_synchronous_fast_path(room_size: usize, sample: Sample) {
    let allocation_operations = sample.stats.allocations + sample.stats.reallocations;
    let maximum_operations_per_relay = match room_size {
        2 => 1,
        8 | 16 => 2,
        _ => panic!("room-{room_size} has no checked-in allocation baseline"),
    };
    let maximum_bytes_per_relay = match room_size {
        2 => 368,
        8 | 16 => 1_048,
        _ => panic!("room-{room_size} has no checked-in allocation baseline"),
    };
    let maximum_operations =
        RELAYS_PER_SAMPLE * maximum_operations_per_relay + SAMPLE_FIXED_ALLOCATION_OPERATIONS;
    let maximum_bytes = RELAYS_PER_SAMPLE * maximum_bytes_per_relay + SAMPLE_FIXED_ALLOCATED_BYTES;
    assert!(
        allocation_operations <= maximum_operations,
        "healthy {room_size}-player fan-out used {allocation_operations} allocation operations \
         across {RELAYS_PER_SAMPLE} relays; expected at most \
         {maximum_operations_per_relay} operations per relay plus one fixed sample operation \
         after removing the routed-recipient snapshot"
    );
    assert!(
        sample.stats.bytes_allocated <= maximum_bytes,
        "healthy {room_size}-player fan-out allocated {} bytes across {RELAYS_PER_SAMPLE} \
         relays; expected at most {maximum_bytes_per_relay} bytes per relay plus \
         {SAMPLE_FIXED_ALLOCATED_BYTES} fixed sample bytes after removing the routed-recipient \
         snapshot",
        sample.stats.bytes_allocated
    );
    assert_eq!(
        sample.delivery_handle_clones, 0,
        "healthy {room_size}-player fan-out cloned delivery handles; borrow routing-map handles \
         and reserve ownership for exceptional backpressure or slow-consumer cleanup"
    );
}

fn assert_production_ingress_ceiling(room_size: usize, sample: Sample) {
    let maximum_operations_per_relay = match room_size {
        2 | 8 | 16 => 1,
        _ => panic!("room-{room_size} has no checked-in allocation baseline"),
    };
    let allocation_operations = sample.stats.allocations + sample.stats.reallocations;
    let maximum_bytes_per_relay = match room_size {
        2 => 296,
        8 | 16 => 752,
        _ => panic!("room-{room_size} has no checked-in allocation baseline"),
    };
    assert!(
        allocation_operations
            <= RELAYS_PER_SAMPLE * maximum_operations_per_relay
                + SAMPLE_FIXED_ALLOCATION_OPERATIONS,
        "production {room_size}-player ingress exceeded {maximum_operations_per_relay} \
         allocation operations per relay"
    );
    assert!(
        sample.stats.bytes_allocated
            <= RELAYS_PER_SAMPLE * maximum_bytes_per_relay + SAMPLE_FIXED_ALLOCATED_BYTES,
        "production {room_size}-player ingress exceeded {maximum_bytes_per_relay} allocated \
         bytes per relay"
    );
    assert_eq!(
        sample.delivery_handle_clones, 0,
        "production {room_size}-player ingress cloned delivery handles; healthy fan-out must \
         borrow them from the guarded routing map"
    );
}

fn print_sample(scope: &str, room_size: usize, recipients: usize, relays: usize, sample: Sample) {
    let allocation_operations = sample.stats.allocations + sample.stats.reallocations;
    println!(
        "{scope},{room_size},{recipients},{relays},{},{},{},{},{},{},{:.4},{:.2},{:.4},{:.2}",
        sample.deliveries,
        sample.delivery_handle_clones,
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
        "scope,room_size,recipients,relays,deliveries,delivery_handle_clones,allocations,reallocations,\
         deallocations,bytes_allocated,allocation_ops_per_relay,bytes_per_relay,\
         allocation_ops_per_delivery,bytes_per_delivery"
    );

    for ingress_kind in IngressKind::ALL {
        for room_size in ROOM_SIZES {
            let mut fixture = FanoutFixture::new(room_size, ingress_kind);
            let sample = repeated_samples(|| fixture.measure_ingress());
            assert_production_ingress_ceiling(room_size, sample);
            print_sample(
                fixture.ingress_kind.name(),
                room_size,
                room_size - 1,
                RELAYS_PER_SAMPLE,
                sample,
            );
        }
    }

    for room_size in ROOM_SIZES {
        let mut fixture = FanoutFixture::new(room_size, IngressKind::Json);
        let sample = repeated_samples(|| fixture.measure_handoff());
        assert_healthy_fanout_uses_synchronous_fast_path(room_size, sample);
        print_sample(
            "borrowed_coordinator_handoff",
            room_size,
            room_size - 1,
            RELAYS_PER_SAMPLE,
            sample,
        );
    }

    let mut fixture = QueueFixture::new();
    let sample = repeated_samples(|| fixture.measure());
    print_sample("classified_queue", 2, 1, RELAYS_PER_SAMPLE, sample);
}
