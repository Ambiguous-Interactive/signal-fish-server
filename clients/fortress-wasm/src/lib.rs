#[path = "../../fortress/src/relay.rs"]
mod relay;
#[path = "../../fortress/src/workload.rs"]
mod workload;

use std::collections::BTreeSet;

use fortress_rollback::{FortressEvent, P2PSession, SessionBuilder, SessionState};
use godot::prelude::*;
use relay::{InboundRelayFrame, RelaySocket};
use serde::{Deserialize, Serialize};
use signal_fish_client::protocol::GameDataEncoding;
use signal_fish_client::{
    JoinRoomParams, SignalFishConfig, SignalFishError, SignalFishEvent, SignalFishPollingClient,
};
use signal_fish_client_godot::GodotWebSocketTransport;
use uuid::Uuid;
use web_time::{Duration, Instant};
use workload::{
    apply_requests, input_for_frame, GameConfig, GameState, MAX_CONFIRMATION_LAG,
    MAX_OLDEST_QUEUE_AGE_US, MAX_PIPELINE_QUEUE_DEPTH, MAX_ROLLBACK_DEPTH, MIN_CHECKSUM_SAMPLES,
    MIN_COMPLETED_MESSAGES_PER_SECOND, NOMINAL_FPS, TARGET_CONFIRMED_FRAMES,
};

const REPORT_SCHEMA_VERSION: u32 = 3;
const MAX_STALL_RATE_PER_MILLE: u64 = 20;
const MIN_ACTIVE_CALLBACKS: u64 = 600;
const NEGATIVE_ACTIVE_CALLBACK_BUDGET: u64 = MIN_ACTIVE_CALLBACKS;
const RUNTIME_DEADLINE: Duration = Duration::from_secs(90);
const SIGNAL_FISH_CLIENT_VERSION: &str = "0.9.0";
const SIGNAL_FISH_CLIENT_GODOT_VERSION: &str = "0.9.0";
const FORTRESS_ROLLBACK_VERSION: &str = "0.12.0";
const GODOT_RUST_VERSION: &str = "0.4.5";
const WASM_TARGET: &str = "wasm32-unknown-emscripten";

type Client = SignalFishPollingClient<GodotWebSocketTransport>;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RunMode {
    Healthy,
    NegativeOneAdmissionPerCallback,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GodotRuntimeIdentity {
    major: u32,
    minor: u32,
    patch: u32,
    status: String,
    build: String,
    hash: String,
    string: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserConfig {
    schema_version: u32,
    server_url: String,
    role: String,
    room_code: Option<String>,
    instance_nonce: Uuid,
    expected_remote_nonce: Uuid,
    run_mode: RunMode,
    build_sha: String,
    browser_process_id: u32,
    browser_artifact: String,
    godot_runtime: GodotRuntimeIdentity,
}

#[derive(Debug, Serialize)]
struct RoomReady<'a> {
    schema_version: u32,
    role: &'a str,
    instance_nonce: Uuid,
    room_code: &'a str,
}

struct PendingInbound {
    from_player: Uuid,
    encoding: GameDataEncoding,
    payload: Vec<u8>,
    seq: Option<u64>,
    epoch: Option<u32>,
}

#[derive(Debug, Default, Serialize)]
struct CallbackIntervals {
    samples: u64,
    min_us: u64,
    max_us: u64,
    mean_us: u64,
    p95_us: u64,
    p99_us: u64,
}

#[derive(Debug, Serialize)]
struct AcceptanceThresholds {
    target_confirmed_frames: i32,
    nominal_fps: usize,
    min_checksum_samples: u64,
    min_completed_messages_per_second: f64,
    max_pipeline_queue_depth: usize,
    max_oldest_queue_age_us: u64,
    max_confirmation_lag: u64,
    max_rollback_depth: u32,
    max_stall_rate_permille: u64,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    status: &'static str,
    origin: &'static str,
    runtime_error: Option<String>,
    role: String,
    run_mode: RunMode,
    instance_nonce: Uuid,
    expected_remote_nonce: Uuid,
    player_id: Option<Uuid>,
    remote_player_id: Option<Uuid>,
    room_code: Option<String>,
    browser_process_id: u32,
    browser_artifact: String,
    build_sha: String,
    signal_fish_client_version: &'static str,
    signal_fish_client_godot_version: &'static str,
    fortress_rollback_version: &'static str,
    godot_rust_version: &'static str,
    godot_runtime: GodotRuntimeIdentity,
    target: &'static str,
    target_os: &'static str,
    godot_threads: bool,
    worker_count: u32,
    callback_count: u64,
    poll_count: u64,
    active_callback_count: u64,
    callback_intervals: CallbackIntervals,
    acceptance_thresholds: AcceptanceThresholds,
    max_admissions_per_callback: u64,
    current_frame: i32,
    confirmed_frame: i32,
    game_frame: i32,
    game_checksum: u64,
    frames_advanced: u64,
    rollback_count: u64,
    max_rollback_depth: u32,
    stall_count: u64,
    wait_recommendations: u64,
    confirmation_lag_current: u64,
    confirmation_lag_max: u64,
    checksums_mismatched: u64,
    checksums_compared: u64,
    checksums_matched: u64,
    events_discarded_total: u64,
    client_game_data_sent: u64,
    client_game_data_sent_during_run: u64,
    client_game_data_received: u64,
    client_messages_undecodable: u64,
    final_pipeline_queue_depth: usize,
    peak_pipeline_queue_depth: usize,
    peak_oldest_queue_age_us: u64,
    relay_frames_enqueued: u64,
    relay_frames_enqueued_during_run: u64,
    relay_frames_received: u64,
    relay_malformed: u64,
    relay_wrong_destination: u64,
    relay_unknown_sender: u64,
    relay_outbound_overflow: u64,
    relay_inbound_overflow: u64,
    relay_encode_failures: u64,
    relay_completion_underflow: u64,
    relay_send_retries: u64,
    running_elapsed_ms: u64,
    relay_sent_sequence_count: u64,
    relay_sent_first_sequence: u64,
    relay_sent_last_sequence: u64,
    relay_sent_sequence_hash: String,
    relay_received_sequence_count: u64,
    relay_received_first_sequence: u64,
    relay_received_last_sequence: u64,
    relay_received_sequence_hash: String,
}

struct Runtime {
    config: BrowserConfig,
    client: Client,
    relay: RelaySocket,
    local: Option<Uuid>,
    roster: BTreeSet<Uuid>,
    session: Option<P2PSession<GameConfig>>,
    state: GameState,
    room_code: Option<String>,
    room_json_pending: Option<String>,
    configured_at: Instant,
    running_since: Option<Instant>,
    running_finished_at: Option<Instant>,
    last_active_callback_at: Option<Instant>,
    callback_intervals_us: Vec<u64>,
    callback_count: u64,
    poll_count: u64,
    active_callback_count: u64,
    recommended_skips: u32,
    relay_retries: u64,
    max_admissions_per_callback: u64,
    running_client_sent_baseline: u64,
    running_relay_enqueued_baseline: u64,
    local_target_reached: bool,
    workload_finished: bool,
    pending_inbound: Vec<PendingInbound>,
}

impl Runtime {
    fn new(config: BrowserConfig) -> Result<Self, String> {
        if config.schema_version != REPORT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported config schema {}, expected {REPORT_SCHEMA_VERSION}",
                config.schema_version
            ));
        }
        if !matches!(config.role.as_str(), "creator" | "joiner") {
            return Err(format!("invalid role {}", config.role));
        }
        if config.role == "creator" && config.room_code.is_some() {
            return Err("creator must not receive a room code".to_owned());
        }
        if config.role == "joiner" && config.room_code.as_deref().is_none_or(str::is_empty) {
            return Err("joiner requires the creator room code".to_owned());
        }
        if config.instance_nonce == config.expected_remote_nonce {
            return Err("local and remote instance nonces must differ".to_owned());
        }
        if config.build_sha.len() != 40
            || !config
                .build_sha
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("build_sha must be a full 40-character Git object id".to_owned());
        }
        if config.godot_runtime.major != 4
            || config.godot_runtime.minor != 5
            || config.godot_runtime.patch != 0
            || config.godot_runtime.status != "stable"
            || config.godot_runtime.build != "official"
            || config.godot_runtime.hash.is_empty()
            || config.godot_runtime.string != "4.5-stable (official)"
        {
            return Err(format!(
                "unsupported Godot runtime identity {} (expected 4.5.0 stable)",
                config.godot_runtime.string
            ));
        }

        let transport = GodotWebSocketTransport::connect(&config.server_url)
            .map_err(|error| format!("connect Godot WebSocket transport: {error}"))?;
        let mut client_config = SignalFishConfig::new("fortress-issue-242-wasm").enable_v3();
        client_config.game_data_format = Some(GameDataEncoding::MessagePack);
        client_config.command_channel_capacity = 64;

        Ok(Self {
            config,
            client: SignalFishPollingClient::new(transport, client_config),
            relay: RelaySocket::default(),
            local: None,
            roster: BTreeSet::new(),
            session: None,
            state: GameState::default(),
            room_code: None,
            room_json_pending: None,
            configured_at: Instant::now(),
            running_since: None,
            running_finished_at: None,
            last_active_callback_at: None,
            callback_intervals_us: Vec::new(),
            callback_count: 0,
            poll_count: 0,
            active_callback_count: 0,
            recommended_skips: 0,
            relay_retries: 0,
            max_admissions_per_callback: 0,
            running_client_sent_baseline: 0,
            running_relay_enqueued_baseline: 0,
            local_target_reached: false,
            workload_finished: false,
            pending_inbound: Vec::new(),
        })
    }

    fn tick(&mut self) -> Result<bool, String> {
        if self.configured_at.elapsed() > RUNTIME_DEADLINE {
            return Err("90-second Rust runtime deadline expired".to_owned());
        }

        self.callback_count = self.callback_count.saturating_add(1);
        let events = self.client.poll();
        self.poll_count = self.poll_count.saturating_add(1);
        self.relay
            .record_client_sent(self.client.stats().game_data_sent);
        for event in events {
            self.handle_event(event)?;
        }
        self.ensure_session()?;
        self.admit_pending_inbound()?;

        self.workload_finished |= self.local_target_reached
            && (self.config.run_mode == RunMode::NegativeOneAdmissionPerCallback
                || self.relay.target_received());
        let mut target_reached = self.local_target_reached;
        if !self.workload_finished {
            if let Some(fortress) = self.session.as_mut() {
                fortress.poll_remote_clients();
                for event in fortress.events() {
                    match event {
                        FortressEvent::WaitRecommendation { skip_frames } => {
                            self.recommended_skips = skip_frames;
                        }
                        FortressEvent::DesyncDetected { frame, .. } => {
                            return Err(format!("Fortress desync at frame {frame:?}"));
                        }
                        FortressEvent::Disconnected { addr } => {
                            return Err(format!("Fortress peer disconnected: {addr}"));
                        }
                        _ => {}
                    }
                }

                if fortress.current_state() == SessionState::Running {
                    let now = Instant::now();
                    if self.running_since.is_none() {
                        self.running_since = Some(now);
                        self.last_active_callback_at = Some(now);
                        self.relay.reset_queue_peak();
                        self.running_client_sent_baseline = self.client.stats().game_data_sent;
                        self.running_relay_enqueued_baseline =
                            self.relay.counters().enqueued_outbound;
                    } else if let Some(previous) = self.last_active_callback_at.replace(now) {
                        self.callback_intervals_us
                            .push(duration_us(now.saturating_duration_since(previous)));
                    }

                    let confirmed_target_reached =
                        fortress.confirmed_frame().as_i32() >= TARGET_CONFIRMED_FRAMES;
                    target_reached =
                        self.config.run_mode == RunMode::Healthy && confirmed_target_reached;
                    if !target_reached {
                        self.active_callback_count = self.active_callback_count.saturating_add(1);
                        if self.recommended_skips > 0 {
                            self.recommended_skips = self.recommended_skips.saturating_sub(1);
                        } else {
                            let current = fortress.current_frame().as_i32();
                            for handle in fortress.local_player_handles() {
                                fortress
                                    .add_local_input(
                                        handle,
                                        input_for_frame(current, handle.as_usize()),
                                    )
                                    .map_err(|error| format!("add Fortress input: {error}"))?;
                            }
                            let requests = fortress
                                .advance_frame()
                                .map_err(|error| format!("advance Fortress: {error}"))?;
                            apply_requests(&mut self.state, requests);
                        }
                    }
                }
            }
        }

        if self.config.run_mode == RunMode::NegativeOneAdmissionPerCallback
            && self.active_callback_count >= NEGATIVE_ACTIVE_CALLBACK_BUDGET
        {
            target_reached = true;
        }
        self.local_target_reached |= target_reached;

        if self.config.run_mode == RunMode::Healthy
            && self.local_target_reached
            && !self.relay.target_enqueued()
        {
            let local = self
                .local
                .ok_or("local id disappeared before target marker")?;
            let remote = self
                .roster
                .iter()
                .copied()
                .find(|player| *player != local)
                .ok_or("remote id disappeared before target marker")?;
            self.relay
                .enqueue_target_reached(&remote)
                .map_err(|error| format!("enqueue relay target marker: {error}"))?;
        }
        self.workload_finished |= self.local_target_reached
            && (self.config.run_mode == RunMode::NegativeOneAdmissionPerCallback
                || self.relay.target_received());

        let admission_cap = match self.config.run_mode {
            RunMode::Healthy => usize::MAX,
            RunMode::NegativeOneAdmissionPerCallback => 1,
        };
        let admitted = drain_relay(
            &mut self.client,
            &self.relay,
            &mut self.relay_retries,
            admission_cap,
        )?;
        self.max_admissions_per_callback = self
            .max_admissions_per_callback
            .max(u64::try_from(admitted).unwrap_or(u64::MAX));
        self.relay.sample_queue();

        let drained = self.relay.queue_depth() == 0
            && self.relay.counters().enqueued_outbound == self.client.stats().game_data_sent;
        if self.workload_finished && drained && !self.relay.completion_enqueued() {
            let local = self.local.ok_or("local id disappeared before completion")?;
            let remote = self
                .roster
                .iter()
                .copied()
                .find(|player| *player != local)
                .ok_or("remote id disappeared before completion")?;
            self.relay
                .enqueue_completion(&remote)
                .map_err(|error| format!("enqueue relay completion: {error}"))?;
            return Ok(false);
        }
        let completion_exchange_done = self.relay.completion_enqueued()
            && self.relay.completion_received()
            && self.relay.queue_depth() == 0
            && self.relay.counters().enqueued_outbound == self.client.stats().game_data_sent;
        if self.config.role == "creator"
            && completion_exchange_done
            && !self.relay.creator_final_enqueued()
        {
            let local = self
                .local
                .ok_or("local id disappeared before creator final")?;
            let remote = self
                .roster
                .iter()
                .copied()
                .find(|player| *player != local)
                .ok_or("remote id disappeared before creator final")?;
            self.relay
                .enqueue_creator_final(&remote)
                .map_err(|error| format!("enqueue creator final marker: {error}"))?;
            return Ok(false);
        }
        if self.config.role == "joiner"
            && completion_exchange_done
            && self.relay.creator_final_received()
            && !self.relay.joiner_ack_enqueued()
        {
            let local = self.local.ok_or("local id disappeared before joiner ack")?;
            let remote = self
                .roster
                .iter()
                .copied()
                .find(|player| *player != local)
                .ok_or("remote id disappeared before joiner ack")?;
            self.relay
                .enqueue_joiner_ack(&remote)
                .map_err(|error| format!("enqueue joiner final ack: {error}"))?;
            return Ok(false);
        }
        let drained = self.relay.queue_depth() == 0
            && self.relay.counters().enqueued_outbound == self.client.stats().game_data_sent;
        let complete =
            (self.config.role == "creator" && self.relay.joiner_ack_received() && drained)
                || (self.config.role == "joiner" && self.relay.joiner_ack_enqueued() && drained);
        if complete {
            self.running_finished_at.get_or_insert_with(Instant::now);
        }
        Ok(complete)
    }

    fn handle_event(&mut self, event: SignalFishEvent) -> Result<(), String> {
        match event {
            SignalFishEvent::Authenticated { .. } => {
                let mut params =
                    JoinRoomParams::new("fortress-issue-242-wasm", self.config.role.clone())
                        .with_max_players(2);
                if let Some(code) = self.config.room_code.as_deref() {
                    params = params.with_room_code(code);
                }
                self.client
                    .join_room(params)
                    .map_err(|error| format!("join room: {error}"))?;
            }
            SignalFishEvent::RoomJoined {
                player_id,
                room_code,
                current_players,
                ..
            } => {
                self.local = Some(player_id);
                self.roster.insert(player_id);
                self.roster
                    .extend(current_players.into_iter().map(|player| player.id));
                self.room_code = Some(room_code.clone());
                if self.config.role == "creator" {
                    self.room_json_pending = Some(
                        serde_json::to_string(&RoomReady {
                            schema_version: REPORT_SCHEMA_VERSION,
                            role: &self.config.role,
                            instance_nonce: self.config.instance_nonce,
                            room_code: &room_code,
                        })
                        .map_err(|error| format!("serialize room readiness: {error}"))?,
                    );
                }
                self.client
                    .set_ready()
                    .map_err(|error| format!("set ready: {error}"))?;
            }
            SignalFishEvent::PlayerJoined { player } => {
                self.roster.insert(player.id);
            }
            SignalFishEvent::GameDataBinary {
                from_player,
                encoding,
                payload,
                seq,
                epoch,
            } => {
                self.pending_inbound.push(PendingInbound {
                    from_player,
                    encoding,
                    payload,
                    seq,
                    epoch,
                });
            }
            SignalFishEvent::PlayerLeft { player_id, .. } => {
                return Err(format!("Signal Fish peer left: {player_id}"));
            }
            SignalFishEvent::Disconnected { reason, .. } => {
                return Err(format!("Signal Fish disconnected: {reason:?}"));
            }
            SignalFishEvent::Error { message, .. }
            | SignalFishEvent::AuthenticationError { error: message, .. } => {
                return Err(format!("Signal Fish rejected peer: {message}"));
            }
            _ => {}
        }
        Ok(())
    }

    fn ensure_session(&mut self) -> Result<(), String> {
        if self.session.is_some() || self.roster.len() != 2 {
            return Ok(());
        }
        let local = self
            .local
            .ok_or("two-player roster arrived before local id")?;
        let remote = self
            .roster
            .iter()
            .copied()
            .find(|player| *player != local)
            .ok_or("two-player roster is missing remote id")?;
        self.relay
            .configure_identity(
                self.config.instance_nonce,
                self.config.expected_remote_nonce,
            )
            .map_err(|error| format!("configure relay identity: {error}"))?;
        self.session = Some(build_session(local, remote, self.relay.clone())?);
        Ok(())
    }

    fn admit_pending_inbound(&mut self) -> Result<(), String> {
        let Some(local) = self.local else {
            return Ok(());
        };
        let Some(remote) = self.roster.iter().copied().find(|player| *player != local) else {
            return Ok(());
        };
        for frame in self.pending_inbound.drain(..) {
            self.relay.admit_inbound(InboundRelayFrame {
                local,
                known_remote: remote,
                from: frame.from_player,
                encoding: frame.encoding,
                seq: frame.seq,
                epoch: frame.epoch,
                payload: &frame.payload,
            });
        }
        Ok(())
    }

    fn take_room_json(&mut self) -> Option<String> {
        self.room_json_pending.take()
    }

    fn report(&self, runtime_error: Option<String>) -> Report {
        let client_stats = self.client.stats();
        let relay_stats = self.relay.counters();
        let sent_ledger = self.relay.sent_ledger();
        let received_ledger = self.relay.received_ledger();
        let fortress_metrics = self.session.as_ref().map(P2PSession::metrics);
        let current_frame = self
            .session
            .as_ref()
            .map_or(0, |session| session.current_frame().as_i32());
        let confirmed_frame = self
            .session
            .as_ref()
            .map_or(0, |session| session.confirmed_frame().as_i32());
        let remote_player_id = self
            .local
            .and_then(|local| self.roster.iter().copied().find(|player| *player != local));
        let running_elapsed_ms = self
            .running_since
            .zip(self.running_finished_at.or(Some(Instant::now())))
            .map_or(0, |(started, finished)| {
                duration_ms(finished.saturating_duration_since(started))
            });

        Report {
            schema_version: REPORT_SCHEMA_VERSION,
            status: "complete",
            origin: "rust-gdextension",
            runtime_error,
            role: self.config.role.clone(),
            run_mode: self.config.run_mode,
            instance_nonce: self.config.instance_nonce,
            expected_remote_nonce: self.config.expected_remote_nonce,
            player_id: self.local,
            remote_player_id,
            room_code: self.room_code.clone(),
            browser_process_id: self.config.browser_process_id,
            browser_artifact: self.config.browser_artifact.clone(),
            build_sha: self.config.build_sha.clone(),
            signal_fish_client_version: SIGNAL_FISH_CLIENT_VERSION,
            signal_fish_client_godot_version: SIGNAL_FISH_CLIENT_GODOT_VERSION,
            fortress_rollback_version: FORTRESS_ROLLBACK_VERSION,
            godot_rust_version: GODOT_RUST_VERSION,
            godot_runtime: self.config.godot_runtime.clone(),
            target: WASM_TARGET,
            target_os: std::env::consts::OS,
            godot_threads: cfg!(target_feature = "atomics"),
            worker_count: 0,
            callback_count: self.callback_count,
            poll_count: self.poll_count,
            active_callback_count: self.active_callback_count,
            callback_intervals: summarize_intervals(&self.callback_intervals_us),
            acceptance_thresholds: AcceptanceThresholds {
                target_confirmed_frames: TARGET_CONFIRMED_FRAMES,
                nominal_fps: NOMINAL_FPS,
                min_checksum_samples: MIN_CHECKSUM_SAMPLES,
                min_completed_messages_per_second: MIN_COMPLETED_MESSAGES_PER_SECOND,
                max_pipeline_queue_depth: MAX_PIPELINE_QUEUE_DEPTH,
                max_oldest_queue_age_us: MAX_OLDEST_QUEUE_AGE_US,
                max_confirmation_lag: MAX_CONFIRMATION_LAG,
                max_rollback_depth: MAX_ROLLBACK_DEPTH,
                max_stall_rate_permille: MAX_STALL_RATE_PER_MILLE,
            },
            max_admissions_per_callback: self.max_admissions_per_callback,
            current_frame,
            confirmed_frame,
            game_frame: self.state.frame,
            game_checksum: self.state.checksum,
            frames_advanced: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.frames_advanced),
            rollback_count: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.rollback_count),
            max_rollback_depth: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.max_rollback_depth),
            stall_count: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.stall_count),
            wait_recommendations: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.wait_recommendations),
            confirmation_lag_current: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.confirmation_lag_current),
            confirmation_lag_max: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.confirmation_lag_max),
            checksums_mismatched: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.checksums_mismatched),
            checksums_compared: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.checksums_compared),
            checksums_matched: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.checksums_matched),
            events_discarded_total: fortress_metrics
                .as_ref()
                .map_or(0, |metrics| metrics.events_discarded_total),
            client_game_data_sent: client_stats.game_data_sent,
            client_game_data_sent_during_run: client_stats
                .game_data_sent
                .saturating_sub(self.running_client_sent_baseline),
            client_game_data_received: client_stats.game_data_received,
            client_messages_undecodable: client_stats.messages_undecodable,
            final_pipeline_queue_depth: self.relay.queue_depth(),
            peak_pipeline_queue_depth: self.relay.peak_queue_depth(),
            peak_oldest_queue_age_us: duration_us(self.relay.peak_oldest_queue_age()),
            relay_frames_enqueued: relay_stats.enqueued_outbound,
            relay_frames_enqueued_during_run: relay_stats
                .enqueued_outbound
                .saturating_sub(self.running_relay_enqueued_baseline),
            relay_frames_received: relay_stats.accepted_inbound,
            relay_malformed: relay_stats.malformed_inbound,
            relay_wrong_destination: relay_stats.wrong_destination,
            relay_unknown_sender: relay_stats.unknown_sender,
            relay_outbound_overflow: relay_stats.outbound_overflow,
            relay_inbound_overflow: relay_stats.inbound_overflow,
            relay_encode_failures: relay_stats.encode_failures,
            relay_completion_underflow: relay_stats.completion_underflow,
            relay_send_retries: self.relay_retries,
            running_elapsed_ms,
            relay_sent_sequence_count: sent_ledger.count,
            relay_sent_first_sequence: sent_ledger.first_sequence,
            relay_sent_last_sequence: sent_ledger.last_sequence,
            relay_sent_sequence_hash: format!("{:016x}", sent_ledger.sequence_hash),
            relay_received_sequence_count: received_ledger.count,
            relay_received_first_sequence: received_ledger.first_sequence,
            relay_received_last_sequence: received_ledger.last_sequence,
            relay_received_sequence_hash: format!("{:016x}", received_ledger.sequence_hash),
        }
    }
}

fn build_session(
    local: Uuid,
    remote: Uuid,
    socket: RelaySocket,
) -> Result<P2PSession<GameConfig>, String> {
    let mut ids = [local, remote];
    ids.sort_unstable();
    let local_handle = usize::from(ids[1] == local);
    let remote_handle = usize::from(ids[1] == remote);
    SessionBuilder::<GameConfig>::new()
        .with_num_players(2)
        .and_then(|builder| builder.with_fps(60))
        .and_then(|builder| builder.add_local_player(local_handle))
        .and_then(|builder| builder.add_remote_player(remote_handle, remote))
        .and_then(|builder| builder.start_p2p_session(socket))
        .map_err(|error| format!("build Fortress session: {error}"))
}

fn drain_relay(
    client: &mut Client,
    relay: &RelaySocket,
    retries: &mut u64,
    admission_cap: usize,
) -> Result<usize, String> {
    let mut admitted = 0usize;
    while admitted < admission_cap {
        let Some(frame) = relay.take_outbound() else {
            break;
        };
        match client.send_binary_game_data(frame.payload.clone()) {
            Ok(()) => {
                relay.mark_admitted(frame);
                admitted = admitted.saturating_add(1);
            }
            Err(SignalFishError::SendBufferFull { .. }) => {
                *retries = retries.saturating_add(1);
                relay.return_outbound_front(frame);
                break;
            }
            Err(error) => return Err(format!("relay send failed: {error}")),
        }
    }
    Ok(admitted)
}

fn summarize_intervals(samples: &[u64]) -> CallbackIntervals {
    if samples.is_empty() {
        return CallbackIntervals::default();
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let total = ordered
        .iter()
        .copied()
        .fold(0u128, |sum, value| sum.saturating_add(u128::from(value)));
    CallbackIntervals {
        samples: u64::try_from(ordered.len()).unwrap_or(u64::MAX),
        min_us: ordered.first().copied().unwrap_or_default(),
        max_us: ordered.last().copied().unwrap_or_default(),
        mean_us: u64::try_from(total / ordered.len() as u128).unwrap_or(u64::MAX),
        p95_us: percentile(&ordered, 95),
        p99_us: percentile(&ordered, 99),
    }
}

fn percentile(ordered: &[u64], percentile: usize) -> u64 {
    let rank = ordered.len().saturating_mul(percentile).saturating_add(99) / 100;
    ordered
        .get(rank.saturating_sub(1))
        .copied()
        .unwrap_or_default()
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[derive(GodotClass)]
#[class(base = Node)]
struct FortressWasmPeer {
    base: Base<Node>,
    runtime: Option<Runtime>,
    report_json: Option<String>,
    completed: bool,
}

#[godot_api]
impl FortressWasmPeer {
    #[func]
    fn configure(&mut self, config_json: GString) -> bool {
        if self.runtime.is_some() || self.report_json.is_some() || self.completed {
            return false;
        }
        let parsed = serde_json::from_str::<BrowserConfig>(&config_json.to_string());
        match parsed.and_then(|config| Runtime::new(config).map_err(serde::de::Error::custom)) {
            Ok(runtime) => {
                self.runtime = Some(runtime);
                true
            }
            Err(error) => {
                godot_error!("FORTRESS_WASM configuration error: {error}");
                false
            }
        }
    }

    #[func]
    fn take_room_json(&mut self) -> GString {
        self.runtime
            .as_mut()
            .and_then(Runtime::take_room_json)
            .map_or_else(GString::new, |json| GString::from(json.as_str()))
    }

    #[func]
    fn take_report_json(&mut self) -> GString {
        if self.report_json.is_some() {
            self.completed = true;
        }
        self.report_json
            .take()
            .map_or_else(GString::new, |json| GString::from(json.as_str()))
    }
}

#[godot_api]
impl INode for FortressWasmPeer {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            runtime: None,
            report_json: None,
            completed: false,
        }
    }

    fn process(&mut self, _delta: f64) {
        if self.completed || self.report_json.is_some() {
            return;
        }
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        let outcome = runtime.tick();
        let runtime_error = match outcome {
            Ok(false) => return,
            Ok(true) => None,
            Err(error) => Some(error),
        };
        let report = runtime.report(runtime_error);
        match serde_json::to_string(&report) {
            Ok(json) => {
                self.report_json = Some(json);
                self.completed = true;
            }
            Err(error) => godot_error!("FORTRESS_WASM report serialization error: {error}"),
        }
    }
}

struct FortressWasmExtension;

// SAFETY: godot-rust requires this marker to register generated GDExtension
// callbacks. The implementation supplies no raw pointers or custom lifecycle code.
//
// This is the fixture's only permitted unsafe site. The allow sits on the module
// rather than the item because `#[gdextension]` is an attribute proc-macro and
// re-emits the impl without item-level attributes, so an `#[allow]` on the impl
// itself does not reach the expanded code. Scoping it to a module holding this
// one item keeps `unsafe_code = "deny"` (Cargo.toml) live for the rest of the
// crate.
#[allow(unsafe_code)]
mod extension_registration {
    use super::{ExtensionLibrary, FortressWasmExtension};
    use godot::init::gdextension;

    #[gdextension]
    unsafe impl ExtensionLibrary for FortressWasmExtension {}
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::{percentile, summarize_intervals};

    #[test]
    fn callback_summary_is_deterministic() {
        let summary = summarize_intervals(&[20_000, 10_000, 30_000, 16_000]);
        assert_eq!(summary.samples, 4);
        assert_eq!(summary.min_us, 10_000);
        assert_eq!(summary.max_us, 30_000);
        assert_eq!(summary.mean_us, 19_000);
        assert_eq!(summary.p95_us, 30_000);
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 95), 5);
    }
}
