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
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use crate::artifacts::{self, BuildIdentity, IntervalSample, ServerIdentity, WorkloadShape};
use crate::config::{count_u64, count_usize, micros, Encoding, RunConfig, SendPause};
use crate::diagnostics;
use crate::oracle::{self, InvalidReason, OutcomeSummary};
use crate::records::{DisconnectEvent, DisconnectObservation, EventLog, ReceiptEvent, SentEvent};
use crate::schedule::{build_plans, Phase, SenderPlan};
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

/// The result of one completed run.
#[derive(Debug)]
pub struct RunOutcome {
    pub run_id: String,
    pub output_dir: PathBuf,
    /// The last interval sample's server counters, as recorded evidence
    /// (delivery counters, slow-consumer disconnects, active connections).
    /// `None` when no scrape succeeded — recorded absence, never omission.
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

    let run_id = uuid::Uuid::new_v4().to_string();
    // Room codes are scoped to this run so two concurrent runs against one
    // shared external server start from distinct room namespaces (12 bits —
    // a rare prefix collision degrades to loud join refusals, never silent
    // cross-talk).
    config.room_code_prefix = Some(room_code_prefix(&run_id));
    let plans = build_plans(&config);
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
    let mut peers: Vec<(String, u32, WsSink, WsReceiver)> = Vec::new();
    for room in 0..config.rooms {
        let room_plans: Vec<&SenderPlan> = plans.iter().filter(|plan| plan.room == room).collect();
        let joins = room_plans.into_iter().map(|plan| {
            let config = &config;
            let ws_url = &ws_url;
            async move { connect_and_join(config, ws_url, plan).await }
        });
        let results = futures_util::future::join_all(joins).await;
        for (plan, result) in plans.iter().filter(|p| p.room == room).zip(results) {
            match result {
                Ok((sink, rx)) => peers.push((plan.name.clone(), plan.room, sink, rx)),
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
        + config.stall_senders.map_or(0, micros);
    // A sender may legitimately emit up to the lag bound late, so
    // quiescence must never expire before a bound-late sender can finish.
    let quiescence_us =
        max_intended_us + micros(config.drain_grace).max(micros(config.generator_lag_bound));
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

    // Slow-reader hook: hold the designated peer's receive half without ever
    // polling it, so its socket wedges exactly like a stalled client. The
    // peer still sends on its own sink.
    let mut handles = Vec::new();
    for (name, room, sink, rx) in peers {
        let plan = plans
            .iter()
            .find(|plan| plan.name == name && plan.room == room)
            .cloned()
            .expect("peer has a schedule");
        if slow_reader_name.as_deref() == Some(name.as_str()) {
            // Hold the receive half unpolled forever: the socket wedges
            // inbound exactly like a stalled client. The sink half may be
            // dropped by the sender task — the wedged rx keeps the
            // connection open.
            tokio::spawn(async move {
                let _keep_alive = rx;
                futures_util::future::pending::<()>().await;
            });
        } else {
            let log = Arc::clone(&log);
            let recipient = name.clone();
            handles.push(tokio::spawn(receiver_task(
                recipient, rx, epoch, log, quiescence,
            )));
        }
        handles.push(tokio::spawn(sender_task(
            plan,
            sink,
            epoch,
            Arc::clone(&log),
            SenderHooks {
                generator_lag_bound_us: micros(config.generator_lag_bound),
                payload_bytes: config.payload_bytes,
                pause_sends: config.pause_sends,
                stall_senders: config.stall_senders,
            },
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

    let records = log.snapshot();
    let bound_us = micros(config.generator_lag_bound);
    let summary = oracle::summarize(&plans, &roster, &records, bound_us);

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
        .last()
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

/// One peer: connect (clamped for the slow reader), negotiate (v3), join.
async fn connect_and_join(
    config: &RunConfig,
    ws_url: &str,
    plan: &SenderPlan,
) -> Result<(WsSink, WsReceiver), String> {
    let name = RunConfig::peer_name(plan.room, plan.player);
    let is_slow_reader = slow_reader_name(config).as_deref() == Some(name.as_str());
    let stream = if is_slow_reader {
        connect_clamped(ws_url, SLOW_READER_RECV_BUFFER_BYTES).await?
    } else {
        connect_plain(ws_url).await?
    };
    let (mut sink, mut rx) = stream.split();
    if config.encoding == Encoding::V3Json {
        authenticate_v3(&mut sink, &mut rx).await?;
    }
    join_room(
        &mut sink,
        &mut rx,
        &config.room_code(plan.room),
        &name,
        config.players_per_room,
    )
    .await?;
    Ok((sink, rx))
}

fn slow_reader_name(config: &RunConfig) -> Option<String> {
    config.slow_reader.then(|| RunConfig::peer_name(0, 0))
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

/// Join the run's room and fail loudly on any refusal.
async fn join_room(
    sink: &mut WsSink,
    rx: &mut WsReceiver,
    room_code: &str,
    player_name: &str,
    max_players: u32,
) -> Result<(), String> {
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
            ServerMessage::RoomJoined(_) => return Ok(()),
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

/// Scheduled sends for one sender: sleep to each intended time, honor the
/// fault hooks, then push the frame and record the actual send.
///
/// Offered traffic follows the schedule independent of response completion:
/// a send is never withheld because earlier deliveries are outstanding. The
/// generator-lag bound is the one stop condition — past it, the run is
/// invalidated as generator-saturated rather than silently stretched.
async fn sender_task(
    plan: SenderPlan,
    mut sink: WsSink,
    epoch: Instant,
    log: Arc<EventLog>,
    hooks: SenderHooks,
) {
    let lag_bound_us = hooks.generator_lag_bound_us;
    let padding: Arc<str> = "x".repeat(count_usize(hooks.payload_bytes)).into();
    let first_measured = plan
        .sends
        .iter()
        .find(|send| send.phase == Phase::Measured)
        .map(|send| send.seq);
    for send in &plan.sends {
        tokio::time::sleep_until(epoch + Duration::from_micros(send.intended_us)).await;
        // Fault hooks: a pause shifts this and every later send (the pause
        // must show up as scheduled-send lag, never as reduced offered load);
        // a stall trips the lag bound deterministically.
        if let Some(SendPause {
            after_seq,
            duration,
        }) = hooks.pause_sends
        {
            if send.seq == after_seq {
                tokio::time::sleep_until(
                    epoch + Duration::from_micros(send.intended_us) + duration,
                )
                .await;
            }
        }
        if let (Some(stall), Some(first)) = (hooks.stall_senders, first_measured) {
            if send.seq == first {
                tokio::time::sleep(stall).await;
            }
        }
        let lag = micros(epoch.elapsed()).saturating_sub(send.intended_us);
        if lag > lag_bound_us {
            log.push_fault(InvalidReason::GeneratorSaturated {
                max_lag_us: lag,
                bound_us: lag_bound_us,
            });
            return;
        }
        let data = json!({
            "ledger_sender": plan.name,
            "seq": send.seq,
            "padding": padding.as_ref(),
        });
        let message = ClientMessage::GameData {
            class: None, // reliable delivery is the class under measurement
            key: None,
            data,
        };
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
            // After a declared termination, socket errors are the expected
            // consequence — the sender stops, and its remainder is unsent
            // work, not an independent fault.
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
            intended_us: send.intended_us,
            sent_us: micros(epoch.elapsed()),
            phase: send.phase,
        });
    }
}

/// The owned config facts a sender task needs (it is spawned, so it cannot
/// borrow the run config).
#[derive(Debug, Clone, Copy)]
struct SenderHooks {
    generator_lag_bound_us: u64,
    payload_bytes: u32,
    pause_sends: Option<SendPause>,
    stall_senders: Option<Duration>,
}

/// Drain one recipient's stream until quiescence or disconnection, recording
/// every relayed delivery with its same-clock receipt time.
async fn receiver_task(
    recipient: String,
    mut rx: WsReceiver,
    epoch: Instant,
    log: Arc<EventLog>,
    until: Instant,
) {
    loop {
        let frame = tokio::select! {
            frame = rx.next() => match frame {
                Some(frame) => frame,
                None => {
                    log.push_disconnect(DisconnectEvent {
                        recipient: recipient.clone(),
                        observation: DisconnectObservation::StreamEnded,
                    });
                    return;
                }
            },
            _ = tokio::time::sleep_until(until) => return, // connected through
        };
        match frame {
            Ok(Message::Text(text)) => match serde_json::from_str::<ServerMessage>(&text) {
                Ok(ServerMessage::GameData { data, .. }) => {
                    if let Some((sender, seq)) =
                        websocket_test_helpers::delivery_ledger::extract(&data)
                    {
                        log.push_receipt(ReceiptEvent {
                            recipient: recipient.clone(),
                            sender,
                            seq,
                            received_us: micros(epoch.elapsed()),
                        });
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    log.push_fault(InvalidReason::MalformedServerFrame {
                        recipient: recipient.clone(),
                        detail: error.to_string(),
                    });
                    return;
                }
            },
            Ok(Message::Close(frame)) => {
                log.push_disconnect(DisconnectEvent {
                    recipient: recipient.clone(),
                    observation: DisconnectObservation::ServerClosed(
                        frame.map(|close| close.code.into()),
                    ),
                });
                return;
            }
            Ok(_) => {}
            Err(_) => {
                log.push_disconnect(DisconnectEvent {
                    recipient: recipient.clone(),
                    observation: DisconnectObservation::StreamEnded,
                });
                return;
            }
        }
    }
}

/// Periodic server resource sampling until `until`. A failed scrape is
/// recorded as an explicit sample with `scrape_error` — never skipped.
async fn sample_loop(
    epoch: Instant,
    until: Instant,
    interval: Duration,
    metrics_url: String,
    pid: Option<u32>,
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
            Ok(diagnostics::TRACKED_COUNTERS
                .iter()
                .map(|name| {
                    let value = match diagnostics::parse_counter(&text, name) {
                        Some(value) => serde_json::Value::from(value),
                        None => serde_json::Value::Null,
                    };
                    ((*name).to_string(), value)
                })
                .collect())
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
