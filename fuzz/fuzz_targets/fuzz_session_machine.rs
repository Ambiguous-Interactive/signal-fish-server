#![no_main]
//! Coverage-guided state-machine fuzzing of the in-process server core.
//!
//! The libFuzzer counterpart to the stable model-based suite
//! (`tests/model_based_state_machines.rs`): `arbitrary`-derived op sequences
//! drive up to four synthetic clients against a real `EnhancedGameServer`
//! (in-memory database, no sockets — the same `connect_client` + bounded mpsc
//! wiring the in-process load tests use). Ops cover the whole room lifecycle:
//! join/create, ready toggle, start game, game-data relay, reconnect-token
//! claims (arbitrary tokens — the tokens never leave the server, so this
//! surface is the *rejection* paths; the accepting claim flow is fuzzed
//! in-process by `fuzz_reconnect_tokens`), leave, receiver drops, and
//! unregistration.
//!
//! Invariants, checked after EVERY op (any violation panics = a finding):
//! - no panic anywhere in the handler paths (implicit);
//! - the #131 delivery conservation law over `ServerMetrics`: every attempt
//!   is exactly one of enqueued / channel-closed / dropped (all deliveries
//!   are awaited inline by the handlers, so the ledger is settled at every
//!   op boundary);
//! - routing consistency: no player is ever a member of two rooms at once
//!   (checked over every room this input has ever created).
//!
//! Each input runs on a FRESH current-thread tokio runtime: the server spawns
//! background loops (rate-limit cleanup, dashboard cache), and dropping the
//! runtime after every input reaps them — a process-wide static runtime would
//! accumulate those tasks across libFuzzer iterations without bound.
//!
//! Run via the nightly `fuzz` CI job, never on stable.
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use signal_fish_server::config::{
    AuthMaintenanceConfig, CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig,
    SessionConfig, TransportSecurityConfig, TurnConfig,
};
use signal_fish_server::database::DatabaseConfig;
use signal_fish_server::protocol::{ClientMessage, RoomId, ServerMessage};
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

/// Synthetic client pool size (ops address clients by index modulo this).
const CLIENTS: usize = 4;
/// Per-client outbound queue capacity — small enough that undrained clients
/// exercise the backpressure/slow-consumer paths within an input.
const QUEUE_CAPACITY: usize = 4;
/// Op budget per input: enough for a full join/ready/start/relay/leave cycle
/// across every client while keeping each libFuzzer iteration fast.
const MAX_OPS: usize = 24;
/// Messages drained per Drain op (bounds work per op).
const DRAIN_LIMIT: usize = 32;

#[derive(Debug, Arbitrary)]
enum Op {
    /// Join or create a room; `use_known_code` targets a room code this input
    /// already observed (so clients actually meet), else creates a new room.
    Join {
        client: u8,
        game: u8,
        use_known_code: bool,
        max_players: u8,
    },
    Leave {
        client: u8,
    },
    Ready {
        client: u8,
    },
    StartGame {
        client: u8,
    },
    GameData {
        client: u8,
        value: u8,
    },
    /// Reconnect-token claim with an arbitrary (never-valid) token against an
    /// arbitrary target/room — the rejection surface of the claim flow.
    Reconnect {
        client: u8,
        target: u8,
        token: String,
        use_known_room: bool,
    },
    Ping {
        client: u8,
    },
    /// Drain the client's outbound queue (learning room codes/ids from
    /// observed RoomJoined frames along the way).
    Drain {
        client: u8,
    },
    /// Drop the client's queue receiver without unregistering: subsequent
    /// deliveries must resolve as ChannelClosed, never as silent loss.
    DropReceiver {
        client: u8,
    },
    Unregister {
        client: u8,
    },
}

#[derive(Debug, Arbitrary)]
struct Plan {
    ops: Vec<Op>,
}

struct ClientSlot {
    player_id: Uuid,
    rx: Option<mpsc::Receiver<Arc<ServerMessage>>>,
    registered: bool,
}

struct Harness {
    server: Arc<EnhancedGameServer>,
    clients: Vec<ClientSlot>,
    /// game name -> a room code observed for it (lets later joins share rooms).
    known_codes: HashMap<String, String>,
    /// Every room id this input has ever observed (for the two-rooms check).
    known_rooms: Vec<RoomId>,
}

fn game_name(game: u8) -> String {
    // Two games keep the cross-game room-cap paths reachable without
    // exploding the state.
    format!("fuzz-game-{}", game % 2)
}

async fn build_server() -> Arc<EnhancedGameServer> {
    let server_config = ServerConfig {
        default_max_players: 4,
        max_connections_per_ip: usize::MAX,
        // A 1ms slow-consumer grace keeps full-queue backpressure paths fast
        // enough to fuzz while still exercising the loud-disconnect contract.
        websocket_config: signal_fish_server::config::WebSocketConfig {
            slow_consumer_timeout_ms: 1,
            send_queue_capacity: QUEUE_CAPACITY,
            ..Default::default()
        },
        // A 3-slot replay ring makes eviction/truncation reachable in-input.
        event_buffer_size: 3,
        enable_reconnection: true,
        heartbeat_throttle: tokio::time::Duration::ZERO,
        region_id: "fuzz".to_string(),
        ..ServerConfig::default()
    };
    EnhancedGameServer::new(
        server_config,
        ProtocolConfig::default(),
        RelayTypeConfig::default(),
        SessionConfig::default(),
        TurnConfig::default(),
        DatabaseConfig::InMemory,
        MetricsConfig::default(),
        AuthMaintenanceConfig::default(),
        CoordinationConfig::default(),
        TransportSecurityConfig::default(),
        Vec::new(),
    )
    .await
    .expect("in-memory server construction must not fail")
}

impl Harness {
    /// Ensure the client slot is registered (fresh bounded channel); an op on
    /// an unregistered client re-registers it first, exercising re-entry.
    async fn ensure_registered(&mut self, client: usize) -> Uuid {
        let slot = &mut self.clients[client];
        if !slot.registered {
            let (tx, rx) = mpsc::channel::<Arc<ServerMessage>>(QUEUE_CAPACITY);
            self.server.connect_client(slot.player_id, tx).await;
            slot.rx = Some(rx);
            slot.registered = true;
        }
        slot.player_id
    }

    /// Drain up to DRAIN_LIMIT queued messages, learning room codes and ids.
    fn drain(&mut self, client: usize) {
        let mut learned = Vec::new();
        if let Some(rx) = self.clients[client].rx.as_mut() {
            for _ in 0..DRAIN_LIMIT {
                match rx.try_recv() {
                    Ok(message) => {
                        if let ServerMessage::RoomJoined(payload) = message.as_ref() {
                            learned.push((
                                payload.game_name.clone(),
                                payload.room_code.clone(),
                                payload.room_id,
                            ));
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty)
                    | Err(mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
        }
        for (game, code, room_id) in learned {
            self.known_codes.insert(game, code);
            if !self.known_rooms.contains(&room_id) {
                self.known_rooms.push(room_id);
            }
        }
    }

    /// The #131 conservation law: every delivery attempt resolved as exactly
    /// one of enqueued / channel-closed / dropped. All handler deliveries are
    /// awaited inline, so the ledger is settled between ops.
    fn assert_conservation(&self) {
        let metrics = self.server.metrics();
        let attempts = metrics.websocket_delivery_attempts.load(Ordering::Relaxed);
        let enqueued = metrics
            .websocket_deliveries_enqueued
            .load(Ordering::Relaxed);
        let channel_closed = metrics
            .websocket_deliveries_channel_closed
            .load(Ordering::Relaxed);
        let dropped = metrics.websocket_messages_dropped.load(Ordering::Relaxed);
        assert_eq!(
            attempts,
            enqueued + channel_closed + dropped,
            "delivery conservation violated: attempts={attempts} != \
             enqueued={enqueued} + channel_closed={channel_closed} + dropped={dropped}"
        );
    }

    /// No player may be a member of two rooms at once (over every room this
    /// input ever observed).
    async fn assert_single_room_membership(&self) {
        let mut seen: HashMap<Uuid, RoomId> = HashMap::new();
        for room_id in &self.known_rooms {
            let Ok(Some(room)) = self.server.database().get_room_by_id(room_id).await else {
                continue; // deleted rooms are fine
            };
            for player_id in room.players.keys() {
                if let Some(previous) = seen.insert(*player_id, *room_id) {
                    assert_eq!(
                        previous, *room_id,
                        "player {player_id} is a member of two rooms at once: \
                         {previous} and {room_id}"
                    );
                }
            }
        }
    }

    async fn apply(&mut self, op: Op) {
        match op {
            Op::Join {
                client,
                game,
                use_known_code,
                max_players,
            } => {
                let client = client as usize % CLIENTS;
                let player_id = self.ensure_registered(client).await;
                let game = game_name(game);
                let room_code = if use_known_code {
                    self.known_codes.get(&game).cloned()
                } else {
                    None
                };
                self.server
                    .handle_client_message(
                        &player_id,
                        ClientMessage::JoinRoom {
                            game_name: game,
                            room_code,
                            player_name: format!("fuzzer-{client}"),
                            max_players: Some(2 + max_players % 3),
                            supports_authority: Some(client == 0),
                            relay_transport: None,
                        },
                    )
                    .await;
                // Immediately learn the room code/id so later ops can meet.
                self.drain(client);
            }
            Op::Leave { client } => {
                let client = client as usize % CLIENTS;
                let player_id = self.ensure_registered(client).await;
                self.server
                    .handle_client_message(&player_id, ClientMessage::LeaveRoom)
                    .await;
            }
            Op::Ready { client } => {
                let client = client as usize % CLIENTS;
                let player_id = self.ensure_registered(client).await;
                self.server
                    .handle_client_message(&player_id, ClientMessage::PlayerReady)
                    .await;
            }
            Op::StartGame { client } => {
                let client = client as usize % CLIENTS;
                let player_id = self.ensure_registered(client).await;
                self.server
                    .handle_client_message(&player_id, ClientMessage::StartGame)
                    .await;
            }
            Op::GameData { client, value } => {
                let client = client as usize % CLIENTS;
                let player_id = self.ensure_registered(client).await;
                self.server
                    .handle_client_message(
                        &player_id,
                        ClientMessage::GameData {
                            data: serde_json::json!({ "v": value }),
                        },
                    )
                    .await;
            }
            Op::Reconnect {
                client,
                target,
                token,
                use_known_room,
            } => {
                let client = client as usize % CLIENTS;
                let target = target as usize % CLIENTS;
                let player_id = self.ensure_registered(client).await;
                let target_id = self.clients[target].player_id;
                let room_id = if use_known_room {
                    self.known_rooms.first().copied().unwrap_or_else(Uuid::nil)
                } else {
                    Uuid::nil()
                };
                self.server
                    .handle_client_message(
                        &player_id,
                        ClientMessage::Reconnect {
                            player_id: target_id,
                            room_id,
                            auth_token: token,
                        },
                    )
                    .await;
            }
            Op::Ping { client } => {
                let client = client as usize % CLIENTS;
                let player_id = self.ensure_registered(client).await;
                self.server
                    .handle_client_message(&player_id, ClientMessage::Ping)
                    .await;
            }
            Op::Drain { client } => {
                let client = client as usize % CLIENTS;
                self.drain(client);
            }
            Op::DropReceiver { client } => {
                let client = client as usize % CLIENTS;
                // Deliveries to this client must now resolve ChannelClosed —
                // conservation still has to hold.
                self.clients[client].rx = None;
            }
            Op::Unregister { client } => {
                let client = client as usize % CLIENTS;
                if self.clients[client].registered {
                    let player_id = self.clients[client].player_id;
                    self.server.unregister_client(&player_id).await;
                    self.clients[client].registered = false;
                    self.clients[client].rx = None;
                }
            }
        }
    }
}

async fn run(plan: Plan) {
    let server = build_server().await;
    let mut harness = Harness {
        server,
        clients: (0..CLIENTS)
            .map(|index| ClientSlot {
                player_id: Uuid::from_u128(0xF022_0000 + index as u128 + 1),
                rx: None,
                registered: false,
            })
            .collect(),
        known_codes: HashMap::new(),
        known_rooms: Vec::new(),
    };

    for op in plan.ops.into_iter().take(MAX_OPS) {
        harness.apply(op).await;
        harness.assert_conservation();
        harness.assert_single_room_membership().await;
    }
}

fuzz_target!(|plan: Plan| {
    // Fresh runtime per input: dropping it reaps the server's background
    // tasks (rate-limit cleanup, dashboard cache) so iterations never leak.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime construction must not fail");
    runtime.block_on(run(plan));
});
