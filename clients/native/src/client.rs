//! Orchestrator: drives one full protocol-v3 client lifecycle.
//!
//! # Flow (PLAN P7 / docs/protocol.md "Protocol v3 additions")
//!
//! 1. **Connect + Authenticate** — `protocol_version`, `supported_transports`,
//!    `supported_topologies` are advertised only in v3 mode (`--protocol-version 2`
//!    omits all of them, producing a pure-v2 `Authenticate`). The server answers
//!    `Authenticated` then `ProtocolInfo` (with the negotiated version on v3).
//! 2. **Room** — `JoinRoom` with no code creates a room (the creator's
//!    `room_created` stdout event carries the code for sibling processes);
//!    `--join-code` joins by code.
//! 3. **Ready barrier** — once `--peers N` members are present, send
//!    `PlayerReady`. The lobby no longer auto-starts on a full ready set:
//!    finalization is driven by an explicit `StartGame`. When the server
//!    reports every current member ready (`LobbyStateChanged.all_ready`), the
//!    room creator sends that `StartGame`; joiners just await the broadcast it
//!    produces.
//! 4. **Finalize** — `GameStarting` (note our own `is_authority`), then every
//!    v3 recipient's authoritative `SessionPlan` (including explicit
//!    Relay/Relay plans with no peers).
//! 5. **P2P** — pair per `peers[].initiate` (`NewPeer` remains accepted for
//!    compatible servers), trickling ICE through `Signal`. The overall WebRTC
//!    transport status resolves (Appendix G) when all expected pairs are
//!    connected (a departure that removes the last unconnected expected pair
//!    counts), or at `--p2p-timeout-secs`. Before that deadline, an incomplete
//!    pair can get configured, bounded coordinated rebuilds so an
//!    ICE-connected but stalled SCTP association cannot wedge forever. At
//!    resolution, `connected: true`
//!    iff at least one pair is connected at that moment; a zero-pair resolution also emits
//!    `fallback_engaged`. Membership churn reports later real state changes;
//!    unchanged states remain suppressed.
//! 6. **Relay floor** — `GameData` keeps flowing over the WebSocket before,
//!    during, and after P2P; `--relay-payload` exercises it explicitly.
//!
//! The orchestrator is a single task: WebSocket frames, engine callbacks
//! (via the [`EngineEvent`] channel), and timers are multiplexed with
//! `tokio::select!`, so all bookkeeping is lock-free and every stdout event is
//! emitted in causal order. The binary-wide `--max-runtime-secs` watchdog in
//! `main` bounds every await in this module.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;

use serde_json::json;
use signal_fish_server::protocol::{
    ClientMessage, DirectEndpoint, ErrorCode, GameDataEncoding, IceServer, LobbyState, PlayerId,
    PlayerInfo, ServerMessage, SessionGeneration, Transport,
};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;
use webrtc::peer_connection::RTCPeerConnectionState;

use crate::accountability::{DeliveryAccountability, GameDataDisposition};
use crate::cli::Cli;
use crate::engine::{
    Engine, EngineEvent, SelectedPairProbeResult, RELIABLE_LABEL, UNRELIABLE_LABEL,
};
use crate::events::{emit, Event, PlanPeer, SignalKind};
use crate::wire::{self, WsStream, HANDSHAKE_TIMEOUT};

/// All success criteria met within the run window.
pub const EXIT_SUCCESS: i32 = 0;
/// `--run-for-secs` elapsed with unmet success criteria.
pub const EXIT_CRITERIA_UNMET: i32 = 1;
/// The server broke the expected protocol flow (app-ID/join rejection, bad frame).
/// NOTE: clap also exits 2 on CLI-usage errors, before the event stream
/// starts (no `exiting` event on that path) — documented in the README.
pub const EXIT_PROTOCOL_ERROR: i32 = 2;
/// Transport-level failure (connect failed, socket died mid-session).
pub const EXIT_CONNECTION_ERROR: i32 = 3;
/// `--max-runtime-secs` watchdog fired (set by `main`, documented here).
pub const EXIT_HARD_TIMEOUT: i32 = 4;

/// Delay between the relay-probe trigger and the `--relay-payload` send,
/// letting the `SessionPlan` that immediately follows the trigger settle
/// first. The trigger is `GameStarting` — or, for a late joiner, entry into
/// the already-Finalized room (`GameStarting` pre-dates the join and is never
/// re-sent, so waiting for it would never fire the probe).
const RELAY_SEND_SETTLE: Duration = Duration::from_millis(250);

fn checked_deadline(start: Instant, duration: Duration) -> Instant {
    start.checked_add(duration).unwrap_or(start)
}

/// Grace period between meeting all success criteria and exiting, so the last
/// unreliable-channel sends are not torn down mid-flight for slower siblings.
const EXIT_LINGER: Duration = Duration::from_millis(250);
/// Poll cadence for an optional harness-controlled success release file.
const SUCCESS_RELEASE_POLL: Duration = Duration::from_millis(100);
/// Poll cadence for selected ICE-pair evidence after a physical link opens.
///
/// This is intentionally a cadence, not a separate timeout. A clean exit keeps
/// the evidence as a postcondition until the existing run deadline, while
/// gameplay exchange proceeds as soon as both data channels open.
const SELECTED_PAIR_POLL: Duration = Duration::from_millis(10);

/// Defensive bounds for signals that legitimately race a held/current planned
/// peer's local engine creation. Authoritative plan-before-signal ordering
/// makes buffering rare; bounds keep a malicious same-room endpoint from
/// turning that race seam into unbounded memory growth.
const MAX_PENDING_SIGNALS_PER_PEER: usize = 32;
const MAX_PENDING_SIGNALS_TOTAL: usize = 128;

/// Retry early enough to leave the original P2P window useful, while allowing
/// ordinary ICE/DTLS/SCTP setup to settle first. The 15-second ceiling is also
/// deliberately puts long-window retries beyond the nightly netem harness's
/// 10-second loss window.
fn p2p_retry_delay(p2p_timeout_secs: u64) -> Duration {
    Duration::from_secs((p2p_timeout_secs / 2).clamp(1, 15))
}

#[derive(Default)]
struct SelectedPairEvidence {
    pending: BTreeSet<PlayerId>,
    in_flight: BTreeSet<PlayerId>,
    poll_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedPairProbeDisposition {
    Ignore,
    Retry,
    Observed,
}

impl SelectedPairEvidence {
    fn require(&mut self, peer: PlayerId) {
        self.pending.insert(peer);
    }

    fn begin_probe(&mut self, peer: PlayerId) -> bool {
        self.pending.contains(&peer) && self.in_flight.insert(peer)
    }

    fn clear(&mut self, peer: PlayerId) {
        self.pending.remove(&peer);
        self.in_flight.remove(&peer);
        if self.pending.is_empty() {
            self.poll_at = None;
        }
    }

    fn complete_probe(
        &mut self,
        peer: PlayerId,
        completed_at: Instant,
        evidence_deadline: Option<Instant>,
        selected: bool,
    ) -> SelectedPairProbeDisposition {
        self.in_flight.remove(&peer);
        if !self.pending.contains(&peer)
            || evidence_deadline.is_some_and(|deadline| completed_at >= deadline)
        {
            return SelectedPairProbeDisposition::Ignore;
        }
        if selected {
            self.clear(peer);
            return SelectedPairProbeDisposition::Observed;
        }
        let retry_at = checked_deadline(completed_at, SELECTED_PAIR_POLL);
        self.poll_at = Some(
            self.poll_at
                .map_or(retry_at, |current| current.min(retry_at)),
        );
        SelectedPairProbeDisposition::Retry
    }

    fn probe_unavailable(&mut self, peer: PlayerId, now: Instant) {
        self.in_flight.remove(&peer);
        if self.pending.contains(&peer) {
            let retry_at = checked_deadline(now, SELECTED_PAIR_POLL);
            self.poll_at = Some(
                self.poll_at
                    .map_or(retry_at, |current| current.min(retry_at)),
            );
        }
    }

    fn take_due(&mut self, now: Instant) -> Vec<PlayerId> {
        if self.poll_at.is_none_or(|at| now < at) {
            return Vec::new();
        }
        self.poll_at = None;
        self.pending.difference(&self.in_flight).copied().collect()
    }

    fn append_unmet(&self, unmet: &mut Vec<String>) {
        for peer in &self.pending {
            unmet.push(format!("selected ICE pair evidence pending for {peer}"));
        }
    }
}

fn take_ready_selected_pair_probes(
    receiver: &mut mpsc::UnboundedReceiver<SelectedPairProbeResult>,
) -> Vec<SelectedPairProbeResult> {
    std::iter::from_fn(|| match receiver.try_recv() {
        Ok(result) => Some(result),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => None,
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
            tracing::debug!("selected-pair probe result channel disconnected");
            None
        }
    })
    .collect()
}

fn apply_selected_pair_probes_before_run_deadline(
    ready: Vec<SelectedPairProbeResult>,
    mut apply: impl FnMut(SelectedPairProbeResult),
    now: Instant,
    run_deadline: Instant,
) -> bool {
    for result in ready {
        apply(result);
    }
    now >= run_deadline
}

fn selected_pair_evidence_deadline(
    run_deadline: Instant,
    success_criteria_reported: bool,
) -> Option<Instant> {
    (!success_criteria_reported).then_some(run_deadline)
}

fn retryable_missing_peers(
    expected: &BTreeSet<PlayerId>,
    connected: &BTreeSet<PlayerId>,
    attempts: &BTreeMap<PlayerId, u8>,
    retry_count: u8,
) -> Vec<PlayerId> {
    expected
        .difference(connected)
        .copied()
        .filter(|peer| attempts.get(peer).copied().unwrap_or(0) < retry_count)
        .collect()
}

fn automatic_p2p_retry_count(rebuild_configured: bool, retry_count: u8) -> u8 {
    retry_count.saturating_sub(u8::from(rebuild_configured))
}

fn validate_p2p_rebuild_retry_count(
    rebuild_configured: bool,
    retry_count: u8,
) -> Result<(), &'static str> {
    if rebuild_configured && retry_count == 0 {
        Err("--p2p-rebuild-release-file requires a nonzero --p2p-retry-count")
    } else {
        Ok(())
    }
}

fn is_coordinated_p2p_rebuild_attempt(
    rebuild_configured: bool,
    attempt: u8,
    retry_count: u8,
) -> bool {
    rebuild_configured && retry_count > 0 && attempt == retry_count
}

fn exchange_label_complete(
    expected: &BTreeSet<PlayerId>,
    sent: &BTreeMap<PlayerId, BTreeSet<String>>,
    received: &BTreeMap<PlayerId, BTreeSet<String>>,
    label: &str,
) -> bool {
    !expected.is_empty()
        && expected.iter().all(|peer| {
            [sent.get(peer), received.get(peer)]
                .into_iter()
                .all(|labels| labels.is_some_and(|labels| labels.contains(label)))
        })
}

fn note_current_pair_connected(
    connected: &mut BTreeSet<PlayerId>,
    reported: &mut BTreeSet<PlayerId>,
    retrying: &mut BTreeSet<PlayerId>,
    peer: PlayerId,
) -> bool {
    connected.insert(peer);
    retrying.remove(&peer);
    reported.insert(peer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairGeneration {
    Initial,
    Retry,
}

impl PairGeneration {
    fn arms_p2p_window(self) -> bool {
        self == Self::Initial
    }
}

fn arm_pair_window(
    deadline: &mut Option<Instant>,
    retry_at: &mut Option<Instant>,
    generation: PairGeneration,
    retry_count: u8,
    timeout: Duration,
    now: Instant,
) {
    if !generation.arms_p2p_window() {
        return;
    }
    let fresh_deadline = checked_deadline(now, timeout);
    *deadline = Some(fresh_deadline);
    *retry_at = (retry_count > 0)
        .then(|| checked_deadline(now, p2p_retry_delay(timeout.as_secs())))
        .filter(|candidate| *candidate < fresh_deadline);
}

/// Keepalive cadence. `docs/guides/building-a-client.md` makes a periodic
/// `Ping` mandatory for every client (the server evicts idle connections);
/// this driver models that contract so anyone using it as a template
/// inherits the keepalive rather than the idle-timeout eviction. Short
/// enough that even a default 30-second conformance run exercises several
/// round-trips.
const PING_INTERVAL: Duration = Duration::from_secs(10);

/// How long a sent `Ping` may go unanswered before the run fails loudly. A
/// missing `Pong` inside this generous window means the connection (or the
/// server's control path) is broken — surfacing that is exactly what a
/// conformance driver is for.
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

/// One-shot drain grace applied when the pong deadline first expires. The
/// deadline check runs at the top of the loop, BEFORE the select reads the
/// socket, so a `Pong` that already arrived could be declared missing
/// without ever being read (deadline and frame ready in the same wake). The
/// extension guarantees at least one more read pass — and because the timer
/// branch is then no longer ready, a pending frame wins that select
/// deterministically.
const PONG_DRAIN_GRACE: Duration = Duration::from_secs(1);

/// A failure that terminates the run with a specific exit code.
#[derive(Debug)]
struct FatalError {
    code: i32,
    message: String,
}

impl FatalError {
    fn protocol(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_PROTOCOL_ERROR,
            message: message.into(),
        }
    }

    fn connection(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_CONNECTION_ERROR,
            message: message.into(),
        }
    }
}

/// Run the full client lifecycle; emits every stdout event except the final
/// `exiting` (owned by `main`) and returns the exit code.
pub async fn run(cli: &Cli) -> i32 {
    match run_inner(cli).await {
        Ok(code) => code,
        Err(fatal) => {
            emit(&Event::Error {
                message: fatal.message.clone(),
            });
            tracing::error!(code = fatal.code, message = %fatal.message, "fatal failure");
            fatal.code
        }
    }
}

async fn run_inner(cli: &Cli) -> Result<i32, FatalError> {
    validate_p2p_rebuild_retry_count(cli.p2p_rebuild_release_file.is_some(), cli.p2p_retry_count)
        .map_err(FatalError::protocol)?;

    // Resolve an explicitly requested `--ip-family` BEFORE touching the
    // network, so a host that cannot serve it fails the process instead of
    // creating or joining a server-side room it could never have used.
    crate::engine::preflight_ip_family(cli.engine_settings(), crate::engine::local_udp_addrs)
        .map_err(|error| FatalError::protocol(format!("{error:#}")))?;

    // Built before the socket too: `Engine::new` touches no network, and its
    // one failure mode (a webrtc build without an async runtime) should not
    // cost a server-side room either.
    let (engine_tx, engine_rx) = mpsc::unbounded_channel();
    let (selected_pair_tx, selected_pair_rx) = mpsc::unbounded_channel();
    let engine = Engine::new(cli.engine_settings(), engine_tx)
        .map_err(|error| FatalError::protocol(format!("webrtc engine init failed: {error:#}")))?;

    // The soft run window starts at process start, handshake included.
    let run_deadline = checked_deadline(Instant::now(), Duration::from_secs(cli.run_for_secs));

    let mut ws = wire::connect(&cli.server_url)
        .await
        .map_err(|error| FatalError::connection(format!("{error:#}")))?;
    emit(&Event::Connected {
        runtime: cli.runtime.as_str().to_string(),
        tick_stall_ms: cli.tick_stall_ms,
    });

    let negotiated_version = authenticate(&mut ws, cli).await?;
    let (my_id, mut present, lobby_state, accountability) =
        join_room(&mut ws, cli, negotiated_version >= 3).await?;

    present.insert(my_id);
    let members_seen = present.clone();
    let mut orchestrator = Orchestrator {
        cli,
        ws,
        engine,
        engine_rx,
        selected_pair_tx,
        selected_pair_rx,
        my_id,
        negotiated_version,
        accountability,
        present,
        members_seen,
        lobby_state: Some(lobby_state.clone()),
        // Joining an already-Lobby room (seat fill) means readiness is
        // immediately possible; a Finalized room means the session is already
        // running — GameStarting was broadcast before we joined and will
        // never be re-sent, so the criterion is satisfied on entry (the
        // late-join SessionPlan carries everything else we need).
        in_lobby: lobby_state == LobbyState::Lobby,
        ready_sent: false,
        start_game_sent: false,
        game_started: lobby_state == LobbyState::Finalized,
        late_joined: lobby_state == LobbyState::Finalized,
        initial_session_plan_pending: negotiated_version >= 3
            && lobby_state == LobbyState::Finalized,
        pending_membership_plans: BTreeMap::new(),
        webrtc_plan_seen: false,
        current_session_generation: None,
        expected_peers: BTreeSet::new(),
        pair_roles: BTreeMap::new(),
        pair_retry_attempts: BTreeMap::new(),
        connected_pairs: BTreeSet::new(),
        pair_connected_reported: BTreeSet::new(),
        selected_pair_evidence: SelectedPairEvidence::default(),
        retrying_pairs: BTreeSet::new(),
        p2p_reconnected_pairs: BTreeSet::new(),
        ice_gathering_complete: BTreeSet::new(),
        last_ice_servers: Vec::new(),
        transport_status: None,
        p2p_deadline: None,
        p2p_retry_at: None,
        p2p_released: cli.p2p_release_file.is_none(),
        p2p_release_poll_at: cli.p2p_release_file.as_ref().map(|_| Instant::now()),
        p2p_rebuild_released: cli.p2p_rebuild_release_file.is_none(),
        p2p_rebuild_release_poll_at: None,
        pending_pair_directives: BTreeMap::new(),
        // Late joiners arm the relay probe on entry (see RELAY_SEND_SETTLE):
        // the GameStarting trigger pre-dates the join and never re-fires.
        relay_send_at: (cli.relay_payload.is_some() && lobby_state == LobbyState::Finalized)
            .then(|| checked_deadline(Instant::now(), RELAY_SEND_SETTLE)),
        relay_sent: false,
        relay_received_from: BTreeSet::new(),
        peer_status_from: BTreeSet::new(),
        sent_labels: BTreeMap::new(),
        received_labels: BTreeMap::new(),
        exchange_released: cli.exchange_release_file.is_none(),
        exchange_ready_reported: false,
        exchange_release_poll_at: None,
        unreliable_exchange_released: cli.unreliable_exchange_release_file.is_none(),
        exchange_reliable_ready_reported: false,
        unreliable_exchange_release_poll_at: None,
        pending_signals: BTreeMap::new(),
        drop_ice_from: None,
        run_deadline,
        linger_until: None,
        success_criteria_reported: false,
        success_release_poll_at: None,
        next_ping_at: checked_deadline(Instant::now(), PING_INTERVAL),
        pong_deadline: None,
        pong_grace_applied: false,
    };
    orchestrator.maybe_send_ready().await?;
    orchestrator.run_loop().await
}

/// Send `Authenticate` and consume `Authenticated` + `ProtocolInfo`.
async fn authenticate(ws: &mut WsStream, cli: &Cli) -> Result<u16, FatalError> {
    let message = ClientMessage::Authenticate {
        app_id: cli.app_id.clone(),
        sdk_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        platform: Some(cli.platform.clone()),
        game_data_format: Some(GameDataEncoding::Json),
        // v2 mode omits every v3 field so the wire shape is pure v2.
        protocol_version: cli.is_v3().then_some(cli.protocol_version),
        supported_transports: cli.is_v3().then(|| cli.transports()),
        supported_topologies: cli.is_v3().then(|| cli.topologies()),
    };
    wire::send_client_message(ws, &message)
        .await
        .map_err(|error| FatalError::connection(format!("{error:#}")))?;

    match next_handshake_message(ws).await? {
        ServerMessage::Authenticated { .. } => emit(&Event::Authenticated),
        ServerMessage::AuthenticationError { error, error_code } => {
            return Err(FatalError::protocol(format!(
                "app-ID handshake rejected: {error} ({error_code:?})"
            )));
        }
        other => {
            return Err(FatalError::protocol(format!(
                "expected Authenticated, got {other:?}"
            )));
        }
    }

    let negotiated_version =
        negotiated_version_from(next_handshake_message(ws).await?, cli.protocol_version)?;
    emit(&Event::ProtocolInfo { negotiated_version });
    Ok(negotiated_version)
}

fn negotiated_version_from(
    message: ServerMessage,
    offered_version: u16,
) -> Result<u16, FatalError> {
    match message {
        // Negotiated v2 omits the additive field by wire contract.
        ServerMessage::ProtocolInfo(info) => match info.protocol_version {
            None => Ok(2),
            Some(version) if (2..=3).contains(&version) && version <= offered_version => {
                Ok(version)
            }
            Some(version) => Err(FatalError::protocol(format!(
                "ProtocolInfo.protocol_version {version} is outside 2..=3 or exceeds offered version {offered_version}"
            ))),
        },
        other => Err(FatalError::protocol(format!(
            "expected ProtocolInfo, got {other:?}"
        ))),
    }
}

/// Create or join the room; returns our player id, the seated member ids, and
/// the room's lobby state at join time.
async fn join_room(
    ws: &mut WsStream,
    cli: &Cli,
    protocol_v3: bool,
) -> Result<
    (
        PlayerId,
        BTreeSet<PlayerId>,
        LobbyState,
        DeliveryAccountability,
    ),
    FatalError,
> {
    let max_players = u8::try_from(cli.peers)
        .map_err(|_overflow| FatalError::protocol(format!("--peers {} exceeds u8", cli.peers)))?;
    let message = ClientMessage::JoinRoom {
        game_name: cli.game_name.clone(),
        room_code: cli.join_code.clone(),
        player_name: cli.player_name.clone(),
        max_players: Some(max_players),
        supports_authority: Some(false),
        relay_transport: None,
    };
    wire::send_client_message(ws, &message)
        .await
        .map_err(|error| FatalError::connection(format!("{error:#}")))?;

    // Read until the atomic `RoomJoined` membership baseline. Connection-level
    // accountability frames can legitimately precede it. Membership deltas
    // remain accepted for compatibility and deterministic test channels.
    let mut accountability = DeliveryAccountability::new(protocol_v3);
    let mut early_joined: Vec<PlayerInfo> = Vec::new();
    let mut early_left: Vec<(PlayerId, Option<u32>, Option<u64>)> = Vec::new();
    loop {
        let message = next_handshake_message(ws).await?;
        if consume_join_accountability_preface(&mut accountability, &message)
            .map_err(FatalError::protocol)?
        {
            continue;
        }
        match message {
            ServerMessage::RoomJoined(payload) => {
                accountability
                    .rebaseline_snapshot(&payload.current_players)
                    .map_err(FatalError::protocol)?;
                for player in &early_joined {
                    accountability
                        .note_player_joined(player)
                        .map_err(FatalError::protocol)?;
                }
                for &(player_id, epoch, final_seq) in &early_left {
                    accountability
                        .note_player_left(player_id, epoch, final_seq)
                        .map_err(FatalError::protocol)?;
                }
                if cli.create_room {
                    emit(&Event::RoomCreated {
                        room_code: payload.room_code.clone(),
                    });
                }
                emit(&Event::RoomJoined {
                    room_id: payload.room_id,
                    player_id: payload.player_id,
                    lobby_state: payload.lobby_state.clone(),
                });
                let mut present: BTreeSet<PlayerId> = payload
                    .current_players
                    .iter()
                    .map(|player| player.id)
                    .collect();
                // Apply the deltas observed ahead of the baseline (set
                // semantics make this order-independent and idempotent).
                for player in early_joined {
                    present.insert(player.id);
                }
                for (id, _, _) in early_left {
                    present.remove(&id);
                }
                return Ok((
                    payload.player_id,
                    present,
                    payload.lobby_state,
                    accountability,
                ));
            }
            ServerMessage::PlayerJoined { player } => early_joined.push(player),
            ServerMessage::PlayerLeft {
                player_id,
                epoch,
                final_seq,
            } => early_left.push((player_id, epoch, final_seq)),
            ServerMessage::RoomJoinFailed { reason, error_code } => {
                return Err(FatalError::protocol(format!(
                    "room join failed: {reason} ({error_code:?})"
                )))
            }
            other => {
                return Err(FatalError::protocol(format!(
                    "expected RoomJoined, got {other:?}"
                )))
            }
        }
    }
}

fn consume_join_accountability_preface(
    accountability: &mut DeliveryAccountability,
    message: &ServerMessage,
) -> Result<bool, String> {
    let is_unsupported_format_error = matches!(
        message,
        ServerMessage::Error {
            error_code: Some(ErrorCode::UnsupportedGameDataFormat),
            ..
        }
    );
    accountability.observe_server_message(is_unsupported_format_error)?;
    match message {
        ServerMessage::DeliveryReport(report) => {
            accountability.record_report(report)?;
            Ok(true)
        }
        ServerMessage::RelayStats {
            interval_ms,
            sent_to_you,
            dropped_for_you,
            backpressure_events,
        } => {
            accountability.record_relay_stats(
                *interval_ms,
                *sent_to_you,
                *dropped_for_you,
                *backpressure_events,
            )?;
            Ok(true)
        }
        ServerMessage::Error {
            error_code: Some(ErrorCode::UnsupportedGameDataFormat),
            ..
        } => Ok(true),
        _ => Ok(false),
    }
}

fn restore_reconnected_member(
    present: &mut BTreeSet<PlayerId>,
    members_seen: &mut BTreeSet<PlayerId>,
    player_id: PlayerId,
) {
    present.insert(player_id);
    members_seen.insert(player_id);
}

fn changed_transport_status(previous: Option<bool>, connected_pair_count: usize) -> Option<bool> {
    let current = connected_pair_count > 0;
    (previous != Some(current)).then_some(current)
}

fn should_resolve_connected_pair(
    previous: Option<bool>,
    all_expected_pairs_connected: bool,
) -> bool {
    previous.is_some() || all_expected_pairs_connected
}

fn should_report_retry_gap(
    webrtc_plan_seen: bool,
    transport_status: Option<bool>,
    coordinated_rebuild: bool,
) -> bool {
    webrtc_plan_seen && transport_status.is_some() && !coordinated_rebuild
}

fn is_terminal_peer_connection_state(state: &RTCPeerConnectionState) -> bool {
    matches!(
        state,
        RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
    )
}

fn should_buffer_signal_for_unpaired_peer(
    expected_peers: &BTreeSet<PlayerId>,
    held_by_p2p_gate: bool,
    peer: PlayerId,
) -> bool {
    held_by_p2p_gate || expected_peers.contains(&peer)
}

fn is_current_session_generation(
    current: Option<SessionGeneration>,
    incoming: SessionGeneration,
) -> bool {
    current == Some(incoming)
}

fn try_buffer_planned_signal(
    pending: &mut BTreeMap<PlayerId, VecDeque<serde_json::Value>>,
    peer: PlayerId,
    signal: serde_json::Value,
) -> bool {
    let total = pending.values().map(VecDeque::len).sum::<usize>();
    if total >= MAX_PENDING_SIGNALS_TOTAL
        || pending
            .get(&peer)
            .is_some_and(|signals| signals.len() >= MAX_PENDING_SIGNALS_PER_PEER)
    {
        return false;
    }
    pending.entry(peer).or_default().push_back(signal);
    true
}

fn requires_authoritative_finalization_plan(negotiated_version: u16) -> bool {
    negotiated_version >= 3
}

#[derive(Debug, PartialEq, Eq)]
struct AuthoritativePeerDelta {
    removed: BTreeSet<PlayerId>,
    added: BTreeSet<PlayerId>,
    retained: BTreeSet<PlayerId>,
}

fn authoritative_peer_delta(
    current: &BTreeSet<PlayerId>,
    planned: &BTreeSet<PlayerId>,
) -> AuthoritativePeerDelta {
    AuthoritativePeerDelta {
        removed: current.difference(planned).copied().collect(),
        added: planned.difference(current).copied().collect(),
        retained: current.intersection(planned).copied().collect(),
    }
}

fn connection_targets_for_generation(
    delta: &AuthoritativePeerDelta,
    rebuild_retained: bool,
) -> BTreeSet<PlayerId> {
    let mut targets = delta.added.clone();
    if rebuild_retained {
        targets.extend(delta.retained.iter().copied());
    }
    targets
}

fn require_finalized_membership_plan(
    pending: &mut BTreeMap<PlayerId, u32>,
    negotiated_version: u16,
    lobby_state: Option<&LobbyState>,
    player_id: PlayerId,
    epoch: Option<u32>,
) -> bool {
    if negotiated_version < 3 || lobby_state != Some(&LobbyState::Finalized) {
        return false;
    }
    let Some(epoch) = epoch.filter(|epoch| *epoch > 0) else {
        return false;
    };
    pending.insert(player_id, epoch);
    true
}

fn clear_departed_membership_plan(
    pending: &mut BTreeMap<PlayerId, u32>,
    player_id: PlayerId,
    epoch: Option<u32>,
) {
    if pending.get(&player_id).copied() == epoch {
        pending.remove(&player_id);
    }
}

async fn next_handshake_message<S>(ws: &mut S) -> Result<ServerMessage, FatalError>
where
    S: futures_util::Stream<
            Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    match wire::next_server_message(ws, HANDSHAKE_TIMEOUT).await {
        Ok(message) => {
            validate_json_negotiated_server_message(&message)?;
            Ok(message)
        }
        Err(wire::ServerMessageReadError::Protocol(message)) => Err(FatalError::protocol(message)),
        Err(wire::ServerMessageReadError::Connection(message)) => {
            Err(FatalError::connection(message))
        }
    }
}

/// Input multiplexed by the main loop.
enum LoopInput {
    Server(Option<Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Engine(Option<EngineEvent>),
    SelectedPair(Option<SelectedPairProbeResult>),
    Tick,
}

fn validate_json_negotiated_server_message(message: &ServerMessage) -> Result<(), FatalError> {
    if matches!(message, ServerMessage::GameDataBinary { .. }) {
        return Err(FatalError::protocol(
            "received text GameDataBinary while game_data_format=json was negotiated",
        ));
    }
    Ok(())
}

/// Single-task state machine driving the session after the room is joined.
struct Orchestrator<'a> {
    cli: &'a Cli,
    ws: WsStream,
    engine: Engine,
    engine_rx: mpsc::UnboundedReceiver<EngineEvent>,
    selected_pair_tx: mpsc::UnboundedSender<SelectedPairProbeResult>,
    selected_pair_rx: mpsc::UnboundedReceiver<SelectedPairProbeResult>,
    my_id: PlayerId,
    negotiated_version: u16,
    /// Per-sender delivery sequence, exact-gap, and cumulative-counter state.
    accountability: DeliveryAccountability,
    /// Members currently seated in the room (self included).
    present: BTreeSet<PlayerId>,
    /// Every distinct member EVER observed in the room (self included);
    /// cumulative, never shrinks on departures. Drives the
    /// `--expect-total-peers` exit gate.
    members_seen: BTreeSet<PlayerId>,
    /// Most recent authoritative room lifecycle state; absent outside a room.
    lobby_state: Option<LobbyState>,
    /// The room has entered the Lobby state. `PlayerReady` is gated on this:
    /// the server rejects readiness while the room is still `Waiting`, and the
    /// transition happens slightly AFTER the final `PlayerJoined` broadcast,
    /// so counting members alone would race the transition.
    in_lobby: bool,
    ready_sent: bool,
    /// The explicit `StartGame` has been sent (room creator only, once). Guards
    /// against re-sending on subsequent `LobbyStateChanged` broadcasts.
    start_game_sent: bool,
    game_started: bool,
    /// This client entered an already-Finalized room (a late join / seat
    /// fill). Late joiners waive the peer-status wait: siblings' reports may
    /// legitimately pre-date the join and are never replayed by the server.
    late_joined: bool,
    /// A finalized-room entry/finalization is awaiting its full v3 plan.
    initial_session_plan_pending: bool,
    /// Finalized membership epochs awaiting the full plan triggered by them.
    pending_membership_plans: BTreeMap<PlayerId, u32>,
    /// A WebRTC `SessionPlan` was received (gates the transport-status criterion).
    webrtc_plan_seen: bool,
    /// Opaque generation from the latest authoritative `SessionPlan`. Every
    /// inbound/outbound signal must match it.
    current_session_generation: Option<SessionGeneration>,
    /// Peers the authoritative plan names (plus compatible `NewPeer` deltas).
    expected_peers: BTreeSet<PlayerId>,
    /// Server-authored glare role for each planned peer; retained so a
    /// coordinated retry reproduces the same initiator/responder assignment.
    pair_roles: BTreeMap<PlayerId, bool>,
    /// Latest coordinated retry attempt applied per peer.
    pair_retry_attempts: BTreeMap<PlayerId, u8>,
    /// Peers whose current generation has both channels open. Drives the
    /// `--exchange` obligations and Appendix G resolution.
    connected_pairs: BTreeSet<PlayerId>,
    /// Peers whose logical `p2p_pair_connected` event was emitted for the
    /// current room obligation. A coordinated rebuild clears current
    /// connectivity but retains this set so the fresh generation is not
    /// misreported as a second logical pair.
    pair_connected_reported: BTreeSet<PlayerId>,
    /// Connected physical links whose authoritative selected ICE-pair
    /// statistics have not reached the wrapper snapshot yet.
    selected_pair_evidence: SelectedPairEvidence,
    /// Peers whose previous generation was discarded for a coordinated retry
    /// and whose replacement is not connected yet. This blocks success during
    /// the reconnect gap without turning partial-connectivity fallback into an
    /// all-pairs requirement after the original P2P window expires.
    retrying_pairs: BTreeSet<PlayerId>,
    /// Peers whose current physical link completed a PairRetry generation.
    p2p_reconnected_pairs: BTreeSet<PlayerId>,
    /// Peers whose current connection generation emitted the terminal local
    /// ICE gathering callback. Harness-held success uses this as the exact
    /// outbound signal-ledger boundary.
    ice_gathering_complete: BTreeSet<PlayerId>,
    /// ICE servers from the most recent plan (also used for compatible `NewPeer`).
    last_ice_servers: Vec<IceServer>,
    /// Last reported overall WebRTC state, retained to suppress duplicates.
    transport_status: Option<bool>,
    /// When the P2P establishment window expires (set at first pairing).
    p2p_deadline: Option<Instant>,
    /// When to rebuild any still-incomplete planned pair.
    p2p_retry_at: Option<Instant>,
    /// Harness-only gate that keeps WebSocket traffic live before any peer
    /// connection is created.
    p2p_released: bool,
    /// Next poll for `--p2p-release-file` while pairing is held.
    p2p_release_poll_at: Option<Instant>,
    /// Whether the optional harness gate triggered a clean-path full rebuild.
    p2p_rebuild_released: bool,
    /// Next poll for `--p2p-rebuild-release-file` while held.
    p2p_rebuild_release_poll_at: Option<Instant>,
    /// Planned peers accumulated while the harness pairing gate is held.
    pending_pair_directives: BTreeMap<PlayerId, bool>,
    /// When to send the `--relay-payload` GameData (trigger + settle; the
    /// trigger is GameStarting, or room entry for a Finalized-room late join).
    relay_send_at: Option<Instant>,
    relay_sent: bool,
    /// Peers whose `relay_msg` GameData we observed over the floor.
    relay_received_from: BTreeSet<PlayerId>,
    /// Peers whose `PeerTransportStatus` fan-out we observed (any state).
    peer_status_from: BTreeSet<PlayerId>,
    /// Exchange bookkeeping: channel labels sent/received per peer.
    sent_labels: BTreeMap<PlayerId, BTreeSet<String>>,
    received_labels: BTreeMap<PlayerId, BTreeSet<String>>,
    /// Whether an optional harness gate has released the application exchange.
    exchange_released: bool,
    /// Whether the stable all-planned-pairs-open barrier event was emitted.
    exchange_ready_reported: bool,
    /// Next poll for `--exchange-release-file` while application traffic is held.
    exchange_release_poll_at: Option<Instant>,
    /// Whether the optional second gate released the exact unreliable half.
    unreliable_exchange_released: bool,
    /// Whether every peer completed the exact reliable half in both directions.
    exchange_reliable_ready_reported: bool,
    /// Next poll for `--unreliable-exchange-release-file` while held.
    unreliable_exchange_release_poll_at: Option<Instant>,
    /// Signals that arrived before their peer was paired (defensive: server
    /// FIFO ordering makes this unreachable in the documented flows).
    pending_signals: BTreeMap<PlayerId, VecDeque<serde_json::Value>>,
    /// Runtime UUID resolved from the harness-only `--drop-ice-from` ordinal
    /// when an authoritative plan supplies its `cNN` peer name.
    drop_ice_from: Option<PlayerId>,
    /// `--run-for-secs` soft cap.
    run_deadline: Instant,
    /// Set once all criteria are met; exit 0 when it elapses.
    linger_until: Option<Instant>,
    /// Whether the stable machine event announcing complete criteria was sent.
    success_criteria_reported: bool,
    /// Next poll for an optional harness-controlled release-file barrier.
    success_release_poll_at: Option<Instant>,
    /// Next keepalive `Ping` send (the mandatory client keepalive contract).
    next_ping_at: Instant,
    /// Deadline for the `Pong` answering the most recent `Ping`; `None` while
    /// no answer is outstanding.
    pong_deadline: Option<Instant>,
    /// Whether the one-shot [`PONG_DRAIN_GRACE`] extension was applied to the
    /// current deadline (the second expiry is fatal).
    pong_grace_applied: bool,
}

impl Orchestrator<'_> {
    async fn run_loop(&mut self) -> Result<i32, FatalError> {
        loop {
            if let Some(code) = self.process_timers().await? {
                return Ok(code);
            }
            let wake_at = self.next_wake();
            let input = tokio::select! {
                frame = futures_util::StreamExt::next(&mut self.ws) => LoopInput::Server(frame),
                event = self.engine_rx.recv() => LoopInput::Engine(event),
                result = self.selected_pair_rx.recv() => LoopInput::SelectedPair(result),
                _ = tokio::time::sleep_until(wake_at) => LoopInput::Tick,
            };
            match input {
                LoopInput::Server(Some(Ok(Message::Text(text)))) => {
                    // ANY inbound frame proves the connection and the server
                    // are alive, so it satisfies the keepalive liveness check —
                    // not just `Pong`. This is cleared BEFORE dispatch, so the
                    // very frame that kicks off a long handler (e.g. WebRTC
                    // pairing) already refreshes liveness; the loop cannot then
                    // declare a still-pending ping dead just because that
                    // handler ran past the window.
                    self.pong_deadline = None;
                    self.pong_grace_applied = false;
                    let message: ServerMessage = serde_json::from_str(&text).map_err(|error| {
                        FatalError::protocol(format!(
                            "invalid ServerMessage frame: {error}; text={text}"
                        ))
                    })?;
                    self.handle_server_message(message).await?;
                }
                LoopInput::Server(Some(Ok(Message::Close(frame)))) => {
                    self.accountability.observe_terminal();
                    return Err(FatalError::connection(format!(
                        "websocket closed by server before success criteria were met: {frame:?}"
                    )));
                }
                LoopInput::Server(Some(Ok(Message::Binary(wire)))) => {
                    self.pong_deadline = None;
                    self.pong_grace_applied = false;
                    self.accountability
                        .observe_server_message(false)
                        .map_err(FatalError::protocol)?;
                    return Err(FatalError::protocol(format!(
                        "received {}-byte binary WebSocket frame while game_data_format=json was negotiated",
                        wire.len()
                    )));
                }
                LoopInput::Server(Some(Ok(other)))
                    if wire::is_transparent_transport_control(&other) =>
                {
                    tracing::debug!(frame = ?other, "ignoring non-text frame");
                }
                LoopInput::Server(Some(Ok(other))) => {
                    self.accountability
                        .observe_server_message(false)
                        .map_err(FatalError::protocol)?;
                    tracing::debug!(frame = ?other, "ignoring non-text application frame");
                }
                LoopInput::Server(Some(Err(error))) => {
                    self.accountability.observe_terminal();
                    return Err(FatalError::connection(format!(
                        "websocket transport error: {error}"
                    )));
                }
                LoopInput::Server(None) => {
                    self.accountability.observe_terminal();
                    return Err(FatalError::connection(
                        "websocket closed by server before success criteria were met",
                    ));
                }
                LoopInput::Engine(Some(event)) => self.handle_engine_event(event).await?,
                LoopInput::Engine(None) => {
                    // Unreachable while the engine (which owns a sender) lives.
                    tracing::warn!("engine event channel closed");
                }
                LoopInput::SelectedPair(Some(result)) => {
                    self.handle_selected_pair_probe(result);
                }
                LoopInput::SelectedPair(None) => {
                    // Unreachable while the orchestrator owns the probe sender.
                    tracing::warn!("selected-pair probe channel closed");
                }
                LoopInput::Tick => {}
            }

            // FAULT INJECTION (--tick-stall-ms): deliberately BLOCK this
            // executor thread after every processed input — a `std::thread`
            // sleep, never an async one, because the injected fault is a game
            // loop that hogs the runtime instead of continuously driving it
            // (docs/protocol.md, "Delivery reliability and backpressure":
            // clients must continuously poll/drive their connection). On the
            // `--runtime current` flavor this starves the entire client,
            // reproducing the #131 reporter's client-side failure mode; the
            // starved-runtime conformance matrix uses it to pin the server's
            // slow-consumer contract as an executable boundary.
            if self.cli.tick_stall_ms > 0 {
                std::thread::sleep(Duration::from_millis(self.cli.tick_stall_ms));
            }
        }
    }

    /// Earliest pending timer (the run deadline at the latest), so the select
    /// loop always wakes for due work even on a silent wire.
    fn next_wake(&self) -> Instant {
        // Once a harness-held client has reported success, the soft run
        // deadline no longer applies: the release path or the binary-wide
        // hard watchdog is authoritative. Starting from the release poll or
        // post-release linger avoids a busy loop after `run_deadline` passes.
        let mut wake = harness_aware_base_wake(
            self.run_deadline,
            self.success_release_poll_at,
            self.linger_until,
            self.success_criteria_reported,
        );
        if !self.relay_sent {
            if let Some(at) = self.relay_send_at {
                wake = wake.min(at);
            }
        }
        if let Some(at) = self.p2p_deadline {
            wake = wake.min(at);
        }
        if let Some(at) = self.p2p_retry_at {
            wake = wake.min(at);
        }
        if let Some(at) = self.p2p_release_poll_at {
            wake = wake.min(at);
        }
        if let Some(at) = self.p2p_rebuild_release_poll_at {
            wake = wake.min(at);
        }
        if let Some(at) = self.linger_until {
            wake = wake.min(at);
        }
        if let Some(at) = self.success_release_poll_at {
            wake = wake.min(at);
        }
        if let Some(at) = self.selected_pair_evidence.poll_at {
            wake = wake.min(at);
        }
        if let Some(at) = self.exchange_release_poll_at {
            wake = wake.min(at);
        }
        if let Some(at) = self.unreliable_exchange_release_poll_at {
            wake = wake.min(at);
        }
        wake = wake.min(keepalive_wake(self.next_ping_at, self.pong_deadline));
        wake
    }

    /// Fire due timers; returns `Some(exit_code)` when the run is over.
    async fn process_timers(&mut self) -> Result<Option<i32>, FatalError> {
        let now = Instant::now();

        // Mandatory keepalive: send `Ping` on cadence and demand a timely
        // `Pong`. An unanswered ping is a broken connection or control path —
        // fail loudly rather than idle into the server's eviction.
        //
        // AT MOST ONE ping is outstanding at a time: a new `Ping` is sent only
        // once the previous one's `Pong` has cleared the deadline. A single
        // deadline then unambiguously tracks the single in-flight ping — a
        // stale `Pong` can never clear the deadline of a newer, still-pending
        // ping (the answer arrives while nothing is pending and is a no-op).
        //
        // The first deadline expiry only arms the drain grace
        // (see PONG_DRAIN_GRACE): a `Pong` already sitting in the socket
        // buffer gets one guaranteed read pass — the deadline check runs
        // before the `select!` reads the socket — before the miss is fatal.
        if self.pong_deadline.is_some_and(|at| now >= at) {
            if self.pong_grace_applied {
                return Err(FatalError::connection(format!(
                    "server did not answer Ping within {PONG_TIMEOUT:?} (+{PONG_DRAIN_GRACE:?} drain grace)"
                )));
            }
            self.pong_deadline = Some(checked_deadline(now, PONG_DRAIN_GRACE));
            self.pong_grace_applied = true;
        }
        if now >= self.next_ping_at && self.pong_deadline.is_none() {
            self.send_message(&ClientMessage::Ping).await?;
            self.next_ping_at = checked_deadline(now, PING_INTERVAL);
            self.pong_deadline = Some(checked_deadline(now, PONG_TIMEOUT));
            self.pong_grace_applied = false;
        }

        if !self.relay_sent && self.relay_send_at.is_some_and(|at| now >= at) {
            self.send_relay_payload().await?;
        }

        self.process_p2p_gate(now).await?;

        if self.p2p_retry_at.is_some_and(|at| now >= at) {
            self.retry_incomplete_pairs(now).await?;
        }

        if self.p2p_deadline.is_some_and(|at| now >= at) {
            // The current P2P window expired: report any real state change.
            self.resolve_transport_status().await?;
            self.p2p_deadline = None;
            self.p2p_retry_at = None;
            self.retrying_pairs.clear();
        }

        self.process_p2p_rebuild_gate(now).await?;
        self.process_exchange_gate(now).await?;
        self.process_unreliable_exchange_gate(now).await?;
        self.process_selected_pair_evidence(now);
        let ready_probes = take_ready_selected_pair_probes(&mut self.selected_pair_rx);
        let run_deadline = self.run_deadline;
        let run_deadline_due = apply_selected_pair_probes_before_run_deadline(
            ready_probes,
            |result| self.handle_selected_pair_probe(result),
            now,
            run_deadline,
        );

        self.arm_success_linger(now).await?;
        if self.linger_until.is_some_and(|at| now >= at) {
            // Criteria can regress during the linger: an authoritative plan or
            // a freshly connected pair adds new obligations (exchange,
            // peer-status). Re-validate at expiry; on regression clear the
            // linger and keep running — it is re-armed when criteria hold
            // again.
            if self.criteria_met() {
                return Ok(Some(EXIT_SUCCESS));
            }
            tracing::debug!(
                unmet = ?self.unmet_criteria(),
                "success criteria regressed during the exit linger; continuing"
            );
            self.linger_until = None;
        }

        if run_deadline_due {
            let release_pending = self.success_release_pending().await?;
            if should_defer_success_at_run_deadline(
                release_pending,
                self.success_criteria_reported,
                self.linger_until.is_some(),
            ) {
                return Ok(None);
            }
            if self.criteria_met() {
                return Ok(Some(EXIT_SUCCESS));
            }
            emit(&Event::Error {
                message: format!(
                    "--run-for-secs elapsed with unmet success criteria: {}",
                    self.unmet_criteria().join(", ")
                ),
            });
            return Ok(Some(EXIT_CRITERIA_UNMET));
        }
        Ok(None)
    }

    async fn arm_success_linger(&mut self, now: Instant) -> Result<(), FatalError> {
        if !self.criteria_met() {
            let release_pending =
                self.success_criteria_reported && self.success_release_pending().await?;
            self.success_release_poll_at =
                release_pending.then_some(checked_deadline(now, SUCCESS_RELEASE_POLL));
            return Ok(());
        }
        if self.cli.success_release_file.is_some() && !self.success_criteria_reported {
            emit(&Event::SuccessCriteriaMet);
            self.success_criteria_reported = true;
        }
        if self.success_release_pending().await? {
            self.linger_until = None;
            self.success_release_poll_at = Some(checked_deadline(now, SUCCESS_RELEASE_POLL));
        } else {
            self.success_release_poll_at = None;
            if self.linger_until.is_none() {
                self.linger_until = Some(checked_deadline(now, EXIT_LINGER));
            }
        }
        Ok(())
    }

    async fn success_release_pending(&self) -> Result<bool, FatalError> {
        Self::release_file_pending(
            self.cli.success_release_file.as_deref(),
            "--success-release-file",
        )
        .await
    }

    async fn exchange_release_pending(&self) -> Result<bool, FatalError> {
        Self::release_file_pending(
            self.cli.exchange_release_file.as_deref(),
            "--exchange-release-file",
        )
        .await
    }

    async fn unreliable_exchange_release_pending(&self) -> Result<bool, FatalError> {
        Self::release_file_pending(
            self.cli.unreliable_exchange_release_file.as_deref(),
            "--unreliable-exchange-release-file",
        )
        .await
    }

    async fn p2p_release_pending(&self) -> Result<bool, FatalError> {
        Self::release_file_pending(self.cli.p2p_release_file.as_deref(), "--p2p-release-file").await
    }

    async fn p2p_rebuild_release_pending(&self) -> Result<bool, FatalError> {
        Self::release_file_pending(
            self.cli.p2p_rebuild_release_file.as_deref(),
            "--p2p-rebuild-release-file",
        )
        .await
    }

    async fn process_p2p_rebuild_gate(&mut self, now: Instant) -> Result<(), FatalError> {
        if self.p2p_rebuild_released
            || !self.exchange_ready_reported
            || self
                .p2p_rebuild_release_poll_at
                .is_some_and(|poll_at| now < poll_at)
        {
            return Ok(());
        }
        if self.p2p_rebuild_release_pending().await? {
            self.p2p_rebuild_release_poll_at = Some(checked_deadline(now, SUCCESS_RELEASE_POLL));
            return Ok(());
        }

        self.p2p_rebuild_released = true;
        self.p2p_rebuild_release_poll_at = None;
        // The coordinated generation owns a fresh bounded P2P window even if
        // the lossy formation consumed the original one. Its reserved final
        // attempt is never scheduled by the automatic retry timer.
        self.p2p_deadline = Some(checked_deadline(
            now,
            Duration::from_secs(self.cli.p2p_timeout_secs),
        ));
        self.p2p_retry_at = None;
        let initiator_peers: Vec<PlayerId> = self
            .expected_peers
            .iter()
            .copied()
            .filter(|peer| self.pair_roles.get(peer) == Some(&true))
            .collect();
        for peer in initiator_peers {
            let attempt = self.cli.p2p_retry_count;
            if self
                .pair_retry_attempts
                .get(&peer)
                .is_some_and(|current| *current >= attempt)
            {
                return Err(FatalError::protocol(format!(
                    "--p2p-rebuild-release-file reserved retry generation was already consumed for {peer}"
                )));
            }
            self.request_pair_retry(peer, attempt).await?;
        }
        Ok(())
    }

    async fn process_p2p_gate(&mut self, now: Instant) -> Result<(), FatalError> {
        if self.p2p_released
            || self
                .p2p_release_poll_at
                .is_some_and(|poll_at| now < poll_at)
        {
            return Ok(());
        }
        if self.p2p_release_pending().await? {
            self.p2p_release_poll_at = Some(checked_deadline(now, SUCCESS_RELEASE_POLL));
            return Ok(());
        }

        self.p2p_released = true;
        self.p2p_release_poll_at = None;
        let pending = std::mem::take(&mut self.pending_pair_directives);
        emit(&Event::P2pGateReleased {
            pending_pairs: pending.len(),
        });
        for (peer, initiate) in pending {
            self.establish_pair(peer, initiate).await?;
        }
        Ok(())
    }

    async fn release_file_pending(
        path: Option<&std::path::Path>,
        flag: &'static str,
    ) -> Result<bool, FatalError> {
        let Some(path) = path else {
            return Ok(false);
        };
        let path = path.to_path_buf();
        let display = path.display().to_string();
        tokio::task::spawn_blocking(move || path.try_exists())
            .await
            .map_err(|error| {
                FatalError::connection(format!(
                    "inspect {flag} {display}: metadata task failed: {error}"
                ))
            })?
            .map(|exists| !exists)
            .map_err(|error| FatalError::connection(format!("inspect {flag} {display}: {error}")))
    }

    async fn process_exchange_gate(&mut self, now: Instant) -> Result<(), FatalError> {
        if !self.cli.exchange
            || self.exchange_released
            || !self.exchange_gate_ready()
            || self
                .exchange_release_poll_at
                .is_some_and(|poll_at| now < poll_at)
        {
            return Ok(());
        }
        if !self.exchange_ready_reported {
            emit(&Event::ExchangeReady);
            self.exchange_ready_reported = true;
        }
        if self.cli.p2p_rebuild_release_file.is_some()
            && (!self.p2p_rebuild_released
                || !self.expected_peers.is_subset(&self.p2p_reconnected_pairs))
        {
            return Ok(());
        }
        if self.exchange_release_pending().await? {
            self.exchange_release_poll_at = Some(checked_deadline(now, SUCCESS_RELEASE_POLL));
            return Ok(());
        }

        self.exchange_released = true;
        self.exchange_release_poll_at = None;
        let peers: Vec<PlayerId> = self.connected_pairs.iter().copied().collect();
        let labels = if self.cli.unreliable_exchange_release_file.is_some() {
            &[RELIABLE_LABEL][..]
        } else {
            &[RELIABLE_LABEL, UNRELIABLE_LABEL][..]
        };
        for peer in peers {
            self.send_exchange(peer, labels).await?;
        }
        Ok(())
    }

    async fn process_unreliable_exchange_gate(&mut self, now: Instant) -> Result<(), FatalError> {
        if self.unreliable_exchange_released
            || !self.exchange_released
            || !self.exchange_reliable_gate_ready()
            || self
                .unreliable_exchange_release_poll_at
                .is_some_and(|poll_at| now < poll_at)
        {
            return Ok(());
        }
        if !self.exchange_reliable_ready_reported {
            emit(&Event::ExchangeReliableReady);
            self.exchange_reliable_ready_reported = true;
        }
        if self.unreliable_exchange_release_pending().await? {
            self.unreliable_exchange_release_poll_at =
                Some(checked_deadline(now, SUCCESS_RELEASE_POLL));
            return Ok(());
        }

        self.unreliable_exchange_released = true;
        self.unreliable_exchange_release_poll_at = None;
        let peers: Vec<PlayerId> = self.connected_pairs.iter().copied().collect();
        for peer in peers {
            self.send_exchange(peer, &[UNRELIABLE_LABEL]).await?;
        }
        Ok(())
    }

    fn exchange_gate_ready(&self) -> bool {
        self.all_expected_pairs_connected()
            && self.expected_peers.iter().all(|peer| {
                !needs_ice_gathering_marker(
                    self.engine.is_paired(*peer),
                    self.ice_gathering_complete.contains(peer),
                )
            })
    }

    fn exchange_reliable_gate_ready(&self) -> bool {
        self.cli.unreliable_exchange_release_file.is_some()
            && self.all_expected_pairs_connected()
            && exchange_label_complete(
                &self.expected_peers,
                &self.sent_labels,
                &self.received_labels,
                RELIABLE_LABEL,
            )
    }

    async fn send_exchange(&mut self, peer: PlayerId, labels: &[&str]) -> Result<(), FatalError> {
        for &label in labels {
            if self
                .sent_labels
                .get(&peer)
                .is_some_and(|labels| labels.contains(label))
            {
                continue;
            }
            let Some(channel) = self.engine.channel(peer, label) else {
                return Err(FatalError::connection(format!(
                    "open pair with {peer} is missing channel {label}"
                )));
            };
            let text = format!(
                r#"{{"from":"{}","channel":"{}","seq":0}}"#,
                self.my_id, label
            );
            channel.send_text(&text).await.map_err(|error| {
                FatalError::connection(format!("send on {label} to {peer} failed: {error}"))
            })?;
            self.sent_labels
                .entry(peer)
                .or_default()
                .insert(label.to_string());
            emit(&Event::ChannelMessageSent {
                peer,
                label: label.to_string(),
                text,
            });
        }
        Ok(())
    }

    async fn handle_server_message(&mut self, message: ServerMessage) -> Result<(), FatalError> {
        validate_json_negotiated_server_message(&message)?;
        let is_unsupported_format_error = matches!(
            &message,
            ServerMessage::Error {
                error_code: Some(ErrorCode::UnsupportedGameDataFormat),
                ..
            }
        );
        self.accountability
            .observe_server_message(is_unsupported_format_error)
            .map_err(FatalError::protocol)?;
        match message {
            ServerMessage::RoomJoined(payload) => {
                self.accountability
                    .rebaseline_snapshot(&payload.current_players)
                    .map_err(FatalError::protocol)?;
            }
            ServerMessage::RoomLeft => {
                self.accountability.reset_room();
                self.initial_session_plan_pending = false;
                self.pending_membership_plans.clear();
                self.lobby_state = None;
                self.current_session_generation = None;
                self.pending_signals.clear();
            }
            ServerMessage::PlayerJoined { player } => {
                self.accountability
                    .note_player_joined(&player)
                    .map_err(FatalError::protocol)?;
                self.present.insert(player.id);
                self.members_seen.insert(player.id);
                if require_finalized_membership_plan(
                    &mut self.pending_membership_plans,
                    self.negotiated_version,
                    self.lobby_state.as_ref(),
                    player.id,
                    player.epoch,
                ) {
                    self.linger_until = None;
                }
                emit(&Event::PeerJoined {
                    player_id: player.id,
                });
                self.maybe_send_ready().await?;
            }
            ServerMessage::PlayerLeft {
                player_id,
                epoch,
                final_seq,
            } => {
                self.accountability
                    .note_player_left(player_id, epoch, final_seq)
                    .map_err(FatalError::protocol)?;
                self.present.remove(&player_id);
                clear_departed_membership_plan(
                    &mut self.pending_membership_plans,
                    player_id,
                    epoch,
                );
                // A departed peer can no longer satisfy any pairing-derived
                // criterion: drop it from the expected set (and drop any
                // buffered signals from it). This matters during staggered
                // teardown of host sessions: a sibling's departure can trigger
                // the server's host-failover replan, which transiently names
                // soon-to-exit members as new pairs — without this removal a
                // client could wait on a peer that is already gone.
                self.remove_pair_obligation(player_id).await;
                // Departure can either complete the remaining set or remove
                // the last live P2P path. Report real state transitions while
                // suppressing duplicate snapshots.
                if self.webrtc_plan_seen
                    && (self.transport_status.is_some()
                        || self.expected_peers.is_empty()
                        || self.all_expected_pairs_connected())
                {
                    self.resolve_transport_status().await?;
                }
                emit(&Event::PlayerLeft { player_id });
            }
            ServerMessage::GameStarting { peer_connections } => {
                self.game_started = true;
                self.lobby_state = Some(LobbyState::Finalized);
                if requires_authoritative_finalization_plan(self.negotiated_version) {
                    self.initial_session_plan_pending = true;
                    self.linger_until = None;
                }
                let is_authority = peer_connections
                    .iter()
                    .find(|peer| peer.player_id == self.my_id)
                    .map(|peer| peer.is_authority)
                    .unwrap_or(false);
                emit(&Event::GameStarting { is_authority });
                if self.cli.relay_payload.is_some() && !self.relay_sent {
                    self.relay_send_at = Some(checked_deadline(Instant::now(), RELAY_SEND_SETTLE));
                }
            }
            ServerMessage::SessionPlan(plan) => {
                self.initial_session_plan_pending = false;
                self.pending_membership_plans.clear();
                let generation_changed = self.current_session_generation != Some(plan.generation);
                self.current_session_generation = Some(plan.generation);
                if generation_changed {
                    self.pending_signals.clear();
                }
                self.drop_ice_from =
                    resolve_drop_ice_from(self.cli.drop_ice_from, self.my_id, &plan.peers)
                        .map_err(FatalError::protocol)?;
                emit(&Event::SessionPlan {
                    topology: plan.topology,
                    transport: plan.transport,
                    host: plan.host,
                    peers: plan
                        .peers
                        .iter()
                        .map(|peer| PlanPeer {
                            player_id: peer.player_id,
                            initiate: peer.initiate,
                        })
                        .collect(),
                    ice_servers_count: plan.ice_servers.len(),
                    fallback: plan.fallback,
                });
                if self.cli.leave_on_game_start {
                    return Ok(());
                }
                if plan.transport == Transport::Direct {
                    self.reject_unsupported_direct_plan(plan.direct_endpoint.as_ref())
                        .await?;
                }
                if plan.transport == Transport::WebRtc {
                    self.webrtc_plan_seen = true;
                    self.last_ice_servers = plan.ice_servers.clone();
                }
                let planned_peers = session_plan_peer_ids(plan.transport, &plan.peers, self.my_id);
                let delta = authoritative_peer_delta(&self.expected_peers, &planned_peers);
                let rebuild_retained = generation_changed && plan.transport == Transport::WebRtc;
                let mut added = connection_targets_for_generation(&delta, rebuild_retained);
                for peer in delta.removed {
                    self.remove_pair_obligation(peer).await;
                }
                if rebuild_retained {
                    for peer in delta.retained {
                        self.prepare_retained_pair_replacement(peer).await;
                    }
                    if self.webrtc_plan_seen && self.transport_status.is_some() {
                        self.resolve_transport_status().await?;
                    }
                }
                for peer in &plan.peers {
                    if plan.transport == Transport::WebRtc && peer.player_id != self.my_id {
                        self.pair_roles.insert(peer.player_id, peer.initiate);
                    }
                    if plan.transport == Transport::WebRtc && added.remove(&peer.player_id) {
                        if self.p2p_released {
                            self.establish_pair(peer.player_id, peer.initiate).await?;
                        } else {
                            self.expected_peers.insert(peer.player_id);
                            self.pending_pair_directives
                                .insert(peer.player_id, peer.initiate);
                        }
                    }
                }
                if self.expected_peers.is_empty() || self.all_expected_pairs_connected() {
                    self.p2p_deadline = None;
                }
                if self.transport_status.is_some()
                    || (plan.transport == Transport::WebRtc
                        && (self.expected_peers.is_empty() || self.all_expected_pairs_connected()))
                {
                    self.resolve_transport_status().await?;
                }
            }
            ServerMessage::NewPeer {
                peer_id,
                you_initiate,
            } => {
                emit(&Event::NewPeer {
                    peer_id,
                    you_initiate,
                });
                // Same pairing path as a plan peer (late join, Appendix E).
                self.pair_roles.insert(peer_id, you_initiate);
                if self.p2p_released {
                    self.establish_pair(peer_id, you_initiate).await?;
                } else {
                    self.expected_peers.insert(peer_id);
                    self.pending_pair_directives.insert(peer_id, you_initiate);
                }
            }
            ServerMessage::Signal {
                from,
                generation,
                signal,
            } => {
                self.handle_signal(from, generation, signal).await?;
            }
            ServerMessage::GameData {
                from_player,
                data,
                seq,
                epoch,
                class,
                key,
            } => {
                let disposition = self
                    .accountability
                    .record_game_data(from_player, seq, epoch, class, key)
                    .map_err(FatalError::protocol)?;
                if disposition == GameDataDisposition::Stale {
                    tracing::debug!(%from_player, ?epoch, ?seq, "discarding stale trailing GameData");
                    return Ok(());
                }
                if data.get("relay_msg").is_some() {
                    self.relay_received_from.insert(from_player);
                }
                emit(&Event::GameDataReceived {
                    from: from_player,
                    payload: data,
                });
            }
            ServerMessage::DeliveryReport(report) => {
                self.accountability
                    .record_report(&report)
                    .map_err(FatalError::protocol)?;
            }
            ServerMessage::RelayStats {
                interval_ms,
                sent_to_you,
                dropped_for_you,
                backpressure_events,
            } => {
                self.accountability
                    .record_relay_stats(
                        interval_ms,
                        sent_to_you,
                        dropped_for_you,
                        backpressure_events,
                    )
                    .map_err(FatalError::protocol)?;
            }
            ServerMessage::Reconnected(payload) => {
                self.accountability
                    .rebaseline_reconnected(&payload.current_players, &payload.sender_watermarks)
                    .map_err(FatalError::protocol)?;
                self.lobby_state = Some(payload.lobby_state.clone());
                self.in_lobby = payload.lobby_state == LobbyState::Lobby;
                self.game_started = payload.lobby_state == LobbyState::Finalized;
                if self.negotiated_version >= 3 && payload.lobby_state == LobbyState::Finalized {
                    self.initial_session_plan_pending = true;
                    self.linger_until = None;
                }
            }
            ServerMessage::PlayerReconnected { player_id, epoch } => {
                self.accountability
                    .note_player_reconnected(player_id, epoch)
                    .map_err(FatalError::protocol)?;
                restore_reconnected_member(&mut self.present, &mut self.members_seen, player_id);
                if require_finalized_membership_plan(
                    &mut self.pending_membership_plans,
                    self.negotiated_version,
                    self.lobby_state.as_ref(),
                    player_id,
                    epoch,
                ) {
                    self.linger_until = None;
                }
                emit(&Event::PeerJoined { player_id });
                self.maybe_send_ready().await?;
            }
            ServerMessage::SpectatorJoined(payload) => {
                self.accountability
                    .rebaseline_snapshot(&payload.current_players)
                    .map_err(FatalError::protocol)?;
            }
            ServerMessage::SpectatorLeft { .. } => {
                self.accountability.reset_room();
            }
            ServerMessage::PeerTransportStatus {
                peer_id,
                transport,
                connected,
            } => {
                self.peer_status_from.insert(peer_id);
                emit(&Event::PeerTransportStatus {
                    peer: peer_id,
                    transport,
                    connected,
                });
            }
            ServerMessage::Error {
                message,
                error_code: Some(ErrorCode::SlowConsumer),
            } => {
                // The server is closing this connection because it could not
                // drain its outbound queue in time. Surface it distinctly so a
                // run failure is attributable to consumption speed rather than
                // a generic server error; the imminent socket close (not this
                // frame) decides the outcome.
                tracing::warn!(%message, "server disconnected this client as a slow consumer");
                emit(&Event::Error {
                    message: format!("server disconnecting us as a slow consumer: {message}"),
                });
            }
            ServerMessage::Error {
                message,
                error_code,
            } => {
                // Server-reported errors are surfaced but non-fatal: the relay
                // floor (and the run window) decide the outcome.
                emit(&Event::Error {
                    message: format!("server error: {message} ({error_code:?})"),
                });
            }
            ServerMessage::LobbyStateChanged {
                lobby_state,
                ready_players,
                all_ready,
            } => {
                self.lobby_state = Some(lobby_state.clone());
                // Info-level so the harness's stderr capture records the lobby
                // progression (state, ready count, all_ready) for any later
                // "GameStarting not received" diagnosis.
                tracing::info!(
                    ?lobby_state,
                    ready = ready_players.len(),
                    all_ready,
                    "lobby state changed"
                );
                if lobby_state == LobbyState::Lobby {
                    self.in_lobby = true;
                    self.maybe_send_ready().await?;
                    // The lobby no longer auto-starts: the room creator issues
                    // the explicit StartGame that produces GameStarting once the
                    // server reports the full ready set.
                    self.maybe_send_start_game(all_ready).await?;
                }
            }
            ServerMessage::Pong => {
                // Keepalive round-trip complete.
                self.pong_deadline = None;
                self.pong_grace_applied = false;
            }
            other => {
                tracing::debug!(message = ?other, "ignoring server message");
            }
        }
        Ok(())
    }

    async fn handle_engine_event(&mut self, event: EngineEvent) -> Result<(), FatalError> {
        let (event_peer, generation) = event.peer_generation();
        if !self.engine.is_current_event(&event) {
            tracing::debug!(%event_peer, generation, "discarding stale peer-link callback");
            return Ok(());
        }
        match event {
            EngineEvent::LocalCandidate {
                peer,
                candidate_json,
                gathered,
                ..
            } => {
                // Crippled mode never reaches here (the engine drops gathered
                // candidates), so this is always a real trickle-ICE relay.
                self.send_signal(
                    peer,
                    SignalKind::IceCandidate,
                    json!({ "IceCandidate": candidate_json }),
                )
                .await?;
                // After the relay, never before: the event means "the peer can
                // see this candidate", which is what a harness asserts on.
                emit(&Event::LocalCandidate {
                    peer,
                    candidate_type: gathered.candidate_type,
                    address: gathered.address,
                    port: gathered.port,
                    protocol: gathered.protocol,
                });
            }
            EngineEvent::IceGatheringComplete { peer, .. } => {
                self.ice_gathering_complete.insert(peer);
            }
            EngineEvent::PcState { peer, state, .. } => {
                emit(&Event::PcState {
                    peer,
                    state: state.to_string(),
                });
                if is_terminal_peer_connection_state(&state) {
                    self.handle_peer_transport_loss(peer).await?;
                }
            }
            EngineEvent::RemoteChannel {
                peer,
                label,
                channel,
                ..
            } => {
                self.engine.store_remote_channel(peer, label, channel);
            }
            EngineEvent::ChannelOpen { peer, label, .. } => {
                emit(&Event::ChannelOpen {
                    peer,
                    label: label.clone(),
                });
                if self.engine.note_channel_open(peer, &label) {
                    self.on_pair_connected(peer).await?;
                }
            }
            EngineEvent::ChannelClosed { peer, label, .. } => {
                emit(&Event::ChannelClosed { peer, label });
                self.handle_peer_transport_loss(peer).await?;
            }
            EngineEvent::ChannelMessage {
                peer, label, text, ..
            } => {
                self.received_labels
                    .entry(peer)
                    .or_default()
                    .insert(label.clone());
                emit(&Event::ChannelMessage { peer, label, text });
            }
        }
        Ok(())
    }

    /// Tear down an unusable P2P link while retaining its planned obligation.
    async fn handle_peer_transport_loss(&mut self, peer: PlayerId) -> Result<(), FatalError> {
        self.selected_pair_evidence.clear(peer);
        self.connected_pairs.remove(&peer);
        self.p2p_reconnected_pairs.remove(&peer);
        self.ice_gathering_complete.remove(&peer);
        self.sent_labels.remove(&peer);
        self.received_labels.remove(&peer);
        self.pending_signals.remove(&peer);
        if let Err(error) = self.engine.remove_peer(peer).await {
            let message = format!(
                "failed to close unusable peer connection {peer}; continuing on relay: {error:#}"
            );
            tracing::warn!(%peer, %error, "peer cleanup failed while engaging relay fallback");
            emit(&Event::Error { message });
        }
        if self.webrtc_plan_seen {
            self.resolve_transport_status().await?;
        }
        Ok(())
    }

    /// Retire one retained physical link before applying a newer authoritative
    /// wire generation. Logical pair/exchange observations deliberately stay
    /// intact: the replacement is a transport generation of the same planned
    /// peer obligation, not a second gameplay peer.
    async fn prepare_retained_pair_replacement(&mut self, peer: PlayerId) {
        self.selected_pair_evidence.clear(peer);
        self.connected_pairs.remove(&peer);
        self.p2p_reconnected_pairs.remove(&peer);
        self.ice_gathering_complete.remove(&peer);
        self.pair_retry_attempts.remove(&peer);
        self.retrying_pairs.insert(peer);
        self.pending_pair_directives.remove(&peer);
        self.pending_signals.remove(&peer);
        if let Err(error) = self.engine.remove_peer(peer).await {
            let message = format!(
                "failed to close superseded peer connection {peer}; continuing on relay: {error:#}"
            );
            tracing::warn!(%peer, %error, "peer cleanup failed during authoritative replacement");
            emit(&Event::Error { message });
        }
    }

    /// Pair with `peer` per the server's directive: create the connection,
    /// offer when told to, then drain any defensively buffered signals.
    async fn establish_pair(&mut self, peer: PlayerId, initiate: bool) -> Result<(), FatalError> {
        self.establish_pair_link(peer, initiate, PairGeneration::Initial)
            .await?;
        if let Some(buffered) = self.pending_signals.remove(&peer) {
            for signal in buffered {
                self.apply_signal(peer, signal).await?;
            }
        }
        Ok(())
    }

    /// Create one link generation without recursively draining signals. Retry
    /// handling calls this after clearing the old generation; normal pairing
    /// uses [`Self::establish_pair`] to add the defensive signal drain.
    async fn establish_pair_link(
        &mut self,
        peer: PlayerId,
        initiate: bool,
        generation: PairGeneration,
    ) -> Result<(), FatalError> {
        if peer == self.my_id {
            tracing::warn!(%peer, "server asked us to pair with ourselves; ignoring");
            return Ok(());
        }
        if self.cli.leave_on_game_start {
            // A seat-vacating client logs directives (events were already
            // emitted by the caller) but does not act on them,
            // so it produces zero signaling traffic before departing.
            tracing::debug!(%peer, "leave-on-game-start: skipping pairing directive");
            return Ok(());
        }
        let newly_expected = self.expected_peers.insert(peer);
        let needs_connection = !self.engine.is_paired(peer);
        if needs_connection {
            self.ice_gathering_complete.remove(&peer);
        }
        if generation.arms_p2p_window()
            && (newly_expected || needs_connection)
            && !self.connected_pairs.contains(&peer)
        {
            arm_pair_window(
                &mut self.p2p_deadline,
                &mut self.p2p_retry_at,
                generation,
                automatic_p2p_retry_count(
                    self.cli.p2p_rebuild_release_file.is_some(),
                    self.cli.p2p_retry_count,
                ),
                Duration::from_secs(self.cli.p2p_timeout_secs),
                Instant::now(),
            );
        }
        self.pair_roles.entry(peer).or_insert(initiate);
        let ice_servers = self.last_ice_servers.clone();
        match self.engine.pair_with(peer, initiate, &ice_servers).await {
            Ok(Some(offer_sdp)) => {
                self.send_signal(peer, SignalKind::Offer, json!({ "Offer": offer_sdp }))
                    .await?;
            }
            Ok(None) => {}
            Err(error) => {
                // A single failed pair is not fatal: the p2p timeout resolves
                // the overall status and the relay floor still carries data.
                emit(&Event::Error {
                    message: format!("pairing with {peer} failed: {error:#}"),
                });
            }
        }
        Ok(())
    }

    /// Ask the peer to rebuild the same planned link, then rebuild locally.
    /// The marker is sent first; the server preserves per-sender FIFO, so a
    /// responder processes it and installs its fresh peer connection before
    /// the initiator's new Offer arrives.
    async fn request_pair_retry(&mut self, peer: PlayerId, attempt: u8) -> Result<(), FatalError> {
        self.send_signal(peer, SignalKind::PairRetry, json!({ "PairRetry": attempt }))
            .await?;
        self.rebuild_pair(peer, attempt).await
    }

    /// Apply a local or remote retry exactly once per attempt number.
    async fn rebuild_pair(&mut self, peer: PlayerId, attempt: u8) -> Result<(), FatalError> {
        if self
            .pair_retry_attempts
            .get(&peer)
            .is_some_and(|current| *current >= attempt)
        {
            return Ok(());
        }
        let coordinated_rebuild = is_coordinated_p2p_rebuild_attempt(
            self.cli.p2p_rebuild_release_file.is_some(),
            attempt,
            self.cli.p2p_retry_count,
        );
        if self.p2p_deadline.is_none() && !coordinated_rebuild {
            tracing::debug!(%peer, attempt, "ignoring pair retry after the original P2P window");
            return Ok(());
        }
        let initiate = *self.pair_roles.get(&peer).ok_or_else(|| {
            FatalError::protocol(format!("pair retry from unplanned peer {peer}"))
        })?;
        self.pair_retry_attempts.insert(peer, attempt);
        self.retrying_pairs.insert(peer);
        self.selected_pair_evidence.clear(peer);
        self.p2p_reconnected_pairs.remove(&peer);
        self.connected_pairs.remove(&peer);
        self.ice_gathering_complete.remove(&peer);
        self.sent_labels.remove(&peer);
        self.received_labels.remove(&peer);
        self.pending_signals.remove(&peer);
        self.pending_pair_directives.remove(&peer);
        self.engine.remove_peer(peer).await.map_err(|error| {
            FatalError::connection(format!(
                "close incomplete peer connection {peer}: {error:#}"
            ))
        })?;
        if should_report_retry_gap(
            self.webrtc_plan_seen,
            self.transport_status,
            coordinated_rebuild,
        ) {
            self.resolve_transport_status().await?;
        }
        self.establish_pair_link(peer, initiate, PairGeneration::Retry)
            .await
    }

    async fn retry_incomplete_pairs(&mut self, now: Instant) -> Result<(), FatalError> {
        self.p2p_retry_at = None;
        let retryable = retryable_missing_peers(
            &self.expected_peers,
            &self.connected_pairs,
            &self.pair_retry_attempts,
            automatic_p2p_retry_count(
                self.cli.p2p_rebuild_release_file.is_some(),
                self.cli.p2p_retry_count,
            ),
        );
        for peer in retryable {
            let attempt = self
                .pair_retry_attempts
                .get(&peer)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            self.request_pair_retry(peer, attempt).await?;
        }
        let another_retry_possible = !retryable_missing_peers(
            &self.expected_peers,
            &self.connected_pairs,
            &self.pair_retry_attempts,
            automatic_p2p_retry_count(
                self.cli.p2p_rebuild_release_file.is_some(),
                self.cli.p2p_retry_count,
            ),
        )
        .is_empty();
        self.p2p_retry_at = another_retry_possible
            .then(|| checked_deadline(now, p2p_retry_delay(self.cli.p2p_timeout_secs)));
        Ok(())
    }

    async fn remove_pair_obligation(&mut self, peer: PlayerId) {
        self.expected_peers.remove(&peer);
        self.pair_roles.remove(&peer);
        self.pair_retry_attempts.remove(&peer);
        self.connected_pairs.remove(&peer);
        self.pair_connected_reported.remove(&peer);
        self.selected_pair_evidence.clear(peer);
        self.retrying_pairs.remove(&peer);
        self.p2p_reconnected_pairs.remove(&peer);
        self.ice_gathering_complete.remove(&peer);
        self.peer_status_from.remove(&peer);
        self.sent_labels.remove(&peer);
        self.received_labels.remove(&peer);
        self.pending_signals.remove(&peer);
        if let Err(error) = self.engine.remove_peer(peer).await {
            tracing::debug!(%peer, %error, "failed to close removed peer connection");
        }
    }

    /// Emit `signal_received` and route an inbound signal (buffering it when
    /// the peer is not paired yet).
    async fn handle_signal(
        &mut self,
        from: PlayerId,
        generation: SessionGeneration,
        signal: serde_json::Value,
    ) -> Result<(), FatalError> {
        if !is_current_session_generation(self.current_session_generation, generation) {
            tracing::debug!(
                %from,
                %generation,
                current_generation = ?self.current_session_generation,
                "discarding signal outside the current authoritative generation"
            );
            return Ok(());
        }
        let kind = SignalKind::classify(&signal);
        emit(&Event::SignalReceived { from, kind });
        if kind == SignalKind::PairRetry {
            return self.apply_signal(from, signal).await;
        }
        if !self.engine.is_paired(from) {
            if should_buffer_signal_for_unpaired_peer(
                &self.expected_peers,
                self.pending_pair_directives.contains_key(&from),
                from,
            ) {
                if !try_buffer_planned_signal(&mut self.pending_signals, from, signal) {
                    tracing::warn!(%from, "dropping planned-peer signal because the defensive buffer is full");
                }
            } else {
                tracing::debug!(%from, "discarding signal from a peer absent from the authoritative plan");
            }
            return Ok(());
        }
        self.apply_signal(from, signal).await
    }

    /// Feed a signal into the engine. Engine-level failures are surfaced as
    /// error events but never abort the run (the relay floor stays live).
    async fn apply_signal(
        &mut self,
        from: PlayerId,
        signal: serde_json::Value,
    ) -> Result<(), FatalError> {
        let kind = SignalKind::classify(&signal);
        let result = match kind {
            SignalKind::Offer => match signal.get("Offer").and_then(|sdp| sdp.as_str()) {
                Some(sdp) => match self.engine.handle_offer(from, sdp.to_string()).await {
                    Ok(answer_sdp) => {
                        self.send_signal(from, SignalKind::Answer, json!({ "Answer": answer_sdp }))
                            .await?;
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                None => Err(anyhow::anyhow!("Offer payload is not a string")),
            },
            SignalKind::Answer => match signal.get("Answer").and_then(|sdp| sdp.as_str()) {
                Some(sdp) => self.engine.handle_answer(from, sdp.to_string()).await,
                None => Err(anyhow::anyhow!("Answer payload is not a string")),
            },
            SignalKind::IceCandidate => {
                let pairwise_drop = self.drop_ice_from == Some(from);
                if self.cli.cripple_ice || pairwise_drop {
                    // Belt and braces with the engine's loopback-only cripple
                    // mode: selected inbound candidates are dropped, never
                    // applied.
                    tracing::debug!(%from, "fault injection: dropping inbound ICE candidate");
                    if pairwise_drop {
                        emit(&Event::IceCandidateDropped { from });
                    }
                    Ok(())
                } else {
                    match signal.get("IceCandidate").and_then(|c| c.as_str()) {
                        Some(payload) => self.engine.handle_remote_candidate(from, payload).await,
                        None => Err(anyhow::anyhow!("IceCandidate payload is not a string")),
                    }
                }
            }
            SignalKind::PairRetry => match signal.get("PairRetry").and_then(|value| value.as_u64())
            {
                Some(attempt) if attempt > 0 && attempt <= u64::from(self.cli.p2p_retry_count) => {
                    self.rebuild_pair(from, attempt as u8)
                        .await
                        .map_err(|error| anyhow::anyhow!(error.message))
                }
                Some(attempt) => Err(anyhow::anyhow!(
                    "PairRetry attempt {attempt} is outside the configured retry budget"
                )),
                None => Err(anyhow::anyhow!(
                    "PairRetry payload is not a positive integer"
                )),
            },
            SignalKind::Other => Err(anyhow::anyhow!(
                "signal does not match the matchbox PeerSignal convention"
            )),
        };
        if let Err(error) = result {
            emit(&Event::Error {
                message: format!("signal from {from} failed: {error:#}"),
            });
        }
        Ok(())
    }

    /// Both channels toward `peer` are open: emit the pair event, run the
    /// optional exchange, and check the all-pairs resolution condition.
    async fn on_pair_connected(&mut self, peer: PlayerId) -> Result<(), FatalError> {
        self.selected_pair_evidence.require(peer);
        self.start_selected_pair_probe(peer);
        let coordinated_reconnect = self.retrying_pairs.contains(&peer)
            && self.pair_retry_attempts.get(&peer).is_some_and(|attempt| {
                is_coordinated_p2p_rebuild_attempt(
                    self.cli.p2p_rebuild_release_file.is_some(),
                    *attempt,
                    self.cli.p2p_retry_count,
                )
            });
        if note_current_pair_connected(
            &mut self.connected_pairs,
            &mut self.pair_connected_reported,
            &mut self.retrying_pairs,
            peer,
        ) {
            emit(&Event::P2pPairConnected { peer });
        }
        if coordinated_reconnect {
            self.p2p_reconnected_pairs.insert(peer);
            emit(&Event::P2pPairReconnected { peer });
        }
        if self.cli.exchange && self.exchange_released {
            let labels = if self.unreliable_exchange_released {
                &[RELIABLE_LABEL, UNRELIABLE_LABEL][..]
            } else {
                &[RELIABLE_LABEL][..]
            };
            self.send_exchange(peer, labels).await?;
        }
        // Initial resolution waits for all pairs; after any prior resolution,
        // one late pair is enough to change the overall any-pair state.
        if should_resolve_connected_pair(self.transport_status, self.all_expected_pairs_connected())
        {
            if self.all_expected_pairs_connected() {
                self.p2p_retry_at = None;
            }
            self.resolve_transport_status().await?;
        }
        Ok(())
    }

    fn start_selected_pair_probe(&mut self, peer: PlayerId) {
        if self.selected_pair_evidence.begin_probe(peer)
            && !self
                .engine
                .start_selected_candidate_pair_probe(peer, self.selected_pair_tx.clone())
        {
            self.selected_pair_evidence
                .probe_unavailable(peer, Instant::now());
        }
    }

    fn handle_selected_pair_probe(&mut self, result: SelectedPairProbeResult) {
        let SelectedPairProbeResult {
            peer,
            generation,
            completed_at,
            selected,
        } = result;
        if !self.engine.is_current_generation(peer, generation) {
            tracing::debug!(%peer, generation, "discarding stale selected-pair probe");
            return;
        }
        let evidence_deadline =
            selected_pair_evidence_deadline(self.run_deadline, self.success_criteria_reported);
        match self.selected_pair_evidence.complete_probe(
            peer,
            completed_at,
            evidence_deadline,
            selected.is_some(),
        ) {
            SelectedPairProbeDisposition::Observed => {
                if let Some(selected) = selected {
                    emit(&Event::SelectedCandidatePair {
                        peer,
                        local_candidate_type: selected.local_candidate_type,
                        remote_candidate_type: selected.remote_candidate_type,
                        local_candidate_address: selected.local_candidate_address,
                        remote_candidate_address: selected.remote_candidate_address,
                    });
                }
            }
            SelectedPairProbeDisposition::Retry => {
                tracing::debug!(%peer, "selected ICE candidate pair evidence is pending");
            }
            SelectedPairProbeDisposition::Ignore => {}
        }
    }

    fn process_selected_pair_evidence(&mut self, now: Instant) {
        let due = self.selected_pair_evidence.take_due(now);
        for peer in due {
            if self.connected_pairs.contains(&peer) {
                self.start_selected_pair_probe(peer);
            } else {
                self.selected_pair_evidence.clear(peer);
            }
        }
    }

    /// Appendix G early-resolution condition: at least one expected pair, and
    /// every CURRENTLY expected peer's pair is connected (departed peers were
    /// removed from the expectation by `PlayerLeft`).
    fn all_expected_pairs_connected(&self) -> bool {
        !self.expected_peers.is_empty()
            && self
                .expected_peers
                .iter()
                .all(|peer| self.connected_pairs.contains(peer))
    }

    /// Report a changed overall WebRTC state; suppress duplicate snapshots.
    async fn resolve_transport_status(&mut self) -> Result<(), FatalError> {
        let Some(connected) =
            changed_transport_status(self.transport_status, self.connected_pairs.len())
        else {
            return Ok(());
        };
        self.send_message(&ClientMessage::TransportStatus {
            transport: Transport::WebRtc,
            connected,
        })
        .await?;
        self.transport_status = Some(connected);
        emit(&Event::TransportStatusSent {
            transport: Transport::WebRtc,
            connected,
        });
        if !connected {
            emit(&Event::FallbackEngaged);
        }
        Ok(())
    }

    /// The native reference client demonstrates WebRTC and the relay floor; it
    /// does not implement a game-specific direct socket. Reject Direct loudly
    /// and report the fallback instead of silently treating its peer list as
    /// empty while having advertised support through a custom CLI override.
    async fn reject_unsupported_direct_plan(
        &mut self,
        endpoint: Option<&DirectEndpoint>,
    ) -> Result<(), FatalError> {
        reject_unsupported_direct_plan_with(
            endpoint,
            |message| async move { self.send_message(&message).await },
            |event| emit(&event),
        )
        .await
    }

    /// Send `PlayerReady` once the expected member count is seated AND the
    /// server has moved the room into the Lobby state (readiness is rejected
    /// while the room is `Waiting`; the `LobbyStateChanged{lobby}` broadcast
    /// is the deterministic go signal).
    async fn maybe_send_ready(&mut self) -> Result<(), FatalError> {
        if !self.ready_sent && self.in_lobby && self.present.len() >= self.cli.peers {
            self.send_message(&ClientMessage::PlayerReady).await?;
            self.ready_sent = true;
        }
        Ok(())
    }

    /// Send the explicit `StartGame` that finalizes the lobby, exactly once,
    /// when this client created the room AND the server reports every current
    /// member ready (`LobbyStateChanged.all_ready`).
    ///
    /// The protocol no longer auto-starts a full, all-ready room: finalization
    /// is driven by an explicit `StartGame` from the authority — or, when no
    /// authority is designated (the interop rooms never set one), any member.
    /// The room creator is elected as that member here: it is always a v3
    /// participant that is present through finalization, so the choice is
    /// deterministic and needs no cross-client coordination. Joiners send no
    /// `StartGame`; they simply await the `GameStarting` the creator's call
    /// produces. `all_ready` already implies a full, seated, ready room (every
    /// client gates `PlayerReady` on having seen all `--peers` members), and the
    /// server re-checks readiness under its room lock, so this never races a
    /// late joiner. The send is idempotent-guarded by `start_game_sent`.
    ///
    /// Assumption: readiness is monotonic until finalize — no member leaves or
    /// un-readies between `all_ready` and the server processing this `StartGame`.
    /// That holds for every interop scenario (rooms cap at `--peers`, so no late
    /// joiner can un-ready the set, and the only departures are AFTER
    /// `GameStarting`). A pre-finalize departure is a deliberate non-goal: it
    /// could leave the latch set after a `NotReady`, and a production game client
    /// (not this test driver) would re-issue `StartGame` on the next ready set.
    async fn maybe_send_start_game(&mut self, all_ready: bool) -> Result<(), FatalError> {
        if self.cli.create_room && all_ready && !self.start_game_sent && !self.game_started {
            self.send_message(&ClientMessage::StartGame).await?;
            self.start_game_sent = true;
            tracing::info!("all members ready; sent StartGame to finalize the lobby");
        }
        Ok(())
    }

    /// Send the `--relay-payload` GameData over the relay floor.
    async fn send_relay_payload(&mut self) -> Result<(), FatalError> {
        let Some(text) = self.cli.relay_payload.clone() else {
            self.relay_sent = true;
            return Ok(());
        };
        wire::send_game_data(&mut self.ws, json!({ "relay_msg": text }))
            .await
            .map_err(|error| FatalError::connection(format!("{error:#}")))?;
        self.relay_sent = true;
        self.relay_send_at = None;
        emit(&Event::GameDataSent);
        Ok(())
    }

    async fn send_signal(
        &mut self,
        to: PlayerId,
        kind: SignalKind,
        signal: serde_json::Value,
    ) -> Result<(), FatalError> {
        let generation = self.current_session_generation.ok_or_else(|| {
            FatalError::protocol(format!(
                "cannot signal peer {to} before an authoritative SessionPlan"
            ))
        })?;
        self.send_message(&ClientMessage::Signal {
            to,
            generation,
            signal,
        })
        .await?;
        emit(&Event::SignalSent { to, kind });
        Ok(())
    }

    async fn send_message(&mut self, message: &ClientMessage) -> Result<(), FatalError> {
        wire::send_client_message(&mut self.ws, message)
            .await
            .map_err(|error| FatalError::connection(format!("{error:#}")))
    }

    /// The flag-driven minimum this client must observe to exit 0. Deep
    /// correctness assertions live in the interop harness, not here.
    fn criteria_met(&self) -> bool {
        self.unmet_criteria().is_empty()
    }

    /// Human-readable list of unmet criteria (for the failure diagnostics).
    fn unmet_criteria(&self) -> Vec<String> {
        let mut unmet = Vec::new();
        if !self.game_started {
            unmet.push("GameStarting not received".to_string());
        }
        // The `--expect-total-peers` gate: late-join incumbents must not exit
        // before the joiner has arrived. For default runs this equals the
        // ready barrier's member count and is met before finalization.
        if self.members_seen.len() < self.cli.effective_total_peers() {
            unmet.push(format!(
                "observed {} of {} expected distinct members",
                self.members_seen.len(),
                self.cli.effective_total_peers()
            ));
        }
        if self.initial_session_plan_pending || !self.pending_membership_plans.is_empty() {
            unmet.push(format!(
                "awaiting authoritative SessionPlan (session_pending={}, membership_epochs={})",
                self.initial_session_plan_pending,
                self.pending_membership_plans.len()
            ));
        }
        if self.webrtc_session_expected() {
            self.selected_pair_evidence.append_unmet(&mut unmet);
            for peer in &self.retrying_pairs {
                unmet.push(format!("pair retry in progress for {peer}"));
            }
            if self.transport_status.is_none() {
                unmet.push("transport status not resolved".to_string());
            }
            // Wait for each expected pair peer's own status fan-out before
            // exiting. Expected pairs are v3 + webrtc by the session
            // predicate, and a reference peer always resolves its status
            // (all-pairs-connected or its own p2p timeout), so this cannot
            // deadlock — it only prevents this process from disconnecting
            // before slower siblings' reports propagate, which would make
            // multi-process fan-out assertions racy. Waived after a late
            // join: fan-outs fire once, at report time, so reports that
            // pre-date this client's entry are never observable and waiting
            // for them would hang (the server replays nothing).
            if !self.late_joined {
                for peer in &self.expected_peers {
                    if !self.peer_status_from.contains(peer) {
                        unmet.push(format!("no PeerTransportStatus from {peer}"));
                    }
                }
            }
            if self.cli.success_release_file.is_some() {
                for peer in &self.expected_peers {
                    if needs_ice_gathering_marker(
                        self.engine.is_paired(*peer),
                        self.ice_gathering_complete.contains(peer),
                    ) {
                        unmet.push(format!("ICE gathering incomplete for {peer}"));
                    }
                }
            }
        }
        if self.cli.exchange {
            // Exchange obligations cover the expected peers whose pair
            // actually connected: in a partially connected session (e.g. an
            // ICE-crippled sibling) a never-connected pair owes no channel
            // traffic — its failure is reported through the transport status,
            // not by hanging the exchange. Fully connected sessions are
            // unchanged: status resolution requires every pair connected, so
            // all expected peers carry obligations by exit time.
            for peer in self
                .expected_peers
                .iter()
                .filter(|peer| self.connected_pairs.contains(*peer))
            {
                for (direction, labels) in [
                    ("sent to", self.sent_labels.get(peer)),
                    ("received from", self.received_labels.get(peer)),
                ] {
                    let complete = labels.is_some_and(|labels| {
                        labels.contains(RELIABLE_LABEL) && labels.contains(UNRELIABLE_LABEL)
                    });
                    if !complete {
                        unmet.push(format!("exchange incomplete ({direction} {peer})"));
                    }
                }
            }
        }
        if self.cli.relay_payload.is_some() {
            if !self.relay_sent {
                unmet.push("relay payload not sent".to_string());
            }
            // Every sibling sends a relay payload in these scenarios; expect
            // one from each of the other `peers - 1` members. Waived after a
            // late join (mirroring the peer-status waiver): GameData fans out
            // at send time and is never replayed, so payloads sent before
            // this client's entry are unobservable and waiting for them
            // would hang. The late joiner's own send stays required.
            if !self.late_joined
                && self.relay_received_from.len().saturating_add(1) < self.cli.peers
            {
                unmet.push(format!(
                    "relay payloads observed from {} of {} peers",
                    self.relay_received_from.len(),
                    self.cli.peers.saturating_sub(1)
                ));
            }
        }
        unmet
    }

    /// A WebRTC plan with at least one pairing was issued, so the Appendix G
    /// status report is owed before this client may exit successfully.
    fn webrtc_session_expected(&self) -> bool {
        self.webrtc_plan_seen && !self.expected_peers.is_empty()
    }
}

fn direct_plan_rejection_message(endpoint: Option<&DirectEndpoint>) -> String {
    let endpoint = endpoint.map_or_else(
        || "without a validated endpoint".to_string(),
        |endpoint| format!("for {}:{}", endpoint.host, endpoint.port),
    );
    format!(
        "direct SessionPlan {endpoint} is unsupported by the native reference client; using relay fallback"
    )
}

fn session_plan_peer_ids(
    transport: Transport,
    peers: &[signal_fish_server::protocol::SessionPeer],
    my_id: PlayerId,
) -> BTreeSet<PlayerId> {
    if transport == Transport::WebRtc {
        peers
            .iter()
            .map(|peer| peer.player_id)
            .filter(|peer| *peer != my_id)
            .collect()
    } else {
        BTreeSet::new()
    }
}

async fn reject_unsupported_direct_plan_with<Send, SendFuture, Emit>(
    endpoint: Option<&DirectEndpoint>,
    send: Send,
    mut emit_event: Emit,
) -> Result<(), FatalError>
where
    Send: FnOnce(ClientMessage) -> SendFuture,
    SendFuture: Future<Output = Result<(), FatalError>>,
    Emit: FnMut(Event),
{
    emit_event(Event::Error {
        message: direct_plan_rejection_message(endpoint),
    });
    send(ClientMessage::TransportStatus {
        transport: Transport::Direct,
        connected: false,
    })
    .await?;
    emit_event(Event::TransportStatusSent {
        transport: Transport::Direct,
        connected: false,
    });
    emit_event(Event::FallbackEngaged);
    Ok(())
}

/// A harness-held client treats the soft run deadline as advisory after it has
/// reported success. It must wait both for the harness release and for the
/// post-release linger that was armed on the release-observation tick.
fn should_defer_success_at_run_deadline(
    release_pending: bool,
    success_criteria_reported: bool,
    success_linger_pending: bool,
) -> bool {
    success_criteria_reported && (release_pending || success_linger_pending)
}

/// A harness freezes the signal ledger only after every live peer-connection
/// generation reaches the end-of-gathering callback. A terminal generation is
/// removed from the engine and cannot emit another candidate, so it is already
/// settled even though its generation-scoped marker is cleared during cleanup.
fn needs_ice_gathering_marker(is_current_generation: bool, gathering_complete: bool) -> bool {
    is_current_generation && !gathering_complete
}

fn resolve_drop_ice_from(
    ordinal: Option<usize>,
    my_id: PlayerId,
    peers: &[signal_fish_server::protocol::SessionPeer],
) -> Result<Option<PlayerId>, String> {
    let Some(ordinal) = ordinal else {
        return Ok(None);
    };
    let target_name = format!("c{ordinal:02}");
    let mut matches = peers
        .iter()
        .filter(|peer| peer.player_name == target_name)
        .map(|peer| peer.player_id);
    let target = matches.next().ok_or_else(|| {
        format!("--drop-ice-from {ordinal} did not resolve to planned peer {target_name:?}")
    })?;
    if matches.next().is_some() {
        return Err(format!(
            "--drop-ice-from {ordinal} ambiguously matched multiple planned peers named {target_name:?}"
        ));
    }
    if target == my_id {
        return Err(format!(
            "--drop-ice-from {ordinal} resolved to this client instead of a peer"
        ));
    }
    Ok(Some(target))
}

fn harness_aware_base_wake(
    run_deadline: Instant,
    success_release_poll_at: Option<Instant>,
    linger_until: Option<Instant>,
    success_criteria_reported: bool,
) -> Instant {
    if success_criteria_reported {
        success_release_poll_at
            .or(linger_until)
            .unwrap_or(run_deadline)
    } else {
        run_deadline
    }
}

fn keepalive_wake(next_ping_at: Instant, pong_deadline: Option<Instant>) -> Instant {
    pong_deadline.unwrap_or(next_ping_at)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    use clap::Parser;
    use tokio::time::Duration;

    use serde_json::json;
    use signal_fish_server::protocol::{
        DeliveryCountersByClass, DeliveryReportPayload, DirectEndpoint, GameDataEncoding,
        LobbyState, PlayerId, ServerMessage,
    };

    use crate::accountability::DeliveryAccountability;
    use crate::cli::Cli;
    use crate::wire;

    use super::{
        apply_selected_pair_probes_before_run_deadline, arm_pair_window, authoritative_peer_delta,
        automatic_p2p_retry_count, changed_transport_status, clear_departed_membership_plan,
        connection_targets_for_generation, consume_join_accountability_preface,
        direct_plan_rejection_message, exchange_label_complete, harness_aware_base_wake,
        is_coordinated_p2p_rebuild_attempt, is_current_session_generation,
        is_terminal_peer_connection_state, needs_ice_gathering_marker, negotiated_version_from,
        next_handshake_message, note_current_pair_connected, p2p_retry_delay,
        reject_unsupported_direct_plan_with, require_finalized_membership_plan,
        requires_authoritative_finalization_plan, resolve_drop_ice_from,
        restore_reconnected_member, retryable_missing_peers, selected_pair_evidence_deadline,
        session_plan_peer_ids, should_buffer_signal_for_unpaired_peer,
        should_defer_success_at_run_deadline, should_report_retry_gap,
        should_resolve_connected_pair, take_ready_selected_pair_probes, try_buffer_planned_signal,
        validate_json_negotiated_server_message, validate_p2p_rebuild_retry_count, PairGeneration,
        SelectedPairEvidence, SelectedPairProbeDisposition, EXIT_PROTOCOL_ERROR,
        MAX_PENDING_SIGNALS_PER_PEER, MAX_PENDING_SIGNALS_TOTAL, SELECTED_PAIR_POLL,
    };
    use crate::engine::{SelectedCandidatePair, SelectedPairProbeResult, RELIABLE_LABEL};
    use tokio_tungstenite::tungstenite::{Bytes, Message};
    use webrtc::peer_connection::RTCPeerConnectionState;

    #[tokio::test(start_paused = true)]
    async fn selected_pair_evidence_retries_past_old_budget_then_completes() {
        let peer = PlayerId::from_u128(0x05E1_EC7E_D001);
        let mut evidence = SelectedPairEvidence::default();
        let started_at = tokio::time::Instant::now();
        let run_deadline = started_at + Duration::from_secs(3);
        evidence.require(peer);
        assert!(evidence.begin_probe(peer));

        // Session 110 used a one-second evidence deadline. The PR #332 macOS
        // recurrence proved that boundary can expire while the live transport
        // is healthy, so missing evidence must remain a success postcondition
        // and continue polling under the existing whole-run deadline.
        for attempt in 0..=100 {
            let completed_at = tokio::time::Instant::now();
            assert_eq!(
                evidence.complete_probe(peer, completed_at, Some(run_deadline), false),
                SelectedPairProbeDisposition::Retry
            );
            tokio::time::advance(SELECTED_PAIR_POLL).await;
            let now = tokio::time::Instant::now();
            assert_eq!(
                evidence.take_due(now),
                vec![peer],
                "attempt {attempt} must retry the still-connected physical link"
            );
            assert!(evidence.begin_probe(peer));
        }

        let completed_at = tokio::time::Instant::now();
        assert!(completed_at > started_at + Duration::from_secs(1));
        assert_eq!(
            evidence.complete_probe(peer, completed_at, Some(run_deadline), true),
            SelectedPairProbeDisposition::Observed
        );
        let mut resolved = Vec::new();
        evidence.append_unmet(&mut resolved);
        assert!(resolved.is_empty());
        assert_eq!(evidence.poll_at, None);
    }

    #[tokio::test(start_paused = true)]
    async fn selected_pair_evidence_deadline_fails_closed_without_busy_retry() {
        let peer = PlayerId::from_u128(0x05E1_EC7E_D002);
        let mut evidence = SelectedPairEvidence::default();
        let run_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        evidence.require(peer);
        assert!(evidence.begin_probe(peer));

        tokio::time::advance(Duration::from_secs(1)).await;
        let deadline_due = apply_selected_pair_probes_before_run_deadline(
            Vec::new(),
            |_result| unreachable!("the deliberately hanging probe has no completion"),
            tokio::time::Instant::now(),
            run_deadline,
        );
        assert!(deadline_due, "the hanging probe cannot postpone run expiry");
        assert_eq!(
            evidence.complete_probe(peer, tokio::time::Instant::now(), Some(run_deadline), true),
            SelectedPairProbeDisposition::Ignore,
            "evidence completing at the absolute run deadline is too late"
        );
        assert_eq!(evidence.poll_at, None, "late work must not busy-retry");
        let mut unmet = Vec::new();
        evidence.append_unmet(&mut unmet);
        assert_eq!(
            unmet,
            [format!("selected ICE pair evidence pending for {peer}")]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn selected_pair_evidence_drains_predeadline_completion_at_boundary() {
        let peer = PlayerId::from_u128(0x05E1_EC7E_D004);
        let mut evidence = SelectedPairEvidence::default();
        let started_at = tokio::time::Instant::now();
        let run_deadline = started_at + Duration::from_secs(1);
        evidence.require(peer);
        assert!(evidence.begin_probe(peer));

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(SelectedPairProbeResult {
            peer,
            generation: 7,
            completed_at: run_deadline - Duration::from_millis(1),
            selected: Some(SelectedCandidatePair {
                local_candidate_type: "host".to_string(),
                remote_candidate_type: "host".to_string(),
                local_candidate_address: Some("127.0.0.1".to_string()),
                remote_candidate_address: Some("127.0.0.1".to_string()),
            }),
        })
        .expect("probe result receiver remains open");
        tokio::time::advance(Duration::from_secs(1)).await;

        let ready = take_ready_selected_pair_probes(&mut rx);
        let mut disposition = SelectedPairProbeDisposition::Ignore;
        let deadline_due = apply_selected_pair_probes_before_run_deadline(
            ready,
            |result| {
                disposition = evidence.complete_probe(
                    result.peer,
                    result.completed_at,
                    selected_pair_evidence_deadline(run_deadline, false),
                    result.selected.is_some(),
                );
            },
            tokio::time::Instant::now(),
            run_deadline,
        );
        assert!(deadline_due);
        assert_eq!(disposition, SelectedPairProbeDisposition::Observed);
        assert!(evidence.pending.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn selected_pair_evidence_held_success_accepts_replacement_after_soft_deadline() {
        let peer = PlayerId::from_u128(0x05E1_EC7E_D005);
        let mut evidence = SelectedPairEvidence::default();
        let run_deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        evidence.require(peer);
        assert!(evidence.begin_probe(peer));
        tokio::time::advance(Duration::from_secs(2)).await;

        let evidence_deadline = selected_pair_evidence_deadline(run_deadline, true);
        assert_eq!(evidence_deadline, None);
        assert_eq!(
            evidence.complete_probe(peer, tokio::time::Instant::now(), evidence_deadline, true),
            SelectedPairProbeDisposition::Observed,
            "a harness-held replacement generation suspends the advisory soft cutoff"
        );
        assert!(tokio::time::Instant::now() > run_deadline);
        assert!(evidence.pending.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn selected_pair_evidence_teardown_and_duplicates_are_generation_safe() {
        let first = PlayerId::from_u128(0x05E1_EC7E_D002);
        let second = PlayerId::from_u128(0x05E1_EC7E_D003);
        let mut evidence = SelectedPairEvidence::default();
        let now = tokio::time::Instant::now();
        let run_deadline = now + Duration::from_secs(2);
        evidence.require(first);
        evidence.require(second);
        assert!(evidence.begin_probe(first));
        assert!(evidence.begin_probe(second));

        evidence.clear(first);
        assert_eq!(
            evidence.complete_probe(first, now, Some(run_deadline), true),
            SelectedPairProbeDisposition::Ignore,
            "a result from a torn-down physical generation must be discarded"
        );
        assert_eq!(
            evidence.complete_probe(second, now, Some(run_deadline), false),
            SelectedPairProbeDisposition::Retry
        );
        tokio::time::advance(SELECTED_PAIR_POLL).await;
        assert_eq!(
            evidence.take_due(tokio::time::Instant::now()),
            vec![second],
            "tearing down or replacing one pair must retain another pair's evidence obligation"
        );

        assert!(evidence.begin_probe(second));
        assert_eq!(
            evidence.complete_probe(
                second,
                tokio::time::Instant::now(),
                Some(run_deadline),
                true
            ),
            SelectedPairProbeDisposition::Observed
        );
        assert_eq!(
            evidence.complete_probe(
                second,
                tokio::time::Instant::now(),
                Some(run_deadline),
                true
            ),
            SelectedPairProbeDisposition::Ignore,
            "one physical generation emits evidence at most once"
        );
        assert!(evidence.pending.is_empty());
        assert_eq!(evidence.poll_at, None);
    }

    #[test]
    fn direct_plan_rejection_is_explicit_with_or_without_endpoint() {
        let endpoint = DirectEndpoint {
            host: "192.0.2.10".to_string(),
            port: 7777,
        };
        assert_eq!(
            direct_plan_rejection_message(Some(&endpoint)),
            "direct SessionPlan for 192.0.2.10:7777 is unsupported by the native reference client; using relay fallback"
        );
        assert_eq!(
            direct_plan_rejection_message(None),
            "direct SessionPlan without a validated endpoint is unsupported by the native reference client; using relay fallback"
        );
    }

    #[tokio::test]
    async fn direct_plan_dispatch_reports_failure_clears_p2p_and_keeps_relay_available() {
        use std::cell::RefCell;

        use signal_fish_server::protocol::{SessionPeer, Transport};

        let endpoint = DirectEndpoint {
            host: "192.0.2.10".to_string(),
            port: 7777,
        };
        let sent = RefCell::new(Vec::new());
        let events = RefCell::new(Vec::new());
        reject_unsupported_direct_plan_with(
            Some(&endpoint),
            |message| {
                sent.borrow_mut()
                    .push(serde_json::to_value(message).expect("serialize client message"));
                std::future::ready(Ok(()))
            },
            |event| {
                events
                    .borrow_mut()
                    .push(serde_json::to_value(event).expect("serialize event"));
            },
        )
        .await
        .expect("Direct rejection action succeeds");

        let me = PlayerId::new_v4();
        let prior_peer = PlayerId::new_v4();
        let peers = vec![SessionPeer {
            player_id: prior_peer,
            player_name: "prior".to_string(),
            is_authority: false,
            initiate: true,
        }];
        let planned = session_plan_peer_ids(Transport::Direct, &peers, me);
        let delta = authoritative_peer_delta(&BTreeSet::from([prior_peer]), &planned);

        assert!(
            planned.is_empty(),
            "Direct must clear WebRTC peer obligations"
        );
        assert_eq!(delta.removed, BTreeSet::from([prior_peer]));
        assert_eq!(
            sent.borrow().as_slice(),
            &[json!({
                "type": "TransportStatus",
                "data": { "transport": "direct", "connected": false }
            })]
        );
        assert_eq!(
            events
                .borrow()
                .iter()
                .map(|event| event["event"].as_str().expect("event name"))
                .collect::<Vec<_>>(),
            ["error", "transport_status_sent", "fallback_engaged"]
        );

        let relay = wire::game_data_message(json!({ "relay_msg": "still-live" }));
        assert_eq!(
            serde_json::to_value(relay).expect("serialize relay GameData")["type"],
            "GameData",
            "relay GameData remains available after the Direct rejection"
        );
    }

    #[test]
    fn only_websocket_ping_pong_are_transparent_to_application_ordering() {
        assert!(wire::is_transparent_transport_control(&Message::Ping(
            Bytes::new()
        )));
        assert!(wire::is_transparent_transport_control(&Message::Pong(
            Bytes::new()
        )));
        assert!(!wire::is_transparent_transport_control(&Message::Binary(
            Bytes::new()
        )));
    }

    #[test]
    fn harness_release_after_soft_deadline_still_honors_exit_linger() {
        assert!(should_defer_success_at_run_deadline(true, true, false));
        assert!(should_defer_success_at_run_deadline(false, true, true));
        assert!(!should_defer_success_at_run_deadline(false, true, false));
        assert!(!should_defer_success_at_run_deadline(true, false, false));
        assert!(!should_defer_success_at_run_deadline(false, false, true));
    }

    #[test]
    fn per_peer_ice_fault_resolves_harness_ordinal_and_rejects_missing_target() {
        let me = PlayerId::new_v4();
        let target = PlayerId::new_v4();
        let peers = vec![signal_fish_server::protocol::SessionPeer {
            player_id: target,
            player_name: "c01".to_string(),
            is_authority: false,
            initiate: true,
        }];

        assert_eq!(resolve_drop_ice_from(None, me, &peers), Ok(None));
        assert_eq!(resolve_drop_ice_from(Some(1), me, &peers), Ok(Some(target)));
        assert!(resolve_drop_ice_from(Some(2), me, &peers)
            .expect_err("an absent target must fail loud")
            .contains("did not resolve"));

        let duplicate = signal_fish_server::protocol::SessionPeer {
            player_id: PlayerId::new_v4(),
            ..peers[0].clone()
        };
        assert!(
            resolve_drop_ice_from(Some(1), me, &[peers[0].clone(), duplicate])
                .expect_err("duplicate harness names must fail loud")
                .contains("ambiguously matched")
        );

        let self_target = signal_fish_server::protocol::SessionPeer {
            player_id: me,
            ..peers[0].clone()
        };
        assert!(resolve_drop_ice_from(Some(1), me, &[self_target])
            .expect_err("self-targeting must fail loud")
            .contains("this client"));
    }

    #[test]
    fn harness_waits_only_for_current_generation_ice_gathering() {
        let cases = [
            ("current and gathering", true, false, true),
            ("current and complete", true, true, false),
            ("terminal before marker", false, false, false),
            ("terminal after marker", false, true, false),
        ];
        for (scenario, is_current, complete, expected) in cases {
            assert_eq!(
                needs_ice_gathering_marker(is_current, complete),
                expected,
                "{scenario}"
            );
        }
    }

    #[test]
    fn reported_success_keeps_soft_deadline_deferred_during_regression() {
        assert!(should_defer_success_at_run_deadline(true, true, false));
        assert!(
            !should_defer_success_at_run_deadline(false, true, false),
            "release ends the hold so regressed criteria can fail normally"
        );
    }

    #[test]
    fn harness_linger_replaces_elapsed_soft_deadline_as_next_wake() {
        let now = tokio::time::Instant::now();
        let elapsed_run_deadline = now - std::time::Duration::from_secs(1);
        let linger_until = now + super::EXIT_LINGER;
        assert_eq!(
            harness_aware_base_wake(elapsed_run_deadline, None, Some(linger_until), true),
            linger_until
        );
        assert_eq!(
            harness_aware_base_wake(elapsed_run_deadline, None, Some(linger_until), false),
            elapsed_run_deadline,
            "ordinary clients retain the soft-deadline behavior"
        );
    }

    #[test]
    fn pending_pong_deadline_owns_keepalive_wake() {
        let now = tokio::time::Instant::now();
        let overdue_ping = now - std::time::Duration::from_secs(1);
        let pong_grace = now + super::PONG_DRAIN_GRACE;
        assert_eq!(
            super::keepalive_wake(overdue_ping, Some(pong_grace)),
            pong_grace
        );
        assert_eq!(super::keepalive_wake(overdue_ping, None), overdue_ping);
    }

    #[test]
    fn player_reconnected_restores_lobby_and_active_session_membership() {
        let self_id = PlayerId::from_u128(1);
        let peer = PlayerId::from_u128(2);
        for in_lobby in [true, false] {
            let mut present = BTreeSet::from([self_id, peer]);
            let mut members_seen = present.clone();
            present.remove(&peer);

            restore_reconnected_member(&mut present, &mut members_seen, peer);

            assert!(present.contains(&peer));
            assert!(members_seen.contains(&peer));
            assert_eq!(in_lobby && present.len() >= 2, in_lobby);

            let delta = authoritative_peer_delta(&BTreeSet::new(), &BTreeSet::from([peer]));
            assert_eq!(delta.added, BTreeSet::from([peer]));
            assert!(delta.removed.is_empty());
        }
        assert_eq!(changed_transport_status(Some(true), 0), Some(false));
        assert_eq!(changed_transport_status(Some(false), 0), None);
        assert_eq!(changed_transport_status(Some(false), 1), Some(true));
        assert!(should_resolve_connected_pair(Some(false), false));
        assert_eq!(changed_transport_status(Some(true), 2), None);
        for _scenario in ["solo finalization", "incapable late join"] {
            assert_eq!(changed_transport_status(None, 0), Some(false));
        }
    }

    #[test]
    fn authoritative_plan_replaces_topology_and_supports_empty_no_pair_plan() {
        let old = PlayerId::from_u128(2);
        let retained = PlayerId::from_u128(3);
        let added = PlayerId::from_u128(4);
        let current = BTreeSet::from([old, retained]);
        let replacement = BTreeSet::from([retained, added]);

        let delta = authoritative_peer_delta(&current, &replacement);
        assert_eq!(delta.removed, BTreeSet::from([old]));
        assert_eq!(delta.retained, BTreeSet::from([retained]));
        assert_eq!(delta.added, BTreeSet::from([added]));

        let empty = authoritative_peer_delta(&replacement, &BTreeSet::new());
        assert_eq!(empty.removed, replacement);
        assert!(empty.retained.is_empty());
        assert!(empty.added.is_empty());
    }

    #[test]
    fn changed_generation_rebuilds_both_asymmetric_pair_endpoints() {
        let peer = PlayerId::from_u128(2);
        let peers = BTreeSet::from([peer]);
        let delta = authoritative_peer_delta(&peers, &peers);

        for (scenario, initiator_paired, responder_paired) in [
            ("initiator retains, responder missing", true, false),
            ("initiator missing, responder retains", false, true),
        ] {
            for (role, paired) in [
                ("initiator", initiator_paired),
                ("responder", responder_paired),
            ] {
                assert_eq!(
                    connection_targets_for_generation(&delta, true),
                    peers,
                    "{scenario}: {role} must install the new physical generation"
                );
                assert!(
                    connection_targets_for_generation(&delta, false).is_empty(),
                    "{scenario}: duplicate generation must not let {role} rebuild unilaterally (paired={paired})"
                );
            }
        }
    }

    #[test]
    fn only_failed_and_closed_peer_connection_states_are_terminal() {
        let cases = [
            (RTCPeerConnectionState::New, false),
            (RTCPeerConnectionState::Connecting, false),
            (RTCPeerConnectionState::Connected, false),
            (RTCPeerConnectionState::Disconnected, false),
            (RTCPeerConnectionState::Failed, true),
            (RTCPeerConnectionState::Closed, true),
        ];
        for (state, expected) in cases {
            assert_eq!(is_terminal_peer_connection_state(&state), expected);
        }
        assert_eq!(changed_transport_status(Some(true), 0), Some(false));
        let peer = PlayerId::from_u128(2);
        assert!(!should_buffer_signal_for_unpaired_peer(
            &BTreeSet::new(),
            false,
            peer
        ));
        assert!(should_buffer_signal_for_unpaired_peer(
            &BTreeSet::from([peer]),
            false,
            peer
        ));
        assert!(should_buffer_signal_for_unpaired_peer(
            &BTreeSet::from([peer]),
            true,
            peer
        ));
    }

    #[test]
    fn planned_signal_buffer_has_per_peer_and_aggregate_bounds() {
        let mut pending = BTreeMap::new();
        let first = PlayerId::from_u128(1);
        for index in 0..MAX_PENDING_SIGNALS_PER_PEER {
            assert!(try_buffer_planned_signal(
                &mut pending,
                first,
                json!({ "IceCandidate": index })
            ));
        }
        assert!(!try_buffer_planned_signal(
            &mut pending,
            first,
            json!({ "IceCandidate": "per-peer overflow" })
        ));

        for peer_index in 2_u128..=4 {
            let peer = PlayerId::from_u128(peer_index);
            for signal_index in 0..MAX_PENDING_SIGNALS_PER_PEER {
                assert!(try_buffer_planned_signal(
                    &mut pending,
                    peer,
                    json!({ "IceCandidate": signal_index })
                ));
            }
        }
        assert_eq!(
            pending.values().map(VecDeque::len).sum::<usize>(),
            MAX_PENDING_SIGNALS_TOTAL
        );
        assert!(!try_buffer_planned_signal(
            &mut pending,
            PlayerId::from_u128(5),
            json!({ "Offer": "aggregate overflow" })
        ));
    }

    #[test]
    fn wire_session_generation_rejects_stale_and_pre_plan_signals() {
        let current = uuid::Uuid::from_u128(1);
        let stale = uuid::Uuid::from_u128(2);
        assert!(is_current_session_generation(Some(current), current));
        assert!(!is_current_session_generation(Some(current), stale));
        assert!(!is_current_session_generation(None, current));
    }

    #[test]
    fn pair_retry_delay_is_early_and_bounded() {
        assert_eq!(p2p_retry_delay(1), Duration::from_secs(1));
        assert_eq!(p2p_retry_delay(15), Duration::from_secs(7));
        assert_eq!(p2p_retry_delay(180), Duration::from_secs(15));
    }

    #[test]
    fn pair_retry_targets_only_missing_peers_with_budget() {
        let connected = PlayerId::from_u128(2);
        let exhausted = PlayerId::from_u128(3);
        let retryable = PlayerId::from_u128(4);
        let expected = BTreeSet::from([connected, exhausted, retryable]);
        let connected_pairs = BTreeSet::from([connected]);
        let attempts = BTreeMap::from([(exhausted, 1)]);
        assert_eq!(
            retryable_missing_peers(&expected, &connected_pairs, &attempts, 1),
            vec![retryable]
        );
        assert!(retryable_missing_peers(&expected, &connected_pairs, &attempts, 0).is_empty());
    }

    #[test]
    fn reliable_exchange_barrier_requires_both_directions_for_every_peer() {
        let left = PlayerId::from_u128(2);
        let right = PlayerId::from_u128(3);
        let expected = BTreeSet::from([left, right]);
        let reliable = BTreeSet::from([RELIABLE_LABEL.to_string()]);
        let sent = BTreeMap::from([(left, reliable.clone()), (right, reliable.clone())]);
        let mut received = BTreeMap::from([(left, reliable.clone())]);

        assert!(!exchange_label_complete(
            &expected,
            &sent,
            &received,
            RELIABLE_LABEL
        ));
        received.insert(right, reliable);
        assert!(exchange_label_complete(
            &expected,
            &sent,
            &received,
            RELIABLE_LABEL
        ));
        assert!(!exchange_label_complete(
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            RELIABLE_LABEL
        ));
    }

    #[test]
    fn retry_reconnects_current_pair_without_duplicate_logical_event() {
        let peer = PlayerId::from_u128(2);
        let mut connected = BTreeSet::new();
        let mut reported = BTreeSet::new();
        let mut retrying = BTreeSet::new();
        assert!(note_current_pair_connected(
            &mut connected,
            &mut reported,
            &mut retrying,
            peer
        ));

        connected.remove(&peer);
        retrying.insert(peer);
        assert!(!note_current_pair_connected(
            &mut connected,
            &mut reported,
            &mut retrying,
            peer
        ));
        assert!(connected.contains(&peer));
        assert!(retrying.is_empty());

        connected.remove(&peer);
        reported.remove(&peer);
        assert!(note_current_pair_connected(
            &mut connected,
            &mut reported,
            &mut retrying,
            peer
        ));
    }

    #[test]
    fn coordinated_rebuild_reserves_the_final_retry_attempt() {
        assert_eq!(automatic_p2p_retry_count(false, 2), 2);
        assert_eq!(automatic_p2p_retry_count(true, 2), 1);
        assert_eq!(automatic_p2p_retry_count(true, 1), 0);

        assert!(validate_p2p_rebuild_retry_count(false, 0).is_ok());
        assert!(validate_p2p_rebuild_retry_count(true, 1).is_ok());
        assert!(validate_p2p_rebuild_retry_count(true, 0).is_err());

        assert!(is_coordinated_p2p_rebuild_attempt(true, 2, 2));
        assert!(!is_coordinated_p2p_rebuild_attempt(true, 1, 2));
        assert!(!is_coordinated_p2p_rebuild_attempt(false, 2, 2));
        assert!(!is_coordinated_p2p_rebuild_attempt(true, 0, 0));

        assert!(should_report_retry_gap(true, Some(true), false));
        assert!(!should_report_retry_gap(true, Some(true), true));
        assert!(!should_report_retry_gap(false, Some(true), false));
        assert!(!should_report_retry_gap(true, None, false));
    }

    #[tokio::test]
    async fn coordinated_rebuild_rejects_zero_budget_before_connecting() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:1/v3/ws",
            "--create-room",
            "--exchange",
            "--exchange-release-file",
            "/tmp/signal-fish-exchange-release",
            "--p2p-rebuild-release-file",
            "/tmp/signal-fish-p2p-rebuild",
        ]);

        let error = super::run_inner(&cli)
            .await
            .expect_err("zero retry budget must fail before the socket connect");
        assert_eq!(error.code, EXIT_PROTOCOL_ERROR);
        assert_eq!(
            error.message,
            "--p2p-rebuild-release-file requires a nonzero --p2p-retry-count"
        );
    }

    #[test]
    fn retry_preserves_the_p2p_window_while_initial_pairing_refreshes_it() {
        assert!(PairGeneration::Initial.arms_p2p_window());
        assert!(!PairGeneration::Retry.arms_p2p_window());

        let started = tokio::time::Instant::now();
        let original_deadline = started + Duration::from_secs(30);
        let original_retry = started + Duration::from_secs(15);
        let mut deadline = Some(original_deadline);
        let mut retry_at = Some(original_retry);
        arm_pair_window(
            &mut deadline,
            &mut retry_at,
            PairGeneration::Retry,
            1,
            Duration::from_secs(30),
            started + Duration::from_secs(10),
        );
        assert_eq!(deadline, Some(original_deadline));
        assert_eq!(retry_at, Some(original_retry));

        arm_pair_window(
            &mut deadline,
            &mut retry_at,
            PairGeneration::Initial,
            1,
            Duration::from_secs(30),
            started + Duration::from_secs(20),
        );
        assert_eq!(deadline, Some(started + Duration::from_secs(50)));
        assert_eq!(retry_at, Some(started + Duration::from_secs(35)));
    }

    #[test]
    fn authoritative_plan_obligations_are_finalized_v3_and_epoch_scoped() {
        for (_scenario, version, expected) in [
            ("v2 relay finalization", 2, false),
            ("v3 WebRTC finalization", 3, true),
            ("v3 relay finalization", 3, true),
        ] {
            let mut plan_pending = requires_authoritative_finalization_plan(version);
            assert_eq!(plan_pending, expected);
            let simulated_plan_delay = super::EXIT_LINGER + std::time::Duration::from_millis(1);
            if expected {
                assert!(simulated_plan_delay > super::EXIT_LINGER && plan_pending);
            }
            plan_pending = false;
            assert!(!plan_pending);
        }

        let peer = PlayerId::from_u128(2);
        let mut pending = BTreeMap::new();
        assert!(!require_finalized_membership_plan(
            &mut pending,
            3,
            Some(&LobbyState::Lobby),
            peer,
            Some(1)
        ));
        assert!(!require_finalized_membership_plan(
            &mut pending,
            2,
            Some(&LobbyState::Finalized),
            peer,
            Some(1)
        ));
        assert!(require_finalized_membership_plan(
            &mut pending,
            3,
            Some(&LobbyState::Finalized),
            peer,
            Some(1)
        ));
        require_finalized_membership_plan(
            &mut pending,
            3,
            Some(&LobbyState::Finalized),
            peer,
            Some(2),
        );
        clear_departed_membership_plan(&mut pending, peer, Some(1));
        assert_eq!(pending.get(&peer), Some(&2));
        clear_departed_membership_plan(&mut pending, peer, Some(2));
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn pre_negotiation_application_frames_are_protocol_errors() {
        let frames = [
            Message::Binary(Bytes::new()),
            Message::Text("{".to_string().into()),
        ];
        for frame in frames {
            let mut input = futures_util::stream::iter([Ok::<
                Message,
                tokio_tungstenite::tungstenite::Error,
            >(frame)]);
            let error = next_handshake_message(&mut input).await.unwrap_err();
            assert_eq!(error.code, EXIT_PROTOCOL_ERROR);
        }
    }

    #[test]
    fn accountability_mode_follows_protocol_info_not_advertised_max() {
        let cases = [
            (3u16, json!({}), 2u16, false),
            (3u16, json!({ "protocol_version": 2 }), 2u16, false),
            (3u16, json!({ "protocol_version": 3 }), 3u16, true),
        ];
        for (offered, payload, expected, expected_v3) in cases {
            let frame: ServerMessage = serde_json::from_value(json!({
                "type": "ProtocolInfo",
                "data": payload,
            }))
            .unwrap();
            let negotiated = negotiated_version_from(frame, offered).unwrap();
            assert_eq!(negotiated, expected, "offered max {offered}");
            assert_eq!(
                negotiated >= 3,
                expected_v3,
                "offered max {offered} must not select accountability mode"
            );
        }

        for (offered, negotiated) in [(2, 3), (3, 1), (3, 4)] {
            let frame: ServerMessage = serde_json::from_value(json!({
                "type": "ProtocolInfo",
                "data": { "protocol_version": negotiated },
            }))
            .unwrap();
            assert!(
                negotiated_version_from(frame, offered).is_err(),
                "offered {offered} must reject negotiated {negotiated}"
            );
        }
        assert!(
            serde_json::from_value::<ServerMessage>(json!({
                "type": "ProtocolInfo",
                "data": { "protocol_version": null },
            }))
            .is_err(),
            "explicit null must not collapse into the absent-v2 sentinel"
        );

        let application_frame = ServerMessage::GameData {
            from_player: signal_fish_server::protocol::PlayerId::nil(),
            data: json!(null),
            seq: None,
            epoch: None,
            class: None,
            key: None,
        };
        assert!(negotiated_version_from(application_frame, 3).is_err());
    }

    #[test]
    fn json_negotiation_rejects_the_in_memory_binary_variant_on_text_wire() {
        let message = ServerMessage::GameDataBinary {
            from_player: PlayerId::nil(),
            encoding: GameDataEncoding::Json,
            payload: Bytes::new(),
            seq: Some(1),
            epoch: Some(1),
        };
        let error = validate_json_negotiated_server_message(&message).unwrap_err();
        assert_eq!(error.code, EXIT_PROTOCOL_ERROR);
        assert!(error.message.contains("text GameDataBinary"));
    }

    #[test]
    fn join_handshake_consumes_connection_accountability_prefaces_statefully() {
        let mut state = DeliveryAccountability::new(true);
        let frames = [
            ServerMessage::DeliveryReport(Box::new(DeliveryReportPayload {
                per_class: DeliveryCountersByClass::default(),
                gaps: Vec::new(),
            })),
            ServerMessage::RelayStats {
                interval_ms: 1_000,
                sent_to_you: 0,
                dropped_for_you: 0,
                backpressure_events: 0,
            },
        ];
        for frame in &frames {
            assert!(consume_join_accountability_preface(&mut state, frame).unwrap());
        }

        let mut v2 = DeliveryAccountability::new(false);
        assert!(consume_join_accountability_preface(&mut v2, &frames[1]).is_err());
    }
}
