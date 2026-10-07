mod relay;
pub mod workload;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use fortress_rollback::{FortressEvent, P2PSession, SessionBuilder, SessionState};
use relay::{InboundRelayFrame, RelaySocket};
use serde::Serialize;
use signal_fish_client::protocol::GameDataEncoding;
use signal_fish_client::{
    JoinRoomParams, SignalFishConfig, SignalFishError, SignalFishEvent, SignalFishPollingClient,
    WebSocketTransport,
};
use uuid::Uuid;
use workload::{apply_requests, input_for_frame, GameConfig, GameState, TARGET_CONFIRMED_FRAMES};

const PROCESS_DEADLINE: Duration = Duration::from_secs(30);
const FRAME_TIME: Duration = Duration::from_nanos(1_000_000_000 / 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunMode {
    Healthy,
    DrainProbe,
    SyncCloseProbe,
    NegativeOneAdmissionPerCallback,
}

const NEGATIVE_ACTIVE_CALLBACKS: u64 = 600;

fn admission_budget(mode: RunMode, active: bool) -> usize {
    if mode == RunMode::NegativeOneAdmissionPerCallback && active {
        1
    } else {
        usize::MAX
    }
}

fn drain_probe_ready(
    confirmed: i32,
    advanced: u64,
    rollbacks: u64,
    checksums: (u64, u64, u64),
    traffic: (u64, u64),
) -> bool {
    (120..TARGET_CONFIRMED_FRAMES).contains(&confirmed)
        && advanced > 0
        && rollbacks > 0
        && checksums.0 > 0
        && checksums.0 == checksums.1
        && checksums.2 == 0
        && traffic.0 > 0
        && traffic.1 > 0
}

fn sync_probe_ready(
    phase: SessionState,
    progress: Option<(u32, u32, u32)>,
    frames: (i32, u64),
    traffic: (u64, u64),
) -> bool {
    phase == SessionState::Synchronizing
        && progress
            .is_some_and(|(count, total, requests)| count > 0 && count < total && requests > 0)
        && frames == (0, 0)
        && traffic.0 > 0
        && traffic.1 > 0
}

struct PendingInbound {
    from_player: Uuid,
    encoding: GameDataEncoding,
    payload: Vec<u8>,
    seq: Option<u64>,
    epoch: Option<u32>,
}

#[derive(Debug, Serialize)]
struct Report {
    player_id: Uuid,
    run_mode: RunMode,
    max_active_admissions_per_callback: u64,
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
    peak_oldest_queue_age_us: u128,
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
    running_elapsed_ms: u128,
    polling_callbacks_during_run: u64,
    relay_sent_sequence_count: u64,
    relay_sent_first_sequence: u64,
    relay_sent_last_sequence: u64,
    relay_sent_sequence_hash: u64,
    relay_received_sequence_count: u64,
    relay_received_first_sequence: u64,
    relay_received_last_sequence: u64,
    relay_received_sequence_hash: u64,
}

fn outbound_is_drained(
    pipeline_queue_depth: usize,
    relay_frames_enqueued: u64,
    client_game_data_sent: u64,
) -> bool {
    pipeline_queue_depth == 0 && relay_frames_enqueued == client_game_data_sent
}

fn observe_running_phase(
    target_reached: bool,
    polling_callbacks: &mut u64,
    finished_at: &mut Option<Instant>,
    now: Instant,
) {
    if target_reached {
        finished_at.get_or_insert(now);
    } else {
        *polling_callbacks = polling_callbacks.saturating_add(1);
    }
}

fn running_elapsed(started_at: Option<Instant>, finished_at: Option<Instant>) -> Duration {
    started_at
        .zip(finished_at)
        .map_or(Duration::ZERO, |(started, finished)| {
            finished.saturating_duration_since(started)
        })
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
    client: &mut SignalFishPollingClient<WebSocketTransport>,
    relay: &RelaySocket,
    retries: &mut u64,
    remaining: &mut usize,
) -> Result<u64, String> {
    let mut admitted = 0;
    while *remaining > 0 {
        let Some(frame) = relay.take_outbound() else {
            break;
        };
        match client.send_binary_game_data(frame.payload.clone()) {
            Ok(()) => {
                relay.mark_admitted(frame);
                *remaining -= 1;
                admitted += 1;
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or("missing server URL")?;
    let role = args.next().ok_or("missing role")?;
    let room_file = args.next().ok_or("missing room-file path")?;
    let room_code = match role.as_str() {
        "creator" => None,
        "joiner" => Some(args.next().ok_or("missing joiner room code")?),
        _ => return Err("role must be creator or joiner".to_string()),
    };
    let run_mode = match args.next().as_deref() {
        None | Some("healthy") => RunMode::Healthy,
        Some("drain-probe") => RunMode::DrainProbe,
        Some("sync-close-probe") => RunMode::SyncCloseProbe,
        Some("negative-one-admission-per-callback") => RunMode::NegativeOneAdmissionPerCallback,
        _ => return Err("unknown run mode".to_string()),
    };
    if args.next().is_some() {
        return Err("unexpected trailing arguments".to_string());
    }

    let transport = WebSocketTransport::connect_with_timeout(&url, Duration::from_secs(5))
        .await
        .map_err(|error| format!("connect: {error}"))?;
    let mut config = SignalFishConfig::new("fortress-issue-242-interop").enable_v3();
    config.game_data_format = Some(GameDataEncoding::MessagePack);
    config.command_channel_capacity = 64;
    let mut client = SignalFishPollingClient::new(transport, config);
    let relay = RelaySocket::default();
    if run_mode != RunMode::NegativeOneAdmissionPerCallback {
        relay.hold_inputs_until_prediction();
    }

    if run_mode == RunMode::SyncCloseProbe {
        relay.allow_first_sync_reply_only();
    }

    let deadline = Instant::now() + PROCESS_DEADLINE;
    let mut local = None;
    let mut roster = BTreeSet::new();
    let mut session = None;
    let mut state = GameState::default();
    let mut next_callback = Instant::now();
    let mut relay_retries = 0u64;
    let mut recommended_skips = 0u32;
    let mut running_since = None;
    let mut running_finished_at = None;
    let mut polling_callbacks_during_run = 0u64;
    let mut running_client_sent_baseline = 0u64;
    let mut running_relay_enqueued_baseline = 0u64;
    let mut running_client_sent_at_end = None;
    let mut running_relay_enqueued_at_end = None;
    let mut max_active_admissions_per_callback = 0;
    let mut local_target_reached = false;
    let mut workload_finished = false;
    let mut peer_left_after_ack = false;
    let mut pending_inbound = Vec::new();
    let mut drain_probe_published = false;
    let mut sync_progress = None;

    while Instant::now() < deadline {
        let events = client.poll();
        relay.record_client_sent(client.stats().game_data_sent);
        if local_target_reached && running_client_sent_at_end.is_none() {
            running_client_sent_at_end = Some(client.stats().game_data_sent);
        }
        let cap_active = !local_target_reached;
        let mut remaining_admissions = admission_budget(run_mode, cap_active);
        let mut callback_admissions = 0;
        for event in events {
            match event {
                SignalFishEvent::Authenticated { .. } => {
                    let mut params =
                        JoinRoomParams::new("fortress-issue-242", &role).with_max_players(2);
                    if let Some(code) = room_code.as_deref() {
                        params = params.with_room_code(code);
                    }
                    client
                        .join_room(params)
                        .map_err(|error| format!("join: {error}"))?;
                }
                SignalFishEvent::RoomJoined {
                    player_id,
                    room_code,
                    current_players,
                    ..
                } => {
                    local = Some(player_id);
                    roster.insert(player_id);
                    roster.extend(current_players.into_iter().map(|player| player.id));
                    if role == "creator" {
                        tokio::fs::write(&room_file, room_code)
                            .await
                            .map_err(|error| format!("publish room code: {error}"))?;
                    }
                    client
                        .set_ready()
                        .map_err(|error| format!("ready: {error}"))?;
                }
                SignalFishEvent::PlayerJoined { player } => {
                    roster.insert(player.id);
                }
                SignalFishEvent::GameDataBinary {
                    from_player,
                    encoding,
                    payload,
                    seq,
                    epoch,
                } => {
                    pending_inbound.push(PendingInbound {
                        from_player,
                        encoding,
                        payload,
                        seq,
                        epoch,
                    });
                }
                SignalFishEvent::GoingAway {
                    deadline_ms,
                    retry_after_secs,
                } => {
                    eprintln!("server going away: deadline_ms={deadline_ms}, retry_after_secs={retry_after_secs:?}");
                }
                SignalFishEvent::Disconnected { reason, .. } => {
                    if run_mode == RunMode::SyncCloseProbe {
                        let evidence = serde_json::json!({
                            "role": role, "player_id": local,
                            "phase": session.as_ref().map(|fortress: &P2PSession<GameConfig>| fortress.current_state().to_string()),
                            "current_frame": session.as_ref().map(|fortress| fortress.current_frame().as_i32()),
                            "frames_advanced": session.as_ref().map(|fortress| fortress.metrics().frames_advanced),
                            "game_frame": state.frame, "sync_progress": sync_progress,
                            "sent": client.stats().game_data_sent, "received": client.stats().game_data_received,
                        });
                        eprintln!("sync shutdown evidence: {evidence}");
                    }

                    return Err(format!("server disconnected peer: {reason:?}"));
                }
                SignalFishEvent::PlayerLeft { player_id, .. } => {
                    if role == "joiner" && relay.joiner_ack_enqueued() {
                        peer_left_after_ack = true;
                    } else {
                        return Err(format!(
                            "Signal Fish peer left before final ack: {player_id}"
                        ));
                    }
                }
                SignalFishEvent::Error { message, .. }
                | SignalFishEvent::AuthenticationError { error: message, .. } => {
                    return Err(format!("server rejected peer: {message}"));
                }
                _ => {}
            }
        }

        if session.is_none() && roster.len() == 2 {
            let local_id = local.ok_or("two-player roster arrived before local id")?;
            let remote = roster
                .iter()
                .copied()
                .find(|id| *id != local_id)
                .ok_or("missing remote player")?;
            relay
                .configure_identity(local_id, remote)
                .map_err(|error| format!("configure relay identity: {error}"))?;
            session = Some(build_session(local_id, remote, relay.clone())?);
        }

        if session.is_some() {
            let local_id = local.ok_or("session exists without local id")?;
            let remote = roster
                .iter()
                .copied()
                .find(|id| *id != local_id)
                .ok_or("session exists without remote id")?;
            for frame in pending_inbound.drain(..) {
                relay.admit_inbound(InboundRelayFrame {
                    local: local_id,
                    known_remote: remote,
                    from: frame.from_player,
                    encoding: frame.encoding,
                    seq: frame.seq,
                    epoch: frame.epoch,
                    payload: &frame.payload,
                });
            }
        }

        workload_finished |= local_target_reached
            && (run_mode == RunMode::NegativeOneAdmissionPerCallback || relay.target_received());
        if let Some(fortress) = session.as_mut() {
            if !workload_finished {
                relay.observe_local_frame(fortress.current_frame().as_i32());
                fortress.poll_remote_clients();
                for event in fortress.events() {
                    match event {
                        FortressEvent::Synchronizing {
                            total,
                            count,
                            total_requests_sent,
                            ..
                        } if run_mode == RunMode::SyncCloseProbe => {
                            sync_progress = Some((count, total, total_requests_sent));
                        }
                        FortressEvent::WaitRecommendation { skip_frames } => {
                            recommended_skips = skip_frames;
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
                    if running_since.is_none() {
                        relay.reset_queue_peak();
                        running_since = Some(Instant::now());
                        running_client_sent_baseline = client.stats().game_data_sent;
                        running_relay_enqueued_baseline = relay.counters().enqueued_outbound;
                    }
                    let target_reached = run_mode != RunMode::NegativeOneAdmissionPerCallback
                        && fortress.confirmed_frame().as_i32() >= TARGET_CONFIRMED_FRAMES;
                    observe_running_phase(
                        target_reached,
                        &mut polling_callbacks_during_run,
                        &mut running_finished_at,
                        Instant::now(),
                    );
                    local_target_reached |= target_reached;
                    if !target_reached && recommended_skips > 0 {
                        recommended_skips = recommended_skips.saturating_sub(1);
                    } else if !target_reached {
                        let current = fortress.current_frame().as_i32();
                        for handle in fortress.local_player_handles() {
                            let input = input_for_frame(current, handle.as_usize());
                            fortress
                                .add_local_input(handle, input)
                                .map_err(|error| format!("add input: {error}"))?;
                        }
                        let requests = fortress
                            .advance_frame()
                            .map_err(|error| format!("advance Fortress: {error}"))?;
                        apply_requests(&mut state, requests);
                    }
                }
            }

            if run_mode == RunMode::NegativeOneAdmissionPerCallback
                && polling_callbacks_during_run >= NEGATIVE_ACTIVE_CALLBACKS
            {
                local_target_reached = true;
                running_finished_at.get_or_insert_with(Instant::now);
            }
            if local_target_reached && running_relay_enqueued_at_end.is_none() {
                running_relay_enqueued_at_end = Some(relay.counters().enqueued_outbound);
                if run_mode != RunMode::NegativeOneAdmissionPerCallback {
                    running_client_sent_at_end = Some(client.stats().game_data_sent);
                }
            }
            if run_mode != RunMode::NegativeOneAdmissionPerCallback
                && local_target_reached
                && !relay.target_enqueued()
            {
                let local_id = local.ok_or("local id disappeared")?;
                let remote = roster
                    .iter()
                    .copied()
                    .find(|id| *id != local_id)
                    .ok_or("remote id disappeared")?;
                relay
                    .enqueue_target_reached(&remote)
                    .map_err(|error| format!("enqueue relay target marker: {error}"))?;
            }

            callback_admissions += drain_relay(
                &mut client,
                &relay,
                &mut relay_retries,
                &mut remaining_admissions,
            )?;
            relay.sample_queue();
            workload_finished |= local_target_reached
                && (run_mode == RunMode::NegativeOneAdmissionPerCallback
                    || relay.target_received());
            if workload_finished
                && !relay.completion_enqueued()
                && outbound_is_drained(
                    relay.queue_depth(),
                    relay.counters().enqueued_outbound,
                    client.stats().game_data_sent,
                )
            {
                let local_id = local.ok_or("local id disappeared")?;
                let remote = roster
                    .iter()
                    .copied()
                    .find(|id| *id != local_id)
                    .ok_or("remote id disappeared")?;
                relay
                    .enqueue_completion(&remote)
                    .map_err(|error| format!("enqueue relay completion: {error}"))?;
                callback_admissions += drain_relay(
                    &mut client,
                    &relay,
                    &mut relay_retries,
                    &mut remaining_admissions,
                )?;
                relay.sample_queue();
            }
            let completion_exchange_done = relay.completion_enqueued()
                && relay.completion_received()
                && outbound_is_drained(
                    relay.queue_depth(),
                    relay.counters().enqueued_outbound,
                    client.stats().game_data_sent,
                );
            if role == "creator" && completion_exchange_done && !relay.creator_final_enqueued() {
                let local_id = local.ok_or("local id disappeared")?;
                let remote = roster
                    .iter()
                    .copied()
                    .find(|id| *id != local_id)
                    .ok_or("remote id disappeared")?;
                relay
                    .enqueue_creator_final(&remote)
                    .map_err(|error| format!("enqueue creator final marker: {error}"))?;
                callback_admissions += drain_relay(
                    &mut client,
                    &relay,
                    &mut relay_retries,
                    &mut remaining_admissions,
                )?;
                relay.sample_queue();
            }
            if role == "joiner"
                && completion_exchange_done
                && relay.creator_final_received()
                && !relay.joiner_ack_enqueued()
            {
                let local_id = local.ok_or("local id disappeared")?;
                let remote = roster
                    .iter()
                    .copied()
                    .find(|id| *id != local_id)
                    .ok_or("remote id disappeared")?;
                relay
                    .enqueue_joiner_ack(&remote)
                    .map_err(|error| format!("enqueue joiner final ack: {error}"))?;
                callback_admissions += drain_relay(
                    &mut client,
                    &relay,
                    &mut relay_retries,
                    &mut remaining_admissions,
                )?;
                relay.sample_queue();
            }
            if cap_active && running_since.is_some() {
                max_active_admissions_per_callback =
                    max_active_admissions_per_callback.max(callback_admissions);
            }
            let relay_stats = relay.counters();
            let client_stats = client.stats();
            let active_ready = run_mode == RunMode::DrainProbe
                && drain_probe_ready(
                    fortress.confirmed_frame().as_i32(),
                    fortress.metrics().frames_advanced,
                    fortress.metrics().rollback_count,
                    (
                        fortress.metrics().checksums_compared,
                        fortress.metrics().checksums_matched,
                        fortress.metrics().checksums_mismatched,
                    ),
                    (client_stats.game_data_sent, client_stats.game_data_received),
                );
            let synchronizing_ready = run_mode == RunMode::SyncCloseProbe
                && sync_probe_ready(
                    fortress.current_state(),
                    sync_progress,
                    (
                        fortress.current_frame().as_i32(),
                        fortress.metrics().frames_advanced,
                    ),
                    (client_stats.game_data_sent, client_stats.game_data_received),
                );
            if !drain_probe_published && (active_ready || synchronizing_ready) {
                let metrics = fortress.metrics();
                let evidence = serde_json::json!({
                    "role": role, "player_id": local, "confirmed_frame": fortress.confirmed_frame().as_i32(),
                    "phase": fortress.current_state().to_string(), "current_frame": fortress.current_frame().as_i32(),
                    "game_frame": state.frame, "sync_progress": sync_progress,
                    "frames_advanced": metrics.frames_advanced, "rollback_count": metrics.rollback_count,
                    "checksums_compared": metrics.checksums_compared,
                    "checksums_matched": metrics.checksums_matched,
                    "checksums_mismatched": metrics.checksums_mismatched,
                    "sent": client_stats.game_data_sent, "received": client_stats.game_data_received,
                    "sent_ledger": relay.sent_ledger().count, "received_ledger": relay.received_ledger().count,
                    "malformed": relay_stats.malformed_inbound, "wrong_destination": relay_stats.wrong_destination,
                    "unknown_sender": relay_stats.unknown_sender, "inbound_overflow": relay_stats.inbound_overflow,
                    "outbound_overflow": relay_stats.outbound_overflow, "encode_failures": relay_stats.encode_failures,
                    "completion_underflow": relay_stats.completion_underflow,
                });
                let path =
                    std::path::Path::new(&room_file).with_extension(format!("active-{role}"));
                let temporary = path.with_extension(format!("active-{role}-tmp"));
                tokio::fs::write(&temporary, evidence.to_string())
                    .await
                    .map_err(|error| format!("write drain readiness: {error}"))?;
                tokio::fs::rename(temporary, path)
                    .await
                    .map_err(|error| format!("publish drain readiness: {error}"))?;
                drain_probe_published = true;
            }
            let role_handshake_done = (role == "creator" && relay.joiner_ack_received())
                || (role == "joiner" && peer_left_after_ack);
            if role_handshake_done
                && outbound_is_drained(
                    relay.queue_depth(),
                    relay_stats.enqueued_outbound,
                    client_stats.game_data_sent,
                )
            {
                let metrics = fortress.metrics();
                let sent_ledger = relay.sent_ledger();
                let received_ledger = relay.received_ledger();
                let report = Report {
                    player_id: local.ok_or("local id disappeared")?,
                    run_mode,
                    max_active_admissions_per_callback,
                    current_frame: fortress.current_frame().as_i32(),
                    confirmed_frame: fortress.confirmed_frame().as_i32(),
                    game_frame: state.frame,
                    game_checksum: state.checksum,
                    frames_advanced: metrics.frames_advanced,
                    rollback_count: metrics.rollback_count,
                    max_rollback_depth: metrics.max_rollback_depth,
                    stall_count: metrics.stall_count,
                    wait_recommendations: metrics.wait_recommendations,
                    confirmation_lag_current: metrics.confirmation_lag_current,
                    confirmation_lag_max: metrics.confirmation_lag_max,
                    checksums_mismatched: metrics.checksums_mismatched,
                    checksums_compared: metrics.checksums_compared,
                    checksums_matched: metrics.checksums_matched,
                    events_discarded_total: metrics.events_discarded_total,
                    client_game_data_sent: client_stats.game_data_sent,
                    client_game_data_sent_during_run: running_client_sent_at_end
                        .ok_or("missing active send boundary")?
                        .saturating_sub(running_client_sent_baseline),
                    client_game_data_received: client_stats.game_data_received,
                    client_messages_undecodable: client_stats.messages_undecodable,
                    final_pipeline_queue_depth: relay.queue_depth(),
                    peak_pipeline_queue_depth: relay.peak_queue_depth(),
                    peak_oldest_queue_age_us: relay.peak_oldest_queue_age().as_micros(),
                    relay_frames_enqueued: relay_stats.enqueued_outbound,
                    relay_frames_enqueued_during_run: running_relay_enqueued_at_end
                        .ok_or("missing active enqueue boundary")?
                        .saturating_sub(running_relay_enqueued_baseline),
                    relay_frames_received: relay_stats.accepted_inbound,
                    relay_malformed: relay_stats.malformed_inbound,
                    relay_wrong_destination: relay_stats.wrong_destination,
                    relay_unknown_sender: relay_stats.unknown_sender,
                    relay_outbound_overflow: relay_stats.outbound_overflow,
                    relay_inbound_overflow: relay_stats.inbound_overflow,
                    relay_encode_failures: relay_stats.encode_failures,
                    relay_completion_underflow: relay_stats.completion_underflow,
                    relay_send_retries: relay_retries,
                    running_elapsed_ms: running_elapsed(running_since, running_finished_at)
                        .as_millis(),
                    polling_callbacks_during_run,
                    relay_sent_sequence_count: sent_ledger.count,
                    relay_sent_first_sequence: sent_ledger.first_sequence,
                    relay_sent_last_sequence: sent_ledger.last_sequence,
                    relay_sent_sequence_hash: sent_ledger.sequence_hash,
                    relay_received_sequence_count: received_ledger.count,
                    relay_received_first_sequence: received_ledger.first_sequence,
                    relay_received_last_sequence: received_ledger.last_sequence,
                    relay_received_sequence_hash: received_ledger.sequence_hash,
                };
                println!(
                    "{}",
                    serde_json::to_string(&report)
                        .map_err(|error| format!("serialize report: {error}"))?
                );
                return Ok(());
            }
        }

        next_callback += FRAME_TIME;
        let now = Instant::now();
        if next_callback < now {
            next_callback = now;
        }
        tokio::time::sleep(next_callback.saturating_duration_since(now)).await;
    }

    let diagnostics = session.as_ref().map(|fortress| {
        format!(
            "state={:?}, current={}, confirmed={}",
            fortress.current_state(),
            fortress.current_frame().as_i32(),
            fortress.confirmed_frame().as_i32()
        )
    });
    Err(format!(
        "peer deadline expired: role={role}, roster={}, session={diagnostics:?}, local_target_reached={local_target_reached}, target_enqueued={}, target_received={}, workload_finished={workload_finished}, completion_enqueued={}, completion_received={}, pipeline_depth={}, relay_stats={:?}, sent_ledger={:?}, received_ledger={:?}, pending_inbound={}, client_stats={:?}",
        roster.len(),
        relay.target_enqueued(),
        relay.target_received(),
        relay.completion_enqueued(),
        relay.completion_received(),
        relay.queue_depth(),
        relay.counters(),
        relay.sent_ledger(),
        relay.received_ledger(),
        pending_inbound.len(),
        client.stats()
    ))
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{observe_running_phase, outbound_is_drained, running_elapsed};

    #[test]
    fn synchronization_readiness_requires_partial_progress_without_gameplay() {
        use super::SessionState::{Running, Synchronizing};
        for (name, phase, progress, frames, traffic, ready) in [
            (
                "partial",
                Synchronizing,
                Some((1, 5, 1)),
                (0, 0),
                (1, 1),
                true,
            ),
            ("not started", Synchronizing, None, (0, 0), (1, 1), false),
            (
                "zero count",
                Synchronizing,
                Some((0, 5, 1)),
                (0, 0),
                (1, 1),
                false,
            ),
            (
                "completed count",
                Synchronizing,
                Some((5, 5, 5)),
                (0, 0),
                (1, 1),
                false,
            ),
            (
                "no requests",
                Synchronizing,
                Some((1, 5, 0)),
                (0, 0),
                (1, 1),
                false,
            ),
            (
                "already running",
                Running,
                Some((1, 5, 1)),
                (0, 0),
                (1, 1),
                false,
            ),
            (
                "game cursor advanced",
                Synchronizing,
                Some((1, 5, 1)),
                (1, 0),
                (1, 1),
                false,
            ),
            (
                "game advanced",
                Synchronizing,
                Some((1, 5, 1)),
                (0, 1),
                (1, 1),
                false,
            ),
            (
                "no send",
                Synchronizing,
                Some((1, 5, 1)),
                (0, 0),
                (0, 1),
                false,
            ),
            (
                "no receipt",
                Synchronizing,
                Some((1, 5, 1)),
                (0, 0),
                (1, 0),
                false,
            ),
        ] {
            assert_eq!(
                super::sync_probe_ready(phase, progress, frames, traffic),
                ready,
                "{name}"
            );
        }
    }

    #[test]
    fn drain_readiness_requires_unfinished_game_progress_and_matching_traffic() {
        for (name, frame, advanced, rollback, checksums, traffic, ready) in [
            ("first ready frame", 120, 120, 1, (1, 1, 0), (1, 1), true),
            (
                "last unfinished frame",
                599,
                599,
                1,
                (1, 1, 0),
                (1, 1),
                true,
            ),
            ("before progress", 119, 120, 1, (1, 1, 0), (1, 1), false),
            ("already completed", 600, 600, 1, (1, 1, 0), (1, 1), false),
            ("no advancement", 120, 0, 1, (1, 1, 0), (1, 1), false),
            ("no rollback", 120, 120, 0, (1, 1, 0), (1, 1), false),
            ("no comparison", 120, 120, 1, (0, 0, 0), (1, 1), false),
            (
                "unmatched comparison",
                120,
                120,
                1,
                (2, 1, 0),
                (1, 1),
                false,
            ),
            ("corrupt comparison", 120, 120, 1, (1, 1, 1), (1, 1), false),
            ("no completed send", 120, 120, 1, (1, 1, 0), (0, 1), false),
            ("no receive", 120, 120, 1, (1, 1, 0), (1, 0), false),
        ] {
            assert_eq!(
                super::drain_probe_ready(frame, advanced, rollback, checksums, traffic),
                ready,
                "{name}"
            );
        }
    }

    #[test]
    fn callback_admission_budget_caps_active_negative_work_and_releases_final_drain() {
        for (mode, active, expected) in [
            (super::RunMode::Healthy, true, usize::MAX),
            (super::RunMode::DrainProbe, true, usize::MAX),
            (super::RunMode::NegativeOneAdmissionPerCallback, true, 1),
            (
                super::RunMode::NegativeOneAdmissionPerCallback,
                false,
                usize::MAX,
            ),
        ] {
            assert_eq!(
                super::admission_budget(mode, active),
                expected,
                "{mode:?}/{active}"
            );
        }
    }

    #[test]
    fn drain_gate_waits_for_transport_accepted_frame_to_finish() {
        assert!(outbound_is_drained(0, 1_200, 1_200));
        assert!(
            !outbound_is_drained(0, 1_200, 1_199),
            "an empty adapter FIFO does not prove its accepted send completed"
        );
        assert!(!outbound_is_drained(1, 1_200, 1_200));
    }

    #[test]
    fn running_phase_metrics_freeze_before_post_target_drain() {
        let started = Instant::now();
        let target = started + Duration::from_secs(10);
        let drained = target + Duration::from_secs(3);
        let mut callbacks = 600;
        let mut finished = None;

        observe_running_phase(true, &mut callbacks, &mut finished, target);
        observe_running_phase(true, &mut callbacks, &mut finished, drained);

        assert_eq!(callbacks, 600, "drain callbacks are not active callbacks");
        assert_eq!(
            finished,
            Some(target),
            "the first target time is authoritative"
        );
        assert_eq!(
            running_elapsed(Some(started), finished),
            Duration::from_secs(10)
        );
    }
}
