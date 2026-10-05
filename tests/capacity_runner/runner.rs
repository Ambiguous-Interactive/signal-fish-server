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
use signal_fish_server::protocol::{ClientMessage, ServerMessage, Topology, Transport};
// The runner's config delivery class and the wire enum share the token set;
// the alias keeps the mapping at the one boundary where they meet.
use signal_fish_server::protocol::DeliveryClass as WireDeliveryClass;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::artifacts::{self, BuildIdentity, IntervalSample, ServerIdentity, WorkloadShape};
use crate::config::{
    count_u64, count_usize, default_latest_keys, micros, ChurnSchedule, DeliveryClass, Encoding,
    RunConfig, SendPause,
};
use crate::diagnostics;
use crate::oracle::{self, InvalidReason, OutcomeSummary};
use crate::records::{
    ChurnEvent, ChurnPhase, DisconnectEvent, DisconnectObservation, EventLog, GapEvent,
    ReceiptEvent, SentEvent,
};
use crate::schedule::{build_run_shape, Phase, SenderPlan};
use crate::websocket_test_helpers;
use crate::websocket_test_helpers::server_process::{spawn_server, ServerProcess};
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
type SenderRegistry = Arc<std::sync::Mutex<BTreeMap<String, (String, u32)>>>;

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
            async move {
                connect_and_join(
                    ws_url,
                    config.encoding,
                    config.room_code(plan.room),
                    config.players_per_room,
                    plan,
                    designated,
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
            binary_sha256: None,
            binary_bytes: None,
            config_overlay_sha256: overlay_sha256(&config)?,
        },
        None => {
            let (binary_sha256, binary_bytes) =
                diagnostics::sha256_file(env!("CARGO_BIN_EXE_signal-fish-server"))?;
            ServerIdentity {
                endpoint: endpoint_base.clone(),
                pid: server_slot.lock().await.as_ref().map(|server| server.pid()),
                binary_sha256: Some(binary_sha256),
                binary_bytes: Some(binary_bytes),
                config_overlay_sha256: overlay_sha256(&config)?,
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

    // Measurement epoch: all intended/send/receive timestamps are micros
    // since this instant on the monotonic clock. Set only after every piece
    // of setup work, so the first intended send is measured from a cold
    // generator.
    let epoch = Instant::now();

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
    let quiescence = epoch + Duration::from_micros(quiescence_us);
    // Belt over the schedule plus hooks: senders self-terminate well inside
    // this; a lapse means the generator itself wedged.
    let hard_deadline = epoch + Duration::from_micros(quiescence_us + hook_extra_us + 5_000_000);

    // Declared slow-reader hook: the designated peer stops reading after its
    // join; the fault is declared at arm time so a replay sees it.
    let slow_reader_name = config.slow_reader.then(|| RunConfig::peer_name(0, 0));
    if let Some(name) = &slow_reader_name {
        log.push_fault(InvalidReason::SlowConsumerDisconnect {
            recipients: vec![name.clone()],
        });
    }

    // Sampler: periodic server and generator resource diagnostics until
    // quiescence (plus hook time, so late hook effects stay sampled).
    let samples: Arc<std::sync::Mutex<Vec<IntervalSample>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let samples = Arc::clone(&samples);
        let pid = server_slot.lock().await.as_ref().map(ServerProcess::pid);
        tokio::spawn(sample_loop(
            epoch,
            epoch + Duration::from_micros(quiescence_us + hook_extra_us),
            config.sample_interval,
            metrics_url(&endpoint_base),
            pid,
            (config.delivery_class != DeliveryClass::Reliable).then_some(config.delivery_class),
            samples,
        ));
    }

    // Kill hook: declared server termination.
    if config.kill_server_after.is_some() {
        let slot = Arc::clone(&server_slot);
        let log = Arc::clone(&log);
        let at = epoch + config.kill_server_after.expect("checked is_some above");
        tokio::spawn(async move {
            tokio::time::sleep_until(at).await;
            // Declare the fault BEFORE acting on it: sender tasks that
            // observe the socket die must never race the declaration into a
            // spurious SendFailed reason.
            log.push_fault(InvalidReason::ServerTerminated);
            let process = slot.lock().await.take();
            if let Some(mut process) = process {
                process.kill_and_wait().await;
            }
        });
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
        let hold_reads_until = paused_reader
            .as_deref()
            .filter(|designated| *designated == name.as_str())
            .map(|_| epoch + config.pause_reads.expect("paused reader carries its pause"));
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
            },
            churn_cycles,
            epoch,
            Arc::clone(&registry),
            (sink, rx, player_id, tails),
            Arc::clone(&log),
            quiescence,
            hold_reads_until,
            is_slow_reader,
        )));
    }

    // Await the generator and receivers under the hard deadline.
    if tokio::time::timeout_at(hard_deadline, futures_util::future::join_all(handles))
        .await
        .is_err()
    {
        log.push_fault(InvalidReason::RunnerDeadlineExceeded {
            detail: "generator or receiver tasks outlived the quiescence margin".to_string(),
        });
    }

    // The registry is complete only after every peer task is done: record
    // it once, before the snapshot the artifacts write.
    log.set_registry(registry.lock().expect("sender registry poisoned").clone());
    let records = log.snapshot();
    let bound_us = micros(config.generator_lag_bound);
    let summary = oracle::summarize(
        &plans,
        &roster,
        &records,
        bound_us,
        config.delivery_class,
        &churn_plan,
    );

    artifacts::write_deliveries(&config.output_dir, &records)?;
    artifacts::write_intervals(
        &config.output_dir,
        &samples.lock().expect("interval samples"),
    )?;
    artifacts::write_histogram(&config.output_dir, &oracle::latency_samples(&records))?;
    artifacts::write_json(config.output_dir.join(artifacts::SUMMARY_FILE), &summary)?;

    let final_counters = samples
        .lock()
        .expect("interval samples poisoned")
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

/// SHA-256 of the server config overlay bytes (the overlay itself is part of
/// the manifest's recorded config, hashed so the effective server posture of
/// a run is pinned).
fn overlay_sha256(config: &RunConfig) -> Result<String, String> {
    let bytes = serde_json::to_vec(&config.server_overlay)
        .map_err(|error| format!("serialize server overlay: {error}"))?;
    Ok(diagnostics::sha256_bytes(&bytes))
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
        authenticate_v3(&mut sink, &mut rx).await?;
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
async fn authenticate_v3(sink: &mut WsSink, rx: &mut WsReceiver) -> Result<(), String> {
    let authenticate = ClientMessage::Authenticate {
        app_id: GAME_NAME.to_string(),
        connect_token: None,
        sdk_version: None,
        platform: Some("capacity-runner".to_string()),
        game_data_format: None,
        protocol_version: Some(3),
        supported_transports: Some(vec![Transport::Relay]),
        supported_topologies: Some(vec![Topology::Relay]),
        requested_capabilities: None,
    };
    send_client(sink, &authenticate).await?;
    let mut authenticated = false;
    let mut negotiated = false;
    let deadline = tokio::time::Instant::now() + STEP_TIMEOUT;
    while !(authenticated && negotiated) {
        let frame = match tokio::time::timeout_at(deadline, rx.next()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => return Err("connection closed during v3 authentication".to_string()),
            Err(_) => return Err("v3 authentication timed out".to_string()),
        };
        let message = decode_server_frame(frame)?;
        match message {
            ServerMessage::Authenticated { .. } => authenticated = true,
            ServerMessage::ProtocolInfo(_) => negotiated = true,
            ServerMessage::AuthenticationError { error, .. } => {
                return Err(format!("v3 authentication refused: {error}"))
            }
            _ => {}
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
    epoch: Instant,
    registry: SenderRegistry,
    initial: (WsSink, WsReceiver, String, BTreeMap<String, (String, u64)>),
    log: Arc<EventLog>,
    until: Instant,
    hold_reads_until: Option<Instant>,
    never_read: bool,
) {
    let padding: Arc<str> = "x".repeat(count_usize(facts.payload_bytes)).into();
    let first_measured = plan
        .sends
        .iter()
        .find(|send| send.phase == Phase::Measured)
        .map(|send| send.seq);
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

        loop {
            let next_send = plan.sends.get(send_cursor);
            let next_churn = churn_instants.get(churn_cursor);
            let reads_held = hold_reads_until.is_some_and(|at| tokio::time::Instant::now() < at);
            tokio::select! {
                biased;

                // Declared churn disconnect: close the socket, record the
                // action, and let the session loop sleep out the window.
                _ = tokio::time::sleep_until(
                    next_churn.map_or(until, |cycle| {
                        epoch + Duration::from_micros(cycle.disconnect_us)
                    }),
                ), if next_churn.is_some() => {
                    log.push_churn(ChurnEvent {
                        recipient: recipient.clone(),
                        phase: ChurnPhase::Disconnect,
                        at_us: micros(epoch.elapsed()),
                        epoch: None,
                        tails: BTreeMap::new(),
                    });
                    churn_cursor += 1;
                    drop(sink);
                    drop(rx);
                    continue 'sessions;
                }

                // Scheduled send: offered traffic follows the schedule
                // independent of response completion.
                _ = tokio::time::sleep_until(
                    next_send.map_or(until, |send| {
                        epoch + Duration::from_micros(send.intended_us)
                    }),
                ), if next_send.is_some() => {
                    let send = next_send.expect("send guarded above");
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
                    let lag = micros(epoch.elapsed()).saturating_sub(send.intended_us);
                    if lag > facts.generator_lag_bound_us {
                        log.push_fault(InvalidReason::GeneratorSaturated {
                            max_lag_us: lag,
                            bound_us: facts.generator_lag_bound_us,
                        });
                        return;
                    }
                    let data = json!({
                        "ledger_sender": plan.name,
                        "seq": send.seq,
                        "padding": padding.as_ref(),
                    });
                    let (class, key) = match facts.delivery_class {
                        DeliveryClass::Reliable => (None, None),
                        DeliveryClass::Latest => {
                            // The coalescing key is a sender-scoped u32; a
                            // sender round-robins its keys so a run can hold
                            // newest-value semantics (one key) or key
                            // isolation (many keys).
                            let key_index =
                                send.seq % u64::from(facts.latest_keys_per_sender);
                            match u32::try_from(key_index) {
                                Ok(index) => {
                                    (Some(WireDeliveryClass::Latest), Some(index))
                                }
                                Err(error) => {
                                    log.push_fault(InvalidReason::SendFailed {
                                        sender: plan.name.clone(),
                                        detail: error.to_string(),
                                    });
                                    return;
                                }
                            }
                        }
                        DeliveryClass::Volatile => {
                            (Some(WireDeliveryClass::Volatile), None)
                        }
                    };
                    let message = ClientMessage::GameData { class, key, data };
                    let frame = match serde_json::to_string(&message) {
                        Ok(frame) => frame,
                        Err(error) => {
                            log.push_fault(InvalidReason::SendFailed {
                                sender: plan.name.clone(),
                                detail: format!("serialize: {error}"),
                            });
                            return;
                        }
                    };
                    if let Err(error) = sink.send(Message::Text(frame.into())).await {
                        // After a declared termination, socket errors are the
                        // expected consequence — the peer stops, and its
                        // remainder is unsent work, not an independent fault.
                        if !log.was_server_terminated() {
                            log.push_fault(InvalidReason::SendFailed {
                                sender: plan.name.clone(),
                                detail: error.to_string(),
                            });
                        }
                        return;
                    }
                    log.push_sent(SentEvent {
                        sender: plan.name.clone(),
                        room: plan.room,
                        seq: send.seq,
                        epoch: incarnation,
                        intended_us: send.intended_us,
                        sent_us: micros(epoch.elapsed()),
                        phase: send.phase,
                    });
                    send_cursor += 1;
                }

                // Inbound frame. A paused reader's stream is not polled
                // until the resume instant (the wedge fills the server's
                // bounded kernel handoff); a slow reader never polls at all.
                frame = rx.next(), if !never_read && !reads_held => {
                    let Some(frame) = frame else {
                        log.push_disconnect(DisconnectEvent {
                            recipient: recipient.clone(),
                            observation: DisconnectObservation::StreamEnded,
                        });
                        return;
                    };
                    if !handle_inbound(&recipient, frame, &registry, epoch, &log).await {
                        return;
                    }
                }

                // Quiescence: this peer connected through.
                _ = tokio::time::sleep_until(until) => return,
            }
        }
    }
}

/// Handle one inbound server frame: record the delivery with its same-clock
/// receipt time and the sending incarnation resolved through the registry,
/// resolve gap reports the same way, and account server rejections. Returns
/// `false` when the session ended.
async fn handle_inbound(
    recipient: &str,
    frame: Result<Message, tokio_tungstenite::tungstenite::Error>,
    registry: &SenderRegistry,
    epoch: Instant,
    log: &Arc<EventLog>,
) -> bool {
    match frame {
        Ok(Message::Text(text)) => match serde_json::from_str::<ServerMessage>(&text) {
            Ok(ServerMessage::GameData {
                from_player,
                data,
                seq,
                ..
            }) => {
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
                        debug_assert_eq!(sender, ledger_sender, "registry and ledger agree");
                        log.push_receipt(ReceiptEvent {
                            recipient: recipient.to_string(),
                            sender,
                            seq: ledger_seq,
                            epoch: sender_incarnation,
                            server_seq: seq.unwrap_or(ledger_seq + 1),
                            received_us: micros(epoch.elapsed()),
                        });
                    } else {
                        // A delivery from a player this run never seated:
                        // misrouted, and the verdict is already invalid —
                        // keep reading for the evidence.
                        log.push_fault(InvalidReason::MisroutedDeliveries {
                            count: 1,
                            first: crate::oracle::DeliveryKey {
                                recipient: recipient.to_string(),
                                sender: from_player.to_string(),
                                epoch: 0,
                                seq: 0,
                            },
                        });
                    }
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
/// lossy-class run also samples its class's accountable outcomes.
#[allow(clippy::too_many_arguments)]
async fn sample_loop(
    epoch: Instant,
    until: Instant,
    interval: Duration,
    metrics_url: String,
    pid: Option<u32>,
    delivery_class: Option<DeliveryClass>,
    out: Arc<std::sync::Mutex<Vec<IntervalSample>>>,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("build capacity metrics scrape client");
    let mut slot = Instant::now() + interval;
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
        let sample = IntervalSample {
            t_us: micros(epoch.elapsed()),
            counters,
            server_rss_bytes: pid.and_then(diagnostics::resident_memory_bytes),
            cgroup_memory_bytes: pid.and_then(diagnostics::cgroup_memory_bytes),
            generator_rss_bytes: diagnostics::resident_memory_bytes(std::process::id()),
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
