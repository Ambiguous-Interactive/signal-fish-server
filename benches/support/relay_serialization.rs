use axum::extract::ws::Message;
use sha2::{Digest, Sha256};
use signal_fish_server::coordination::allocation_benchmark::{
    channel_with_metrics, OutboundPayload, OutboundReceiver, OutboundSender,
};
use signal_fish_server::coordination::{
    ClientDeliveryHandle, ConnectionCloseSignal, MessageCoordinator,
};
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::{
    DeliveryClass, GameDataEncoding, LobbyState, PlayerId, RoomId, ServerMessage,
    SpectatorJoinedPayload,
};
use signal_fish_server::server::InMemoryMessageCoordinator;
use signal_fish_server::websocket::allocation_benchmark::materialize_game_data;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};

pub const RELAYS_PER_SAMPLE: usize = 1_024;
pub const ROOM_SIZES: [usize; 3] = [2, 8, 16];

const DATA_CAPACITY: usize = 32;
const CONTROL_CAPACITY: usize = 8;
const ROOM_ID: RoomId = RoomId::from_u128(0x222);
const SENDER_ID: PlayerId = PlayerId::from_u128(0x2220);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    V3JsonText,
    V3MessagePackBinary,
    MixedMessagePackSource,
}

impl Scenario {
    pub const ALL: [Self; 3] = [
        Self::V3JsonText,
        Self::V3MessagePackBinary,
        Self::MixedMessagePackSource,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::V3JsonText => "v3_json_text",
            Self::V3MessagePackBinary => "v3_message_pack_binary",
            Self::MixedMessagePackSource => "mixed_message_pack_source",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RecipientProfile {
    protocol_version: u16,
    format: GameDataEncoding,
}

impl RecipientProfile {
    const fn supports_v3(self) -> bool {
        self.protocol_version >= 3
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ledger {
    pub attempts: u64,
    pub enqueued: u64,
    pub dequeued: u64,
    pub materialized: u64,
    pub text_frames: u64,
    pub binary_frames: u64,
    pub wire_bytes: u64,
    pub json_encodes: u64,
    pub message_pack_encodes: u64,
    pub message_pack_decodes: u64,
    pub output_sha256: [u8; 32],
}

pub struct Fixture {
    runtime: Runtime,
    coordinator: Arc<InMemoryMessageCoordinator>,
    metrics: Arc<ServerMetrics>,
    receivers: Vec<(PlayerId, RecipientProfile, OutboundReceiver)>,
    scenario: Scenario,
    messages: Vec<Arc<ServerMessage>>,
}

impl Fixture {
    pub fn new(room_size: usize, scenario: Scenario) -> Self {
        assert!(
            room_size >= 2,
            "serialization fixture needs a sender and recipient"
        );

        let runtime = Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread serialization runtime must build");
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
                    PlayerId::from_u128(0x2220 + index as u128)
                };
                let profile = recipient_profile(scenario, index.saturating_sub(1));
                let (sender, receiver) =
                    classified_room_queue(Arc::clone(&metrics), player_id, profile);
                let handle = ClientDeliveryHandle::classified_for_allocation_benchmark(
                    sender,
                    1,
                    ConnectionCloseSignal::detached(),
                );
                coordinator
                    .register_local_client(player_id, Some(ROOM_ID), handle)
                    .await
                    .expect("serialization fixture route must register");
                receivers.push((player_id, profile, receiver));
            }
        });

        let messages = relay_messages(scenario, RELAYS_PER_SAMPLE);
        let mut fixture = Self {
            runtime,
            coordinator,
            metrics,
            receivers,
            scenario,
            messages,
        };

        let warm_message = relay_message(scenario, RELAYS_PER_SAMPLE as u64 + 1);
        let warmed = fixture.run_messages(std::slice::from_ref(&warm_message));
        fixture.assert_non_vacuous(&warmed, 1);
        fixture
    }

    pub fn run_sample(&mut self) -> Ledger {
        let messages = std::mem::take(&mut self.messages);
        let ledger = self.run_messages(&messages);
        self.messages = messages;
        self.assert_non_vacuous(&ledger, RELAYS_PER_SAMPLE);
        ledger
    }

    fn run_messages(&mut self, messages: &[Arc<ServerMessage>]) -> Ledger {
        let attempts_before = self
            .metrics
            .websocket_delivery_attempts
            .load(Ordering::Relaxed);
        let enqueued_before = self
            .metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed);
        let coordinator = Arc::clone(&self.coordinator);
        let receivers = &mut self.receivers;

        let mut ledger = self.runtime.block_on(async move {
            let mut ledger = Ledger {
                attempts: 0,
                enqueued: 0,
                dequeued: 0,
                materialized: 0,
                text_frames: 0,
                binary_frames: 0,
                wire_bytes: 0,
                json_encodes: 0,
                message_pack_encodes: 0,
                message_pack_decodes: 0,
                output_sha256: [0; 32],
            };
            let mut digest = Sha256::new();

            for message in messages {
                let relay = Arc::clone(message);
                coordinator
                    .broadcast_to_room_except_with_message(
                        &ROOM_ID,
                        &SENDER_ID,
                        Box::new(move || Some(relay)),
                    )
                    .await
                    .expect("serialization fan-out broadcast must succeed");

                for (player_id, profile, receiver) in receivers.iter_mut() {
                    if *player_id == SENDER_ID {
                        continue;
                    }
                    let queued = receiver
                        .try_recv()
                        .expect("every non-sender recipient must have one queued relay");
                    ledger.dequeued += 1;
                    let class = queued
                        .class()
                        .expect("relay serialization queue item must be classified");
                    let delivery = match queued.payload {
                        OutboundPayload::Data(delivery) => delivery,
                        OutboundPayload::Message(message) => {
                            signal_fish_server::coordination::allocation_benchmark::DeliveryMessage::new(message)
                        }
                        OutboundPayload::DeliveryReport(_) => {
                            panic!("relay serialization fixture received a delivery report")
                        }
                    };
                    let projected =
                        materialize_game_data(&delivery, profile.supports_v3(), profile.format)
                            .expect("production game-data projection must succeed");
                    ledger.materialized += 1;
                    ledger.json_encodes += projected.json_encodes;
                    ledger.message_pack_encodes += projected.message_pack_encodes;
                    ledger.message_pack_decodes += projected.message_pack_decodes;
                    match projected.frame {
                        Message::Text(text) => {
                            ledger.text_frames += 1;
                            ledger.wire_bytes += text.len() as u64;
                            digest.update([0]);
                            digest.update((text.len() as u64).to_le_bytes());
                            digest.update(text.as_bytes());
                        }
                        Message::Binary(bytes) => {
                            ledger.binary_frames += 1;
                            ledger.wire_bytes += bytes.len() as u64;
                            digest.update([1]);
                            digest.update((bytes.len() as u64).to_le_bytes());
                            digest.update(&bytes);
                        }
                        other => panic!("game-data projector emitted non-data frame: {other:?}"),
                    }
                    receiver.record_written(class);
                }
            }

            for (_, _, receiver) in receivers.iter() {
                assert!(
                    receiver.is_empty(),
                    "serialization fixture left queued frames behind"
                );
            }
            ledger.output_sha256 = digest.finalize().into();
            ledger
        });
        ledger.attempts = self
            .metrics
            .websocket_delivery_attempts
            .load(Ordering::Relaxed)
            - attempts_before;
        ledger.enqueued = self
            .metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed)
            - enqueued_before;
        ledger
    }

    fn assert_non_vacuous(&self, ledger: &Ledger, relays: usize) {
        let recipients = self.receivers.len() - 1;
        let expected = (relays * recipients) as u64;
        assert_eq!(ledger.attempts, expected, "delivery attempts disagree");
        assert_eq!(ledger.enqueued, expected, "successful enqueues disagree");
        assert_eq!(ledger.dequeued, expected, "dequeued frames disagree");
        assert_eq!(
            ledger.materialized, expected,
            "materialized frames disagree"
        );
        assert_eq!(
            ledger.text_frames + ledger.binary_frames,
            expected,
            "wire frame cohorts disagree"
        );
        assert!(ledger.wire_bytes > 0, "wire ledger recorded no bytes");

        match self.scenario {
            Scenario::V3JsonText => {
                assert_eq!(ledger.text_frames, expected);
                assert_eq!(ledger.binary_frames, 0);
                assert_eq!(ledger.json_encodes, relays as u64);
                assert_eq!(ledger.message_pack_encodes, 0);
                assert_eq!(ledger.message_pack_decodes, 0);
            }
            Scenario::V3MessagePackBinary => {
                assert_eq!(ledger.text_frames, 0);
                assert_eq!(ledger.binary_frames, expected);
                assert_eq!(ledger.json_encodes, 0);
                assert_eq!(ledger.message_pack_encodes, relays as u64);
                assert_eq!(ledger.message_pack_decodes, 0);
            }
            Scenario::MixedMessagePackSource => {
                let json_recipients = recipients.div_ceil(2) as u64;
                let binary_recipients = recipients as u64 - json_recipients;
                assert_eq!(ledger.text_frames, relays as u64 * json_recipients);
                assert_eq!(ledger.binary_frames, relays as u64 * binary_recipients);
                let profiles =
                    || (0..recipients).map(|index| recipient_profile(self.scenario, index));
                let json_cohorts = [
                    profiles().any(|profile| {
                        !profile.supports_v3() && profile.format == GameDataEncoding::Json
                    }),
                    profiles().any(|profile| {
                        profile.supports_v3() && profile.format == GameDataEncoding::Json
                    }),
                ]
                .into_iter()
                .filter(|present| *present)
                .count() as u64;
                let binary_cohorts = [
                    profiles().any(|profile| {
                        !profile.supports_v3() && profile.format == GameDataEncoding::MessagePack
                    }),
                    profiles().any(|profile| {
                        profile.supports_v3() && profile.format == GameDataEncoding::MessagePack
                    }),
                ]
                .into_iter()
                .filter(|present| *present)
                .count() as u64;
                assert_eq!(ledger.json_encodes, relays as u64 * json_cohorts);
                assert_eq!(ledger.message_pack_decodes, relays as u64);
                assert_eq!(ledger.message_pack_encodes, relays as u64 * binary_cohorts);
            }
        }
    }
}

fn recipient_profile(scenario: Scenario, recipient_index: usize) -> RecipientProfile {
    match scenario {
        Scenario::V3JsonText => RecipientProfile {
            protocol_version: 3,
            format: GameDataEncoding::Json,
        },
        Scenario::V3MessagePackBinary => RecipientProfile {
            protocol_version: 3,
            format: GameDataEncoding::MessagePack,
        },
        Scenario::MixedMessagePackSource => {
            const PROFILES: [RecipientProfile; 4] = [
                RecipientProfile {
                    protocol_version: 2,
                    format: GameDataEncoding::Json,
                },
                RecipientProfile {
                    protocol_version: 3,
                    format: GameDataEncoding::Json,
                },
                RecipientProfile {
                    protocol_version: 2,
                    format: GameDataEncoding::MessagePack,
                },
                RecipientProfile {
                    protocol_version: 3,
                    format: GameDataEncoding::MessagePack,
                },
            ];
            PROFILES[recipient_index % PROFILES.len()]
        }
    }
}

fn classified_room_queue(
    metrics: Arc<ServerMetrics>,
    player_id: PlayerId,
    profile: RecipientProfile,
) -> (OutboundSender, OutboundReceiver) {
    let (sender, mut receiver) = channel_with_metrics(DATA_CAPACITY, CONTROL_CAPACITY, metrics);
    sender.set_protocol_version(profile.protocol_version);
    sender.set_game_data_format(profile.format);
    let transition = Arc::new(ServerMessage::SpectatorJoined(Box::new(
        SpectatorJoinedPayload {
            room_id: ROOM_ID,
            room_code: "SER222".to_string(),
            spectator_id: player_id,
            game_name: "relay-serialization-benchmark".to_string(),
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

fn relay_messages(scenario: Scenario, count: usize) -> Vec<Arc<ServerMessage>> {
    (1..=count)
        .map(|seq| relay_message(scenario, seq as u64))
        .collect()
}

fn relay_message(scenario: Scenario, seq: u64) -> Arc<ServerMessage> {
    let data = serde_json::json!({
        "entity": 42,
        "position": [123.25, -87.5, 9.75],
        "rotation": [0.0, 0.707, 0.0, 0.707],
        "state": "x".repeat(896),
        "tick": seq,
    });
    match scenario {
        Scenario::V3JsonText => Arc::new(ServerMessage::GameData {
            from_player: SENDER_ID,
            data,
            seq: Some(seq),
            epoch: Some(1),
            class: Some(DeliveryClass::Reliable),
            key: None,
        }),
        Scenario::V3MessagePackBinary | Scenario::MixedMessagePackSource => {
            let payload = rmp_serde::to_vec_named(&data)
                .expect("representative JSON payload must encode as MessagePack");
            Arc::new(ServerMessage::GameDataBinary {
                from_player: SENDER_ID,
                encoding: GameDataEncoding::MessagePack,
                payload: payload.into(),
                seq: Some(seq),
                epoch: Some(1),
            })
        }
    }
}
