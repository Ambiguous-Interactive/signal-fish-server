//! Run orchestration: spawn-or-connect, join, scheduled sends, receipts,
//! resource sampling, artifact writing.
//!
//! Clock discipline: every send and receipt timestamp is
//! `tokio::time::Instant` (monotonic) micros relative to one shared run
//! epoch, and sender and receiver tasks live in the same process — so every
//! latency pair is a same-clock one-way measurement and no run ever compares
//! unsynchronized host clocks to the 50 ms target.
//!
//! Generator discipline: offered traffic follows the schedule independent of
//! response completion. Sends are bounded only by the generator-lag bound:
//! when a sender falls past its intended time by more than the bound, the
//! run is invalidated as generator-saturated instead of silently stretching
//! the schedule.
//!
//! Fault hooks (`pause_sends`, `stall_senders`, `slow_reader`,
//! `kill_server_after`) exist for the negative controls and are recorded in
//! the manifest config, so a replay sees exactly what the run did.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use signal_fish_server::protocol::{
    ClientMessage, GameDataEncoding, ServerMessage, Topology, Transport,
};
// The runner's config delivery class and the wire enum share the token set;
// the alias keeps the mapping at the one boundary where they meet.
use signal_fish_server::protocol::DeliveryClass as WireDeliveryClass;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::artifacts::{
    self, BuildIdentity, ConfigProvenance, IntervalSample, ServerIdentity, WorkloadShape,
};
use crate::config::{
    count_u64, count_usize, default_latest_keys, micros, ChurnSchedule, DeliveryClass, Encoding,
    Experiment, RunConfig, SendPause,
};
use crate::diagnostics;
use crate::oracle::{self, InvalidReason, OutcomeSummary};
use crate::records::{
    ChurnEvent, ChurnPhase, DisconnectEvent, DisconnectObservation, EventLog, GapEvent,
    ReceiptEvent, RunRecords, SentEvent, UnsupportedNoticeEvent,
};
use crate::schedule::{build_run_shape, ScheduledSend, SenderPlan};
use crate::websocket_test_helpers;
use crate::websocket_test_helpers::server_process::{
    effective_server_config, spawn_server, ServerProcess,
};
use crate::websocket_test_helpers::WsStream;

/// Timeout for one client connect or handshake step (saturation-tolerant
/// ceiling, same rationale as the shared harness `CONNECT_TIMEOUT`).
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Receive-buffer clamp for the slow-reader control's designated peer (see
/// `websocket_test_helpers::connect_with_small_recv_buffer`).
const SLOW_READER_RECV_BUFFER_BYTES: u32 = 4_096;

const GAME_NAME: &str = "capacity-runner";

pub type WsSink = futures_util::stream::SplitSink<WsStream, Message>;
pub type WsReceiver = futures_util::stream::SplitStream<WsStream>;

/// Sender-identity registry: `PlayerId` -> `(peer name, incarnation)`.
///
/// The server's relay stream stamps are per connection — a fresh rejoin is
/// a new member whose `(epoch, seq)` restarts at `(1, 1)` under a new
/// `PlayerId` — so the stream identity the oracle validates is the runner's
/// own incarnation index. Each peer task registers its id at every join;
/// receiving tasks resolve every inbound frame's `from_player` through
/// here, which classifies deliveries exactly even across a rejoin storm.
/// `pub(crate)` for the deterministic inbound-classification controls.
pub(crate) type SenderRegistry = Arc<std::sync::Mutex<BTreeMap<String, (String, u32)>>>;

/// The result of one completed run.
#[derive(Debug)]
pub struct RunOutcome {
    pub run_id: String,
    pub output_dir: PathBuf,
    /// The most recent successful scrape's server counters, as recorded
    /// evidence (delivery counters, slow-consumer disconnects, active
    /// connections, class outcomes). `None` when no scrape succeeded —
    /// recorded absence, never omission. Failed scrapes stay visible as
    /// explicit samples in `intervals.jsonl`.
    pub final_counters: Option<serde_json::Value>,
    pub summary: OutcomeSummary,
}

/// Run one configured capacity experiment end to end and write its
/// artifacts. Returns the exact outcome summary the run recorded.
pub async fn run(mut config: RunConfig) -> Result<RunOutcome, String> {
    if let Some(evidence) = &config.external_host_evidence {
        evidence.validate(config.endpoint.as_deref())?;
    }
    if config.rooms == 0 {
        return Err("rooms must be at least 1".to_string());
    }
    if config.rooms > 999 {
        return Err(format!(
            "rooms must be at most 999 (room codes carry three decimal digits), got {}",
            config.rooms
        ));
    }
    if config.players_per_room < 2 {
        return Err(
            "players_per_room must be at least 2 (a solo peer has no recipients)".to_string(),
        );
    }
    if config.send_rate_per_sender <= 0.0 {
        return Err("send_rate_per_sender must be positive".to_string());
    }
    if config.payload_bytes == 0 {
        return Err("payload_bytes must be at least 1".to_string());
    }
    if config.duration.is_zero() {
        return Err("duration must be positive".to_string());
    }
    // Hook/class coherence: each fault hook belongs to exactly one delivery
    // contract. A wedged reliable reader exercises the slow-consumer
    // eviction; a paused lossy-class reader exercises supersession/eviction
    // accounting. Mixing them would measure neither.
    match config.delivery_class {
        DeliveryClass::Reliable => {
            if config.pause_reads.is_some() {
                return Err(
                    "pause_reads belongs to the latest/volatile cells; a reliable run has no \
                     policy loss for a paused reader to surface"
                        .to_string(),
                );
            }
            if config.latest_keys_per_sender != default_latest_keys() {
                return Err(
                    "latest_keys_per_sender belongs to class \"latest\"; a reliable run \
                     coalesces nothing"
                        .to_string(),
                );
            }
        }
        DeliveryClass::Latest | DeliveryClass::Volatile => {
            if config.slow_reader {
                return Err(
                    "slow_reader belongs to the reliable cell; latest/volatile never \
                     backpressure a reader, so the wedge would exercise lossy-class \
                     accounting under the wrong declared fault"
                        .to_string(),
                );
            }
        }
    }
    if config.delivery_class == DeliveryClass::Latest && config.latest_keys_per_sender == 0 {
        return Err("latest_keys_per_sender must be at least 1".to_string());
    }
    // Delivery classes are a protocol-v3 feature: on the frozen v2 wire the
    // server rejects every classed frame, so a v2 run with a lossy class
    // could only produce an unexplained deficit.
    if config.encoding == Encoding::V2Json && config.delivery_class != DeliveryClass::Reliable {
        return Err(
            "delivery classes require encoding \"v3-json\"; the server rejects classed frames \
             on the frozen v2 wire"
                .to_string(),
        );
    }
    // The unsupported-format experiment is a single-fault, v3, reliable cell:
    // binary frames carry no delivery class (the opaque lane is the reliable
    // lane), v2 recipients have no DeliveryReports to validate (advisories
    // only, by contract), and the churn storms compose with their own stream
    // contract in a later cell rather than half-measured here.
    if config.experiment == Some(Experiment::UnsupportedFormat) {
        if config.encoding != Encoding::V3Json {
            return Err(
                "the unsupported-format experiment requires encoding \"v3-json\": only v3 \
                 recipients receive the exact unsupported_format DeliveryReports the oracle \
                 validates"
                    .to_string(),
            );
        }
        if config.delivery_class != DeliveryClass::Reliable {
            return Err(
                "the unsupported-format experiment requires delivery class \"reliable\": \
                 binary frames carry no class, so the opaque lane is the reliable lane"
                    .to_string(),
            );
        }
        if config.churn != ChurnSchedule::None {
            return Err(
                "the unsupported-format experiment does not compose with churn; run them as \
                 separate cells"
                    .to_string(),
            );
        }
        let rkyv_enabled = config
            .server_overlay
            .get("protocol")
            .and_then(|protocol| protocol.get("enable_rkyv_game_data"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        if !rkyv_enabled {
            return Err(
                "the unsupported-format experiment requires the server overlay to enable \
                 protocol.enable_rkyv_game_data (RunConfig::unsupported_format_overlay); \
                 otherwise the opaque negotiation silently downgrades to JSON and the cell \
                 measures nothing"
                    .to_string(),
            );
        }
    }
    // Churn cells run on the v3 wire alone: receipts validate against the
    // server's per-`(sender, epoch)` relay stamps, which the frozen v2 wire
    // does not carry. Each churn run is also a single-fault cell: the other
    // hooks own the designated peer's socket or its schedule, so they cannot
    // compose with the storm.
    if config.churn != ChurnSchedule::None {
        if config.encoding == Encoding::V2Json {
            return Err(
                "churn requires encoding \"v3-json\": only v3 receipts carry the per-epoch \
                 stream stamps the oracle validates across a rejoin"
                    .to_string(),
            );
        }
        if config.slow_reader {
            return Err(
                "slow_reader and churn both own the designated peer's socket; run them as \
                 separate cells"
                    .to_string(),
            );
        }
        if config.pause_reads.is_some() {
            return Err(
                "pause_reads and churn both own the designated peer's socket; run them as \
                 separate cells"
                    .to_string(),
            );
        }
        if config.pause_sends.is_some() || config.stall_senders.is_some() {
            return Err(
                "the generator-latency hooks (pause_sends, stall_senders) do not compose with \
                 churn; a churn cell's lag must come from its own schedule"
                    .to_string(),
            );
        }
        if config.kill_server_after.is_some() {
            return Err(
                "kill_server_after and churn are both run-level faults; a churn cell's \
                 disconnects must come from its own storm"
                    .to_string(),
            );
        }
        // A send whose wake slips past the churn boundary loses the race to
        // the biased churn arm and fires after the rejoin with the whole
        // offline window as lag — the bound must hold that margin, or a
        // scheduling artifact would be mislabeled a generator fault.
        if let Some(window) = config.churn.window() {
            if micros(window) >= micros(config.generator_lag_bound) {
                return Err(format!(
                    "the churn stagger window ({window:?}) must stay below the generator-lag \
                     bound ({:?}) so a boundary race cannot inflate a send's lag past it",
                    config.generator_lag_bound
                ));
            }
        }
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    // Room codes are scoped to this run so two concurrent runs against one
    // shared external server start from distinct room namespaces (12 bits —
    // a rare prefix collision degrades to loud join refusals, never silent
    // cross-talk).
    config.room_code_prefix = Some(room_code_prefix(&run_id));
    let (plans, churn_plan) = build_run_shape(&config)?;
    validate_payload_size(&plans, config.payload_bytes)?;
    let roster: Vec<(String, u32)> = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect();
    let max_intended_us = plans
        .iter()
        .map(SenderPlan::last_intended_us)
        .max()
        .unwrap_or(0);
    // Config validations that only need the plans run before anything is
    // written or spawned, so a refused config never poisons an output dir.
    // A declared kill must land inside the scheduled sends (a later kill
    // would fire after the verdict is recorded and go unrecorded), and it
    // requires a spawned server: an external endpoint is outside this
    // process's control, and claiming "server terminated" over a live
    // endpoint would write a false fact into the artifacts.
    if config.endpoint.is_none() {
        let effective = effective_server_config(0, &config.server_overlay)?;
        if !effective["security"]["app_auth_path"].is_null()
            || !effective["security"]["connect_token"]["public_key_path"].is_null()
        {
            return Err(
                "capacity provenance does not support file-backed app_auth_path or \
                 connect_token.public_key_path; use inline values"
                    .to_string(),
            );
        }
    }
    if let Some(kill_after) = config.kill_server_after {
        if config.endpoint.is_some() {
            return Err(
                "kill_server_after requires the runner to spawn the server; it cannot \
                 terminate an external endpoint"
                    .to_string(),
            );
        }
        if micros(kill_after) >= max_intended_us {
            return Err(format!(
                "kill_server_after ({kill_after:?}) must fall inside the scheduled sends \
                 (last intended send at {max_intended_us} µs into the run)"
            ));
        }
    }
    // The paused reader must resume while traffic still flows, so the run
    // measures the pressure phase and the drain — not a silence window.
    if let Some(pause_reads) = config.pause_reads {
        if micros(pause_reads) >= max_intended_us {
            return Err(format!(
                "pause_reads ({pause_reads:?}) must end before the last scheduled send \
                 (last intended send at {max_intended_us} µs into the run)"
            ));
        }
    }
    // A directory that already holds a manifest belongs to another run:
    // mixing two runs' artifacts would make the replay a lie. Refuse loudly.
    if config.output_dir.join(artifacts::MANIFEST_FILE).exists() {
        return Err(format!(
            "output dir {} already holds a run manifest; use a fresh output directory per run",
            config.output_dir.display()
        ));
    }
    std::fs::create_dir_all(&config.output_dir)
        .map_err(|error| format!("create output dir {}: {error}", config.output_dir.display()))?;
    let log = Arc::new(EventLog::new());

    // Server: spawn the compiled binary or connect to an external endpoint.
    let server_slot: Arc<tokio::sync::Mutex<Option<ServerProcess>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    let (_port, endpoint_base) = match &config.endpoint {
        Some(base) => {
            let port = parse_endpoint_port(base)?;
            (port, base.clone())
        }
        None => {
            let server = spawn_server(config.server_overlay.clone()).await;
            let port = server.port;
            *server_slot.lock().await = Some(server);
            (port, format!("ws://127.0.0.1:{port}"))
        }
    };
    let ws_url = format!("{}{}", endpoint_base, config.encoding.ws_path());

    // Join phase: every peer of a room joins before the next room starts
    // (the first join creates the room; intra-room joins are concurrent).
    // Each join records the room's member roster (`PlayerId` -> peer name)
    // so `DeliveryReport` gap ranges attribute their omissions to a sender.
    let registry: SenderRegistry = Arc::new(std::sync::Mutex::new(BTreeMap::new()));
    // One joined peer: its plan identity, the session halves, its PlayerId
    // (the registry key), and the join snapshot's `(id, seq tail)` stamps.
    struct JoinedPeer {
        name: String,
        room: u32,
        sink: WsSink,
        rx: WsReceiver,
        player_id: String,
        tails: BTreeMap<String, (String, u64)>,
    }
    let mut peers: Vec<JoinedPeer> = Vec::new();
    for room in 0..config.rooms {
        let room_plans: Vec<&SenderPlan> = plans.iter().filter(|plan| plan.room == room).collect();
        let joins = room_plans.into_iter().map(|plan| {
            let config = &config;
            let ws_url = &ws_url;
            let designated = slow_reader_name(config).as_deref()
                == Some(RunConfig::peer_name(plan.room, plan.player).as_str())
                || paused_reader_name(config).as_deref()
                    == Some(RunConfig::peer_name(plan.room, plan.player).as_str());
            // The experiment's opaque sender: peer 0 negotiates rkyv (the
            // overlay enables it; the negotiation verifies the server
            // advertised it).
            let game_data_format = opaque_sender_format(config, plan.player);
            async move {
                connect_and_join(
                    ws_url,
                    config.encoding,
                    config.room_code(plan.room),
                    config.players_per_room,
                    plan,
                    designated,
                    game_data_format,
                )
                .await
            }
        });
        let results = futures_util::future::join_all(joins).await;
        for (plan, result) in plans.iter().filter(|p| p.room == room).zip(results) {
            match result {
                Ok((sink, rx, player_id, tails)) => peers.push(JoinedPeer {
                    name: plan.name.clone(),
                    room: plan.room,
                    sink,
                    rx,
                    player_id: player_id.to_string(),
                    tails,
                }),
                Err(failure) => log.push_join_failure(format!(
                    "{}: {failure}",
                    RunConfig::peer_name(plan.room, plan.player)
                )),
            }
        }
    }

    // Manifest first (BEFORE the measurement epoch: hashing the server
    // binary and writing the manifest are setup work that must not eat into
    // any scheduled send's budget): a crashed run still leaves its identity
    // and inputs.
    let workload = WorkloadShape {
        senders: count_u64(plans.len()),
        recipients: count_u64(roster.len()),
        scheduled_sends_per_sender: plans.first().map_or(0, |plan| count_u64(plan.sends.len())),
        warmup_sends_per_sender: config.warmup_sends_per_sender(),
        measured_sends_per_sender: config.measured_sends_per_sender(),
    };
    let identity = match &config.endpoint {
        Some(base) => ServerIdentity {
            endpoint: base.clone(),
            pid: None,
            binary_sha256: config
                .external_host_evidence
                .as_ref()
                .map(|evidence| evidence.binary_sha256.clone()),
            binary_bytes: config
                .external_host_evidence
                .as_ref()
                .map(|evidence| evidence.binary_bytes),
            config_overlay_sha256: overlay_sha256(&config)?,
            config_provenance: config.external_host_evidence.clone().map_or(
                ConfigProvenance::UnknownExternal,
                |evidence| ConfigProvenance::ExternalHostDeclared {
                    evidence: Box::new(evidence),
                },
            ),
        },
        None => {
            let (pid, binary_sha256, binary_bytes, config_provenance) = {
                let slot = server_slot.lock().await;
                let server = slot.as_ref().expect("spawned server exists during setup");
                let (binary_sha256, binary_bytes) = diagnostics::sha256_file(server.binary_path())?;
                (
                    server.pid(),
                    binary_sha256,
                    binary_bytes,
                    ConfigProvenance::spawned(server.port, server.effective_config().clone())?,
                )
            };
            ServerIdentity {
                endpoint: endpoint_base.clone(),
                pid: Some(pid),
                binary_sha256: Some(binary_sha256),
                binary_bytes: Some(binary_bytes),
                config_overlay_sha256: overlay_sha256(&config)?,
                config_provenance,
            }
        }
    };
    artifacts::write_manifest(
        &config.output_dir,
        &run_id,
        &config,
        workload,
        identity,
        BuildIdentity::current(),
    )?;

    let max_intended_us = plans
        .iter()
        .map(SenderPlan::last_intended_us)
        .max()
        .unwrap_or(0);
    let hook_extra_us = config.pause_sends.map_or(0, |pause| micros(pause.duration))
        + config.stall_senders.map_or(0, micros)
        + config.pause_reads.map_or(0, micros);
    // Quiescence must cover both a bound-late last send (up to the
    // generator-lag bound past its intended time) AND the quiet delivery
    // drain after that send — never one at the expense of the other.
    let quiescence_us =
        max_intended_us + micros(config.generator_lag_bound) + micros(config.drain_grace);
    let (mut start_tx, start_rx) = tokio::sync::watch::channel(None);
    let mut readiness = Vec::new();

    // Declared slow-reader hook: the designated peer stops reading after its
    // join; the fault is declared at arm time so a replay sees it.
    let slow_reader_name = config.slow_reader.then(|| RunConfig::peer_name(0, 0));
    if let Some(name) = &slow_reader_name {
        log.push_fault(InvalidReason::SlowConsumerDisconnect {
            recipients: vec![name.clone()],
        });
    }

    // Build the sampler client and collect its process facts before arming.
    let sampler_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| format!("build capacity metrics scrape client: {error}"))?;
    let sampler_pid = server_slot.lock().await.as_ref().map(ServerProcess::pid);
    let sampler_url = metrics_url(&endpoint_base);
    let samples: Arc<std::sync::Mutex<Vec<IntervalSample>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut auxiliary_handles = Vec::new();
    {
        let samples = Arc::clone(&samples);
        let log = Arc::clone(&log);
        let mut start = start_rx.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        readiness.push(ready_rx);
        let interval = config.sample_interval;
        let class = (config.delivery_class != DeliveryClass::Reliable
            || config.experiment.is_some())
        .then_some(config.delivery_class);
        auxiliary_handles.push(tokio::spawn(async move {
            match ready_for_epoch(ready_tx, &mut start).await {
                Ok(epoch) => {
                    sample_loop(
                        sampler_client,
                        epoch,
                        epoch + Duration::from_micros(quiescence_us + hook_extra_us),
                        interval,
                        sampler_url,
                        sampler_pid,
                        class,
                        samples,
                    )
                    .await
                }
                Err(error) => {
                    log.push_fault(InvalidReason::RunnerDeadlineExceeded { detail: error })
                }
            }
        }));
    }

    // Prepare the kill hook before arming; its timer uses the shared epoch.
    if let Some(kill_after) = config.kill_server_after {
        let slot = Arc::clone(&server_slot);
        let log = Arc::clone(&log);
        let mut start = start_rx.clone();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        readiness.push(ready_rx);
        auxiliary_handles.push(tokio::spawn(async move {
            let epoch = match ready_for_epoch(ready_tx, &mut start).await {
                Ok(epoch) => epoch,
                Err(error) => {
                    log.push_fault(InvalidReason::RunnerDeadlineExceeded { detail: error });
                    return;
                }
            };
            tokio::time::sleep_until(epoch + kill_after).await;
            // Declare the fault before socket errors can observe the action.
            log.push_fault(InvalidReason::ServerTerminated);
            let process = slot.lock().await.take();
            if let Some(mut process) = process {
                process.kill_and_wait().await;
            }
        }));
    }

    // Slow-reader hook: the designated peer's receive half joins its task
    // but is never polled, so its socket wedges exactly like a stalled
    // client while its send schedule continues. The paused-reader hook is
    // the lossy-class analog: the designated peer does not read until the
    // resume instant (server-side lossy-class pressure engages once the
    // bounded kernel handoff and outbound queue fill), then drains.
    let paused_reader = paused_reader_name(&config);
    let mut handles = Vec::new();
    for JoinedPeer {
        name,
        room,
        sink,
        rx,
        player_id,
        tails,
    } in peers
    {
        let plan = plans
            .iter()
            .find(|plan| plan.name == name && plan.room == room)
            .cloned()
            .expect("peer has a schedule");
        let hold_reads_for = paused_reader
            .as_deref()
            .filter(|designated| *designated == name.as_str())
            .map(|_| config.pause_reads.expect("paused reader carries its pause"));
        let churn_cycles: Vec<PeerChurnCycle> = churn_plan
            .victim_instants(&name)
            .into_iter()
            .enumerate()
            .map(|(cycle_index, (disconnect_us, reconnect_us))| {
                // A room replacement's k-th wave (per this peer) rejoins the
                // room's generation k + 1 — a fresh room code, so the
                // replacement creates a genuinely new room. A burst rejoins
                // the same room every cycle.
                let generation = match config.churn {
                    ChurnSchedule::RoomReplacement { .. } => {
                        u32::try_from(cycle_index).map_or(0, |index| index + 1)
                    }
                    _ => 0,
                };
                PeerChurnCycle {
                    disconnect_us,
                    reconnect_us,
                    rejoin_room_code: RunConfig::room_code_for_generation(
                        config.room_code_prefix.as_deref(),
                        room,
                        generation,
                    ),
                }
            })
            .collect();
        let is_slow_reader = slow_reader_name.as_deref() == Some(name.as_str());
        let game_data_format = opaque_sender_format(&config, plan.player);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        readiness.push(ready_rx);
        handles.push(tokio::spawn(peer_task(
            name,
            plan,
            PeerFacts {
                ws_url: ws_url.clone(),
                encoding: config.encoding,
                players_per_room: config.players_per_room,
                payload_bytes: config.payload_bytes,
                delivery_class: config.delivery_class,
                latest_keys_per_sender: config.latest_keys_per_sender,
                generator_lag_bound_us: micros(config.generator_lag_bound),
                pause_sends: config.pause_sends,
                stall_senders: config.stall_senders,
                experiment: ExperimentContext {
                    active: config.experiment == Some(Experiment::UnsupportedFormat),
                    opaque_sender: game_data_format.is_some(),
                },
                game_data_format,
            },
            churn_cycles,
            start_rx.clone(),
            ready_tx,
            Arc::clone(&registry),
            (sink, rx, player_id, tails),
            Arc::clone(&log),
            quiescence_us,
            hold_reads_for,
            is_slow_reader,
        )));
    }

    // Only the owned tasks retain epoch receivers during arming.
    drop(start_rx);
    // Every task has finished initial setup before one immutable clock starts.
    let epoch = arm_tasks(
        readiness,
        &mut start_tx,
        Instant::now() + STEP_TIMEOUT,
        &mut handles,
        &mut auxiliary_handles,
    )
    .await?;
    // This covers schedule, declared hooks, lag allowance, and delivery drain.
    let hard_deadline = epoch + Duration::from_micros(quiescence_us + hook_extra_us + 5_000_000);

    // Stop every evidence producer before either snapshot or error propagation.
    let peer_result = await_peers(handles, hard_deadline).await;
    let auxiliary_result = abort_and_join(auxiliary_handles).await;
    if !peer_result? {
        log.push_fault(InvalidReason::RunnerDeadlineExceeded {
            detail: "generator or receiver tasks outlived the quiescence margin".to_string(),
        });
    }
    auxiliary_result?;
    let interval_samples = samples.lock().expect("interval samples poisoned").clone();

    // The registry is complete only after every peer task is done: record
    // it once, before the snapshot the artifacts write.
    log.set_registry(registry.lock().expect("sender registry poisoned").clone());
    let records = log.take_records();
    let evidence_finished_us = micros(epoch.elapsed());
    let bound_us = micros(config.generator_lag_bound);
    let summary = oracle::summarize(
        &plans,
        &roster,
        &records,
        bound_us,
        config.payload_bytes,
        config.delivery_class,
        &churn_plan,
        config.experiment,
    );

    if !summary.valid {
        let evidence = invalid_run_diagnostics(
            &run_id,
            &config,
            &plans,
            &records,
            &interval_samples,
            &summary,
            evidence_finished_us,
            tokio::runtime::Handle::current().metrics().num_workers(),
        );
        eprintln!("capacity invalid run: {evidence}");
    }

    artifacts::write_deliveries(&config.output_dir, &records)?;
    artifacts::write_intervals(&config.output_dir, &interval_samples)?;
    artifacts::write_histogram(&config.output_dir, &records)?;
    artifacts::write_json(config.output_dir.join(artifacts::SUMMARY_FILE), &summary)?;

    let final_counters = interval_samples
        .iter()
        .rev()
        .find(|sample| sample.scrape_error.is_none())
        .map(|sample| sample.counters.clone());
    Ok(RunOutcome {
        run_id,
        output_dir: config.output_dir,
        final_counters,
        summary,
    })
}

#[derive(Default)]
struct SenderProgress<'a> {
    completed: u64,
    first: Option<&'a SentEvent>,
    last: Option<&'a SentEvent>,
    worst: Option<&'a SentEvent>,
}

fn completed_send_evidence(send: &SentEvent) -> serde_json::Value {
    json!({"seq": send.seq, "phase": send.phase,
        "intended_us": send.intended_us, "sent_us": send.sent_us,
        "lag_us": send.sent_us.saturating_sub(send.intended_us)})
}

fn interval_evidence(sample: &IntervalSample) -> serde_json::Value {
    json!({"t_us": sample.t_us, "scrape_failed": sample.scrape_error.is_some(),
        "server_cpu_seconds": sample.server_cpu_seconds,
        "generator_cpu_seconds": sample.generator_cpu_seconds,
        "server_rss_bytes": sample.server_rss_bytes,
        "generator_rss_bytes": sample.generator_rss_bytes})
}

// Connection and scrape errors can carry endpoint credentials. Keep error
// kinds and numeric context here; the artifacts retain the original details.
fn reason_evidence(reason: &InvalidReason) -> serde_json::Value {
    if let InvalidReason::JoinFailed { failures } = reason {
        return json!({"kind":"join_failed", "failure_count":failures.len(),
            "details_omitted":true});
    }
    fn omit_details(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                if fields.remove("detail").is_some() {
                    fields.insert("detail_omitted".into(), serde_json::Value::Bool(true));
                }
                for child in fields.values_mut() {
                    omit_details(child);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    omit_details(child);
                }
            }
            _ => {}
        }
    }
    let mut evidence = serde_json::to_value(reason).expect("reason evidence serializes");
    omit_details(&mut evidence);
    evidence
}

/// Failure-only context retains three send observations per peer and a
/// constant sampler context. It does not infer a cause from timing gaps.
#[allow(clippy::too_many_arguments)]
fn invalid_run_diagnostics(
    run_id: &str,
    config: &RunConfig,
    plans: &[SenderPlan],
    records: &RunRecords,
    samples: &[IntervalSample],
    summary: &OutcomeSummary,
    finished_us: u64,
    runtime_workers: usize,
) -> serde_json::Value {
    let mut progress: BTreeMap<&str, SenderProgress<'_>> = plans
        .iter()
        .map(|plan| (plan.name.as_str(), SenderProgress::default()))
        .collect();
    for sent in &records.sent {
        let peer = progress.entry(sent.sender.as_str()).or_default();
        peer.completed += 1;
        if peer
            .first
            .is_none_or(|first| (sent.sent_us, sent.seq) < (first.sent_us, first.seq))
        {
            peer.first = Some(sent);
        }
        if peer
            .last
            .is_none_or(|last| (sent.sent_us, sent.seq) > (last.sent_us, last.seq))
        {
            peer.last = Some(sent);
        }
        if peer.worst.is_none_or(|worst| {
            sent.sent_us.saturating_sub(sent.intended_us)
                > worst.sent_us.saturating_sub(worst.intended_us)
        }) {
            peer.worst = Some(sent);
        }
    }
    let peers: Vec<_> = progress
        .into_iter()
        .map(|(sender, peer)| {
            json!({
                "sender": sender, "completed": peer.completed,
                "first": peer.first.map(completed_send_evidence),
                "last": peer.last.map(completed_send_evidence),
                "worst": peer.worst.map(completed_send_evidence),
            })
        })
        .collect();
    let mut largest_gap = (0_u64, None, None);
    let mut previous: Option<&IntervalSample> = None;
    for next in samples {
        let gap = next
            .t_us
            .saturating_sub(previous.map_or(0, |sample| sample.t_us));
        if gap > largest_gap.0 {
            largest_gap = (gap, previous, Some(next));
        }
        previous = Some(next);
    }
    let tail_gap = finished_us.saturating_sub(previous.map_or(0, |sample| sample.t_us));
    if tail_gap > largest_gap.0 {
        largest_gap = (tail_gap, previous, None);
    }
    json!({"event":"capacity_invalid_run", "run_id":run_id, "runtime_workers":runtime_workers,
        "workload": {"encoding":config.encoding, "delivery_class":config.delivery_class,
            "seed":config.seed, "churn":config.churn, "experiment":config.experiment,
            "stall_senders_us":config.stall_senders.map(micros),
            "pause_reads_us":config.pause_reads.map(micros),
            "kill_server_after_us":config.kill_server_after.map(micros),
            "pause_sends":config.pause_sends.map(|pause|json!({
                "after_seq":pause.after_seq, "duration_us":micros(pause.duration)})),
            "slow_reader":config.slow_reader, "latest_keys_per_sender":config.latest_keys_per_sender,
            "payload_bytes":config.payload_bytes, "rooms":config.rooms,
            "players_per_room":config.players_per_room,
            "send_rate_per_sender":config.send_rate_per_sender,
            "warmup_us":micros(config.warmup), "duration_us":micros(config.duration),
            "lag_bound_us":micros(config.generator_lag_bound),
            "drain_grace_us":micros(config.drain_grace),
            "sample_interval_us":micros(config.sample_interval)},
        "raw_fault_count":records.faults.len(),
        "raw_faults":records.faults.iter().take(16).map(reason_evidence).collect::<Vec<_>>(),
        "summary_reason_count":summary.reasons.len(),
        "summary_reasons":summary.reasons.iter().take(16).map(reason_evidence).collect::<Vec<_>>(),
        "senders":peers,
        "sampler": {"count":samples.len(), "finished_us":finished_us,
            "largest_gap_us":largest_gap.0,
            "gap_start_us":largest_gap.1.map_or(0, |sample| sample.t_us),
            "gap_end_us":largest_gap.2.map_or(finished_us, |sample| sample.t_us),
            "before":largest_gap.1.map(interval_evidence),
            "after":largest_gap.2.map(interval_evidence),
            "recent":samples.iter().rev().take(3).map(interval_evidence).collect::<Vec<_>>()}})
}

/// A failed scheduled send and an over-bound completed send are different
/// observations. These timestamps separate wake, preparation, and write time.
#[derive(serde::Serialize)]
struct SendLagObservation<'a> {
    event: &'static str,
    stage: &'static str,
    sender: &'a str,
    seq: u64,
    phase: crate::schedule::Phase,
    intended_us: u64,
    observed_us: u64,
    lag_us: u64,
    bound_us: u64,
    wake_us: u64,
    write_start_us: Option<u64>,
    sent_us: Option<u64>,
    preparation_us: Option<u64>,
    write_elapsed_us: Option<u64>,
}

fn send_lag_observation<'a>(
    sender: &'a str,
    send: ScheduledSend,
    bound_us: u64,
    wake_us: u64,
    write_times: Option<(u64, u64)>,
) -> SendLagObservation<'a> {
    let observed_us = write_times.map_or(wake_us, |(_, sent_us)| sent_us);
    SendLagObservation {
        event: "capacity_generator_lag",
        stage: if write_times.is_some() {
            "completed_write"
        } else {
            "before_write"
        },
        sender,
        seq: send.seq,
        phase: send.phase,
        intended_us: send.intended_us,
        observed_us,
        lag_us: observed_us.saturating_sub(send.intended_us),
        bound_us,
        wake_us,
        write_start_us: write_times.map(|(start, _)| start),
        sent_us: write_times.map(|(_, sent)| sent),
        preparation_us: write_times.map(|(start, _)| start.saturating_sub(wake_us)),
        write_elapsed_us: write_times.map(|(start, sent)| sent.saturating_sub(start)),
    }
}

fn report_send_lag(observation: &SendLagObservation<'_>) {
    let evidence = serde_json::to_string(observation).expect("send lag evidence serializes");
    eprintln!("capacity generator lag: {evidence}");
}

/// SHA-256 of the server config overlay bytes (the overlay itself is part of
/// the manifest's recorded config). This does not identify an external server.
fn overlay_sha256(config: &RunConfig) -> Result<String, String> {
    let bytes = serde_json::to_vec(&config.server_overlay)
        .map_err(|error| format!("serialize server overlay: {error}"))?;
    Ok(diagnostics::sha256_bytes(&bytes))
}

/// The negotiated game-data format for a peer in an unsupported-format
/// experiment run: peer 0 of every room is the opaque sender, everyone else
/// stays JSON. `None` outside the experiment.
fn opaque_sender_format(config: &RunConfig, player: u32) -> Option<GameDataEncoding> {
    match config.experiment {
        Some(Experiment::UnsupportedFormat) if player == 0 => Some(GameDataEncoding::Rkyv),
        _ => None,
    }
}

/// One peer: connect (clamped for the designated reader hooks), negotiate
/// (v3), join. Returns the sink, the receive half, the peer's own
/// `PlayerId`, and the snapshot's per-member `(id, seq tail)` stamps for
/// the churn record. The incarnation index is the runner's own counter (one
/// join per connection), not the snapshot's server-epoch field: the
/// server's epoch is per connection, so a fresh rejoin is a new member
/// whose relay stream restarts at `(epoch 1, seq 1)` under a new
/// `PlayerId`.
async fn connect_and_join(
    ws_url: &str,
    encoding: Encoding,
    room_code: String,
    players_per_room: u32,
    plan: &SenderPlan,
    designated: bool,
    game_data_format: Option<GameDataEncoding>,
) -> Result<
    (
        WsSink,
        WsReceiver,
        signal_fish_server::protocol::PlayerId,
        BTreeMap<String, (String, u64)>,
    ),
    String,
> {
    let name = RunConfig::peer_name(plan.room, plan.player);
    let stream = if designated {
        connect_clamped(ws_url, SLOW_READER_RECV_BUFFER_BYTES).await?
    } else {
        connect_plain(ws_url).await?
    };
    let (mut sink, mut rx) = stream.split();
    if encoding == Encoding::V3Json {
        authenticate_v3(&mut sink, &mut rx, game_data_format).await?;
    }
    let joined = join_room(&mut sink, &mut rx, &room_code, &name, players_per_room).await?;
    // Snapshot tails: per member, the id whose registry entry names its
    // incarnation, and the seq tail its stream already reached ("a
    // recipient owes no GameData at or below this sequence").
    let tails = joined
        .current_players
        .iter()
        .map(|player| {
            (
                player.name.clone(),
                (player.id.to_string(), player.seq.unwrap_or(0)),
            )
        })
        .collect();
    Ok((sink, rx, joined.player_id, tails))
}

fn slow_reader_name(config: &RunConfig) -> Option<String> {
    config.slow_reader.then(|| RunConfig::peer_name(0, 0))
}

/// The designated read-pause peer (the lossy-class pressure control).
fn paused_reader_name(config: &RunConfig) -> Option<String> {
    config
        .pause_reads
        .is_some()
        .then(|| RunConfig::peer_name(0, 0))
}

/// Connect to `ws://{url}` with default buffers.
async fn connect_plain(url: &str) -> Result<WsStream, String> {
    let connect = tokio_tungstenite::connect_async(url);
    match tokio::time::timeout(STEP_TIMEOUT, connect).await {
        Ok(Ok((stream, _response))) => Ok(stream),
        Ok(Err(error)) => Err(format!("connect {url}: {error}")),
        Err(_) => Err(format!("connect {url}: timed out after {STEP_TIMEOUT:?}")),
    }
}

/// Connect with the kernel receive buffer clamped BEFORE the handshake (the
/// slow-reader control's wedge must not depend on autotuned loopback sysctls).
async fn connect_clamped(url: &str, recv_buffer_bytes: u32) -> Result<WsStream, String> {
    let addr = url
        .trim_start_matches("ws://")
        .trim_end_matches('/')
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let socket =
        tokio::net::TcpSocket::new_v4().map_err(|error| format!("create TCP socket: {error}"))?;
    socket
        .set_recv_buffer_size(recv_buffer_bytes)
        .map_err(|error| format!("clamp SO_RCVBUF: {error}"))?;
    let target: std::net::SocketAddr = addr
        .parse()
        .map_err(|error| format!("parse endpoint address {addr}: {error}"))?;
    let stream = match tokio::time::timeout(STEP_TIMEOUT, socket.connect(target)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => return Err(format!("clamped connect {addr}: {error}")),
        Err(_) => return Err(format!("clamped connect {addr}: timed out")),
    };
    let handshake =
        tokio_tungstenite::client_async(url, tokio_tungstenite::MaybeTlsStream::Plain(stream));
    match tokio::time::timeout(STEP_TIMEOUT, handshake).await {
        Ok(Ok((stream, _response))) => Ok(stream),
        Ok(Err(error)) => Err(format!("clamped handshake {url}: {error}")),
        Err(_) => Err(format!("clamped handshake {url}: timed out")),
    }
}

/// v3 negotiation: `Authenticate` then await `Authenticated` + `ProtocolInfo`.
/// A requested `game_data_format` must be advertised by the server's
/// `ProtocolInfo` — the negotiation refusing it would silently downgrade the
/// session to JSON and the run would measure a different cell than its
/// manifest claims, so that refusal is loud here.
async fn authenticate_v3(
    sink: &mut WsSink,
    rx: &mut WsReceiver,
    game_data_format: Option<GameDataEncoding>,
) -> Result<(), String> {
    let authenticate = ClientMessage::Authenticate {
        app_id: GAME_NAME.to_string(),
        connect_token: None,
        sdk_version: None,
        platform: Some("capacity-runner".to_string()),
        game_data_format,
        protocol_version: Some(3),
        supported_transports: Some(vec![Transport::Relay]),
        supported_topologies: Some(vec![Topology::Relay]),
        requested_capabilities: None,
    };
    send_client(sink, &authenticate).await?;
    let mut authenticated = false;
    let mut advertised_formats: Option<Vec<GameDataEncoding>> = None;
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
    while !(authenticated && advertised_formats.is_some()) {
        let frame = match tokio::time::timeout_at(deadline, rx.next()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => return Err("connection closed during v3 authentication".to_string()),
            Err(_) => return Err("v3 authentication timed out".to_string()),
        };
        let message = decode_server_frame(frame)?;
        match message {
            ServerMessage::Authenticated { .. } => authenticated = true,
            ServerMessage::ProtocolInfo(payload) => {
                advertised_formats = Some(payload.game_data_formats);
            }
            ServerMessage::AuthenticationError { error, .. } => {
                return Err(format!("v3 authentication refused: {error}"))
            }
            _ => {}
        }
    }
    if let Some(requested) = game_data_format {
        let advertised = advertised_formats.unwrap_or_default();
        if !advertised.contains(&requested) {
            return Err(format!(
                "the server did not advertise the requested game-data format {requested:?} \
                 (advertised: {advertised:?}); the run would silently negotiate down to JSON"
            ));
        }
    }
    Ok(())
}

/// Join the run's room and fail loudly on any refusal. Returns the join
/// payload (the room's member roster rides in `current_players`).
async fn join_room(
    sink: &mut WsSink,
    rx: &mut WsReceiver,
    room_code: &str,
    player_name: &str,
    max_players: u32,
) -> Result<signal_fish_server::protocol::RoomJoinedPayload, String> {
    let join = ClientMessage::JoinRoom {
        game_name: GAME_NAME.to_string(),
        room_code: Some(room_code.to_string()),
        player_name: player_name.to_string(),
        max_players: Some(u8::try_from(max_players).map_err(|_| {
            format!("players_per_room {max_players} exceeds the wire limit of 255")
        })?),
        supports_authority: Some(false),
        relay_transport: None,
        password: None,
        join_only: None,
    };
    send_client(sink, &join).await?;
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
    loop {
        let frame = match tokio::time::timeout_at(deadline, rx.next()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => return Err("connection closed while joining".to_string()),
            Err(_) => return Err(format!("join {room_code} timed out")),
        };
        match decode_server_frame(frame)? {
            ServerMessage::RoomJoined(payload) => return Ok(*payload),
            ServerMessage::RoomJoinFailed { reason, .. } => {
                return Err(format!("join {room_code} refused: {reason:?}"))
            }
            _ => {}
        }
    }
}

async fn send_client(sink: &mut WsSink, message: &ClientMessage) -> Result<(), String> {
    let frame = serde_json::to_string(message).map_err(|error| format!("serialize: {error}"))?;
    sink.send(Message::Text(frame.into()))
        .await
        .map_err(|error| format!("send: {error}"))
}

fn decode_server_frame(
    frame: Result<Message, tokio_tungstenite::tungstenite::Error>,
) -> Result<ServerMessage, String> {
    let Message::Text(text) = frame.map_err(|error| format!("websocket: {error}"))? else {
        return Err("unexpected non-text frame during handshake".to_string());
    };
    serde_json::from_str(&text).map_err(|error| format!("malformed server frame: {error}"))
}

/// Construct one exact-size application ledger document. ASCII padding does
/// not escape in JSON, so each padding character adds exactly one byte.
pub(crate) fn ledger_application_data(
    sender: &str,
    seq: u64,
    payload_bytes: u32,
) -> Result<serde_json::Value, String> {
    let mut data = json!({"ledger_sender": sender, "seq": seq, "padding": ""});
    let metadata_bytes = serde_json::to_vec(&data)
        .map_err(|error| format!("serialize ledger metadata: {error}"))?
        .len();
    let padding_bytes = count_usize(payload_bytes).checked_sub(metadata_bytes).ok_or_else(|| {
        format!("payload_bytes ({payload_bytes}) is below ledger metadata size ({metadata_bytes}) for {sender} seq {seq}")
    })?;
    data["padding"] = serde_json::Value::String("x".repeat(padding_bytes));
    Ok(data)
}

/// Encode the actual frame once. The protocol serializer also supplies the
/// envelope size, so JSON byte accounting does not serialize the body twice.
fn encode_application_frame(
    data: serde_json::Value,
    class: Option<WireDeliveryClass>,
    key: Option<u32>,
    opaque: bool,
) -> Result<(Message, u64), String> {
    if opaque {
        let payload = serde_json::to_vec(&data).map_err(|error| format!("serialize: {error}"))?;
        let application_bytes = count_u64(payload.len());
        return Ok((Message::Binary(payload.into()), application_bytes));
    }
    let envelope = serde_json::to_string(&ClientMessage::GameData {
        class,
        key,
        data: serde_json::Value::Null,
    })
    .map_err(|error| format!("serialize envelope: {error}"))?;
    let frame = serde_json::to_string(&ClientMessage::GameData { class, key, data })
        .map_err(|error| format!("serialize: {error}"))?;
    // The null placeholder contributes four bytes to the canonical envelope.
    let application_bytes = frame
        .len()
        .checked_add(4)
        .and_then(|size| size.checked_sub(envelope.len()))
        .ok_or_else(|| "serialized GameData frame is smaller than its envelope".to_string())?;
    Ok((Message::Text(frame.into()), count_u64(application_bytes)))
}

/// Reject an undersized target for any scheduled sequence before run effects.
pub(crate) fn validate_payload_size(
    plans: &[SenderPlan],
    payload_bytes: u32,
) -> Result<(), String> {
    for plan in plans {
        if let Some(seq) = plan.sends.last().map(|send| send.seq) {
            let data = json!({"ledger_sender": plan.name, "seq": seq, "padding": ""});
            let metadata_bytes = serde_json::to_vec(&data)
                .map_err(|error| format!("serialize ledger metadata: {error}"))?
                .len();
            if count_usize(payload_bytes) < metadata_bytes {
                return Err(format!("payload_bytes ({payload_bytes}) is below maximum scheduled ledger metadata size ({metadata_bytes}) for {} seq {seq}", plan.name));
            }
        }
    }
    Ok(())
}

/// The owned config facts one peer task needs (it is spawned, so it cannot
/// borrow the run config). Room codes are not here: the first join happens
/// before the task spawns, and every rejoin carries its cycle's code.
#[derive(Debug, Clone)]
struct PeerFacts {
    ws_url: String,
    encoding: Encoding,
    players_per_room: u32,
    payload_bytes: u32,
    delivery_class: DeliveryClass,
    latest_keys_per_sender: u32,
    generator_lag_bound_us: u64,
    pause_sends: Option<SendPause>,
    stall_senders: Option<Duration>,
    /// The unsupported-format experiment context: peer 0 of every room is
    /// the room's opaque sender (it sends binary frames), every other peer
    /// is a cross-format observer.
    experiment: ExperimentContext,
    game_data_format: Option<GameDataEncoding>,
}

/// The unsupported-format experiment context one peer's sends and inbound
/// frames are classified under.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ExperimentContext {
    /// The run executes the unsupported-format contract experiment.
    pub(crate) active: bool,
    /// This peer is the room's opaque sender: its own stream is binary, and
    /// it must never observe an unsupported-format advisory (it converts for
    /// nobody, and text reaches it format-blind).
    pub(crate) opaque_sender: bool,
}

/// One churn cycle for one peer: the peer disconnects at `disconnect_us`,
/// rejoins at `reconnect_us` into the room code `rejoin_room_code`. For a
/// reconnect burst the code is the peer's room (a rejoin re-seats the same
/// room); for a room replacement it is the room's next generation — a fresh
/// room — so the schedule knowledge stays in `run` and the task stays
/// mechanical.
struct PeerChurnCycle {
    disconnect_us: u64,
    reconnect_us: u64,
    rejoin_room_code: String,
}

/// One peer's whole session lifecycle.
///
/// Sends follow the schedule independent of response completion (generator
/// discipline: sleep to each intended time, honor the fault hooks, push the
/// frame, record the actual send — past the generator-lag bound the run is
/// invalidated as generator-saturated, never silently stretched). Inbound
/// frames are recorded with their same-clock receipt times; `DeliveryReport`
/// gap ranges resolve their sender through the member roster the join
/// snapshot provides and the `PlayerJoined`/`PlayerLeft` broadcasts
/// maintain. At each declared churn instant the socket closes, the peer
/// sleeps out its offline window, rejoins under a fresh incarnation epoch
/// (recorded with the rejoin snapshot's per-member `(epoch, tail)` stamps),
/// and resumes — the offline gap is workload shape because the schedule
/// shifted it, never generator lag.
#[allow(clippy::too_many_arguments)]
async fn peer_task(
    recipient: String,
    plan: SenderPlan,
    facts: PeerFacts,
    churn_instants: Vec<PeerChurnCycle>,
    mut start: tokio::sync::watch::Receiver<Option<Instant>>,
    ready: tokio::sync::oneshot::Sender<()>,
    registry: SenderRegistry,
    initial: (WsSink, WsReceiver, String, BTreeMap<String, (String, u64)>),
    log: Arc<EventLog>,
    quiescence_us: u64,
    hold_reads_for: Option<Duration>,
    never_read: bool,
) {
    let first_measured = plan.sends.first_measured_seq();
    // This peer's incarnation index: 1 on the first connection, +1 on every
    // rejoin. It — not the server's per-connection epoch — is the stream
    // identity the sends are stamped with.
    let mut incarnation: u32 = 1;
    registry
        .lock()
        .expect("sender registry")
        .insert(initial.2, (recipient.clone(), incarnation));
    // The first join's snapshot tails are not recorded: every member's
    // stream is empty at the initial join (joins complete before the first
    // scheduled send), which is the oracle's default floor.
    let _ = initial.3;
    // The next session's halves; `None` between a churn disconnect and its
    // rejoin.
    let mut session: Option<(WsSink, WsReceiver)> = Some((initial.0, initial.1));
    let mut send_cursor = 0usize;
    let mut churn_cursor = 0usize;
    // Generator failure stops sends for every later incarnation. Keep the
    // socket and its inbound evidence alive through the normal lifecycle.
    let mut outbound_stopped = false;
    let epoch = match ready_for_epoch(ready, &mut start).await {
        Ok(epoch) => epoch,
        Err(error) => {
            log.push_fault(InvalidReason::RunnerDeadlineExceeded { detail: error });
            return;
        }
    };
    let until = epoch + Duration::from_micros(quiescence_us);
    let hold_reads_until = hold_reads_for.map(|pause| epoch + pause);

    'sessions: loop {
        let (mut sink, mut rx) = match session.take() {
            Some(pair) => pair,
            None => {
                // Offline window: reconnect at the scheduled instant of the
                // cycle that just disconnected this peer, into that cycle's
                // room code.
                let cycle = &churn_instants[churn_cursor - 1];
                tokio::time::sleep_until(epoch + Duration::from_micros(cycle.reconnect_us)).await;
                match connect_and_join(
                    &facts.ws_url,
                    facts.encoding,
                    cycle.rejoin_room_code.clone(),
                    facts.players_per_room,
                    &plan,
                    false,
                    facts.game_data_format,
                )
                .await
                {
                    Ok((next_sink, next_rx, player_id, tails)) => {
                        incarnation += 1;
                        registry
                            .lock()
                            .expect("sender registry")
                            .insert(player_id.to_string(), (recipient.clone(), incarnation));
                        // The snapshot tails are recorded UNRESOLVED
                        // (`PlayerId`, tail): the oracle resolves them
                        // against the registry recorded at end of run, so
                        // the mapping never depends on task scheduling
                        // order.
                        log.push_churn(ChurnEvent {
                            recipient: recipient.clone(),
                            phase: ChurnPhase::Rejoined,
                            at_us: micros(epoch.elapsed()),
                            epoch: Some(incarnation),
                            tails,
                        });
                        (next_sink, next_rx)
                    }
                    Err(failure) => {
                        log.push_fault(InvalidReason::ReconnectFailed {
                            peer: recipient.clone(),
                            detail: failure,
                        });
                        return;
                    }
                }
            }
        };

        let mut prefer_read = true;
        loop {
            // Keep one send future alive while handling any number of inbound
            // frames. Recreating SinkExt::send after a read could resend a frame.
            let event = {
                let next_send = if outbound_stopped {
                    None
                } else {
                    plan.sends.get(send_cursor)
                };
                let mut writer = std::pin::pin!(async {
                    let Some(send) = next_send else {
                        return std::future::pending::<SendOutcome>().await;
                    };
                    tokio::time::sleep_until(epoch + Duration::from_micros(send.intended_us)).await;
                    // Fault hooks: a pause shifts this and every later send
                    // (the pause must show up as scheduled-send lag, never as
                    // reduced offered load); a stall trips the lag bound
                    // deterministically.
                    if let Some(crate::config::SendPause {
                        after_seq,
                        duration,
                    }) = facts.pause_sends
                    {
                        if send.seq == after_seq {
                            tokio::time::sleep_until(
                                epoch + Duration::from_micros(send.intended_us) + duration,
                            )
                            .await;
                        }
                    }
                    if let (Some(stall), Some(first)) = (facts.stall_senders, first_measured) {
                        if send.seq == first {
                            tokio::time::sleep(stall).await;
                        }
                    }
                    let wake_us = micros(epoch.elapsed());
                    let lag = wake_us.saturating_sub(send.intended_us);
                    if lag > facts.generator_lag_bound_us {
                        report_send_lag(&send_lag_observation(
                            &plan.name,
                            send,
                            facts.generator_lag_bound_us,
                            wake_us,
                            None,
                        ));
                        log.push_fault(InvalidReason::GeneratorSaturated {
                            max_lag_us: lag,
                            bound_us: facts.generator_lag_bound_us,
                        });
                        return SendOutcome::Stopped;
                    }
                    let data =
                        match ledger_application_data(&plan.name, send.seq, facts.payload_bytes) {
                            Ok(data) => data,
                            Err(detail) => {
                                log.push_fault(InvalidReason::SendFailed {
                                    sender: plan.name.clone(),
                                    detail,
                                });
                                return SendOutcome::Stopped;
                            }
                        };
                    let (class, key) = if facts.experiment.opaque_sender {
                        (None, None)
                    } else {
                        match facts.delivery_class {
                            DeliveryClass::Reliable => (None, None),
                            DeliveryClass::Latest => {
                                // The coalescing key is a sender-scoped u32; a
                                // sender round-robins its keys so a run can hold
                                // newest-value semantics (one key) or key
                                // isolation (many keys).
                                let key_index = send.seq % u64::from(facts.latest_keys_per_sender);
                                match u32::try_from(key_index) {
                                    Ok(index) => (Some(WireDeliveryClass::Latest), Some(index)),
                                    Err(error) => {
                                        log.push_fault(InvalidReason::SendFailed {
                                            sender: plan.name.clone(),
                                            detail: error.to_string(),
                                        });
                                        return SendOutcome::Stopped;
                                    }
                                }
                            }
                            DeliveryClass::Volatile => (Some(WireDeliveryClass::Volatile), None),
                        }
                    };
                    let (frame, application_bytes) = match encode_application_frame(
                        data,
                        class,
                        key,
                        facts.experiment.opaque_sender,
                    ) {
                        Ok(encoded) => encoded,
                        Err(detail) => {
                            log.push_fault(InvalidReason::SendFailed {
                                sender: plan.name.clone(),
                                detail,
                            });
                            return SendOutcome::Stopped;
                        }
                    };
                    let encoded_frame_body_bytes = count_u64(frame.len());
                    let write_start_us = micros(epoch.elapsed());
                    if let Err(error) = sink.send(frame).await {
                        // After a declared termination, socket errors are the
                        // expected consequence — the peer stops, and its
                        // remainder is unsent work, not an independent fault.
                        return failed_transport_write(&log, &plan.name, error.to_string());
                    }
                    let sent_us = micros(epoch.elapsed());
                    if sent_us.saturating_sub(send.intended_us) > facts.generator_lag_bound_us {
                        report_send_lag(&send_lag_observation(
                            &plan.name,
                            send,
                            facts.generator_lag_bound_us,
                            wake_us,
                            Some((write_start_us, sent_us)),
                        ));
                    }
                    log.push_sent(SentEvent {
                        sender: plan.name.clone(),
                        room: plan.room,
                        seq: send.seq,
                        epoch: incarnation,
                        intended_us: send.intended_us,
                        sent_us,
                        phase: send.phase,
                        application_bytes,
                        encoded_frame_body_bytes,
                    });
                    SendOutcome::Sent
                });
                loop {
                    let event = next_session_event(
                        writer.as_mut(),
                        &mut rx,
                        until,
                        churn_instants
                            .get(churn_cursor)
                            .map(|cycle| epoch + Duration::from_micros(cycle.disconnect_us)),
                        hold_reads_until,
                        never_read,
                        &mut prefer_read,
                    )
                    .await;
                    match event {
                        SessionEvent::Frame(Some(frame)) => {
                            if !handle_inbound(
                                &recipient,
                                frame,
                                &registry,
                                epoch,
                                &log,
                                facts.experiment,
                            )
                            .await
                            {
                                return;
                            }
                        }
                        SessionEvent::Frame(None) => {
                            log.push_disconnect(DisconnectEvent {
                                recipient: recipient.clone(),
                                observation: DisconnectObservation::StreamEnded,
                            });
                            return;
                        }
                        SessionEvent::Resume => {}
                        other => break other,
                    }
                }
            };
            match event {
                SessionEvent::Send(SendOutcome::Sent) => send_cursor += 1,
                SessionEvent::Send(SendOutcome::Stopped) => outbound_stopped = true,
                SessionEvent::Quiescence => return,
                SessionEvent::Send(SendOutcome::TransportFailed) => {
                    drain_after_failed_write(
                        &recipient,
                        &mut rx,
                        &registry,
                        epoch,
                        until,
                        hold_reads_until,
                        never_read,
                        &log,
                        facts.experiment,
                    )
                    .await;
                    return;
                }
                SessionEvent::Churn => {
                    log.push_churn(ChurnEvent {
                        recipient: recipient.clone(),
                        phase: ChurnPhase::Disconnect,
                        at_us: micros(epoch.elapsed()),
                        epoch: None,
                        tails: BTreeMap::new(),
                    });
                    churn_cursor += 1;
                    // The pending writer has dropped before either socket half
                    // or the next incarnation can proceed.
                    drop(sink);
                    drop(rx);
                    continue 'sessions;
                }
                SessionEvent::Frame(_) | SessionEvent::Resume => {}
            }
        }
    }
}

fn failed_transport_write(log: &EventLog, sender: &str, detail: String) -> SendOutcome {
    if !log.was_server_terminated() {
        log.push_fault(InvalidReason::SendFailed {
            sender: sender.to_string(),
            detail,
        });
    }
    SendOutcome::TransportFailed
}

// A failed write stops outbound work; inbound evidence still belongs to this run.
#[allow(clippy::too_many_arguments)]
async fn drain_after_failed_write<R>(
    recipient: &str,
    reader: &mut R,
    registry: &SenderRegistry,
    epoch: Instant,
    until: Instant,
    hold_reads_until: Option<Instant>,
    never_read: bool,
    log: &Arc<EventLog>,
    experiment: ExperimentContext,
) where
    R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut writer = std::pin::pin!(std::future::pending::<SendOutcome>());
    let mut prefer_read = true;
    loop {
        match next_session_event(
            writer.as_mut(),
            reader,
            until,
            None,
            hold_reads_until,
            never_read,
            &mut prefer_read,
        )
        .await
        {
            SessionEvent::Frame(Some(frame)) => {
                if !handle_inbound(recipient, frame, registry, epoch, log, experiment).await {
                    return;
                }
            }
            SessionEvent::Frame(None) => {
                log.push_disconnect(DisconnectEvent {
                    recipient: recipient.to_string(),
                    observation: DisconnectObservation::StreamEnded,
                });
                return;
            }
            SessionEvent::Quiescence => {
                log.push_fault(InvalidReason::RunnerDeadlineExceeded {
                    detail: format!(
                        "{recipient}: failed write receiver did not terminate before quiescence"
                    ),
                });
                return;
            }
            SessionEvent::Resume => {}
            SessionEvent::Send(_) | SessionEvent::Churn => {
                unreachable!("drain has no writes or churn")
            }
        }
    }
}

/// Handle one inbound server frame: record the delivery with its same-clock
/// receipt time and the sending incarnation resolved through the registry,
/// resolve gap reports the same way, and account server rejections. Under
/// the unsupported-format experiment, the rate-limited advisories at
/// cross-format observers are permitted evidence (recorded, bounded by the
/// oracle) and any binary frame is the leak class. Returns `false` when the
/// session ended. `pub(crate)` for the deterministic inbound-classification
/// controls in the runner's suite.
pub(crate) async fn handle_inbound(
    recipient: &str,
    frame: Result<Message, tokio_tungstenite::tungstenite::Error>,
    registry: &SenderRegistry,
    epoch: Instant,
    log: &Arc<EventLog>,
    experiment: ExperimentContext,
) -> bool {
    let received_us = micros(epoch.elapsed());
    match frame {
        Ok(Message::Text(text)) => match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::GameData {
                from_player,
                data,
                seq,
                ..
            }) => {
                let application_bytes = count_u64(
                    serde_json::to_vec(&data)
                        .expect("received JSON value serializes")
                        .len(),
                );
                let encoded_frame_body_bytes = count_u64(text.len());
                if let Some((ledger_sender, ledger_seq)) =
                    websocket_test_helpers::delivery_ledger::extract(&data)
                {
                    // The sending incarnation comes from the registry (the
                    // sender's own join registration), keyed by the exact
                    // `PlayerId` the server stamped on the frame — a rejoin
                    // storm therefore classifies every delivery exactly.
                    // The stream sequence is the v3 wire's per-connection
                    // stamp; the frozen v2 wire carries none, and its
                    // single-epoch, send-ordered stream maps
                    // `server_seq = ledger_seq + 1`.
                    let resolved = registry
                        .lock()
                        .expect("sender registry")
                        .get(&from_player.to_string())
                        .cloned();
                    if let Some((sender, sender_incarnation)) = resolved {
                        if sender != ledger_sender {
                            log.push_fault(InvalidReason::UnidentifiedGameData {
                                recipient: recipient.to_string(),
                                received_us,
                                application_bytes,
                                encoded_frame_body_bytes,
                                detail: "ledger sender disagrees with the registered player"
                                    .to_string(),
                            });
                            return true;
                        }
                        let Some(server_seq) = seq.or_else(|| ledger_seq.checked_add(1)) else {
                            log.push_fault(InvalidReason::UnidentifiedGameData {
                                recipient: recipient.to_string(),
                                received_us,
                                application_bytes,
                                encoded_frame_body_bytes,
                                detail: "ledger sequence cannot produce a v2 stream sequence"
                                    .to_string(),
                            });
                            return true;
                        };
                        log.push_receipt(ReceiptEvent {
                            recipient: recipient.to_string(),
                            sender,
                            seq: ledger_seq,
                            epoch: sender_incarnation,
                            server_seq,
                            received_us,
                            application_bytes,
                            encoded_frame_body_bytes,
                        });
                    } else {
                        log.push_fault(InvalidReason::UnidentifiedGameData {
                            recipient: recipient.to_string(),
                            received_us,
                            application_bytes,
                            encoded_frame_body_bytes,
                            detail: format!(
                                "player {from_player} is absent from the sender registry"
                            ),
                        });
                    }
                } else {
                    log.push_fault(InvalidReason::UnidentifiedGameData {
                        recipient: recipient.to_string(),
                        received_us,
                        application_bytes,
                        encoded_frame_body_bytes,
                        detail: "application data has no valid ledger identity".to_string(),
                    });
                }
            }
            Ok(ServerMessage::PlayerJoined { .. }) | Ok(ServerMessage::PlayerLeft { .. }) => {}
            Ok(ServerMessage::DeliveryReport(report)) => {
                for gap in &report.gaps {
                    let resolved = registry
                        .lock()
                        .expect("sender registry")
                        .get(&gap.from_player.to_string())
                        .cloned();
                    let Some((sender, sender_incarnation)) = resolved else {
                        // Record the violation and keep reading: the
                        // remaining stream is evidence, and the verdict
                        // is already invalid.
                        log.push_fault(InvalidReason::InvalidGapReports {
                            count: 1,
                            first: crate::oracle::GapViolation {
                                recipient: recipient.to_string(),
                                sender: gap.from_player.to_string(),
                                detail: "gap names a player absent from the roster".to_string(),
                            },
                        });
                        continue;
                    };
                    log.push_gap(GapEvent {
                        recipient: recipient.to_string(),
                        sender,
                        // The report's epoch is the sender's per-connection
                        // stream; the runner's incarnation index is the
                        // stream identity (one join per connection, so the
                        // connection epoch is always 1 and the index is the
                        // exact discriminator).
                        epoch: sender_incarnation,
                        from_seq: gap.from_seq,
                        to_seq: gap.to_seq,
                        reason: gap.reason,
                    });
                }
            }
            Ok(ServerMessage::Error {
                message,
                error_code,
            }) => {
                // The experiment's rate-limited advisory at a cross-format
                // observer is the permitted prose companion of the exact
                // unsupported_format reports — evidence, not a rejection.
                // Every other error (and any error at the opaque sender,
                // who converts for nobody) stays a fault.
                if experiment.active
                    && !experiment.opaque_sender
                    && error_code
                        == Some(signal_fish_server::protocol::ErrorCode::UnsupportedGameDataFormat)
                {
                    log.push_unsupported_notice(UnsupportedNoticeEvent {
                        recipient: recipient.to_string(),
                        at_us: micros(epoch.elapsed()),
                    });
                    return true;
                }
                // A mid-run server rejection (bad class, payload cap, rate
                // limit) must not decay into an unexplained delivery
                // deficit: record it and keep reading.
                log.push_fault(InvalidReason::ServerRejected {
                    recipient: recipient.to_string(),
                    detail: match error_code {
                        Some(code) => format!("{message} ({code:?})"),
                        None => message,
                    },
                });
            }
            Ok(_) => {}
            Err(error) => {
                log.push_fault(InvalidReason::MalformedServerFrame {
                    recipient: recipient.to_string(),
                    detail: error.to_string(),
                });
                return false;
            }
        },
        Ok(Message::Close(frame)) => {
            log.push_disconnect(DisconnectEvent {
                recipient: recipient.to_string(),
                observation: DisconnectObservation::ServerClosed(
                    frame.map(|close| close.code.into()),
                ),
            });
            return false;
        }
        Ok(Message::Binary(_)) if experiment.active => {
            // The experiment permits no binary frame at any peer: the opaque
            // sender is the room's only binary producer, and the server must
            // refuse its payload to every cross-format recipient. A binary
            // frame here is the leak class — the payload crossed formats.
            log.push_fault(InvalidReason::UnsupportedFormatLeak {
                count: 1,
                first: crate::oracle::DeliveryKey {
                    recipient: recipient.to_string(),
                    sender: "<binary frame>".to_string(),
                    epoch: 0,
                    seq: 0,
                },
            });
        }
        Ok(_) => {}
        Err(_) => {
            log.push_disconnect(DisconnectEvent {
                recipient: recipient.to_string(),
                observation: DisconnectObservation::StreamEnded,
            });
            return false;
        }
    }
    true
}

/// Periodic server resource sampling until `until`. A failed scrape is
/// recorded as an explicit sample with `scrape_error` — never skipped. A
/// lossy-class run or an unsupported-format experiment also samples its
/// class's accountable outcomes.
#[allow(clippy::too_many_arguments)]
async fn sample_loop(
    client: reqwest::Client,
    epoch: Instant,
    until: Instant,
    interval: Duration,
    metrics_url: String,
    pid: Option<u32>,
    delivery_class: Option<DeliveryClass>,
    out: Arc<std::sync::Mutex<Vec<IntervalSample>>>,
) {
    let mut slot = epoch + interval;
    loop {
        if Instant::now() >= until {
            return;
        }
        tokio::time::sleep_until(slot.min(until)).await;
        if Instant::now() >= until {
            return;
        }
        slot += interval;

        let scrape: Result<BTreeMap<String, serde_json::Value>, String> = async {
            let response = client
                .get(&metrics_url)
                .send()
                .await
                .map_err(|error| format!("{metrics_url}: {error}"))?;
            let status = response.status();
            let text = response
                .text()
                .await
                .map_err(|error| format!("{metrics_url}: {error}"))?;
            if !status.is_success() {
                return Err(format!("{metrics_url}: {status}"));
            }
            // Every tracked counter is recorded — its value, or null when
            // the exposition no longer carries it.
            let mut counters: BTreeMap<String, serde_json::Value> = diagnostics::TRACKED_COUNTERS
                .iter()
                .map(|name| {
                    let value = match diagnostics::parse_counter(&text, name) {
                        Some(value) => serde_json::Value::from(value),
                        None => serde_json::Value::Null,
                    };
                    ((*name).to_string(), value)
                })
                .collect();
            // A lossy-class run also records its class's seven accountable
            // outcomes, so server-side accounting can be checked against the
            // gap reports the oracle validated.
            if let Some(class) = delivery_class {
                let class_token = match class {
                    DeliveryClass::Latest => "latest",
                    DeliveryClass::Volatile => "volatile",
                    DeliveryClass::Reliable => "reliable",
                };
                for outcome in [
                    "attempted",
                    "delivered",
                    "superseded",
                    "dropped_full",
                    "dropped",
                    "abandoned",
                    "unsupported_format",
                ] {
                    let value = diagnostics::parse_labeled_counter(
                        &text,
                        "signal_fish_websocket_delivery_class_outcomes_total",
                        &[("class", class_token), ("outcome", outcome)],
                    );
                    counters.insert(
                        format!("class_outcome_{outcome}"),
                        value
                            .map(serde_json::Value::from)
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
            }
            Ok(counters)
        }
        .await;
        let (counters, scrape_error) = match scrape {
            Ok(counters) => (
                serde_json::to_value(counters).unwrap_or(serde_json::Value::Null),
                None,
            ),
            Err(error) => (serde_json::Value::Null, Some(error)),
        };
        let sockets = pid.and_then(diagnostics::socket_memory_pages);
        let sample = IntervalSample {
            t_us: micros(epoch.elapsed()),
            counters,
            server_rss_bytes: pid.and_then(diagnostics::resident_memory_bytes),
            server_cpu_seconds: pid.and_then(diagnostics::process_cpu_seconds),
            cgroup_memory_bytes: pid.and_then(diagnostics::cgroup_memory_bytes),
            generator_rss_bytes: diagnostics::resident_memory_bytes(std::process::id()),
            generator_cpu_seconds: diagnostics::process_cpu_seconds(std::process::id()),
            // One procfs read serves the whole pair, so both fields always
            // come from the same scrape instant (the parser's contract is
            // all-or-nothing per family).
            server_socket_tcp_mem_pages: sockets.as_ref().map(|pages| pages.tcp),
            server_socket_udp_mem_pages: sockets.as_ref().map(|pages| pages.udp),
            scrape_error,
        };
        out.lock().expect("interval samples poisoned").push(sample);
    }
}

/// Metrics endpoint of a server base (`ws://host:port` ->
/// `http://host:port/metrics/prom`): an external endpoint's counters are
/// scraped on the same authority the clients talk to, never a hardcoded
/// loopback.
fn metrics_url(endpoint_base: &str) -> String {
    let base = endpoint_base
        .trim_end_matches('/')
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    format!("{base}/metrics/prom")
}

/// Run-scoped three-hex-character room-code prefix derived from the run ID:
/// concurrent runs against one shared server get distinct room namespaces.
fn room_code_prefix(run_id: &str) -> String {
    let hash = run_id
        .bytes()
        .enumerate()
        .map(|(index, byte)| u64::from(byte).wrapping_mul(1 + index as u64))
        .fold(0x5EED, u64::wrapping_add);
    format!("{:03X}", hash & 0xFFF)
}

/// Port of an external endpoint (`ws://host:port`). The value is a
/// validation: the endpoint must carry an explicit port (no service
/// defaults), which every later URL rewrite (metrics scrape, WebSocket
/// connect) relies on.
fn parse_endpoint_port(endpoint: &str) -> Result<u16, String> {
    let authority = endpoint
        .split("://")
        .nth(1)
        .unwrap_or(endpoint)
        .split('/')
        .next()
        .unwrap_or_default();
    let port = authority
        .rsplit(':')
        .next()
        .and_then(|raw| raw.parse::<u16>().ok())
        .ok_or_else(|| format!("endpoint {endpoint} carries no parseable port"))?;
    Ok(port)
}

#[derive(Debug)]
enum SendOutcome {
    Sent,
    Stopped,
    TransportFailed,
}

#[derive(Debug)]
enum SessionEvent {
    Send(SendOutcome),
    Frame(Option<Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Resume,
    Churn,
    Quiescence,
}

// Alternate ready reads and writes, while lifecycle deadlines always win.
// A pending write remains pinned by the caller across receive events.
#[allow(clippy::too_many_arguments)]
async fn next_session_event<W, R>(
    mut writer: std::pin::Pin<&mut W>,
    reader: &mut R,
    until: Instant,
    churn_at: Option<Instant>,
    resume_at: Option<Instant>,
    never_read: bool,
    prefer_read: &mut bool,
) -> SessionEvent
where
    W: std::future::Future<Output = SendOutcome>,
    R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let reads_held = resume_at.is_some_and(|at| Instant::now() < at);
    let event = tokio::select! {
        biased;
        _ = tokio::time::sleep_until(churn_at.unwrap_or(until)), if churn_at.is_some() => SessionEvent::Churn,
        _ = tokio::time::sleep_until(until) => SessionEvent::Quiescence,
        _ = tokio::time::sleep_until(resume_at.unwrap_or(until)), if reads_held => SessionEvent::Resume,
        event = async {
            if *prefer_read {
                tokio::select! {
                    biased;
                    frame = reader.next(), if !never_read && !reads_held => SessionEvent::Frame(frame),
                    sent = writer.as_mut() => SessionEvent::Send(sent),
                }
            } else {
                tokio::select! {
                    biased;
                    sent = writer.as_mut() => SessionEvent::Send(sent),
                    frame = reader.next(), if !never_read && !reads_held => SessionEvent::Frame(frame),
                }
            }
        } => event,
    };
    match &event {
        SessionEvent::Frame(_) => *prefer_read = false,
        SessionEvent::Send(_) => *prefer_read = true,
        _ => {}
    }
    event
}

async fn ready_for_epoch(
    ready: tokio::sync::oneshot::Sender<()>,
    start: &mut tokio::sync::watch::Receiver<Option<Instant>>,
) -> Result<Instant, String> {
    ready
        .send(())
        .map_err(|()| "run readiness receiver closed before arming".to_string())?;
    loop {
        if let Some(epoch) = *start.borrow_and_update() {
            return Ok(epoch);
        }
        start
            .changed()
            .await
            .map_err(|_| "run epoch channel closed before arming".to_string())?;
    }
}

async fn arm_tasks(
    readiness: Vec<tokio::sync::oneshot::Receiver<()>>,
    start: &mut tokio::sync::watch::Sender<Option<Instant>>,
    deadline: Instant,
    peers: &mut Vec<tokio::task::JoinHandle<()>>,
    auxiliaries: &mut Vec<tokio::task::JoinHandle<()>>,
) -> Result<Instant, String> {
    let result = async {
        if start.borrow().is_some() {
            return Err("run epoch was already armed".to_string());
        }
        tokio::time::timeout_at(deadline, futures_util::future::try_join_all(readiness))
            .await
            .map_err(|_| "run preparation readiness deadline exceeded".to_string())?
            .map_err(|_| "run preparation task ended before readiness".to_string())?;
        let epoch = Instant::now();
        start
            .send(Some(epoch))
            .map_err(|_| "run epoch has no task receivers".to_string())?;
        Ok(epoch)
    }
    .await;
    if result.is_err() {
        // Readiness failures must stop and await every task before returning.
        let peer_cleanup = abort_and_join(std::mem::take(peers)).await;
        let auxiliary_cleanup = abort_and_join(std::mem::take(auxiliaries)).await;
        peer_cleanup?;
        auxiliary_cleanup?;
    }
    result
}

async fn await_peers(
    handles: Vec<tokio::task::JoinHandle<()>>,
    deadline: Instant,
) -> Result<bool, String> {
    // Remove completed handles as they finish; a JoinHandle cannot be polled
    // again after yielding its result, including during deadline cleanup.
    let mut pending: futures_util::stream::FuturesUnordered<_> = handles.into_iter().collect();
    let mut failure = None;
    let completed = tokio::time::timeout_at(deadline, async {
        while let Some(result) = pending.next().await {
            if let Err(error) = result {
                failure = Some(format!("capacity peer task failed: {error}"));
                return false;
            }
        }
        true
    })
    .await
    .unwrap_or(false);
    if !completed {
        for handle in pending.iter() {
            handle.abort();
        }
        while let Some(result) = pending.next().await {
            if let Err(error) = result {
                if !error.is_cancelled() && failure.is_none() {
                    failure = Some(format!("capacity peer task failed during cleanup: {error}"));
                }
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(completed),
    }
}

async fn abort_and_join(handles: Vec<tokio::task::JoinHandle<()>>) -> Result<(), String> {
    for handle in &handles {
        handle.abort();
    }
    // Await cancellation before the caller snapshots any records or registry.
    for result in futures_util::future::join_all(handles).await {
        if let Err(error) = result {
            if !error.is_cancelled() {
                return Err(format!("capacity task failed during cleanup: {error}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod session_poll_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn outgoing_frame_bytes_match_canonical_body_and_protocol_serializers() {
        let mut documents = vec![
            serde_json::Value::Null,
            json!(true),
            json!(1234567890),
            json!(-12.25),
            json!("quotes\", slash\\, newline\n, 雪 😀"),
            json!({"escaped\"key": [null, false, 0, "\t\r漢字"]}),
        ];
        for bytes in [96, 1_024, 16_384, 65_536] {
            documents.push(ledger_application_data("r0p1", u64::MAX, bytes).expect("ledger body"));
        }
        for data in documents {
            let reference_body = serde_json::to_vec(&data).expect("canonical body");
            for (class, key) in [
                (None, None),
                (Some(WireDeliveryClass::Reliable), None),
                (Some(WireDeliveryClass::Volatile), None),
                (Some(WireDeliveryClass::Latest), Some(0)),
                (Some(WireDeliveryClass::Latest), Some(9)),
                (Some(WireDeliveryClass::Latest), Some(10)),
                (Some(WireDeliveryClass::Latest), Some(u32::MAX)),
            ] {
                let reference_frame = serde_json::to_string(&ClientMessage::GameData {
                    class,
                    key,
                    data: data.clone(),
                })
                .expect("canonical frame");
                let (frame, bytes) =
                    encode_application_frame(data.clone(), class, key, false).expect("JSON frame");
                assert_eq!(frame, Message::Text(reference_frame.into()));
                assert_eq!(bytes, count_u64(reference_body.len()));
            }
            let (frame, bytes) =
                encode_application_frame(data, None, None, true).expect("opaque frame");
            assert_eq!(bytes, count_u64(reference_body.len()));
            assert_eq!(frame, Message::Binary(reference_body.into()));
        }
    }

    #[test]
    fn send_lag_evidence_distinguishes_before_write_and_completed_write() {
        let send = ScheduledSend {
            seq: 17,
            intended_us: 100_000,
            phase: crate::schedule::Phase::Measured,
        };
        for (write_times, stage, observed, preparation, write) in [
            (None, "before_write", 517_628, None, None),
            (
                Some((120_000, 517_628)),
                "completed_write",
                517_628,
                Some(10_000),
                Some(397_628),
            ),
        ] {
            let wake = if write_times.is_some() {
                110_000
            } else {
                517_628
            };
            let evidence = serde_json::to_value(send_lag_observation(
                "r0p0",
                send,
                250_000,
                wake,
                write_times,
            ))
            .expect("lag JSON");
            assert_eq!(evidence["stage"], stage);
            assert_eq!(evidence["sender"], "r0p0");
            assert_eq!(evidence["seq"], 17);
            assert_eq!(evidence["phase"], "Measured");
            assert_eq!(evidence["intended_us"], 100_000);
            assert_eq!(evidence["observed_us"], observed);
            assert_eq!(evidence["lag_us"], 417_628);
            assert_eq!(evidence["bound_us"], 250_000);
            assert_eq!(evidence["preparation_us"], serde_json::json!(preparation));
            assert_eq!(evidence["write_elapsed_us"], serde_json::json!(write));
        }
    }

    #[test]
    fn invalid_run_evidence_preserves_all_senders_and_fault_origins() {
        let context = crate::unit_context();
        let mut config = crate::scenario_config(Encoding::V3Json);
        config.endpoint = Some("wss://user:private-secret@example.invalid:1234".into());
        config.server_overlay = json!({"secret":"private-secret"});
        for live_fault in [true, false] {
            let mut records = crate::complete_records(&context.plans);
            records.sent.retain(|send| send.sender != "r0p2");
            if live_fault {
                records.faults.push(InvalidReason::GeneratorSaturated {
                    max_lag_us: 417_628,
                    bound_us: 250_000,
                });
            } else {
                let culprit = records
                    .sent
                    .iter_mut()
                    .rev()
                    .find(|send| send.sender == "r0p0")
                    .expect("culprit");
                culprit.sent_us = culprit.intended_us + 417_628;
            }
            let summary = oracle::summarize(
                &context.plans,
                &context.roster,
                &records,
                250_000,
                96,
                context.delivery_class,
                &crate::schedule::ChurnPlan::default(),
                None,
            );
            assert!(!summary.valid);
            let evidence = invalid_run_diagnostics(
                "run-test",
                &config,
                &context.plans,
                &records,
                &[],
                &summary,
                750_000,
                4,
            );
            assert_eq!(evidence["run_id"], "run-test");
            assert_eq!(evidence["runtime_workers"], 4);
            assert_eq!(evidence["workload"]["lag_bound_us"], 250_000);
            assert_eq!(
                evidence["raw_faults"].as_array().expect("raw faults").len(),
                usize::from(live_fault)
            );
            assert_eq!(
                evidence["summary_reasons"],
                serde_json::to_value(&summary.reasons).expect("reasons")
            );
            let senders = evidence["senders"].as_array().expect("senders");
            assert_eq!(senders.len(), 4, "include empty configured peers");
            for plan in &context.plans {
                let peer = senders
                    .iter()
                    .find(|peer| peer["sender"] == plan.name)
                    .expect("each peer");
                let count = records
                    .sent
                    .iter()
                    .filter(|send| send.sender == plan.name)
                    .count();
                assert_eq!(peer["completed"], count);
                if count == 0 {
                    assert!(
                        peer["first"].is_null()
                            && peer["last"].is_null()
                            && peer["worst"].is_null()
                    );
                } else {
                    assert_eq!(peer["first"]["seq"], 0);
                    assert_eq!(peer["last"]["seq"], 3);
                }
            }
            assert_eq!(senders[0]["sender"], "r0p0");
            if !live_fault {
                assert_eq!(senders[0]["worst"]["lag_us"], 417_628);
            }
            assert_eq!(evidence["sampler"]["count"], 0);
            assert_eq!(evidence["sampler"]["largest_gap_us"], 750_000);
            assert!(evidence["sampler"]["before"].is_null());
            assert!(evidence["sampler"]["after"].is_null());
            assert!(!evidence.to_string().contains("private-secret"));
        }
    }

    #[test]
    fn invalid_run_evidence_records_declared_hooks_and_workload_identity() {
        let context = crate::unit_context();
        let mut config = crate::scenario_config(Encoding::V3Json);
        config.seed = 17;
        config.churn = ChurnSchedule::ReconnectBurst {
            fraction_percent: 50,
            start: Duration::from_micros(123_456),
            window: Duration::from_micros(78_901),
        };
        config.experiment = Some(Experiment::UnsupportedFormat);
        config.stall_senders = Some(Duration::from_millis(800));
        config.pause_reads = Some(Duration::from_micros(111_222));
        config.kill_server_after = Some(Duration::from_micros(333_444));
        config.pause_sends = Some(SendPause {
            after_seq: 7,
            duration: Duration::from_micros(555_666),
        });
        config.slow_reader = true;
        config.latest_keys_per_sender = 23;
        let records = RunRecords::default();
        let summary = oracle::summarize(
            &context.plans,
            &context.roster,
            &records,
            250_000,
            96,
            context.delivery_class,
            &crate::schedule::ChurnPlan::default(),
            None,
        );
        for declared in [true, false] {
            if !declared {
                config.churn = ChurnSchedule::None;
                config.experiment = None;
                config.stall_senders = None;
                config.pause_reads = None;
                config.kill_server_after = None;
                config.pause_sends = None;
                config.slow_reader = false;
            }
            let evidence = invalid_run_diagnostics(
                "run-test",
                &config,
                &context.plans,
                &records,
                &[],
                &summary,
                750_000,
                4,
            );
            let workload = &evidence["workload"];
            assert_eq!(workload["seed"], 17);
            assert_eq!(workload["lag_bound_us"], 250_000);
            assert_eq!(workload["drain_grace_us"], 1_500_000);
            assert_eq!(workload["slow_reader"], declared);
            assert_eq!(workload["latest_keys_per_sender"], 23);
            if declared {
                assert_eq!(
                    workload["churn"],
                    json!({"ReconnectBurst":{
                    "fraction_percent":50, "start":123_456, "window":78_901}})
                );
                assert_eq!(workload["experiment"], "UnsupportedFormat");
                assert_eq!(workload["stall_senders_us"], 800_000);
                assert_eq!(workload["pause_reads_us"], 111_222);
                assert_eq!(workload["kill_server_after_us"], 333_444);
                assert_eq!(
                    workload["pause_sends"],
                    json!({"after_seq":7,"duration_us":555_666})
                );
            } else {
                assert_eq!(workload["churn"], "None");
                for field in [
                    "experiment",
                    "stall_senders_us",
                    "pause_reads_us",
                    "kill_server_after_us",
                    "pause_sends",
                ] {
                    assert!(
                        workload[field].is_null(),
                        "absent {field} must remain explicit"
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_run_evidence_omits_endpoint_text_from_error_context() {
        let context = crate::unit_context();
        let config = crate::scenario_config(Encoding::V3Json);
        let private_error = "connect wss://user:private-secret@example.invalid:1234: failed";
        let mut records = crate::complete_records(&context.plans);
        records.join_failures = vec![private_error.into(); 32];
        records.faults = vec![
            InvalidReason::ReconnectFailed {
                peer: "r0p0".into(),
                detail: private_error.into(),
            },
            InvalidReason::InvalidGapReports {
                count: 1,
                first: crate::oracle::GapViolation {
                    recipient: "r0p1".into(),
                    sender: "r0p0".into(),
                    detail: private_error.into(),
                },
            },
        ];
        let summary = oracle::summarize(
            &context.plans,
            &context.roster,
            &records,
            250_000,
            96,
            context.delivery_class,
            &crate::schedule::ChurnPlan::default(),
            None,
        );
        let sample = serde_json::from_value(json!({"t_us": 100, "counters": {},
            "scrape_error": "https://user:private-secret@example.invalid:1234/metrics/prom: failed"}))
            .expect("private scrape failure");
        let evidence = invalid_run_diagnostics(
            "run-test",
            &config,
            &context.plans,
            &records,
            &[sample],
            &summary,
            200,
            4,
        );
        assert!(!evidence.to_string().contains("private-secret"));
        assert!(!evidence.to_string().contains("example.invalid"));
        assert_eq!(evidence["raw_fault_count"], 2);
        let reconnect = &evidence["raw_faults"][0];
        assert_eq!(reconnect["kind"], "reconnect_failed");
        assert_eq!(reconnect["peer"], "r0p0");
        assert_eq!(reconnect["detail_omitted"], true);
        assert!(reconnect.get("detail").is_none());
        assert_eq!(evidence["raw_faults"][1]["first"]["detail_omitted"], true);
        let joined = evidence["summary_reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .find(|reason| reason["kind"] == "join_failed")
            .expect("join reason");
        assert_eq!(joined["failure_count"], 32);
        assert_eq!(joined["details_omitted"], true);
        assert!(joined.get("failures").is_none());
        assert_eq!(evidence["sampler"]["recent"][0]["scrape_failed"], true);
        assert!(evidence["sampler"]["recent"][0]
            .get("scrape_error")
            .is_none());
        assert_eq!(records.join_failures, vec![private_error.to_string(); 32]);
        assert!(serde_json::to_value(&records.faults)
            .expect("original faults")
            .to_string()
            .contains("private-secret"));
        assert!(serde_json::to_value(&summary.reasons)
            .expect("original reasons")
            .to_string()
            .contains("private-secret"));
    }

    #[test]
    fn invalid_run_evidence_bounds_fault_and_reason_context_without_hiding_counts() {
        let context = crate::unit_context();
        let config = crate::scenario_config(Encoding::V3Json);
        let mut records = crate::complete_records(&context.plans);
        records.faults = (0..64)
            .map(|index| InvalidReason::MalformedServerFrame {
                recipient: format!("r0p{index}"),
                detail: format!("fault-{index}"),
            })
            .collect();
        let summary = oracle::summarize(
            &context.plans,
            &context.roster,
            &records,
            250_000,
            96,
            context.delivery_class,
            &crate::schedule::ChurnPlan::default(),
            None,
        );
        let evidence = invalid_run_diagnostics(
            "run-test",
            &config,
            &context.plans,
            &records,
            &[],
            &summary,
            750_000,
            4,
        );
        assert_eq!(evidence["raw_fault_count"], 64);
        assert_eq!(evidence["summary_reason_count"], summary.reasons.len());
        let expected: Vec<_> = (0..16)
            .map(|index| {
                json!({
                    "kind":"malformed_server_frame", "recipient":format!("r0p{index}"),
                    "detail_omitted":true,
                })
            })
            .collect();
        assert_eq!(evidence["raw_faults"], json!(expected));
        assert_eq!(evidence["summary_reasons"], json!(expected));
        assert_eq!(evidence["raw_faults"].as_array().expect("faults").len(), 16);
        assert_eq!(
            evidence["summary_reasons"]
                .as_array()
                .expect("reasons")
                .len(),
            16
        );
    }

    #[test]
    fn invalid_run_evidence_keeps_sampler_gap_context_and_null_resources() {
        let context = crate::unit_context();
        let config = crate::scenario_config(Encoding::V3Json);
        let records = RunRecords::default();
        let summary = oracle::summarize(
            &context.plans,
            &context.roster,
            &records,
            250_000,
            96,
            context.delivery_class,
            &crate::schedule::ChurnPlan::default(),
            None,
        );
        for (times, finished, gap, start, end, before, after) in [
            (
                vec![600_000, 650_000],
                700_000,
                600_000,
                0,
                600_000,
                None,
                Some(600_000),
            ),
            (
                vec![10, 1000, 1100, 1150],
                1200,
                990,
                10,
                1000,
                Some(10),
                Some(1000),
            ),
            (vec![10, 20], 1000, 980, 20, 1000, Some(20), None),
        ] {
            let samples: Vec<IntervalSample> = times
                .iter()
                .map(|time| {
                    serde_json::from_value(
                        json!({"t_us":time, "counters":{"secret":"private-secret"},
                    "scrape_error":if Some(time) == times.last() { None } else { Some("https://user:private-secret@example.invalid/metrics/prom: failed") }}),
                    )
                    .expect("sample")
                })
                .collect();
            let evidence = invalid_run_diagnostics(
                "run-test",
                &config,
                &context.plans,
                &records,
                &samples,
                &summary,
                finished,
                4,
            );
            assert!(evidence["senders"]
                .as_array()
                .expect("empty peers")
                .iter()
                .all(|peer| peer["completed"] == 0 && peer["first"].is_null()));
            let sampler = &evidence["sampler"];
            assert_eq!(sampler["count"], times.len());
            assert_eq!(sampler["largest_gap_us"], gap);
            assert_eq!(sampler["gap_start_us"], start);
            assert_eq!(sampler["gap_end_us"], end);
            assert_eq!(
                sampler["before"]
                    .get("t_us")
                    .and_then(serde_json::Value::as_u64),
                before
            );
            assert_eq!(
                sampler["after"]
                    .get("t_us")
                    .and_then(serde_json::Value::as_u64),
                after
            );
            let recent = sampler["recent"].as_array().expect("recent");
            assert_eq!(recent.len(), times.len().min(3));
            assert_eq!(recent[0]["t_us"], *times.last().expect("last"));
            for sample in recent {
                assert_eq!(
                    sample["scrape_failed"],
                    sample["t_us"] != *times.last().expect("last")
                );
                assert!(sample.get("scrape_error").is_none());
                for field in [
                    "server_cpu_seconds",
                    "generator_cpu_seconds",
                    "server_rss_bytes",
                    "generator_rss_bytes",
                ] {
                    assert!(sample[field].is_null(), "{field} remains unavailable");
                }
            }
            assert!(!evidence.to_string().contains("private-secret"));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn failed_write_drains_buffered_receipts_before_observing_disconnect() {
        use signal_fish_server::protocol::PlayerId;
        for (terminal, declared_kill) in [
            ("eof", true),
            ("close", true),
            ("error", true),
            ("deadline", true),
            ("eof", false),
            ("error", false),
        ] {
            let player = PlayerId::new_v4();
            let registry = Arc::new(std::sync::Mutex::new(BTreeMap::from([(
                player.to_string(),
                ("r0p1".to_string(), 1),
            )])));
            let log = Arc::new(EventLog::new());
            if declared_kill {
                log.push_fault(InvalidReason::ServerTerminated);
            }
            let expected_fault = if declared_kill {
                InvalidReason::ServerTerminated
            } else {
                InvalidReason::SendFailed {
                    sender: "r0p0".to_string(),
                    detail: "scripted transport error".to_string(),
                }
            };
            let failure =
                failed_transport_write(&log, "r0p0", "scripted transport error".to_string());
            let frames = (0..3)
                .map(|seq| {
                    Ok(Message::Text(
                        serde_json::to_string(&ServerMessage::GameData {
                            from_player: player,
                            data: ledger_application_data("r0p1", seq, 96).expect("ledger"),
                            seq: Some(seq + 1),
                            epoch: Some(1),
                            class: None,
                            key: None,
                        })
                        .expect("frame")
                        .into(),
                    ))
                })
                .collect::<Vec<_>>();
            let tail: std::pin::Pin<
                Box<
                    dyn futures_util::Stream<
                            Item = Result<Message, tokio_tungstenite::tungstenite::Error>,
                        > + Send,
                >,
            > = match terminal {
                "eof" => Box::pin(futures_util::stream::empty()),
                "close" => Box::pin(futures_util::stream::iter([Ok(Message::Close(None))])),
                "error" => Box::pin(futures_util::stream::iter([Err(
                    tokio_tungstenite::tungstenite::Error::ConnectionClosed,
                )])),
                "deadline" => Box::pin(futures_util::stream::pending()),
                _ => unreachable!(),
            };
            let mut reader = futures_util::stream::iter(frames).chain(tail);
            let epoch = Instant::now();
            let until = epoch + Duration::from_secs(1);
            let mut writer = std::pin::pin!(std::future::ready(failure));
            let mut prefer_read = false;
            assert!(matches!(
                next_session_event(
                    writer.as_mut(),
                    &mut reader,
                    until,
                    None,
                    None,
                    false,
                    &mut prefer_read
                )
                .await,
                SessionEvent::Send(SendOutcome::TransportFailed)
            ));
            drain_after_failed_write(
                "r0p0",
                &mut reader,
                &registry,
                epoch,
                until,
                None,
                false,
                &log,
                ExperimentContext {
                    active: false,
                    opaque_sender: false,
                },
            )
            .await;
            let records = log.take_records();
            assert_eq!(
                records
                    .receipts
                    .iter()
                    .map(|receipt| receipt.seq)
                    .collect::<Vec<_>>(),
                vec![0, 1, 2],
                "{terminal}: preserve queued receipts"
            );
            if terminal == "deadline" {
                assert!(
                    records.disconnects.is_empty(),
                    "deadline is not an observed disconnect"
                );
                assert!(records
                    .faults
                    .iter()
                    .any(|reason| matches!(reason, InvalidReason::RunnerDeadlineExceeded { .. })));
            } else {
                assert_eq!(
                    records.disconnects.len(),
                    1,
                    "{terminal}: observe terminal after receipts"
                );
                assert_eq!(records.disconnects[0].recipient, "r0p0");
                assert_eq!(records.faults, vec![expected_fault]);
                if declared_kill && terminal == "eof" {
                    let context = crate::unit_context();
                    let mut evidence = crate::complete_records(&context.plans);
                    evidence
                        .receipts
                        .retain(|receipt| receipt.recipient != "r0p0" || receipt.sender != "r0p1");
                    evidence.receipts.extend(records.receipts.clone());
                    evidence.disconnects = records.disconnects.clone();
                    evidence.faults = records.faults.clone();
                    let summarize = |evidence: &crate::records::RunRecords| {
                        oracle::summarize(
                            &context.plans,
                            &context.roster,
                            evidence,
                            500_000,
                            96,
                            context.delivery_class,
                            &crate::schedule::ChurnPlan::default(),
                            None,
                        )
                    };
                    let prefix = summarize(&evidence);
                    assert_eq!(prefix.reasons, vec![InvalidReason::ServerTerminated]);
                    let recipient = prefix
                        .per_recipient
                        .iter()
                        .find(|entry| entry.recipient == "r0p0")
                        .expect("recipient");
                    assert!(!recipient.connected_through);
                    assert_eq!(recipient.missing, 0);
                    assert_eq!(recipient.undelivered_at_disconnect, 1);
                    evidence.receipts.retain(|receipt| {
                        !(receipt.recipient == "r0p0"
                            && receipt.sender == "r0p1"
                            && receipt.seq == 1)
                    });
                    let holed = summarize(&evidence);
                    assert!(
                        holed.reasons.contains(&InvalidReason::MissingDeliveries {
                            count: 1,
                            first: crate::oracle::DeliveryKey {
                                recipient: "r0p0".to_string(),
                                sender: "r0p1".to_string(),
                                epoch: 1,
                                seq: 2
                            },
                        }),
                        "an interior hole remains invalid after a transport disconnect: {:?}",
                        holed.reasons
                    );
                    let decoded = serde_json::from_value(
                        serde_json::to_value(&evidence).expect("serialized evidence"),
                    )
                    .expect("decoded evidence");
                    assert_eq!(
                        serde_json::to_value(summarize(&decoded)).expect("decoded verdict"),
                        serde_json::to_value(&holed).expect("original verdict")
                    );
                }
            }
        }
    }

    // Poll socket I/O without letting paused Tokio time jump to quiescence.
    async fn poll_without_clock_advance<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let std::task::Poll::Ready(output) = futures_util::poll!(future.as_mut()) {
                return output;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "socket I/O did not progress"
            );
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn stopped_generator_preserves_held_and_later_receipts_until_session_end() {
        use signal_fish_server::protocol::{
            DeliveryGap, DeliveryGapReason, DeliveryReportPayload, ErrorCode, PlayerId,
        };
        // The preparation failure and saturation take separate writer branches.
        // The socket must remain open for ordinary quiescence and real close.
        for (saturated, close, churn) in [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, false),
            (false, false, true),
            (true, false, true),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind mock server");
            let address = listener.local_addr().expect("mock address");
            let (client, server) = tokio::join!(
                tokio_tungstenite::connect_async(format!("ws://{address}")),
                async {
                    let (socket, _) = listener.accept().await.expect("accept mock peer");
                    tokio_tungstenite::accept_async(socket)
                        .await
                        .expect("mock handshake")
                }
            );
            let (sink, reader) = client.expect("client handshake").0.split();
            let mut server = server;
            let player = PlayerId::new_v4();
            let frame = |seq| {
                Message::Text(
                    serde_json::to_string(&ServerMessage::GameData {
                        from_player: player,
                        data: ledger_application_data("r0p1", seq, 96).expect("ledger"),
                        seq: Some(seq + 1),
                        epoch: Some(1),
                        class: None,
                        key: None,
                    })
                    .expect("inbound frame")
                    .into(),
                )
            };
            server.send(frame(0)).await.expect("queue initial inbound");
            let registry = Arc::new(std::sync::Mutex::new(BTreeMap::from([(
                player.to_string(),
                ("r0p1".to_string(), 1),
            )])));
            let log = Arc::new(EventLog::new());
            let plan = crate::unit_context().plans.remove(0);
            let first_send_us = plan.sends.get(0).expect("scheduled send").intended_us;
            tokio::time::pause();
            let epoch = Instant::now();
            let (start, receiver) = tokio::sync::watch::channel(Some(epoch));
            let (ready, readiness) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(peer_task(
                plan.name.clone(),
                plan,
                PeerFacts {
                    ws_url: format!("ws://{address}"),
                    encoding: Encoding::V2Json,
                    players_per_room: 4,
                    payload_bytes: if saturated { 96 } else { 0 },
                    delivery_class: DeliveryClass::Reliable,
                    latest_keys_per_sender: 1,
                    generator_lag_bound_us: if saturated { 0 } else { 1_000_000 },
                    pause_sends: None,
                    stall_senders: None,
                    experiment: ExperimentContext {
                        active: false,
                        opaque_sender: false,
                    },
                    game_data_format: None,
                },
                if churn {
                    vec![PeerChurnCycle {
                        disconnect_us: 2_500_000,
                        reconnect_us: 3_000_000,
                        rejoin_room_code: "STOP01".to_string(),
                    }]
                } else {
                    Vec::new()
                },
                receiver,
                ready,
                Arc::clone(&registry),
                (
                    sink,
                    reader,
                    PlayerId::new_v4().to_string(),
                    BTreeMap::new(),
                ),
                Arc::clone(&log),
                4_000_000,
                Some(Duration::from_secs(2)),
                false,
            ));
            poll_without_clock_advance(readiness)
                .await
                .expect("peer ready");
            // Advance past timer granularity; any lateness trips the zero lag bound.
            tokio::time::advance(Duration::from_micros(first_send_us + 2_000)).await;
            poll_without_clock_advance(futures_util::future::poll_fn(|_| {
                if log.observation_counts().1 == 0 {
                    std::task::Poll::Pending
                } else {
                    std::task::Poll::Ready(())
                }
            }))
            .await;
            assert!(
                !peer.is_finished(),
                "stopping outbound must preserve the session"
            );
            assert!(
                log.observation_counts().0 == 0,
                "the read pause remains active"
            );
            poll_without_clock_advance(server.send(frame(1)))
                .await
                .expect("send after stop");
            for message in [
                ServerMessage::DeliveryReport(Box::new(DeliveryReportPayload {
                    gaps: vec![DeliveryGap {
                        from_player: player,
                        epoch: 1,
                        from_seq: 4,
                        to_seq: 4,
                        reason: DeliveryGapReason::VolatileDropped,
                    }],
                    ..Default::default()
                })),
                ServerMessage::Error {
                    message: "scripted rejection".into(),
                    error_code: Some(ErrorCode::RateLimitExceeded),
                },
            ] {
                poll_without_clock_advance(
                    server.send(Message::Text(
                        serde_json::to_string(&message)
                            .expect("evidence frame")
                            .into(),
                    )),
                )
                .await
                .expect("queue evidence after stop");
            }
            tokio::time::advance(epoch + Duration::from_millis(2_002) - Instant::now()).await;
            poll_without_clock_advance(futures_util::future::poll_fn(|_| {
                if log.observation_counts().0 == 2 {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }))
            .await;
            poll_without_clock_advance(server.send(frame(2)))
                .await
                .expect("send after resume");
            poll_without_clock_advance(futures_util::future::poll_fn(|_| {
                if log.observation_counts().0 == 3 {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }))
            .await;
            assert!(
                !peer.is_finished(),
                "reading receipts must preserve the socket"
            );
            assert!(
                futures_util::poll!(server.next()).is_pending(),
                "outbound work stays stopped"
            );
            if churn {
                tokio::time::advance(Duration::from_millis(500)).await;
                let terminal = poll_without_clock_advance(server.next()).await;
                assert!(
                    matches!(terminal, None | Some(Err(_))),
                    "churn drops the old socket"
                );
                assert!(!peer.is_finished(), "churn must retain the observer task");
                tokio::time::advance(Duration::from_millis(500)).await;
                let mut rejoined = poll_without_clock_advance(async {
                    let (socket, _) = listener.accept().await.expect("accept rejoined peer");
                    tokio_tungstenite::accept_async(socket)
                        .await
                        .expect("rejoin handshake")
                })
                .await;
                let join = poll_without_clock_advance(rejoined.next())
                    .await
                    .expect("rejoin frame")
                    .expect("rejoin transport");
                let Message::Text(join) = join else {
                    panic!("expected JSON join");
                };
                assert!(
                    matches!(serde_json::from_str::<ClientMessage>(&join).expect("join message"),
                    ClientMessage::JoinRoom { room_code: Some(code), .. } if code == "STOP01")
                );
                let rejoined_id = PlayerId::new_v4();
                let joined = ServerMessage::RoomJoined(Box::new(
                    signal_fish_server::protocol::RoomJoinedPayload {
                        room_id: PlayerId::new_v4(),
                        room_code: "STOP01".to_string(),
                        player_id: rejoined_id,
                        game_name: GAME_NAME.to_string(),
                        max_players: 4,
                        supports_authority: false,
                        current_players: vec![signal_fish_server::protocol::PlayerInfo {
                            id: player,
                            name: "r0p1".to_string(),
                            is_authority: false,
                            is_ready: false,
                            connected_at: None,
                            connection_info: None,
                            epoch: Some(1),
                            seq: Some(3),
                            region_id: String::new(),
                        }],
                        is_authority: false,
                        lobby_state: signal_fish_server::protocol::LobbyState::Lobby,
                        ready_players: Vec::new(),
                        relay_type: "WebSocket".to_string(),
                        current_spectators: Vec::new(),
                        ice_servers: Vec::new(),
                        reconnection_token: None,
                    },
                ));
                poll_without_clock_advance(
                    rejoined.send(Message::Text(
                        serde_json::to_string(&joined)
                            .expect("join snapshot")
                            .into(),
                    )),
                )
                .await
                .expect("send rejoin snapshot");
                poll_without_clock_advance(rejoined.send(frame(4)))
                    .await
                    .expect("send after rejoin");
                poll_without_clock_advance(futures_util::future::poll_fn(|_| {
                    if log.observation_counts().0 == 4 {
                        std::task::Poll::Ready(())
                    } else {
                        std::task::Poll::Pending
                    }
                }))
                .await;
                assert!(!peer.is_finished(), "new incarnation remains an observer");
                assert!(
                    futures_util::poll!(rejoined.next()).is_pending(),
                    "rejoin must not restart outbound work"
                );
                assert_eq!(log.observation_counts().1, 2, "stop state survives rejoin");
                // Keep the new server half alive through ordinary quiescence.
                // Both server values have the same concrete TCP WebSocket type.
                server = rejoined;
            }

            if close {
                poll_without_clock_advance(server.send(Message::Close(None)))
                    .await
                    .expect("close peer");
            } else {
                tokio::time::advance(epoch + Duration::from_millis(4_002) - Instant::now()).await;
            }
            poll_without_clock_advance(peer)
                .await
                .expect("peer task completes");
            let records = log.take_records();
            assert_eq!(
                records
                    .receipts
                    .iter()
                    .map(|receipt| receipt.seq)
                    .collect::<Vec<_>>(),
                if churn {
                    vec![0, 1, 2, 4]
                } else {
                    vec![0, 1, 2]
                }
            );
            assert!(
                records.sent.is_empty(),
                "the failed send and its remainder are unsent"
            );
            assert_eq!(records.disconnects.len(), usize::from(close));
            assert_eq!(
                records.faults.len(),
                2,
                "ordinary quiescence adds no deadline fault"
            );
            assert!(matches!(
                (&records.faults[0], saturated),
                (InvalidReason::GeneratorSaturated { .. }, true)
                    | (InvalidReason::SendFailed { .. }, false)
            ));
            if saturated {
                assert_eq!(
                    records.faults[0],
                    InvalidReason::GeneratorSaturated {
                        max_lag_us: 2_000,
                        bound_us: 0
                    }
                );
            }
            assert!(matches!(
                &records.faults[1],
                InvalidReason::ServerRejected { .. }
            ));
            assert_eq!(records.gaps.len(), 1);
            assert_eq!((records.gaps[0].from_seq, records.gaps[0].to_seq), (4, 4));
            assert_eq!(records.gaps[0].reason, DeliveryGapReason::VolatileDropped);
            assert!(records.receipts[..3]
                .iter()
                .all(|receipt| receipt.received_us == 2_002_000));
            if churn {
                assert_eq!(records.churn.len(), 2);
                assert_eq!(records.churn[0].phase, ChurnPhase::Disconnect);
                assert_eq!(records.churn[1].phase, ChurnPhase::Rejoined);
                assert_eq!(records.churn[1].epoch, Some(2));
                assert_eq!(
                    records.churn[1].tails.get("r0p1"),
                    Some(&(player.to_string(), 3))
                );
                assert!(registry
                    .lock()
                    .expect("registry")
                    .values()
                    .any(|identity| identity == &("r0p0".to_string(), 2)));
                assert_eq!(records.receipts[3].received_us, 3_002_000);
            }
            let output = tempfile::tempdir().expect("event artifacts");
            artifacts::write_deliveries(output.path(), &records).expect("write retained evidence");
            let replayed = artifacts::read_records(output.path()).expect("read retained evidence");
            assert_eq!(
                serde_json::to_value(&replayed).expect("replayed records"),
                serde_json::to_value(&records).expect("records")
            );
            drop(start);
            tokio::time::resume();
        }
    }

    struct DropCount(Arc<AtomicUsize>);
    impl Drop for DropCount {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct PendingReader(DropCount);
    impl futures_util::Stream for PendingReader {
        type Item = Result<Message, tokio_tungstenite::tungstenite::Error>;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            let _ = &self.0;
            std::task::Poll::Pending
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pending_write_does_not_block_ready_inbound() {
        let mut writer = std::pin::pin!(std::future::pending::<SendOutcome>());
        let mut reader = futures_util::stream::iter([Ok(Message::Ping(vec![1].into()))]);
        let event = tokio::time::timeout(
            Duration::from_millis(50),
            next_session_event(
                writer.as_mut(),
                &mut reader,
                Instant::now() + Duration::from_secs(1),
                None,
                None,
                false,
                &mut true,
            ),
        )
        .await
        .expect("ready inbound must progress while the write remains pending");
        assert!(matches!(
            event,
            SessionEvent::Frame(Some(Ok(Message::Ping(_))))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn inbound_recording_keeps_one_pending_write_until_one_completion() {
        use signal_fish_server::protocol::PlayerId;
        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicUsize::new(0));
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let mut writer = Box::pin({
            let started = Arc::clone(&started);
            let completed = Arc::clone(&completed);
            let guard = DropCount(Arc::clone(&dropped));
            async move {
                let _guard = guard;
                started.fetch_add(1, Ordering::SeqCst);
                wait.await.expect("release the pending write");
                completed.fetch_add(1, Ordering::SeqCst);
                SendOutcome::Sent
            }
        });
        assert!(futures_util::poll!(writer.as_mut()).is_pending());
        let player = PlayerId::new_v4();
        let registry = Arc::new(std::sync::Mutex::new(BTreeMap::from([(
            player.to_string(),
            ("r0p0".to_string(), 1),
        )])));
        let log = Arc::new(EventLog::new());
        let epoch = Instant::now();
        let frames = (0..2).map(|seq| {
            Ok(Message::Text(
                serde_json::to_string(&ServerMessage::GameData {
                    from_player: player,
                    data: ledger_application_data("r0p0", seq, 96).expect("ledger"),
                    seq: Some(seq + 1),
                    epoch: Some(1),
                    class: None,
                    key: None,
                })
                .expect("inbound frame")
                .into(),
            ))
        });
        let mut reader = futures_util::stream::iter(frames).chain(futures_util::stream::pending());
        let until = epoch + Duration::from_secs(1);
        let mut prefer_read = true;
        for _ in 0..2 {
            let event = next_session_event(
                writer.as_mut(),
                &mut reader,
                until,
                None,
                None,
                false,
                &mut prefer_read,
            )
            .await;
            let SessionEvent::Frame(Some(frame)) = event else {
                panic!("inbound must progress: {event:?}")
            };
            assert!(
                handle_inbound(
                    "r0p1",
                    frame,
                    &registry,
                    epoch,
                    &log,
                    ExperimentContext {
                        active: false,
                        opaque_sender: false
                    }
                )
                .await
            );
        }
        assert_eq!(log.take_records().receipts.len(), 2);
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(completed.load(Ordering::SeqCst), 0);
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        release.send(()).expect("writer remains alive");
        assert!(matches!(
            next_session_event(
                writer.as_mut(),
                &mut reader,
                until,
                None,
                None,
                false,
                &mut prefer_read
            )
            .await,
            SessionEvent::Send(SendOutcome::Sent)
        ));
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn overdue_writes_and_continuous_inbound_alternate_without_starvation() {
        let mut reader = futures_util::stream::repeat_with(|| Ok(Message::Ping(vec![1].into())));
        let mut prefer_read = true;
        let until = Instant::now() + Duration::from_secs(1);
        for _ in 0..4 {
            let mut writer = std::pin::pin!(std::future::ready(SendOutcome::Sent));
            assert!(matches!(
                next_session_event(
                    writer.as_mut(),
                    &mut reader,
                    until,
                    None,
                    None,
                    false,
                    &mut prefer_read
                )
                .await,
                SessionEvent::Frame(Some(Ok(_)))
            ));
            assert!(matches!(
                next_session_event(
                    writer.as_mut(),
                    &mut reader,
                    until,
                    None,
                    None,
                    false,
                    &mut prefer_read
                )
                .await,
                SessionEvent::Send(SendOutcome::Sent)
            ));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn read_pause_resumes_with_a_pending_write_and_never_read_stays_held() {
        let epoch = Instant::now();
        let until = epoch + Duration::from_secs(2);
        let resume = epoch + Duration::from_millis(600);
        let mut writer = std::pin::pin!(std::future::pending::<SendOutcome>());
        let mut reader = futures_util::stream::repeat_with(|| Ok(Message::Ping(Vec::new().into())));
        let mut prefer_read = true;
        assert!(matches!(
            next_session_event(
                writer.as_mut(),
                &mut reader,
                until,
                None,
                Some(resume),
                false,
                &mut prefer_read
            )
            .await,
            SessionEvent::Resume
        ));
        assert_eq!(Instant::now(), resume);
        assert!(matches!(
            next_session_event(
                writer.as_mut(),
                &mut reader,
                until,
                None,
                Some(resume),
                false,
                &mut prefer_read
            )
            .await,
            SessionEvent::Frame(Some(Ok(_)))
        ));
        assert!(matches!(
            next_session_event(
                writer.as_mut(),
                &mut reader,
                until,
                None,
                Some(resume),
                true,
                &mut prefer_read
            )
            .await,
            SessionEvent::Quiescence
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn lifecycle_deadlines_cancel_pending_resources_before_the_next_incarnation() {
        for churn in [false, true] {
            let dropped = Arc::new(AtomicUsize::new(0));
            let guard = DropCount(Arc::clone(&dropped));
            let mut writer = Box::pin(async move {
                let _guard = guard;
                std::future::pending::<SendOutcome>().await
            });
            let mut reader = PendingReader(DropCount(Arc::clone(&dropped)));
            let now = Instant::now();
            let event = next_session_event(
                writer.as_mut(),
                &mut reader,
                if churn {
                    now + Duration::from_secs(1)
                } else {
                    now
                },
                churn.then_some(now),
                None,
                false,
                &mut true,
            )
            .await;
            assert!(matches!(
                (churn, event),
                (true, SessionEvent::Churn) | (false, SessionEvent::Quiescence)
            ));
            drop(writer);
            drop(reader);
            assert_eq!(
                dropped.load(Ordering::SeqCst),
                2,
                "both halves must drop before rejoin"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hard_deadline_and_auxiliary_cleanup_await_task_cancellation() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let guard = DropCount(Arc::clone(&dropped));
            let (started, ready) = tokio::sync::oneshot::channel();
            handles.push(tokio::spawn(async move {
                let _guard = guard;
                started.send(()).expect("observe task start");
                std::future::pending::<()>().await;
            }));
            ready.await.expect("task owns its resources");
        }
        handles.push(tokio::spawn(async {}));
        assert!(
            !await_peers(handles, Instant::now() + Duration::from_millis(10))
                .await
                .expect("deadline cancels pending tasks")
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        let guard = DropCount(Arc::clone(&dropped));
        let task = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        abort_and_join(vec![task])
            .await
            .expect("owned cancellation is expected");
        assert_eq!(dropped.load(Ordering::SeqCst), 3);
        assert!(await_peers(
            vec![tokio::spawn(async {})],
            Instant::now() + Duration::from_secs(1)
        )
        .await
        .expect("completed peer succeeds"));
    }

    #[tokio::test(start_paused = true)]
    async fn peer_panics_and_unexpected_cancellation_refuse_success_and_clean_up() {
        for panic_task in [false, true] {
            let dropped = Arc::new(AtomicUsize::new(0));
            let guard = DropCount(Arc::clone(&dropped));
            let blocked = tokio::spawn(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            });
            let failed = tokio::spawn(async move {
                if panic_task {
                    panic!("injected peer panic");
                }
                std::future::pending::<()>().await;
            });
            if !panic_task {
                failed.abort();
            }
            let error = await_peers(
                vec![blocked, failed],
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect_err("unexpected task termination cannot pass");
            assert!(error.contains("capacity peer task failed"), "{error}");
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn auxiliary_panics_refuse_artifact_capture_after_all_tasks_stop() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let guard = DropCount(Arc::clone(&dropped));
        let blocked = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        });
        let (started, ready) = tokio::sync::oneshot::channel();
        let failed = tokio::spawn(async move {
            started.send(()).expect("observe auxiliary start");
            panic!("injected sampler panic");
        });
        ready.await.expect("auxiliary ran before cleanup");
        let error = abort_and_join(vec![failed, blocked])
            .await
            .expect_err("auxiliary panic refuses capture");
        assert!(
            error.contains("capacity task failed during cleanup"),
            "{error}"
        );
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn arm_waits_for_preparation_and_post_arm_delay_still_invalidates() {
        let before_setup = Instant::now();
        let (mut start, mut receiver) = tokio::sync::watch::channel(None);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
        let mut peers = vec![tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            let epoch = ready_for_epoch(ready_tx, &mut receiver)
                .await
                .expect("run arms");
            observed_tx.send(epoch).expect("observe epoch");
        })];
        let epoch = arm_tasks(
            vec![ready_rx],
            &mut start,
            before_setup + Duration::from_secs(2),
            &mut peers,
            &mut Vec::new(),
        )
        .await
        .expect("prepared tasks arm");
        assert_eq!(
            epoch.duration_since(before_setup),
            Duration::from_millis(600)
        );
        assert_eq!(
            micros(epoch.elapsed()),
            0,
            "setup must not consume generator lag"
        );
        assert_eq!(observed_rx.await.expect("peer observed epoch"), epoch);
        let context = crate::unit_context();
        let mut records = crate::complete_records(&context.plans);
        let summary = oracle::summarize(
            &context.plans,
            &context.roster,
            &records,
            500_000,
            96,
            context.delivery_class,
            &crate::schedule::ChurnPlan::default(),
            None,
        );
        assert!(
            summary.valid,
            "ready generator control must pass: {:?}",
            summary.reasons
        );
        tokio::time::sleep(Duration::from_millis(600)).await;
        let measured_delay = micros(epoch.elapsed());
        for sent in &mut records.sent {
            sent.sent_us += measured_delay;
        }
        for receipt in &mut records.receipts {
            receipt.received_us += measured_delay;
        }
        let delayed = oracle::summarize(
            &context.plans,
            &context.roster,
            &records,
            500_000,
            96,
            context.delivery_class,
            &crate::schedule::ChurnPlan::default(),
            None,
        );
        assert!(!delayed.valid);
        assert!(delayed.reasons.iter().any(|reason| matches!(reason, InvalidReason::GeneratorSaturated { max_lag_us, bound_us: 500_000 } if *max_lag_us >= measured_delay)));
        assert!(await_peers(peers, Instant::now() + Duration::from_secs(1))
            .await
            .expect("prepared peer completes"));
    }

    #[tokio::test(start_paused = true)]
    async fn all_prepared_tasks_receive_one_immutable_epoch() {
        let (mut start, receiver) = tokio::sync::watch::channel(None);
        let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut readiness = Vec::new();
        let mut peers = Vec::new();
        for delay in [0, 200, 600] {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            readiness.push(ready_rx);
            let mut receiver = receiver.clone();
            let observed = observed_tx.clone();
            peers.push(tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                observed
                    .send(
                        ready_for_epoch(ready_tx, &mut receiver)
                            .await
                            .expect("all peers arm"),
                    )
                    .expect("observe shared epoch");
            }));
        }
        drop(receiver);
        drop(observed_tx);
        let epoch = arm_tasks(
            readiness,
            &mut start,
            Instant::now() + Duration::from_secs(2),
            &mut peers,
            &mut Vec::new(),
        )
        .await
        .expect("all tasks prepared");
        for _ in 0..3 {
            assert_eq!(observed_rx.recv().await.expect("peer epoch"), epoch);
        }
        assert!(await_peers(peers, Instant::now() + Duration::from_secs(1))
            .await
            .expect("all peers finish"));
        let error = arm_tasks(
            Vec::new(),
            &mut start,
            Instant::now() + Duration::from_secs(1),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .await
        .expect_err("epoch cannot be reset");
        assert!(error.contains("already armed"), "{error}");
        assert_eq!(*start.borrow(), Some(epoch));
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_failure_and_timeout_cancel_all_preparation_tasks() {
        for timeout in [false, true] {
            let dropped = Arc::new(AtomicUsize::new(0));
            let (mut start, _receiver) = tokio::sync::watch::channel(None);
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let guard = DropCount(Arc::clone(&dropped));
            let mut peers = vec![tokio::spawn(async move {
                let _guard = guard;
                let _ready = if timeout {
                    Some(ready_tx)
                } else {
                    drop(ready_tx);
                    None
                };
                std::future::pending::<()>().await;
            })];
            let guard = DropCount(Arc::clone(&dropped));
            let mut auxiliaries = vec![tokio::spawn(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            })];
            let error = arm_tasks(
                vec![ready_rx],
                &mut start,
                Instant::now() + Duration::from_secs(1),
                &mut peers,
                &mut auxiliaries,
            )
            .await
            .expect_err("incomplete preparation refuses arming");
            assert!(
                error.contains(if timeout {
                    "readiness deadline"
                } else {
                    "before readiness"
                }),
                "{error}"
            );
            assert_eq!(*start.borrow(), None);
            assert!(peers.is_empty() && auxiliaries.is_empty());
            assert_eq!(
                dropped.load(Ordering::SeqCst),
                2,
                "cleanup must complete before returning"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn preparation_panic_is_loud_and_cleans_auxiliary_tasks() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let (mut start, _receiver) = tokio::sync::watch::channel(None);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let mut peers = vec![tokio::spawn(async move {
            let _ready = ready_tx;
            panic!("injected preparation panic");
        })];
        let guard = DropCount(Arc::clone(&dropped));
        let mut auxiliaries = vec![tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        })];
        let error = arm_tasks(
            vec![ready_rx],
            &mut start,
            Instant::now() + Duration::from_secs(1),
            &mut peers,
            &mut auxiliaries,
        )
        .await
        .expect_err("preparation panic refuses arming");
        assert!(error.contains("injected preparation panic"), "{error}");
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
        assert_eq!(*start.borrow(), None);
        assert!(peers.is_empty() && auxiliaries.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn closed_start_channel_cannot_supply_a_fallback_epoch() {
        let (start, mut receiver) = tokio::sync::watch::channel(None);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        drop(start);
        let error = ready_for_epoch(ready_tx, &mut receiver)
            .await
            .expect_err("missing epoch is a preparation failure");
        assert!(error.contains("epoch channel closed"), "{error}");
        ready_rx.await.expect("readiness was reported");
    }
}
