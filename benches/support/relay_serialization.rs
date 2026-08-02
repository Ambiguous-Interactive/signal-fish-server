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
    V2JsonBinary,
    V2RkyvBinary,
    V3JsonText,
    V3MessagePackBinary,
    MixedMessagePackSource,
}

impl Scenario {
    pub const ALL: [Self; 5] = [
        Self::V2JsonBinary,
        Self::V2RkyvBinary,
        Self::V3JsonText,
        Self::V3MessagePackBinary,
        Self::MixedMessagePackSource,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::V2JsonBinary => "v2_json_binary",
            Self::V2RkyvBinary => "v2_rkyv_binary",
            Self::V3JsonText => "v3_json_text",
            Self::V3MessagePackBinary => "v3_message_pack_binary",
            Self::MixedMessagePackSource => "mixed_message_pack_source",
        }
    }
}

pub fn assert_expected_output_digest(scenario: Scenario, room_size: usize, ledger: &Ledger) {
    let expected = match (scenario, room_size) {
        (Scenario::V2JsonBinary | Scenario::V2RkyvBinary, 2) => {
            "6ed4ed0eb9160a5355d0715e16f015fc221dfe9051f854a87c62bb2c0fa95c6f"
        }
        (Scenario::V2JsonBinary | Scenario::V2RkyvBinary, 8) => {
            "93019166377ed5d8626c6686eed0d2a6d28217d242693ff410271ca6dc4261fa"
        }
        (Scenario::V2JsonBinary | Scenario::V2RkyvBinary, 16) => {
            "21d58310cb589e36b24318278b7cd268788c593f825347bccd7dfff84cf53cdc"
        }
        (Scenario::V3JsonText, 2) => {
            "62d00507ecc1a67fc1ed211ced5a98baab6e5f4201b17530c96e59a1ac2404ea"
        }
        (Scenario::V3JsonText, 8) => {
            "7b9cfb3ca7b90be7977093b3ef4d88db29987b07e776415e18cb6fce8a9b1f28"
        }
        (Scenario::V3JsonText, 16) => {
            "0b5c7c01bdc82181ba8396b82ce7b33f19c079ef4946aa7b75198c9977df52d9"
        }
        (Scenario::V3MessagePackBinary, 2) => {
            "bc4bbff551e662c7d5392af2ffdd3bb5bbff93c5d5d3e59b2ae37bf2c39da65f"
        }
        (Scenario::V3MessagePackBinary, 8) => {
            "985df9bddb2264627da2a9fed3dafcdc62f8c5b620e889906b92a13ebad9364d"
        }
        (Scenario::V3MessagePackBinary, 16) => {
            "713b6cc49acf584f3ba10188262aca9e49dc075c9680cf4eb07cc7ec5292487a"
        }
        (Scenario::MixedMessagePackSource, 2) => {
            "8ee28a2f5fa4828a9f56cb88ca5c672b1ac82739c298875affc68265da280bf5"
        }
        (Scenario::MixedMessagePackSource, 8) => {
            "27eb27a2ca4a7975f67e50a2ad7c9892510c01fcf656322fb79ca98e36e30648"
        }
        (Scenario::MixedMessagePackSource, 16) => {
            "1c461f688ee193d43e61695e07536caad92f8e46254787641ac59dc7db012658"
        }
        _ => panic!("room-{room_size} has no checked-in wire digest"),
    };
    let actual: String = ledger
        .output_sha256
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        actual,
        expected,
        "{} room-{room_size} exact relay wire digest changed",
        scenario.name()
    );
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
        let warmed = fixture.run_messages(std::slice::from_ref(&warm_message), true);
        fixture.assert_non_vacuous(&warmed, 1);
        fixture
    }

    pub fn run_sample(&mut self) -> Ledger {
        let messages = std::mem::take(&mut self.messages);
        let ledger = self.run_messages(&messages, true);
        self.messages = messages;
        self.assert_non_vacuous(&ledger, RELAYS_PER_SAMPLE);
        ledger
    }

    /// Run the production seam without hashing every emitted byte.
    ///
    /// The allocation harness uses [`Self::run_sample`] so exact wire output
    /// remains validated. Criterion uses this variant because production does
    /// not SHA-256 relay frames and timing that work obscures the code under
    /// measurement, especially in larger rooms.
    #[allow(dead_code)] // Shared support is compiled separately for the allocation benchmark.
    pub fn run_timed_sample(&mut self) -> Ledger {
        let messages = std::mem::take(&mut self.messages);
        let ledger = self.run_messages(&messages, false);
        self.messages = messages;
        self.assert_non_vacuous(&ledger, RELAYS_PER_SAMPLE);
        debug_assert_eq!(ledger.output_sha256, [0; 32]);
        ledger
    }

    fn run_messages(&mut self, messages: &[Arc<ServerMessage>], hash_output: bool) -> Ledger {
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
                            if hash_output {
                                digest.update([0]);
                                digest.update((text.len() as u64).to_le_bytes());
                                digest.update(text.as_bytes());
                            } else {
                                std::hint::black_box(&text);
                            }
                        }
                        Message::Binary(bytes) => {
                            ledger.binary_frames += 1;
                            ledger.wire_bytes += bytes.len() as u64;
                            if hash_output {
                                digest.update([1]);
                                digest.update((bytes.len() as u64).to_le_bytes());
                                digest.update(&bytes);
                            } else {
                                std::hint::black_box(&bytes);
                            }
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
            if hash_output {
                ledger.output_sha256 = digest.finalize().into();
            }
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
            Scenario::V2JsonBinary | Scenario::V2RkyvBinary => {
                assert_eq!(ledger.text_frames, 0);
                assert_eq!(ledger.binary_frames, expected);
                assert_eq!(ledger.json_encodes, 0);
                assert_eq!(ledger.message_pack_encodes, 0);
                assert_eq!(ledger.message_pack_decodes, 0);
            }
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
        Scenario::V2JsonBinary => RecipientProfile {
            protocol_version: 2,
            format: GameDataEncoding::Json,
        },
        Scenario::V2RkyvBinary => RecipientProfile {
            protocol_version: 2,
            format: GameDataEncoding::Rkyv,
        },
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
        Scenario::V2JsonBinary | Scenario::V2RkyvBinary => {
            let encoding = match scenario {
                Scenario::V2JsonBinary => GameDataEncoding::Json,
                Scenario::V2RkyvBinary => GameDataEncoding::Rkyv,
                _ => unreachable!("matched v2 raw binary scenario"),
            };
            let payload =
                serde_json::to_vec(&data).expect("representative payload must encode as raw bytes");
            Arc::new(ServerMessage::GameDataBinary {
                from_player: SENDER_ID,
                encoding,
                payload: payload.into(),
                seq: Some(seq),
                epoch: Some(1),
            })
        }
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
