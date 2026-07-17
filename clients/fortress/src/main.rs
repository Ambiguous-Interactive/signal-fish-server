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

#[derive(Debug, Serialize)]
struct Report {
    player_id: Uuid,
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
) -> Result<(), String> {
    while let Some(frame) = relay.take_outbound() {
        match client.send_binary_game_data(frame.payload.clone()) {
            Ok(()) => relay.mark_admitted(frame),
            Err(SignalFishError::SendBufferFull { .. }) => {
                *retries = retries.saturating_add(1);
                relay.return_outbound_front(frame);
                break;
            }
            Err(error) => return Err(format!("relay send failed: {error}")),
        }
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or("missing server URL")?;
    let role = args.next().ok_or("missing role")?;
    let room_file = args.next().ok_or("missing room-file path")?;
    let room_code = args.next();

    let transport = WebSocketTransport::connect_with_timeout(&url, Duration::from_secs(5))
        .await
        .map_err(|error| format!("connect: {error}"))?;
    let mut config = SignalFishConfig::new("fortress-issue-242-interop").enable_v3();
    config.game_data_format = Some(GameDataEncoding::MessagePack);
    config.command_channel_capacity = 64;
    let mut client = SignalFishPollingClient::new(transport, config);
    let relay = RelaySocket::default();

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

    while Instant::now() < deadline {
        let events = client.poll();
        relay.record_client_sent(client.stats().game_data_sent);
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
                    if let Some(local_id) = local {
                        if let Some(remote) = roster.iter().copied().find(|id| *id != local_id) {
                            relay.admit_inbound(InboundRelayFrame {
                                local: local_id,
                                known_remote: remote,
                                from: from_player,
                                encoding,
                                seq,
                                epoch,
                                payload: &payload,
                            });
                        }
                    }
                }
                SignalFishEvent::Disconnected { reason, .. } => {
                    return Err(format!("server disconnected peer: {reason:?}"));
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

        if let Some(fortress) = session.as_mut() {
            fortress.poll_remote_clients();
            for event in fortress.events() {
                match event {
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
                let target_reached = fortress.confirmed_frame().as_i32() >= TARGET_CONFIRMED_FRAMES;
                observe_running_phase(
                    target_reached,
                    &mut polling_callbacks_during_run,
                    &mut running_finished_at,
                    Instant::now(),
                );
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

            drain_relay(&mut client, &relay, &mut relay_retries)?;
            relay.sample_queue();
            let relay_stats = relay.counters();
            let client_stats = client.stats();
            if fortress.confirmed_frame().as_i32() >= TARGET_CONFIRMED_FRAMES
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
                    client_game_data_sent_during_run: client_stats
                        .game_data_sent
                        .saturating_sub(running_client_sent_baseline),
                    client_game_data_received: client_stats.game_data_received,
                    client_messages_undecodable: client_stats.messages_undecodable,
                    final_pipeline_queue_depth: relay.queue_depth(),
                    peak_pipeline_queue_depth: relay.peak_queue_depth(),
                    peak_oldest_queue_age_us: relay.peak_oldest_queue_age().as_micros(),
                    relay_frames_enqueued: relay_stats.enqueued_outbound,
                    relay_frames_enqueued_during_run: relay_stats
                        .enqueued_outbound
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
        "peer deadline expired: role={role}, roster={}, session={diagnostics:?}, pipeline_depth={}, client_stats={:?}",
        roster.len(),
        relay.queue_depth(),
        client.stats()
    ))
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{observe_running_phase, outbound_is_drained, running_elapsed};

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
