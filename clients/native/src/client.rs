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
//! 4. **Finalize** — `GameStarting` (note our own `is_authority`), then, for
//!    non-relay rooms, the per-recipient `SessionPlan`.
//! 5. **P2P** — pair per `peers[].initiate` (and `NewPeer.you_initiate` for
//!    late joiners), trickling ICE through `Signal`. The overall WebRTC
//!    transport status resolves once (Appendix G): when all expected pairs are
//!    connected (a departure that removes the last unconnected expected pair
//!    counts), or at `--p2p-timeout-secs` — `connected: true` iff at least
//!    one pair is connected at that moment; a zero-pair resolution also emits
//!    `fallback_engaged`. Pairs that connect later (late-join `NewPeer`) do
//!    not re-send the report: the state did not change, and the server
//!    deduplicates repeat states anyway.
//! 6. **Relay floor** — `GameData` keeps flowing over the WebSocket before,
//!    during, and after P2P; `--relay-payload` exercises it explicitly.
//!
//! The orchestrator is a single task: WebSocket frames, engine callbacks
//! (via the [`EngineEvent`] channel), and timers are multiplexed with
//! `tokio::select!`, so all bookkeeping is lock-free and every stdout event is
//! emitted in causal order. The binary-wide `--max-runtime-secs` watchdog in
//! `main` bounds every await in this module.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::json;
use signal_fish_server::protocol::{
    ClientMessage, GameDataEncoding, IceServer, LobbyState, PlayerId, ServerMessage, Transport,
};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;

use crate::cli::Cli;
use crate::engine::{Engine, EngineEvent, RELIABLE_LABEL, UNRELIABLE_LABEL};
use crate::events::{emit, Event, PlanPeer, SignalKind};
use crate::wire::{self, WsStream, HANDSHAKE_TIMEOUT};

/// All success criteria met within the run window.
pub const EXIT_SUCCESS: i32 = 0;
/// `--run-for-secs` elapsed with unmet success criteria.
pub const EXIT_CRITERIA_UNMET: i32 = 1;
/// The server broke the expected protocol flow (auth/join rejection, bad frame).
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

/// Grace period between meeting all success criteria and exiting, so the last
/// unreliable-channel sends are not torn down mid-flight for slower siblings.
const EXIT_LINGER: Duration = Duration::from_millis(250);

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
    // The soft run window starts at process start, handshake included.
    let run_deadline = Instant::now() + Duration::from_secs(cli.run_for_secs);

    let mut ws = wire::connect(&cli.server_url)
        .await
        .map_err(|error| FatalError::connection(format!("{error:#}")))?;
    emit(&Event::Connected);

    authenticate(&mut ws, cli).await?;
    let (my_id, mut present, lobby_state) = join_room(&mut ws, cli).await?;

    let (engine_tx, engine_rx) = mpsc::unbounded_channel();
    let engine = Engine::new(cli.cripple_ice, engine_tx)
        .map_err(|error| FatalError::protocol(format!("webrtc engine init failed: {error:#}")))?;

    present.insert(my_id);
    let members_seen = present.clone();
    let mut orchestrator = Orchestrator {
        cli,
        ws,
        engine,
        engine_rx,
        my_id,
        present,
        members_seen,
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
        webrtc_plan_seen: false,
        expected_peers: BTreeSet::new(),
        connected_pairs: BTreeSet::new(),
        last_ice_servers: Vec::new(),
        transport_status: None,
        p2p_deadline: None,
        // Late joiners arm the relay probe on entry (see RELAY_SEND_SETTLE):
        // the GameStarting trigger pre-dates the join and never re-fires.
        relay_send_at: (cli.relay_payload.is_some() && lobby_state == LobbyState::Finalized)
            .then(|| Instant::now() + RELAY_SEND_SETTLE),
        relay_sent: false,
        relay_received_from: BTreeSet::new(),
        peer_status_from: BTreeSet::new(),
        sent_labels: BTreeMap::new(),
        received_labels: BTreeMap::new(),
        pending_signals: BTreeMap::new(),
        run_deadline,
        linger_until: None,
    };
    orchestrator.maybe_send_ready().await?;
    orchestrator.run_loop().await
}

/// Send `Authenticate` and consume `Authenticated` + `ProtocolInfo`.
async fn authenticate(ws: &mut WsStream, cli: &Cli) -> Result<(), FatalError> {
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
                "authentication rejected: {error} ({error_code:?})"
            )));
        }
        other => {
            return Err(FatalError::protocol(format!(
                "expected Authenticated, got {other:?}"
            )));
        }
    }

    match next_handshake_message(ws).await? {
        ServerMessage::ProtocolInfo(info) => {
            // v2 connections omit the field; the effective version is 2.
            let negotiated_version = info.protocol_version.unwrap_or(2);
            emit(&Event::ProtocolInfo { negotiated_version });
        }
        other => {
            return Err(FatalError::protocol(format!(
                "expected ProtocolInfo, got {other:?}"
            )));
        }
    }
    Ok(())
}

/// Create or join the room; returns our player id, the seated member ids, and
/// the room's lobby state at join time.
async fn join_room(
    ws: &mut WsStream,
    cli: &Cli,
) -> Result<(PlayerId, BTreeSet<PlayerId>, LobbyState), FatalError> {
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

    match next_handshake_message(ws).await? {
        ServerMessage::RoomJoined(payload) => {
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
            let present = payload
                .current_players
                .iter()
                .map(|player| player.id)
                .collect();
            Ok((payload.player_id, present, payload.lobby_state))
        }
        ServerMessage::RoomJoinFailed { reason, error_code } => Err(FatalError::protocol(format!(
            "room join failed: {reason} ({error_code:?})"
        ))),
        other => Err(FatalError::protocol(format!(
            "expected RoomJoined, got {other:?}"
        ))),
    }
}

async fn next_handshake_message(ws: &mut WsStream) -> Result<ServerMessage, FatalError> {
    wire::next_server_message(ws, HANDSHAKE_TIMEOUT)
        .await
        .map_err(|error| FatalError::connection(format!("{error:#}")))
}

/// Input multiplexed by the main loop.
enum LoopInput {
    Server(Option<Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Engine(Option<EngineEvent>),
    Tick,
}

/// Single-task state machine driving the session after the room is joined.
struct Orchestrator<'a> {
    cli: &'a Cli,
    ws: WsStream,
    engine: Engine,
    engine_rx: mpsc::UnboundedReceiver<EngineEvent>,
    my_id: PlayerId,
    /// Members currently seated in the room (self included).
    present: BTreeSet<PlayerId>,
    /// Every distinct member EVER observed in the room (self included);
    /// cumulative, never shrinks on departures. Drives the
    /// `--expect-total-peers` exit gate.
    members_seen: BTreeSet<PlayerId>,
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
    /// A WebRTC `SessionPlan` was received (gates the transport-status criterion).
    webrtc_plan_seen: bool,
    /// Peers the server told us to pair with (plan peers + late-join `NewPeer`s).
    expected_peers: BTreeSet<PlayerId>,
    /// Peers whose pair fully connected (both channels open) at some point.
    /// Drives the `--exchange` obligations and the Appendix G resolution.
    connected_pairs: BTreeSet<PlayerId>,
    /// ICE servers from the most recent plan (reused for `NewPeer` pairings).
    last_ice_servers: Vec<IceServer>,
    /// The single resolved overall WebRTC status, once sent (Appendix G).
    transport_status: Option<bool>,
    /// When the P2P establishment window expires (set at first pairing).
    p2p_deadline: Option<Instant>,
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
    /// Signals that arrived before their peer was paired (defensive: server
    /// FIFO ordering makes this unreachable in the documented flows).
    pending_signals: BTreeMap<PlayerId, VecDeque<serde_json::Value>>,
    /// `--run-for-secs` soft cap.
    run_deadline: Instant,
    /// Set once all criteria are met; exit 0 when it elapses.
    linger_until: Option<Instant>,
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
                _ = tokio::time::sleep_until(wake_at) => LoopInput::Tick,
            };
            match input {
                LoopInput::Server(Some(Ok(Message::Text(text)))) => {
                    let message: ServerMessage = serde_json::from_str(&text).map_err(|error| {
                        FatalError::protocol(format!(
                            "invalid ServerMessage frame: {error}; text={text}"
                        ))
                    })?;
                    self.handle_server_message(message).await?;
                }
                LoopInput::Server(Some(Ok(other))) => {
                    tracing::debug!(frame = ?other, "ignoring non-text frame");
                }
                LoopInput::Server(Some(Err(error))) => {
                    return Err(FatalError::connection(format!(
                        "websocket transport error: {error}"
                    )));
                }
                LoopInput::Server(None) => {
                    return Err(FatalError::connection(
                        "websocket closed by server before success criteria were met",
                    ));
                }
                LoopInput::Engine(Some(event)) => self.handle_engine_event(event).await?,
                LoopInput::Engine(None) => {
                    // Unreachable while the engine (which owns a sender) lives.
                    tracing::warn!("engine event channel closed");
                }
                LoopInput::Tick => {}
            }
        }
    }

    /// Earliest pending timer (the run deadline at the latest), so the select
    /// loop always wakes for due work even on a silent wire.
    fn next_wake(&self) -> Instant {
        let mut wake = self.run_deadline;
        if !self.relay_sent {
            if let Some(at) = self.relay_send_at {
                wake = wake.min(at);
            }
        }
        if self.transport_status.is_none() {
            if let Some(at) = self.p2p_deadline {
                wake = wake.min(at);
            }
        }
        if let Some(at) = self.linger_until {
            wake = wake.min(at);
        }
        wake
    }

    /// Fire due timers; returns `Some(exit_code)` when the run is over.
    async fn process_timers(&mut self) -> Result<Option<i32>, FatalError> {
        let now = Instant::now();

        if !self.relay_sent && self.relay_send_at.is_some_and(|at| now >= at) {
            self.send_relay_payload().await?;
        }

        if self.transport_status.is_none() && self.p2p_deadline.is_some_and(|at| now >= at) {
            // The P2P window expired: resolve with whatever is connected now.
            self.resolve_transport_status().await?;
        }

        if self.linger_until.is_none() && self.criteria_met() {
            self.linger_until = Some(now + EXIT_LINGER);
        }
        if self.linger_until.is_some_and(|at| now >= at) {
            // Criteria can regress during the linger: a late `NewPeer` or a
            // freshly connected pair adds new obligations (exchange,
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

        if now >= self.run_deadline {
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

    async fn handle_server_message(&mut self, message: ServerMessage) -> Result<(), FatalError> {
        match message {
            ServerMessage::PlayerJoined { player } => {
                self.present.insert(player.id);
                self.members_seen.insert(player.id);
                emit(&Event::PeerJoined {
                    player_id: player.id,
                });
                self.maybe_send_ready().await?;
            }
            ServerMessage::PlayerLeft { player_id } => {
                self.present.remove(&player_id);
                // A departed peer can no longer satisfy any pairing-derived
                // criterion: drop it from the expected set (and drop any
                // buffered signals from it). This matters during staggered
                // teardown of host sessions: a sibling's departure can trigger
                // the server's host-failover replan, which transiently names
                // soon-to-exit members as new pairs — without this removal a
                // client could wait on a peer that is already gone.
                self.expected_peers.remove(&player_id);
                self.pending_signals.remove(&player_id);
                // The departure can complete the Appendix G all-pairs
                // condition (the departed peer was the only unconnected pair,
                // e.g. a seat-holder that left without ever pairing): resolve
                // now instead of waiting out the full P2P window.
                if self.transport_status.is_none() && self.all_expected_pairs_connected() {
                    self.resolve_transport_status().await?;
                }
                emit(&Event::PlayerLeft { player_id });
            }
            ServerMessage::GameStarting { peer_connections } => {
                self.game_started = true;
                let is_authority = peer_connections
                    .iter()
                    .find(|peer| peer.player_id == self.my_id)
                    .map(|peer| peer.is_authority)
                    .unwrap_or(false);
                emit(&Event::GameStarting { is_authority });
                if self.cli.relay_payload.is_some() && !self.relay_sent {
                    self.relay_send_at = Some(Instant::now() + RELAY_SEND_SETTLE);
                }
            }
            ServerMessage::SessionPlan(plan) => {
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
                if plan.transport == Transport::WebRtc {
                    self.webrtc_plan_seen = true;
                    self.last_ice_servers = plan.ice_servers.clone();
                    for peer in &plan.peers {
                        self.establish_pair(peer.player_id, peer.initiate).await?;
                    }
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
                self.establish_pair(peer_id, you_initiate).await?;
            }
            ServerMessage::Signal { from, signal } => {
                self.handle_signal(from, signal).await?;
            }
            ServerMessage::GameData { from_player, data } => {
                if data.get("relay_msg").is_some() {
                    self.relay_received_from.insert(from_player);
                }
                emit(&Event::GameDataReceived {
                    from: from_player,
                    payload: data,
                });
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
            ServerMessage::Pong => {}
            other => {
                tracing::debug!(message = ?other, "ignoring server message");
            }
        }
        Ok(())
    }

    async fn handle_engine_event(&mut self, event: EngineEvent) -> Result<(), FatalError> {
        match event {
            EngineEvent::LocalCandidate {
                peer,
                candidate_json,
            } => {
                // Crippled mode never reaches here (the engine drops gathered
                // candidates), so this is always a real trickle-ICE relay.
                self.send_signal(
                    peer,
                    SignalKind::IceCandidate,
                    json!({ "IceCandidate": candidate_json }),
                )
                .await?;
            }
            EngineEvent::PcState { peer, state } => {
                emit(&Event::PcState {
                    peer,
                    state: state.to_string(),
                });
            }
            EngineEvent::RemoteChannel { peer, channel } => {
                self.engine.store_remote_channel(peer, channel);
            }
            EngineEvent::ChannelOpen { peer, label } => {
                emit(&Event::ChannelOpen {
                    peer,
                    label: label.clone(),
                });
                if self.engine.note_channel_open(peer, &label) {
                    self.on_pair_connected(peer).await?;
                }
            }
            EngineEvent::ChannelMessage { peer, label, text } => {
                self.received_labels
                    .entry(peer)
                    .or_default()
                    .insert(label.clone());
                emit(&Event::ChannelMessage { peer, label, text });
            }
        }
        Ok(())
    }

    /// Pair with `peer` per the server's directive: create the connection,
    /// offer when told to, then drain any defensively buffered signals.
    async fn establish_pair(&mut self, peer: PlayerId, initiate: bool) -> Result<(), FatalError> {
        if peer == self.my_id {
            tracing::warn!(%peer, "server asked us to pair with ourselves; ignoring");
            return Ok(());
        }
        if self.cli.leave_on_game_start {
            // A seat-vacating client never pairs: the plan/NewPeer is logged
            // (events were already emitted by the caller) but not acted on,
            // so it produces zero signaling traffic before departing.
            tracing::debug!(%peer, "leave-on-game-start: skipping pairing directive");
            return Ok(());
        }
        self.expected_peers.insert(peer);
        if self.p2p_deadline.is_none() {
            self.p2p_deadline =
                Some(Instant::now() + Duration::from_secs(self.cli.p2p_timeout_secs));
        }
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
        if let Some(buffered) = self.pending_signals.remove(&peer) {
            for signal in buffered {
                self.apply_signal(peer, signal).await?;
            }
        }
        Ok(())
    }

    /// Emit `signal_received` and route an inbound signal (buffering it when
    /// the peer is not paired yet).
    async fn handle_signal(
        &mut self,
        from: PlayerId,
        signal: serde_json::Value,
    ) -> Result<(), FatalError> {
        let kind = SignalKind::classify(&signal);
        emit(&Event::SignalReceived { from, kind });
        if !self.engine.is_paired(from) {
            self.pending_signals
                .entry(from)
                .or_default()
                .push_back(signal);
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
                if self.cli.cripple_ice {
                    // Belt and braces with the engine's interface filter:
                    // inbound candidates are dropped, never applied.
                    tracing::debug!(%from, "cripple-ice: dropping inbound candidate");
                    Ok(())
                } else {
                    match signal.get("IceCandidate").and_then(|c| c.as_str()) {
                        Some(payload) => self.engine.handle_remote_candidate(from, payload).await,
                        None => Err(anyhow::anyhow!("IceCandidate payload is not a string")),
                    }
                }
            }
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
        self.connected_pairs.insert(peer);
        emit(&Event::P2pPairConnected { peer });
        if self.cli.exchange {
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                let Some(channel) = self.engine.channel(peer, label) else {
                    emit(&Event::Error {
                        message: format!("open pair with {peer} is missing channel {label}"),
                    });
                    continue;
                };
                // The exact documented exchange payload (stable field order).
                let text = format!(
                    r#"{{"from":"{}","channel":"{}","seq":0}}"#,
                    self.my_id, label
                );
                match channel.send_text(text.clone()).await {
                    Ok(_bytes_sent) => {
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
                    Err(error) => {
                        emit(&Event::Error {
                            message: format!("send on {label} to {peer} failed: {error}"),
                        });
                    }
                }
            }
        }
        // All expected pairs connected resolves the overall status early.
        if self.transport_status.is_none() && self.all_expected_pairs_connected() {
            self.resolve_transport_status().await?;
        }
        Ok(())
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

    /// Resolve and report the single overall WebRTC transport status
    /// (Appendix G): `connected: true` iff at least one pair is connected at
    /// resolution time; a zero-pair resolution engages the relay fallback.
    async fn resolve_transport_status(&mut self) -> Result<(), FatalError> {
        let connected = !self.connected_pairs.is_empty();
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
        self.send_message(&ClientMessage::GameData {
            data: json!({ "relay_msg": text }),
        })
        .await?;
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
        self.send_message(&ClientMessage::Signal { to, signal })
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
        if self.webrtc_session_expected() {
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
            if !self.late_joined && self.relay_received_from.len() + 1 < self.cli.peers {
                unmet.push(format!(
                    "relay payloads observed from {} of {} peers",
                    self.relay_received_from.len(),
                    self.cli.peers - 1
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
