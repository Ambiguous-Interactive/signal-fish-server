/// Reconnection support module
///
/// Handles player reconnection after network disruptions including:
/// - Authentication token generation and validation
/// - Event buffering for missed messages
/// - Player disconnection tracking
/// - Reconnection window management
use crate::metrics::ServerMetrics;
use crate::protocol::{ErrorCode, PlayerId, PlayerInfo, RoomId, ServerMessage};
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::sync::{RwLock, RwLockReadGuard};
use tokio::time::Instant;
use uuid::Uuid;

/// Why a reconnection attempt was rejected.
///
/// Typed so the server maps each case to the correct client `ErrorCode` via
/// [`Self::error_code`] — never by inspecting the human-readable reason string.
/// `Display` yields the exact wire `reason` the client receives, so the typed
/// representation is the single source of truth for both the code and the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectionError {
    /// No disconnection record exists for the player id.
    NoRecord,
    /// Another socket already holds an in-flight claim on this record.
    AlreadyInProgress,
    /// The supplied token does not match the stored token.
    TokenMismatch,
    /// The token failed its own validity check (wrong binding or past its
    /// embedded expiry).
    TokenInvalid,
    /// The reconnection window elapsed.
    WindowExpired,
}

impl ReconnectionError {
    /// The client-facing [`ErrorCode`] for this rejection.
    ///
    /// A bad/mismatched token is a `RECONNECTION_TOKEN_INVALID`; only an elapsed
    /// *window* is `RECONNECTION_EXPIRED`; everything else is the generic
    /// `RECONNECTION_FAILED`.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::NoRecord | Self::AlreadyInProgress => ErrorCode::ReconnectionFailed,
            Self::TokenMismatch | Self::TokenInvalid => ErrorCode::ReconnectionTokenInvalid,
            Self::WindowExpired => ErrorCode::ReconnectionExpired,
        }
    }
}

impl std::fmt::Display for ReconnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::NoRecord => "No disconnection record found",
            Self::AlreadyInProgress => "Reconnection already in progress",
            Self::TokenMismatch => "Invalid reconnection token",
            Self::TokenInvalid => "Reconnection token is invalid or expired",
            Self::WindowExpired => "Reconnection window has expired",
        };
        f.write_str(reason)
    }
}

impl std::error::Error for ReconnectionError {}

/// Authentication token for reconnection
#[derive(Debug, Clone)]
pub struct ReconnectionToken {
    /// Token value (UUID)
    pub token: String,
    /// Player ID this token is for
    pub player_id: PlayerId,
    /// Room ID this token is for
    pub room_id: RoomId,
    /// When the token was created
    pub created_at: DateTime<Utc>,
    /// When the token expires
    pub expires_at: DateTime<Utc>,
}

impl ReconnectionToken {
    /// Create a new reconnection token
    pub fn new(player_id: PlayerId, room_id: RoomId, validity_seconds: i64) -> Self {
        let now = Utc::now();
        Self {
            token: Uuid::new_v4().to_string(),
            player_id,
            room_id,
            created_at: now,
            expires_at: expiration_from_signed(now, validity_seconds),
        }
    }

    fn new_with_unsigned_window(
        player_id: PlayerId,
        room_id: RoomId,
        validity_seconds: u64,
    ) -> Self {
        let now = Utc::now();
        Self {
            token: Uuid::new_v4().to_string(),
            player_id,
            room_id,
            created_at: now,
            expires_at: expiration_from_unsigned(now, validity_seconds),
        }
    }

    /// Check if token is expired
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    /// Check if token is valid for given player and room.
    ///
    /// Combines the binding with this token's own wall-clock expiry, so it is a
    /// convenience for embedders rather than the server's admission decision:
    /// the manager admits a reconnect on the binding plus its own monotonic
    /// deadline, captured at the disconnect.
    pub fn is_valid(&self, player_id: &PlayerId, room_id: &RoomId) -> bool {
        !self.is_expired() && self.player_id == *player_id && self.room_id == *room_id
    }

    /// Whether the token is bound to exactly this player and room.
    ///
    /// The manager checks the binding here and elapsed time against its own
    /// monotonic deadline, so an elapsed window is reported as
    /// [`ReconnectionError::WindowExpired`] rather than being masked by the
    /// token's wall-clock `expires_at` (which lands at the same instant).
    fn matches_binding(&self, player_id: &PlayerId, room_id: &RoomId) -> bool {
        self.player_id == *player_id && self.room_id == *room_id
    }
}

fn expiration_from_signed(now: DateTime<Utc>, validity_seconds: i64) -> DateTime<Utc> {
    Duration::try_seconds(validity_seconds)
        .and_then(|validity| now.checked_add_signed(validity))
        .unwrap_or_else(|| {
            if validity_seconds.is_negative() {
                DateTime::<Utc>::MIN_UTC
            } else {
                DateTime::<Utc>::MAX_UTC
            }
        })
}

fn expiration_from_unsigned(now: DateTime<Utc>, validity_seconds: u64) -> DateTime<Utc> {
    let Ok(validity_seconds) = i64::try_from(validity_seconds) else {
        return DateTime::<Utc>::MAX_UTC;
    };
    expiration_from_signed(now, validity_seconds)
}

/// A reconnect window opened at `now` closes at this MONOTONIC instant.
///
/// Reconnect eligibility must not move when the wall clock does: an NTP step,
/// a manual clock correction, or a host suspend/resume would otherwise expire a
/// live reconnector early or keep an elapsed one claimable. The UTC timestamps
/// on the record and its token are retained verbatim for diagnostics and wire
/// compatibility; only this deadline decides eligibility.
///
/// Saturates rather than panicking: an absurd configured window becomes a
/// practically unreachable deadline instead of an overflow.
fn monotonic_deadline(now: Instant, window_seconds: u64) -> Instant {
    crate::deadline::saturating_after(now, StdDuration::from_secs(window_seconds))
}

/// Event buffer for a room
#[derive(Debug, Clone)]
pub struct EventBuffer {
    /// Room ID
    pub room_id: RoomId,
    /// Maximum number of events to buffer
    pub max_size: usize,
    /// Buffered events (oldest to newest)
    pub events: VecDeque<BufferedEvent>,
    /// Highest sequence number ever evicted from this ring, or `None` if
    /// nothing has been evicted. The truncation watermark: sequence numbers
    /// are GLOBAL across rooms, so a *gap* in a room's buffered sequences is
    /// benign (another room consumed the intervening numbers) — only an
    /// explicit eviction record can prove a reconnecting player's replay is
    /// incomplete (`evicted_watermark > last_sequence`).
    pub evicted_watermark: Option<u64>,
}

/// A buffered event with metadata
#[derive(Debug, Clone)]
pub struct BufferedEvent {
    /// The actual server message
    pub message: ServerMessage,
    /// When the event occurred
    pub timestamp: DateTime<Utc>,
    /// Sequence number (for ordering)
    pub sequence: u64,
}

impl EventBuffer {
    /// Create a new event buffer
    pub fn new(room_id: RoomId, max_size: usize) -> Self {
        let max_size = max_size.min(crate::config::server::MAX_EVENT_BUFFER_SIZE);
        Self {
            room_id,
            max_size,
            // Avoid allocating from an untrusted public/configuration value.
            // The bounded ring grows only as replayable events arrive.
            events: VecDeque::new(),
            evicted_watermark: None,
        }
    }

    /// Add an event to the buffer, evicting the oldest events when the ring is
    /// full. Every evicted event raises [`Self::evicted_watermark`] so a later
    /// replay can report truncation honestly. Returns the number of events
    /// evicted by this push.
    pub fn push(&mut self, message: ServerMessage, sequence: u64) -> usize {
        let event = BufferedEvent {
            message,
            timestamp: Utc::now(),
            sequence,
        };

        self.events.push_back(event);

        // Remove oldest events if buffer is full
        let mut evicted = 0usize;
        while self.events.len() > self.max_size {
            if let Some(oldest) = self.events.pop_front() {
                self.evicted_watermark = Some(
                    self.evicted_watermark
                        .map_or(oldest.sequence, |watermark| watermark.max(oldest.sequence)),
                );
                evicted = evicted.saturating_add(1);
            }
        }
        evicted
    }

    /// Get events that occurred after a specific sequence number
    pub fn get_events_after(&self, after_sequence: u64) -> Vec<ServerMessage> {
        self.events
            .iter()
            .filter(|e| e.sequence > after_sequence)
            .map(|e| e.message.clone())
            .collect()
    }

    /// Get all buffered events
    pub fn get_all_events(&self) -> Vec<ServerMessage> {
        self.events.iter().map(|e| e.message.clone()).collect()
    }

    /// Clear all events
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

/// Disconnected player information
#[derive(Debug, Clone)]
pub struct DisconnectedPlayer {
    /// Player ID
    pub player_id: PlayerId,
    /// Room ID they were in
    pub room_id: RoomId,
    /// When they disconnected
    pub disconnected_at: DateTime<Utc>,
    /// Reconnection token
    pub token: ReconnectionToken,
    /// Last event sequence number they saw
    pub last_sequence: u64,
    /// Was player authority?
    pub was_authority: bool,
    /// Room membership snapshot used to restore the player on reconnect.
    pub player_info: Option<PlayerInfo>,
    /// The disconnecting connection's game-data incarnation epoch (protocol
    /// v3). Captured here so it SURVIVES the connection removal: on reconnect
    /// the restored connection resumes at `last_epoch + 1`, keeping the
    /// per-(sender, room) `(epoch, seq)` stream strictly increasing for a
    /// recipient that stayed connected across the sender's absence (a fresh
    /// reconnect socket would otherwise reset the epoch to 1, colliding with
    /// the first incarnation). The epoch is tracked for every sender regardless
    /// of protocol version (it bumps on each room join), so this is `0` only
    /// when the sender disconnected before ever joining a room.
    pub last_epoch: u32,
}

/// The subset of a still-pending disconnect record that a same-room
/// re-registration must PRESERVE — the player has not reconnected, so this
/// state is unchanged and re-deriving it from a (possibly racing, possibly
/// `None`) second `register_disconnection` call would clobber the real capture.
/// See [`ReconnectionManager::register_disconnection`].
struct PreservedPending {
    token: String,
    token_created_at: DateTime<Utc>,
    token_expires_at: DateTime<Utc>,
    disconnected_at: DateTime<Utc>,
    identity: Option<Arc<str>>,
    last_sequence: u64,
    last_epoch: u32,
    was_authority: bool,
    player_info: Option<PlayerInfo>,
    /// The monotonic reconnect deadline captured at the FIRST disconnect. A
    /// duplicate teardown is not a new disconnect, so it must not restart it.
    deadline: Instant,
}

impl DisconnectedPlayer {
    /// Whether `window_seconds` has elapsed since the captured wall-clock
    /// disconnect instant.
    ///
    /// Diagnostic only. The manager decides real reconnect eligibility from a
    /// monotonic deadline captured at the genuine disconnect, so this answer
    /// can differ from the server's after a wall-clock adjustment.
    pub fn is_expired(&self, window_seconds: i64) -> bool {
        let expiry = expiration_from_signed(self.disconnected_at, window_seconds);
        Utc::now() > expiry
    }
}

#[derive(Debug, Clone)]
struct ReconnectionRecord {
    disconnected: DisconnectedPlayer,
    identity: Option<Arc<str>>,
    claim: Option<ReconnectionClaimState>,
    /// The single monotonic instant after which this record's reconnect window
    /// is closed. Captured once per GENUINE disconnect and carried verbatim
    /// through duplicate same-room registration, so a repeated teardown cannot
    /// extend it and a wall-clock jump cannot move it.
    deadline: Instant,
}

impl ReconnectionRecord {
    /// Whether the reconnect window is closed at `now`.
    ///
    /// Exactly at the deadline the record is still claimable; only a strictly
    /// later instant closes it. Every eligibility decision — validation,
    /// claiming, expiry cleanup, and room-GC protection — asks this one
    /// question, so those four can never disagree at a boundary.
    fn window_closed_at(&self, now: Instant) -> bool {
        now > self.deadline
    }
}

#[derive(Debug, Clone)]
struct PreIssuedCredential {
    token: ReconnectionToken,
    identity: Option<Arc<str>>,
}

#[derive(Debug, Clone)]
struct ReconnectionClaimState {
    claim_id: Uuid,
    claimed_by: PlayerId,
    claimed_at: DateTime<Utc>,
}

impl ReconnectionClaimState {
    fn new(claimed_by: PlayerId) -> Self {
        let claimed_at = Utc::now();
        Self {
            claim_id: Uuid::new_v4(),
            claimed_by,
            claimed_at,
        }
    }
}

/// A validated reconnection claim that must be completed or released.
#[derive(Debug, Clone)]
pub struct ClaimedReconnection {
    pub disconnected: DisconnectedPlayer,
    claim_id: Uuid,
}

/// Missed-event lookup result for a reconnecting player.
///
/// `truncated` is true when the room's bounded replay ring evicted an event
/// the player needed (an event with a sequence above their `last_sequence`),
/// so `events` is only a suffix of what was broadcast while they were away.
/// It is decided by the ring's eviction watermark, NOT by sequence gaps:
/// sequence numbers are global across rooms, so another room's events create
/// benign gaps in this room's buffered sequences.
#[derive(Debug, Clone)]
pub struct MissedEvents {
    /// Replayable control events buffered after `last_sequence`, oldest first.
    pub events: Vec<ServerMessage>,
    /// Whether the ring evicted an event the player needed.
    pub truncated: bool,
}

/// Whether a server message is a room-uniform control event eligible for
/// reconnection replay via `Reconnected.missed_events`.
///
/// Replayable: events broadcast identically to every room member, describing
/// membership/lobby transitions a reconnector must not miss. Explicitly NOT
/// replayable:
/// - `GameStarting`: its `peer_connections` are per-recipient (self-declared
///   `ConnectionInfo` differs by viewer), so replaying another player's copy
///   would be wrong. A reconnector into a started session is served by the
///   `Reconnected` snapshot plus the dedicated late-join `SessionPlan` flow.
/// - `GameData` / `GameDataBinary` / `Signal`: high-rate data-path traffic
///   that would purge the control events that matter from the bounded ring.
/// - Everything directed at a single recipient (errors, pongs, `RoomJoined`,
///   `SessionPlan`, spectator self-confirmations, ...): a reconnector was
///   never owed another player's directed messages.
fn is_replayable_control_event(message: &ServerMessage) -> bool {
    matches!(
        message,
        ServerMessage::PlayerJoined { .. }
            | ServerMessage::PlayerLeft { .. }
            | ServerMessage::PlayerReconnected { .. }
            | ServerMessage::NewSpectatorJoined { .. }
            | ServerMessage::SpectatorDisconnected { .. }
            | ServerMessage::LobbyStateChanged { .. }
            | ServerMessage::AuthorityChanged { .. }
    )
}

/// Reconnection manager
#[derive(Default)]
struct ReplayState {
    disconnected_players: HashMap<PlayerId, ReconnectionRecord>,
    event_buffers: HashMap<RoomId, EventBuffer>,
    next_sequence: u64,
}

pub struct ReconnectionManager {
    /// Pending reconnectors, room replay gates, and global event sequencing.
    /// Keeping these in one lock makes registration, capture, completion, and
    /// cleanup atomic: no event can slip between a pending record and its room
    /// buffer, and stale cleanup cannot delete a newly registered gate.
    replay_state: RwLock<ReplayState>,
    /// Tokens minted at room join, BEFORE any disconnect (issue #136, F4):
    /// a token minted only at disconnect time can never legitimately reach
    /// the client it is for, making reconnection unusable in practice. One
    /// entry per currently-joined player; consumed by
    /// [`Self::register_disconnection`], discarded on voluntary leave /
    /// roomless teardown, and overwritten (rotated) by the next join.
    pre_issued: RwLock<HashMap<PlayerId, PreIssuedCredential>>,
    /// Reconnection window in seconds
    reconnection_window: u64,
    /// Event buffer size per room
    event_buffer_size: usize,
    /// Metrics sink
    metrics: Arc<ServerMetrics>,
    #[cfg(test)]
    pause_record_room_event: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    record_room_event_reached: tokio::sync::Notify,
    #[cfg(test)]
    release_record_room_event: tokio::sync::Notify,
}

/// A stable reconnection-room view held while room garbage collection runs.
///
/// Keeping the replay-state read lock alive prevents a disconnect from
/// publishing a new reconnection record between GC's protection snapshot and
/// its storage deletion. The room set alone is insufficient: it becomes stale
/// as soon as the lock used to build it is released.
pub(crate) struct RoomGcProtection<'a> {
    _state: RwLockReadGuard<'a, ReplayState>,
    room_ids: HashSet<RoomId>,
}

impl RoomGcProtection<'_> {
    pub(crate) fn room_ids(&self) -> &HashSet<RoomId> {
        &self.room_ids
    }
}

impl ReconnectionManager {
    /// Create a new reconnection manager
    pub fn new(
        reconnection_window: u64,
        event_buffer_size: usize,
        metrics: Arc<ServerMetrics>,
    ) -> Self {
        Self {
            replay_state: RwLock::new(ReplayState::default()),
            pre_issued: RwLock::new(HashMap::new()),
            reconnection_window,
            event_buffer_size,
            metrics,
            #[cfg(test)]
            pause_record_room_event: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            record_room_event_reached: tokio::sync::Notify::new(),
            #[cfg(test)]
            release_record_room_event: tokio::sync::Notify::new(),
        }
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn pause_record_room_event_for_test(&self) {
        self.pause_record_room_event
            .store(true, std::sync::atomic::Ordering::Release);
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) async fn wait_for_record_room_event_for_test(&self) {
        self.record_room_event_reached.notified().await;
    }

    #[cfg(test)]
    #[cfg(signal_fish_repository_tests)]
    pub(crate) fn release_record_room_event_for_test(&self) {
        self.release_record_room_event.notify_one();
    }

    /// Mint (or rotate) the reconnection token for a player joining `room_id`
    /// and return the token string to surface on the wire (`RoomJoined` /
    /// `Reconnected`, v3+ recipients).
    ///
    /// The string is stable from join through a later disconnect, but its
    /// EXPIRY is only armed by [`Self::register_disconnection`] (re-stamped to
    /// `now + reconnection_window` at disconnect time), so pre-issuing does
    /// not widen the reconnect window: the gate stays "window seconds from
    /// the disconnect", exactly as before — the client just finally KNOWS the
    /// token it will need.
    pub async fn pre_issue_token(&self, player_id: PlayerId, room_id: RoomId) -> String {
        self.pre_issue_token_with_identity(player_id, room_id, None)
            .await
    }

    pub(crate) async fn pre_issue_token_with_identity(
        &self,
        player_id: PlayerId,
        room_id: RoomId,
        identity: Option<Arc<str>>,
    ) -> String {
        let token = ReconnectionToken::new_with_unsigned_window(
            player_id,
            room_id,
            self.reconnection_window,
        );
        let token_string = token.token.clone();
        self.pre_issued
            .write()
            .await
            .insert(player_id, PreIssuedCredential { token, identity });
        self.metrics.increment_reconnection_tokens_issued();
        token_string
    }

    /// Discard a player's pre-issued token: a voluntary leave (or a teardown
    /// with no room to reconnect into) is not a disconnect, so the token must
    /// never become claimable — and the entry must not outlive the player
    /// (the map is bounded by currently-joined players ONLY because every
    /// exit path either consumes or discards).
    pub async fn discard_pre_issued(&self, player_id: &PlayerId) {
        self.pre_issued.write().await.remove(player_id);
    }

    /// Whether a pre-issued token is currently held for `player_id`.
    #[cfg(all(test, signal_fish_repository_tests))]
    pub(crate) async fn has_pre_issued_token(&self, player_id: &PlayerId) -> bool {
        self.pre_issued.read().await.contains_key(player_id)
    }

    pub async fn discard_pending_reconnection(&self, player_id: &PlayerId) -> bool {
        let mut state = self.replay_state.write().await;
        let removed = state.disconnected_players.remove(player_id);
        if let Some(record) = &removed {
            let room_id = record.disconnected.room_id;
            let others_waiting = state
                .disconnected_players
                .values()
                .any(|pending| pending.disconnected.room_id == room_id);
            if !others_waiting {
                state.event_buffers.remove(&room_id);
            }
        }
        if removed.is_some() {
            self.metrics.decrement_reconnection_sessions_active();
        }
        drop(state);

        removed.is_some()
    }

    /// Register a player disconnection.
    ///
    /// `last_epoch` is the disconnecting connection's game-data incarnation
    /// epoch, captured by the caller from the still-live connection (via
    /// `ConnectionManager::game_data_epoch`) so it survives into
    /// [`DisconnectedPlayer::last_epoch`] and the reconnect can resume at
    /// `last_epoch + 1` (see that field). The epoch is tracked for every sender
    /// regardless of negotiated version — it bumps on each room join — so
    /// whenever the still-live connection's epoch is reachable, pass it. `0`
    /// (resume at epoch 1) is the fallback for when there is no incarnation to
    /// preserve: the connection never joined a room, or it was already removed
    /// (`game_data_epoch` returns `None`) before this call.
    pub async fn register_disconnection(
        &self,
        player_id: PlayerId,
        room_id: RoomId,
        was_authority: bool,
        player_info: Option<PlayerInfo>,
        last_epoch: u32,
    ) -> String {
        self.register_disconnection_with_identity(
            player_id,
            room_id,
            was_authority,
            player_info,
            last_epoch,
            None,
        )
        .await
    }

    pub(crate) async fn register_disconnection_with_identity(
        &self,
        player_id: PlayerId,
        room_id: RoomId,
        was_authority: bool,
        player_info: Option<PlayerInfo>,
        last_epoch: u32,
        identity: Option<Arc<str>>,
    ) -> String {
        let mut state = self.replay_state.write().await;
        let fresh_last_sequence = state.next_sequence;

        // A same-room re-registration — the player registered a SECOND
        // disconnection for the same room while still pending, so it has NOT
        // reconnected — must preserve everything the client already
        // holds/expects from its first pending record. Snapshot those preserved
        // fields up front (owned, so the later `players.insert` is
        // unencumbered). A record from a DIFFERENT room, or a genuinely new one,
        // takes fresh values instead.
        let existing_same_room = state
            .disconnected_players
            .get(&player_id)
            .filter(|existing| existing.disconnected.room_id == room_id)
            .map(|existing| PreservedPending {
                token: existing.disconnected.token.token.clone(),
                token_created_at: existing.disconnected.token.created_at,
                token_expires_at: existing.disconnected.token.expires_at,
                disconnected_at: existing.disconnected.disconnected_at,
                identity: existing.identity.clone(),
                last_sequence: existing.disconnected.last_sequence,
                last_epoch: existing.disconnected.last_epoch,
                was_authority: existing.disconnected.was_authority,
                player_info: existing.disconnected.player_info.clone(),
                deadline: existing.deadline,
            });

        // A late teardown from the replaced socket may overlap an in-flight
        // reconnect. Once the credential is claimed, registration is a strict
        // no-op: replacing the record would reopen the single-use credential,
        // invalidate the original claim handle, and could consume the freshly
        // pre-issued token for the replacement connection's next disconnect.
        if let Some(existing) = existing_same_room.as_ref() {
            if state
                .disconnected_players
                .get(&player_id)
                .is_some_and(|record| record.claim.is_some())
            {
                return existing.token.clone();
            }
        }

        // Reuse the token STRING pre-issued at join (the client already holds
        // it — issue #136, F4), re-stamping its expiry so the reconnect gate
        // stays "window seconds from THIS disconnect". A duplicate same-room
        // registration already has an armed record and must not remove a token
        // rotated for the replacement connection. A missing or wrong-room
        // entry falls back to minting fresh (embedders that never pre-issue keep
        // the old disconnect-time semantics; such a token is unclaimable by an
        // honest client, exactly as before). The replay-state -> pre-issued
        // lock order is unique to registration; no path holds the latter while
        // awaiting the former.
        let pre_issued = if existing_same_room.is_none() {
            self.pre_issued.write().await.remove(&player_id)
        } else {
            None
        };

        // Both clocks are read at the same moment: the UTC captures stay the
        // human-readable record, while the monotonic instant anchors the only
        // deadline that decides eligibility.
        let now = Utc::now();
        let monotonic_now = Instant::now();
        // Token, in preference order: (1) the token from an existing same-room
        // pending record — the FIRST registration already consumed the
        // pre-issued entry, so minting fresh here would overwrite the record
        // with a token the client never received (issue #136, F4: the client
        // holds the join-time token); (2) the pre-issued join token; (3) a fresh
        // fallback mint (embedders that never pre-issue). Only (3) is a NEW
        // token, so only it counts toward the issued-token metric.
        let (token, credential_identity, minted_fresh) = match &existing_same_room {
            Some(existing) => (
                ReconnectionToken {
                    token: existing.token.clone(),
                    player_id,
                    room_id,
                    created_at: existing.token_created_at,
                    expires_at: existing.token_expires_at,
                },
                existing.identity.clone(),
                false,
            ),
            None => match pre_issued {
                Some(pre_issued) if pre_issued.token.room_id == room_id => {
                    let credential_identity = pre_issued.identity;
                    (
                        ReconnectionToken {
                            token: pre_issued.token.token,
                            player_id,
                            room_id,
                            created_at: pre_issued.token.created_at,
                            expires_at: expiration_from_unsigned(now, self.reconnection_window),
                        },
                        credential_identity,
                        false,
                    )
                }
                _ => (
                    ReconnectionToken::new_with_unsigned_window(
                        player_id,
                        room_id,
                        self.reconnection_window,
                    ),
                    identity,
                    true,
                ),
            },
        };
        let token_string = token.token.clone();

        // Preserve the ORIGINAL replay snapshot point across a same-room
        // re-registration: the player has not seen anything after its FIRST
        // disconnect, so advancing `last_sequence` to the current counter would
        // silently exclude the control events buffered in `(original, now]` from
        // a later `get_missed_events`, dropping events the client never saw
        // while `replay` still reported `complete`.
        let last_sequence = existing_same_room
            .as_ref()
            .map(|existing| existing.last_sequence)
            .unwrap_or(fresh_last_sequence);

        // Same-room re-registration keeps the ORIGINAL incarnation epoch: the
        // player has not reconnected, so its epoch has not advanced, and a
        // second capture reads `0` from the already-removed connection — `.max`
        // ensures that can never clobber the real value.
        let last_epoch = existing_same_room
            .as_ref()
            .map(|existing| existing.last_epoch.max(last_epoch))
            .unwrap_or(last_epoch);

        // Likewise keep the ORIGINAL disconnect snapshot — the authority flag
        // and room-membership `player_info` captured at the FIRST disconnect. A
        // racing second registration carrying `player_info: None` (or a stale
        // `was_authority`) must NOT clobber them: `reconnection_service` REJECTS
        // a reconnect whose stored `player_info` is `None`. Fall back to the new
        // call's values only when there is no original to preserve (or the
        // original itself never captured `player_info`).
        let was_authority = existing_same_room
            .as_ref()
            .map(|existing| existing.was_authority)
            .unwrap_or(was_authority);
        let player_info = match &existing_same_room {
            Some(existing) if existing.player_info.is_some() => existing.player_info.clone(),
            _ => player_info,
        };

        let record = ReconnectionRecord {
            disconnected: DisconnectedPlayer {
                player_id,
                room_id,
                disconnected_at: existing_same_room
                    .as_ref()
                    .map_or(now, |existing| existing.disconnected_at),
                token,
                last_sequence,
                was_authority,
                player_info,
                last_epoch,
            },
            identity: credential_identity,
            claim: None,
            deadline: existing_same_room.as_ref().map_or_else(
                || monotonic_deadline(monotonic_now, self.reconnection_window),
                |existing| existing.deadline,
            ),
        };
        let previous = state.disconnected_players.insert(player_id, record);
        // A re-registration from a NEW room replaces the old pending record.
        // If this player was the old room's last pending reconnector, nothing
        // else ever releases that room's replay buffer (completion and expiry
        // sweeps walk pending records, and the old room no longer has one) —
        // it would capture control events forever and replay ghosts. Release
        // it exactly like a completed reconnection would, while still holding
        // the records lock so the others-waiting check cannot race.
        let orphaned_room = previous
            .as_ref()
            .map(|record| record.disconnected.room_id)
            .filter(|previous_room| *previous_room != room_id)
            .filter(|previous_room| {
                !state
                    .disconnected_players
                    .values()
                    .any(|pending| pending.disconnected.room_id == *previous_room)
            });

        if let Some(orphaned_room) = orphaned_room {
            state.event_buffers.remove(&orphaned_room);
        }

        // Gate ON: an (empty) buffer marks the room as having a pending
        // reconnection, so `record_room_event` starts capturing its control
        // events. Skipped when the ring is disabled (`event_buffer_size` 0) —
        // replay is then reported `Unavailable` and nothing is captured.
        if self.event_buffer_size > 0 {
            state
                .event_buffers
                .entry(room_id)
                .or_insert_with(|| EventBuffer::new(room_id, self.event_buffer_size));
        }
        if previous.is_none() {
            self.metrics.increment_reconnection_sessions_active();
        }
        drop(state);

        // A reused pre-issued token was already counted at its join-time
        // mint; only a fresh fallback mint counts again.
        if minted_fresh {
            self.metrics.increment_reconnection_tokens_issued();
        }
        tracing::info!(
            %player_id,
            %room_id,
            "Player disconnection registered for reconnection"
        );

        token_string
    }

    /// Validate reconnection attempt
    ///
    /// This is an inspection-only API. Real reconnect attempts must use
    /// [`Self::claim_reconnection`] so duplicate sockets cannot race through
    /// validation and restore the same player ID.
    pub async fn validate_reconnection(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        token: &str,
    ) -> Result<DisconnectedPlayer, ReconnectionError> {
        self.validate_reconnection_with_identity(player_id, room_id, token, None)
            .await
    }

    async fn validate_reconnection_with_identity(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        token: &str,
        identity: Option<&str>,
    ) -> Result<DisconnectedPlayer, ReconnectionError> {
        let state = self.replay_state.read().await;

        let Some(record) = state.disconnected_players.get(player_id) else {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::NoRecord);
        };
        let player = &record.disconnected;

        if !crate::security::constant_time_eq(&player.token.token, token) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::TokenMismatch);
        }

        if !player.token.matches_binding(player_id, room_id) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::TokenInvalid);
        }

        if record.window_closed_at(Instant::now()) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::WindowExpired);
        }

        if !reconnection_identity_matches(record.identity.as_deref(), identity) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::TokenMismatch);
        }

        Ok(player.clone())
    }

    /// Atomically validate and reserve a reconnection record.
    ///
    /// This is the server-side entry point for real reconnect attempts. It
    /// prevents two fresh sockets from claiming the same token concurrently
    /// without burning the token before downstream room and connection
    /// restoration succeeds.
    pub async fn claim_reconnection(
        &self,
        claimed_by: &PlayerId,
        player_id: &PlayerId,
        room_id: &RoomId,
        token: &str,
    ) -> Result<ClaimedReconnection, ReconnectionError> {
        self.claim_reconnection_with_identity(claimed_by, player_id, room_id, token, None)
            .await
    }

    pub(crate) async fn claim_reconnection_with_identity(
        &self,
        claimed_by: &PlayerId,
        player_id: &PlayerId,
        room_id: &RoomId,
        token: &str,
        identity: Option<&str>,
    ) -> Result<ClaimedReconnection, ReconnectionError> {
        let mut state = self.replay_state.write().await;

        let Some(record) = state.disconnected_players.get_mut(player_id) else {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::NoRecord);
        };

        if let Some(claim) = &record.claim {
            self.metrics.increment_reconnection_validation_failure();
            tracing::warn!(
                %player_id,
                claimed_by = %claim.claimed_by,
                new_claimed_by = %claimed_by,
                claimed_at = %claim.claimed_at,
                "Reconnection claim already in progress"
            );
            return Err(ReconnectionError::AlreadyInProgress);
        }

        let player = &record.disconnected;

        if !crate::security::constant_time_eq(&player.token.token, token) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::TokenMismatch);
        }

        if !player.token.matches_binding(player_id, room_id) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::TokenInvalid);
        }

        if record.window_closed_at(Instant::now()) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::WindowExpired);
        }

        if !reconnection_identity_matches(record.identity.as_deref(), identity) {
            self.metrics.increment_reconnection_validation_failure();
            tracing::warn!(%player_id, "Reconnection credential identity mismatch");
            return Err(ReconnectionError::TokenMismatch);
        }

        let claim = ReconnectionClaimState::new(*claimed_by);
        let claimed = ClaimedReconnection {
            disconnected: record.disconnected.clone(),
            claim_id: claim.claim_id,
        };
        record.claim = Some(claim);

        Ok(claimed)
    }

    /// Complete reconnection and remove from disconnected players
    pub async fn complete_reconnection(&self, player_id: &PlayerId) {
        let mut state = self.replay_state.write().await;
        let removed = state.disconnected_players.remove(player_id);
        if let Some(record) = &removed {
            let room_id = record.disconnected.room_id;
            let others_waiting = state
                .disconnected_players
                .values()
                .any(|pending| pending.disconnected.room_id == room_id);
            if !others_waiting {
                state.event_buffers.remove(&room_id);
            }
        }
        if removed.is_some() {
            self.metrics.decrement_reconnection_sessions_active();
            self.metrics.increment_reconnection_completions();
        }
        drop(state);

        tracing::info!(%player_id, "Player reconnection completed");
    }

    /// Complete a reconnection record that was already reserved by
    /// [`Self::claim_reconnection`].
    pub async fn complete_claimed_reconnection(&self, claim: &ClaimedReconnection) -> bool {
        let mut state = self.replay_state.write().await;
        let Some(record) = state
            .disconnected_players
            .get(&claim.disconnected.player_id)
        else {
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                "Claimed reconnection completion found no pending record"
            );
            return false;
        };
        if record
            .claim
            .as_ref()
            .is_none_or(|state| state.claim_id != claim.claim_id)
        {
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                "Claimed reconnection completion did not match the active claim"
            );
            return false;
        }

        let Some(record) = state
            .disconnected_players
            .remove(&claim.disconnected.player_id)
        else {
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                "Claimed reconnection completion lost its pending record"
            );
            return false;
        };
        let room_id = record.disconnected.room_id;
        let others_waiting = state
            .disconnected_players
            .values()
            .any(|pending| pending.disconnected.room_id == room_id);
        if !others_waiting {
            state.event_buffers.remove(&room_id);
        }
        self.metrics.decrement_reconnection_sessions_active();
        self.metrics.increment_reconnection_completions();
        drop(state);

        tracing::info!(
            player_id = %record.disconnected.player_id,
            "Player reconnection completed"
        );
        true
    }

    /// Release a reserved reconnection record after a failed restore attempt.
    pub async fn release_reconnection_claim(&self, claim: &ClaimedReconnection) -> bool {
        let mut state = self.replay_state.write().await;
        let Some(record) = state
            .disconnected_players
            .get_mut(&claim.disconnected.player_id)
        else {
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                "Reconnection claim release found no pending record"
            );
            return false;
        };

        let Some(active_claim) = &record.claim else {
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                "Reconnection claim release found no active claim"
            );
            return false;
        };

        if active_claim.claim_id != claim.claim_id {
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                "Reconnection claim release did not match the active claim"
            );
            return false;
        }

        record.claim = None;
        tracing::debug!(
            player_id = %claim.disconnected.player_id,
            "Reconnection claim released for retry"
        );
        true
    }

    /// Get missed events for a reconnecting player.
    ///
    /// Returns the room's buffered replayable control events after
    /// `last_sequence` plus whether that list was truncated by ring eviction
    /// (see [`MissedEvents`]). No buffer for the room means no reconnection
    /// was pending there — buffer existence IS the "someone is pending" gate
    /// (`register_disconnection` creates it, the last completion/expiry
    /// removes it) — so absence means no replayable event occurred while
    /// anyone was pending: `events` empty, `truncated` false.
    pub async fn get_missed_events(&self, room_id: &RoomId, last_sequence: u64) -> MissedEvents {
        let state = self.replay_state.read().await;
        match state.event_buffers.get(room_id) {
            Some(buffer) => MissedEvents {
                events: buffer.get_events_after(last_sequence),
                truncated: buffer
                    .evicted_watermark
                    .is_some_and(|watermark| watermark > last_sequence),
            },
            None => MissedEvents {
                events: Vec::new(),
                truncated: false,
            },
        }
    }

    /// Record a room broadcast on the production delivery path.
    ///
    /// The single entry point the server's uniform-broadcast sites call for
    /// every room-wide control message. Cheap when idle: a non-replayable
    /// message (see `is_replayable_control_event`) returns immediately. For a
    /// replayable event, one ReplayState write-lock lookup both tests the
    /// pending-room gate and, when open, allocates the sequence and pushes the
    /// event. Events are recorded even if the subsequent broadcast partially
    /// fails, matching "what a connected player would have been sent".
    pub async fn record_room_event(&self, room_id: &RoomId, message: &ServerMessage) {
        if self.event_buffer_size == 0 || !is_replayable_control_event(message) {
            return;
        }
        #[cfg(test)]
        if self
            .pause_record_room_event
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            self.record_room_event_reached.notify_one();
            self.release_record_room_event.notified().await;
        }
        self.push_event(room_id, message.clone(), false).await;
    }

    /// Buffer an event for a room unconditionally (no replayable filter, no
    /// pending-reconnection gate; creates the room's ring if absent). Retained
    /// for embedders that wire their own delivery layer; the server itself
    /// records via [`Self::record_room_event`]. GameData/Signal/GameStarting
    /// are never replayed on the production path — reconnectors resync via the
    /// `Reconnected` snapshot and, for started sessions, the late-join
    /// `SessionPlan` flow.
    pub async fn buffer_event(&self, room_id: &RoomId, message: ServerMessage) {
        self.push_event(room_id, message, true).await;
    }

    /// Shared push path for [`Self::record_room_event`] and
    /// [`Self::buffer_event`]: assigns the next global sequence number, pushes
    /// into the room's ring, and advances the buffered/evicted metrics.
    async fn push_event(&self, room_id: &RoomId, message: ServerMessage, create_if_missing: bool) {
        let mut state = self.replay_state.write().await;
        if !create_if_missing && !state.event_buffers.contains_key(room_id) {
            return;
        }
        state.next_sequence = state.next_sequence.saturating_add(1);
        let sequence = state.next_sequence;
        let buffer = if create_if_missing {
            state
                .event_buffers
                .entry(*room_id)
                .or_insert_with(|| EventBuffer::new(*room_id, self.event_buffer_size))
        } else {
            let Some(buffer) = state.event_buffers.get_mut(room_id) else {
                tracing::warn!(%room_id, "Replay buffer disappeared while holding its state lock");
                return;
            };
            buffer
        };
        let evicted = buffer.push(message, sequence);
        drop(state);

        self.metrics.add_reconnection_events_buffered(1);
        self.metrics.add_reconnection_events_evicted(evicted as u64);
    }

    /// Configured per-room replay ring capacity (0 disables event replay; the
    /// `Reconnected.replay` field then reports `Unavailable` to v3 clients).
    pub fn event_buffer_size(&self) -> usize {
        self.event_buffer_size
    }

    /// Clear event buffer for a room (when room is deleted)
    pub async fn clear_room_buffer(&self, room_id: &RoomId) {
        let mut state = self.replay_state.write().await;
        let has_pending = state
            .disconnected_players
            .values()
            .any(|record| record.disconnected.room_id == *room_id);
        if !has_pending {
            state.event_buffers.remove(room_id);
            tracing::debug!(%room_id, "Event buffer cleared for room");
        } else {
            tracing::debug!(%room_id, "Retained event buffer for pending reconnection");
        }
    }

    /// Snapshot expired, unclaimed reconnect records that require durable room
    /// cleanup before their reservation can be discarded.
    pub async fn expired_cleanup_candidates(&self) -> Vec<(PlayerId, RoomId)> {
        let now = Instant::now();
        self.replay_state
            .read()
            .await
            .disconnected_players
            .iter()
            .filter(|(_, record)| record.claim.is_none() && record.window_closed_at(now))
            .map(|(player_id, record)| (*player_id, record.disconnected.room_id))
            .collect()
    }

    /// Remove one record only if it is still expired and unclaimed after the
    /// caller completed its durable cleanup. Returns whether it was removed.
    pub async fn remove_expired_reconnection(&self, player_id: &PlayerId) -> bool {
        let mut state = self.replay_state.write().await;
        let now = Instant::now();
        let removable = state
            .disconnected_players
            .get(player_id)
            .is_some_and(|record| record.claim.is_none() && record.window_closed_at(now));
        if !removable {
            return false;
        }
        let Some(record) = state.disconnected_players.remove(player_id) else {
            return false;
        };
        let room_id = record.disconnected.room_id;
        if !state
            .disconnected_players
            .values()
            .any(|pending| pending.disconnected.room_id == room_id)
        {
            state.event_buffers.remove(&room_id);
        }
        let remaining = state.disconnected_players.len();
        self.metrics
            .set_reconnection_sessions_active(remaining as u64);
        tracing::info!(%player_id, %room_id, "Removed durably cleaned expired reconnection record");
        true
    }

    /// Clean up expired disconnections.
    ///
    /// Also releases a room's replay ring once its LAST pending player
    /// expires (mirroring the "others_waiting" removal on completion): buffer
    /// existence is the "someone is pending" gate for `record_room_event`, so
    /// an expired-out room must stop capturing events immediately rather than
    /// waiting for room deletion to sweep the buffer.
    pub async fn cleanup_expired(&self) -> usize {
        let mut state = self.replay_state.write().await;
        let initial_count = state.disconnected_players.len();
        let mut expired_rooms = Vec::new();

        let now = Instant::now();
        state.disconnected_players.retain(|player_id, record| {
            let expired = record.claim.is_none() && record.window_closed_at(now);
            if expired {
                tracing::info!(%player_id, "Removing expired reconnection record");
                expired_rooms.push(record.disconnected.room_id);
            }
            !expired
        });
        let removed = initial_count.saturating_sub(state.disconnected_players.len());
        let remaining = state.disconnected_players.len();
        let rooms_still_pending: std::collections::HashSet<RoomId> = state
            .disconnected_players
            .values()
            .map(|record| record.disconnected.room_id)
            .collect();

        expired_rooms.retain(|room_id| !rooms_still_pending.contains(room_id));
        for room_id in expired_rooms {
            state.event_buffers.remove(&room_id);
        }
        if removed > 0 {
            self.metrics
                .set_reconnection_sessions_active(remaining as u64);
        }
        drop(state);

        if removed > 0 {
            tracing::info!(count = removed, "Cleaned up expired reconnection records");
        }

        removed
    }

    /// Check if a player has a pending disconnection
    pub async fn has_pending_reconnection(&self, player_id: &PlayerId) -> bool {
        self.replay_state
            .read()
            .await
            .disconnected_players
            .contains_key(player_id)
    }

    /// Get all disconnected players for a room
    pub async fn get_disconnected_players_in_room(&self, room_id: &RoomId) -> Vec<PlayerId> {
        self.replay_state
            .read()
            .await
            .disconnected_players
            .values()
            .filter(|p| p.disconnected.room_id == *room_id)
            .map(|p| p.disconnected.player_id)
            .collect()
    }

    /// Room IDs that currently hold at least one unexpired or actively claimed
    /// reconnection record. Room garbage collection must not delete these rooms: a room
    /// whose members disconnected simultaneously is empty, so it would
    /// otherwise be reaped by the empty-room sweep before its still-valid
    /// reconnection tokens could be redeemed — the reconnect then fails
    /// `RoomNotFound` with a token the client was told is good (BUG-1
    /// corollary B). Expired records are excluded (they are swept by
    /// [`Self::cleanup_expired`] and no longer protect anything); claimed and
    /// unclaimed records both protect, since an in-flight claim still needs
    /// the room to exist.
    pub(crate) async fn room_gc_protection(&self) -> RoomGcProtection<'_> {
        let now = Instant::now();
        let state = self.replay_state.read().await;
        let room_ids = state
            .disconnected_players
            .values()
            .filter(|record| record.claim.is_some() || !record.window_closed_at(now))
            .map(|record| record.disconnected.room_id)
            .collect();
        RoomGcProtection {
            _state: state,
            room_ids,
        }
    }

    /// Return a point-in-time snapshot of rooms with active reconnection
    /// records. Callers that authorize room deletion must use the internally
    /// pinned GC view instead so registration cannot race that decision.
    pub async fn rooms_with_active_reconnections(&self) -> HashSet<RoomId> {
        self.room_gc_protection().await.room_ids().clone()
    }
}

fn reconnection_identity_matches(expected: Option<&str>, provided: Option<&str>) -> bool {
    match (expected, provided) {
        (None, None) => true,
        (Some(expected), Some(provided)) => crate::security::constant_time_eq(expected, provided),
        (None, Some(_)) | (Some(_), None) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ServerMetrics;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use tokio::sync::{Barrier, Notify};

    #[test]
    fn reconnection_error_maps_each_variant_to_its_client_code() {
        // A bad/mismatched token is a token error; only an elapsed *window* is
        // expired. Guards the prior latent bug where the "token is invalid or
        // expired" reason matched `contains("expired")` and was mislabeled as
        // RECONNECTION_EXPIRED.
        assert_eq!(
            ReconnectionError::NoRecord.error_code(),
            ErrorCode::ReconnectionFailed
        );
        assert_eq!(
            ReconnectionError::AlreadyInProgress.error_code(),
            ErrorCode::ReconnectionFailed
        );
        assert_eq!(
            ReconnectionError::TokenMismatch.error_code(),
            ErrorCode::ReconnectionTokenInvalid
        );
        assert_eq!(
            ReconnectionError::TokenInvalid.error_code(),
            ErrorCode::ReconnectionTokenInvalid
        );
        assert_eq!(
            ReconnectionError::WindowExpired.error_code(),
            ErrorCode::ReconnectionExpired
        );
    }

    #[test]
    fn reconnection_error_display_preserves_wire_reason_strings() {
        // The `Display` text is the client-facing wire `reason`; keep it stable.
        assert_eq!(
            ReconnectionError::NoRecord.to_string(),
            "No disconnection record found"
        );
        assert_eq!(
            ReconnectionError::AlreadyInProgress.to_string(),
            "Reconnection already in progress"
        );
        assert_eq!(
            ReconnectionError::TokenMismatch.to_string(),
            "Invalid reconnection token"
        );
        assert_eq!(
            ReconnectionError::TokenInvalid.to_string(),
            "Reconnection token is invalid or expired"
        );
        assert_eq!(
            ReconnectionError::WindowExpired.to_string(),
            "Reconnection window has expired"
        );
    }

    #[test]
    fn test_reconnection_token_creation() {
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let validity_seconds: i64 = 300;
        let token = ReconnectionToken::new(player_id, room_id, validity_seconds);

        assert_eq!(token.player_id, player_id);
        assert_eq!(token.room_id, room_id);
        assert!(!token.is_expired());
        assert!(token.is_valid(&player_id, &room_id));

        let now = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("fixed test timestamp must parse")
            .with_timezone(&Utc);

        assert_eq!(
            expiration_from_unsigned(now, 300),
            now.checked_add_signed(Duration::seconds(300))
                .expect("ordinary window fits")
        );
        for window in [i64::MAX as u64, (i64::MAX as u64) + 1, u64::MAX] {
            assert_eq!(
                expiration_from_unsigned(now, window),
                DateTime::<Utc>::MAX_UTC,
                "window {window} must saturate at Chrono's upper bound"
            );
        }

        let token = ReconnectionToken::new_with_unsigned_window(player_id, room_id, u64::MAX);

        assert_eq!(token.expires_at, DateTime::<Utc>::MAX_UTC);
        assert!(!token.is_expired());
        assert!(token.is_valid(&player_id, &room_id));
    }

    #[test]
    fn test_reconnection_token_validation() {
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let other_player = Uuid::new_v4();
        let other_room = Uuid::new_v4();

        let token = ReconnectionToken::new(player_id, room_id, 300);

        // Should be valid for correct player and room
        assert!(token.is_valid(&player_id, &room_id));

        // Should be invalid for wrong player
        assert!(!token.is_valid(&other_player, &room_id));

        // Should be invalid for wrong room
        assert!(!token.is_valid(&player_id, &other_room));
    }

    #[test]
    fn test_event_buffer_push() {
        let room_id = Uuid::new_v4();
        let mut buffer = EventBuffer::new(room_id, 3);

        use crate::protocol::ServerMessage;

        // Add 5 events (buffer size is 3)
        for i in 0..5 {
            buffer.push(ServerMessage::Pong, i);
        }

        // Should only keep last 3 events
        assert_eq!(buffer.events.len(), 3);
        assert_eq!(buffer.events[0].sequence, 2); // Oldest kept
        assert_eq!(buffer.events[2].sequence, 4); // Newest
    }

    #[test]
    fn extreme_event_buffer_size_does_not_panic_during_construction() {
        let buffer = std::panic::catch_unwind(|| EventBuffer::new(Uuid::new_v4(), usize::MAX));
        assert!(buffer.is_ok());
    }

    #[test]
    fn test_event_buffer_get_events_after() {
        let room_id = Uuid::new_v4();
        let mut buffer = EventBuffer::new(room_id, 10);

        use crate::protocol::ServerMessage;

        for i in 0..5 {
            buffer.push(ServerMessage::Pong, i);
        }

        // Get events after sequence 2
        let events = buffer.get_events_after(2);
        assert_eq!(events.len(), 2); // Sequences 3 and 4
    }

    #[tokio::test]
    async fn test_reconnection_manager_flow() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = Arc::new(ReconnectionManager::new(300, 100, metrics));
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();

        // Register disconnection
        let token = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;

        // Validate reconnection
        let result = manager
            .validate_reconnection(&player_id, &room_id, &token)
            .await;
        assert!(result.is_ok());

        // Complete reconnection
        manager.complete_reconnection(&player_id).await;

        // Should no longer have pending reconnection
        assert!(!manager.has_pending_reconnection(&player_id).await);
    }

    /// `rooms_with_active_reconnections` reports the rooms that room GC must
    /// spare. A registered (unexpired) disconnection lists its room; an empty
    /// manager lists nothing; completing the reconnection drops the room. Per-
    /// record expiry is delegated to `DisconnectedPlayer::is_expired` (covered
    /// by `reconnection_error_maps_each_variant_to_its_client_code` semantics).
    #[tokio::test]
    async fn rooms_with_active_reconnections_tracks_protected_rooms() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);

        assert!(
            manager.rooms_with_active_reconnections().await.is_empty(),
            "a fresh manager protects no rooms"
        );

        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;

        let protected = manager.rooms_with_active_reconnections().await;
        assert!(
            protected.contains(&room_id),
            "a live reconnection record must protect its room from GC"
        );
        assert_eq!(protected.len(), 1);

        manager.complete_reconnection(&player_id).await;
        assert!(
            manager.rooms_with_active_reconnections().await.is_empty(),
            "completing the reconnection releases the room"
        );
    }

    /// Issue #257: once a reconnect is admitted, its exclusive claim owns the
    /// restore attempt even if the original wall-clock window expires while
    /// storage or room-lane work is still in flight. Room GC must therefore
    /// continue protecting the empty room until the claim completes or is
    /// released.
    #[tokio::test(start_paused = true)]
    async fn claimed_reconnection_remains_gc_protected_after_original_window() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let token = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;
        let claimant = Uuid::new_v4();

        manager
            .claim_reconnection(&claimant, &player_id, &room_id, &token)
            .await
            .expect("the still-valid reconnect is admitted and claimed");

        tokio::time::advance(StdDuration::from_secs(301)).await;

        let protected = manager.rooms_with_active_reconnections().await;
        assert!(
            protected.contains(&room_id),
            "an in-flight claim must keep its room alive after the admission window crosses"
        );
    }

    #[tokio::test]
    async fn test_reconnection_claim_is_single_use_under_concurrency() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = Arc::new(ReconnectionManager::new(300, 100, metrics));
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let token = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;
        let current_a = Uuid::new_v4();
        let current_b = Uuid::new_v4();

        let barrier = Arc::new(Barrier::new(2));
        let task_a = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            let token = token.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                manager
                    .claim_reconnection(&current_a, &player_id, &room_id, &token)
                    .await
                    .is_ok()
            })
        };
        let task_b = {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            let token = token.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                manager
                    .claim_reconnection(&current_b, &player_id, &room_id, &token)
                    .await
                    .is_ok()
            })
        };

        let (claimed_a, claimed_b) = tokio::join!(task_a, task_b);
        let successes = [claimed_a.unwrap(), claimed_b.unwrap()]
            .into_iter()
            .filter(|claimed| *claimed)
            .count();
        assert_eq!(successes, 1, "exactly one same-token claim may succeed");
        assert!(
            manager.has_pending_reconnection(&player_id).await,
            "a claimed record remains pending until completed"
        );
        assert!(manager
            .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_reconnection_claim_release_allows_retry() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let token = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;

        let first_claim = manager
            .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
            .await
            .expect("first claim succeeds");
        assert!(manager
            .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
            .await
            .is_err());

        assert!(manager.release_reconnection_claim(&first_claim).await);
        let second_claim = manager
            .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
            .await
            .expect("released claim can be retried");
        assert!(manager.complete_claimed_reconnection(&second_claim).await);
        assert!(!manager.has_pending_reconnection(&player_id).await);
    }

    /// A released claim handle may outlive its restore attempt. If another
    /// socket subsequently reserves the same record, the stale handle must be
    /// unable to release or complete that newer reservation.
    #[tokio::test]
    async fn stale_reconnection_claim_handle_cannot_mutate_retry() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let token = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;

        let stale_claim = manager
            .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
            .await
            .expect("first claim succeeds");
        assert!(manager.release_reconnection_claim(&stale_claim).await);

        let active_claim = manager
            .claim_reconnection(&Uuid::new_v4(), &player_id, &room_id, &token)
            .await
            .expect("released credential can be claimed again");

        assert!(
            !manager.release_reconnection_claim(&stale_claim).await,
            "a stale handle must not release the active retry"
        );
        assert!(
            !manager.complete_claimed_reconnection(&stale_claim).await,
            "a stale handle must not consume the active retry"
        );
        assert!(
            manager.has_pending_reconnection(&player_id).await,
            "rejecting stale mutations must preserve the active record"
        );

        assert!(manager.complete_claimed_reconnection(&active_claim).await);
        assert!(!manager.has_pending_reconnection(&player_id).await);
    }

    #[tokio::test(start_paused = true)]
    async fn test_reconnection_cleanup_updates_active_session_gauge() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(1, 100, Arc::clone(&metrics));
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let _token = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;
        tokio::time::advance(StdDuration::from_secs(5)).await;

        assert_eq!(
            metrics.reconnection_sessions_active.load(Ordering::Relaxed),
            1
        );
        assert_eq!(manager.cleanup_expired().await, 1);
        assert_eq!(
            metrics.reconnection_sessions_active.load(Ordering::Relaxed),
            0
        );
    }

    #[tokio::test]
    async fn test_event_buffering() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let room_id = Uuid::new_v4();

        use crate::protocol::ServerMessage;

        // Buffer some events
        manager.buffer_event(&room_id, ServerMessage::Pong).await;
        manager.buffer_event(&room_id, ServerMessage::Pong).await;
        manager.buffer_event(&room_id, ServerMessage::Pong).await;

        // Get all events
        let missed = manager.get_missed_events(&room_id, 0).await;
        assert_eq!(missed.events.len(), 3);
        assert!(!missed.truncated, "nothing was evicted");
    }

    /// A room-uniform control event for the replay-path tests.
    fn control_event() -> ServerMessage {
        ServerMessage::PlayerLeft {
            player_id: Uuid::new_v4(),
            epoch: None,
            final_seq: None,
        }
    }

    #[tokio::test]
    async fn overflowed_ring_reports_truncation_and_counts_evictions() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 3, Arc::clone(&metrics));
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;

        // 5 events into a 3-slot ring: 2 evictions.
        for _ in 0..5 {
            manager.record_room_event(&room_id, &control_event()).await;
        }

        let missed = manager.get_missed_events(&room_id, 0).await;
        assert_eq!(missed.events.len(), 3, "the ring keeps only the newest 3");
        assert!(
            missed.truncated,
            "events the player needed were evicted, so the replay must report truncation"
        );
        assert_eq!(
            metrics.reconnection_events_evicted.load(Ordering::Relaxed),
            2,
            "each ring eviction advances the eviction metric"
        );
    }

    #[tokio::test]
    async fn cross_room_sequence_gaps_are_not_truncation() {
        // Sequence numbers are GLOBAL across rooms, so another room's events
        // create benign gaps in this room's buffered sequence numbers. Only
        // the explicit eviction watermark may report truncation — this pins
        // why "gap in sequence numbers" is not a valid truncation predicate.
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, Arc::clone(&metrics));
        let room_a = Uuid::new_v4();
        let room_b = Uuid::new_v4();
        manager
            .register_disconnection(Uuid::new_v4(), room_a, false, None, 0)
            .await;
        manager
            .register_disconnection(Uuid::new_v4(), room_b, false, None, 0)
            .await;

        // Interleave: room B consumes global sequence numbers between room
        // A's events, so room A's buffered sequences are non-contiguous.
        manager.record_room_event(&room_a, &control_event()).await;
        manager.record_room_event(&room_b, &control_event()).await;
        manager.record_room_event(&room_b, &control_event()).await;
        manager.record_room_event(&room_a, &control_event()).await;

        let missed = manager.get_missed_events(&room_a, 0).await;
        assert_eq!(missed.events.len(), 2);
        assert!(
            !missed.truncated,
            "nothing was evicted from room A; room B's global sequence numbers must not mark it truncated"
        );
        assert_eq!(
            metrics.reconnection_events_evicted.load(Ordering::Relaxed),
            0
        );
    }

    #[tokio::test]
    async fn record_room_event_buffers_only_while_a_reconnection_is_pending() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();

        // No pending disconnection: the gate is closed, nothing buffers.
        manager.record_room_event(&room_id, &control_event()).await;
        assert!(
            manager
                .get_missed_events(&room_id, 0)
                .await
                .events
                .is_empty(),
            "a room with nobody pending must not accumulate events"
        );

        // Pending: the gate is open.
        manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;
        manager.record_room_event(&room_id, &control_event()).await;
        assert_eq!(manager.get_missed_events(&room_id, 0).await.events.len(), 1);

        // Last pending player completed: the buffer is released and the gate
        // closes again.
        manager.complete_reconnection(&player_id).await;
        assert!(
            !manager
                .replay_state
                .read()
                .await
                .event_buffers
                .contains_key(&room_id),
            "completing the last pending reconnection must release the room buffer"
        );
        manager.record_room_event(&room_id, &control_event()).await;
        assert!(manager
            .get_missed_events(&room_id, 0)
            .await
            .events
            .is_empty());
    }

    #[tokio::test]
    async fn registration_and_first_room_event_are_one_ordered_replay_transition() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = Arc::new(ReconnectionManager::new(300, 100, metrics));
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        manager.pre_issue_token(player_id, room_id).await;

        // Queue registration first and event capture second behind the one
        // ReplayState write lock. Tokio's fair write-lock queue makes this a
        // deterministic version of the old next-sequence/buffer-creation gap.
        let state_guard = manager.replay_state.write().await;
        let register_started = Arc::new(Notify::new());
        let registration = {
            let manager = Arc::clone(&manager);
            let started = Arc::clone(&register_started);
            tokio::spawn(async move {
                started.notify_one();
                manager
                    .register_disconnection(player_id, room_id, false, None, 0)
                    .await
            })
        };
        register_started.notified().await;
        tokio::task::yield_now().await;

        let event_started = Arc::new(Notify::new());
        let event = {
            let manager = Arc::clone(&manager);
            let started = Arc::clone(&event_started);
            tokio::spawn(async move {
                started.notify_one();
                manager.record_room_event(&room_id, &control_event()).await;
            })
        };
        event_started.notified().await;
        tokio::task::yield_now().await;
        drop(state_guard);

        registration
            .await
            .expect("registration task should not panic");
        event.await.expect("event task should not panic");
        let last_sequence = manager.replay_state.read().await.disconnected_players[&player_id]
            .disconnected
            .last_sequence;
        let missed = manager.get_missed_events(&room_id, last_sequence).await;
        assert_eq!(
            missed.events.len(),
            1,
            "an event ordered after registration must be included in replay"
        );
        assert!(!missed.truncated);
    }

    #[tokio::test(start_paused = true)]
    async fn last_expiry_cleanup_cannot_delete_a_new_registration_gate() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = Arc::new(ReconnectionManager::new(1, 100, Arc::clone(&metrics)));
        let expired_player = Uuid::new_v4();
        let new_player = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        manager
            .register_disconnection(expired_player, room_id, false, None, 0)
            .await;
        tokio::time::advance(StdDuration::from_secs(5)).await;

        // Hold the write lock so the spawned sweep parks on it and the new
        // registration can be ordered against it deterministically.
        let state_guard = manager.replay_state.write().await;

        let cleanup_started = Arc::new(Notify::new());
        let cleanup = {
            let manager = Arc::clone(&manager);
            let started = Arc::clone(&cleanup_started);
            tokio::spawn(async move {
                started.notify_one();
                manager.cleanup_expired().await
            })
        };
        cleanup_started.notified().await;
        tokio::task::yield_now().await;

        let registration_started = Arc::new(Notify::new());
        let registration = {
            let manager = Arc::clone(&manager);
            let started = Arc::clone(&registration_started);
            tokio::spawn(async move {
                started.notify_one();
                manager
                    .register_disconnection(new_player, room_id, false, None, 0)
                    .await
            })
        };
        registration_started.notified().await;
        tokio::task::yield_now().await;
        drop(state_guard);

        assert_eq!(cleanup.await.expect("cleanup task should not panic"), 1);
        registration
            .await
            .expect("registration task should not panic");
        manager.record_room_event(&room_id, &control_event()).await;
        let state = manager.replay_state.read().await;
        assert!(state.disconnected_players.contains_key(&new_player));
        assert_eq!(
            state.event_buffers[&room_id].events.len(),
            1,
            "new registration must retain a live replay gate after old cleanup"
        );
        assert_eq!(
            metrics.reconnection_sessions_active.load(Ordering::Relaxed),
            1,
            "active-session gauge must match the one surviving registration"
        );
    }

    /// Issue #136 (F4): the token surfaced at join is the SAME string the
    /// disconnect arms — reusing it re-stamps only the expiry (window counted
    /// from the disconnect, not the join), so pre-issuing never widens the
    /// reconnect window.
    #[tokio::test]
    async fn pre_issued_token_is_reused_at_disconnect_with_restamped_expiry() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics.clone());
        let player = Uuid::new_v4();
        let room = Uuid::new_v4();

        let wire_token = manager.pre_issue_token(player, room).await;
        assert_eq!(
            metrics
                .reconnection_tokens_issued
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );

        let armed_token = manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        assert_eq!(
            armed_token, wire_token,
            "the disconnect must arm the SAME token the client already holds"
        );
        // Reuse must not double-count the mint.
        assert_eq!(
            metrics
                .reconnection_tokens_issued
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        // A SECOND same-room disconnect while STILL PENDING (no reconnect in
        // between) must PRESERVE the client-held token: the first registration
        // already consumed the pre-issued entry, so re-minting here would
        // overwrite the record with a token the client never received (delivered
        // at join) and silently break its reconnect. No new mint is counted.
        let re_armed = manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        assert_eq!(
            re_armed, wire_token,
            "a still-pending same-room re-registration keeps the client's token"
        );
        assert_eq!(
            metrics
                .reconnection_tokens_issued
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "preserving the token must not count a new mint"
        );
    }

    /// Regression: a same-room re-registration while still pending must keep the
    /// client's pre-issued token CLAIMABLE. Before the fix, the second
    /// registration found the pre-issued entry already consumed, minted a fresh
    /// token, and overwrote the record — so a client reconnecting with the token
    /// it received at join (the only token it ever held) was rejected.
    #[tokio::test]
    async fn same_room_reregistration_keeps_pre_issued_token_claimable() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player = Uuid::new_v4();
        let room = Uuid::new_v4();

        let wire_token = manager.pre_issue_token(player, room).await;
        manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        // A benign second registration for the same still-pending room (e.g. a
        // concurrent teardown path re-invoking the disconnect handler).
        manager
            .register_disconnection(player, room, false, None, 0)
            .await;

        // The client only ever held `wire_token`; it must still reconnect.
        let claim = manager
            .claim_reconnection(&Uuid::new_v4(), &player, &room, &wire_token)
            .await;
        assert!(
            claim.is_ok(),
            "the join-time token must stay claimable after re-registration: {claim:?}"
        );
    }

    /// An unrepresentable reconnection window saturates to a deadline beyond
    /// any realistic horizon instead of inverting into an already-expired
    /// instant (the same failure class `deadline::after` exists to prevent).
    /// The saturated result must also exceed the retired fixed 100-year
    /// fallback so this pin distinguishes the two saturation strategies.
    #[test]
    fn absurd_reconnect_windows_never_expire_immediately() {
        let now = Instant::now();
        let saturated = monotonic_deadline(now, u64::MAX);
        assert!(
            saturated > now,
            "an unrepresentable window is beyond the process lifetime, not elapsed"
        );
        assert!(
            saturated
                > now
                    .checked_add(StdDuration::from_secs(150 * 365 * 24 * 60 * 60))
                    .expect("150-year horizon is representable on every supported platform"),
            "saturation must reach past the retired 100-year fallback"
        );
        assert_eq!(
            monotonic_deadline(now, 300),
            now + StdDuration::from_secs(300),
            "representable windows keep their exact absolute instant"
        );
    }

    /// Every eligibility surface reads the same monotonic deadline, so all of
    /// them are live at exactly the deadline and all of them are closed one
    /// tick later. A record still claimable at its exact deadline must not be
    /// nominated for cleanup or dropped from room-GC protection, and the
    /// elapsed window must surface as `WindowExpired` — not as the
    /// token's own wall-clock expiry, which lands at the same instant.
    #[tokio::test(start_paused = true)]
    async fn reconnect_eligibility_flips_once_at_the_monotonic_deadline() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let token = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;

        for (label, step) in [
            (
                "one second before the deadline",
                StdDuration::from_secs(299),
            ),
            ("exactly at the deadline", StdDuration::from_secs(1)),
        ] {
            tokio::time::advance(step).await;
            assert!(
                manager
                    .validate_reconnection(&player_id, &room_id, &token)
                    .await
                    .is_ok(),
                "{label}: validation must still admit the record"
            );
            assert!(
                manager
                    .rooms_with_active_reconnections()
                    .await
                    .contains(&room_id),
                "{label}: room GC must still spare the room"
            );
            assert!(
                manager.expired_cleanup_candidates().await.is_empty(),
                "{label}: cleanup must not nominate the record"
            );
        }

        tokio::time::advance(StdDuration::from_millis(1)).await;
        assert_eq!(
            manager
                .validate_reconnection(&player_id, &room_id, &token)
                .await
                .expect_err("the window closed"),
            ReconnectionError::WindowExpired,
            "an elapsed window is RECONNECTION_EXPIRED, not a token rejection"
        );
        assert!(
            !manager
                .rooms_with_active_reconnections()
                .await
                .contains(&room_id),
            "past the deadline the room loses reconnection protection"
        );
        assert_eq!(
            manager.expired_cleanup_candidates().await,
            vec![(player_id, room_id)],
            "past the deadline the record is a cleanup candidate"
        );
        assert_eq!(manager.cleanup_expired().await, 1);
    }

    /// The deadline is monotonic, so no wall-clock adjustment can move it in
    /// either direction. Both cases rewrite every UTC field the eligibility
    /// decision used to read.
    #[tokio::test(start_paused = true)]
    async fn wall_clock_jumps_cannot_open_or_close_the_reconnect_window() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let token = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;

        // A forward jump of a day ages both UTC captures far past the window.
        {
            let mut state = manager.replay_state.write().await;
            let disconnected = &mut state
                .disconnected_players
                .get_mut(&player_id)
                .expect("pending record")
                .disconnected;
            disconnected.disconnected_at = Utc::now() - Duration::days(1);
            disconnected.token.expires_at = Utc::now() - Duration::days(1);
        }
        assert!(
            manager
                .validate_reconnection(&player_id, &room_id, &token)
                .await
                .is_ok(),
            "a forward wall-clock jump must not close a live reconnect window"
        );
        assert!(
            manager.expired_cleanup_candidates().await.is_empty(),
            "a forward wall-clock jump must not make the record collectable"
        );

        // A backward jump cannot resurrect a genuinely elapsed window.
        tokio::time::advance(StdDuration::from_secs(301)).await;
        {
            let mut state = manager.replay_state.write().await;
            let disconnected = &mut state
                .disconnected_players
                .get_mut(&player_id)
                .expect("pending record")
                .disconnected;
            disconnected.disconnected_at = Utc::now() + Duration::days(1);
            disconnected.token.expires_at = Utc::now() + Duration::days(1);
        }
        assert_eq!(
            manager
                .validate_reconnection(&player_id, &room_id, &token)
                .await
                .expect_err("the monotonic window elapsed"),
            ReconnectionError::WindowExpired,
            "a backward wall-clock jump must not reopen an elapsed window"
        );
    }

    /// Registration only opens a NEW monotonic window for a genuine disconnect.
    /// A duplicate same-room teardown carries the original deadline forward; a
    /// different-room registration is a new disconnect and starts a fresh one.
    #[tokio::test(start_paused = true)]
    async fn only_a_genuine_disconnect_opens_a_new_monotonic_window() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let other_room = Uuid::new_v4();
        let token = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;

        tokio::time::advance(StdDuration::from_secs(200)).await;
        let duplicate = manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;
        assert_eq!(duplicate, token, "a duplicate teardown reuses the token");

        tokio::time::advance(StdDuration::from_secs(100)).await;
        assert!(
            manager
                .validate_reconnection(&player_id, &room_id, &token)
                .await
                .is_ok(),
            "the original deadline is exactly here, so the record is still live"
        );
        tokio::time::advance(StdDuration::from_millis(1)).await;
        assert_eq!(
            manager
                .validate_reconnection(&player_id, &room_id, &token)
                .await
                .expect_err("the original window elapsed"),
            ReconnectionError::WindowExpired,
            "a duplicate teardown must not extend the original window"
        );

        let replacement = manager
            .register_disconnection(player_id, other_room, false, None, 0)
            .await;
        tokio::time::advance(StdDuration::from_secs(300)).await;
        assert!(
            manager
                .validate_reconnection(&player_id, &other_room, &replacement)
                .await
                .is_ok(),
            "a different-room registration is a genuine disconnect with a fresh window"
        );
        tokio::time::advance(StdDuration::from_millis(1)).await;
        assert_eq!(
            manager
                .validate_reconnection(&player_id, &other_room, &replacement)
                .await
                .expect_err("the replacement window elapsed"),
            ReconnectionError::WindowExpired,
            "the fresh window closes exactly one window after the replacement"
        );
    }

    /// A duplicate teardown is not a new disconnect and therefore cannot
    /// extend either deadline exposed by the pending record.
    #[tokio::test]
    async fn same_room_reregistration_preserves_original_disconnect_deadline() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player = Uuid::new_v4();
        let room = Uuid::new_v4();

        manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        let (original_disconnected_at, original_token_expiry) = {
            let mut state = manager.replay_state.write().await;
            let disconnected = &mut state
                .disconnected_players
                .get_mut(&player)
                .expect("pending disconnect")
                .disconnected;
            disconnected.disconnected_at = Utc::now() - Duration::seconds(120);
            disconnected.token.expires_at =
                expiration_from_unsigned(disconnected.disconnected_at, 300);
            (disconnected.disconnected_at, disconnected.token.expires_at)
        };

        manager
            .register_disconnection(player, room, false, None, 0)
            .await;

        let state = manager.replay_state.read().await;
        let disconnected = &state.disconnected_players[&player].disconnected;
        assert_eq!(
            disconnected.disconnected_at, original_disconnected_at,
            "same-room re-registration must not restart the window clock"
        );
        assert_eq!(
            disconnected.token.expires_at, original_token_expiry,
            "same-room re-registration must not extend token expiry"
        );
    }

    /// A late teardown from the replaced socket can overlap a successful
    /// reconnect after the claim is reserved and the next token is rotated.
    /// It must be a no-op for both pieces of state: the active claim stays
    /// single-owner, and the fresh token remains armed for the next genuine
    /// disconnect.
    #[tokio::test]
    async fn same_room_reregistration_preserves_active_claim_and_rotated_token() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player = Uuid::new_v4();
        let room = Uuid::new_v4();
        let first_token = manager.pre_issue_token(player, room).await;
        manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        let claim = manager
            .claim_reconnection(&Uuid::new_v4(), &player, &room, &first_token)
            .await
            .expect("first socket reserves the reconnect record");
        let next_token = manager.pre_issue_token(player, room).await;

        manager
            .register_disconnection(player, room, false, None, 0)
            .await;

        assert!(matches!(
            manager
                .claim_reconnection(&Uuid::new_v4(), &player, &room, &first_token)
                .await,
            Err(ReconnectionError::AlreadyInProgress)
        ));
        assert!(
            manager.complete_claimed_reconnection(&claim).await,
            "duplicate teardown must not invalidate the active claim handle"
        );
        assert_eq!(
            manager
                .register_disconnection(player, room, false, None, 0)
                .await,
            next_token,
            "duplicate teardown must not consume the replacement connection's token"
        );
    }

    #[tokio::test]
    async fn certificate_bound_claim_is_atomic_and_identity_preserving() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = Arc::new(ReconnectionManager::new(300, 100, metrics));
        let player = Uuid::new_v4();
        let room = Uuid::new_v4();
        let identity_a: Arc<str> = Arc::from("aa".repeat(32));
        let identity_b: Arc<str> = Arc::from("bb".repeat(32));

        let token = manager
            .pre_issue_token_with_identity(player, room, Some(Arc::clone(&identity_a)))
            .await;
        manager
            .register_disconnection_with_identity(
                player,
                room,
                false,
                None,
                0,
                Some(Arc::clone(&identity_a)),
            )
            .await;
        // A duplicate teardown cannot strip or rotate the original issuance
        // identity, even when its transient connection metadata differs.
        manager
            .register_disconnection_with_identity(
                player,
                room,
                false,
                None,
                0,
                Some(Arc::clone(&identity_b)),
            )
            .await;

        for rejected in [None, Some(identity_b.as_ref())] {
            let result = manager
                .claim_reconnection_with_identity(&Uuid::new_v4(), &player, &room, &token, rejected)
                .await;
            assert!(matches!(result, Err(ReconnectionError::TokenMismatch)));
        }

        assert!(matches!(
            manager.validate_reconnection(&player, &room, &token).await,
            Err(ReconnectionError::TokenMismatch)
        ));

        // Race a valid A claim with B. B can either lose to A's reservation or
        // check its identity first, but it can never reserve the credential;
        // A must still win exactly once.
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let spawn_claim = |identity: Arc<str>| {
            let manager = Arc::clone(&manager);
            let barrier = Arc::clone(&barrier);
            let token = token.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                manager
                    .claim_reconnection_with_identity(
                        &Uuid::new_v4(),
                        &player,
                        &room,
                        &token,
                        Some(identity.as_ref()),
                    )
                    .await
            })
        };
        let claim_a = spawn_claim(Arc::clone(&identity_a));
        let claim_b = spawn_claim(Arc::clone(&identity_b));
        barrier.wait().await;
        let (claim_a, claim_b) = tokio::join!(claim_a, claim_b);
        let claim = claim_a
            .expect("A claim task")
            .expect("matching certificate identity claims the original token");
        assert!(matches!(
            claim_b.expect("B claim task"),
            Err(ReconnectionError::TokenMismatch | ReconnectionError::AlreadyInProgress)
        ));
        assert_eq!(claim.disconnected.player_id, player);
    }

    /// Regression: a same-room re-registration must NOT clobber the disconnect
    /// snapshot captured at the FIRST disconnect. A racing second call with
    /// `player_info: None` (and a stale `was_authority`) would otherwise erase
    /// the membership snapshot — and `reconnection_service` rejects a reconnect
    /// whose stored `player_info` is `None`.
    #[tokio::test]
    async fn same_room_reregistration_preserves_disconnect_snapshot() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player = Uuid::new_v4();
        let room = Uuid::new_v4();
        let info = PlayerInfo {
            id: player,
            name: "Player".to_string(),
            is_authority: true,
            is_ready: false,
            connected_at: Utc::now(),
            connection_info: None,
            epoch: None,
            seq: None,
            region_id: "test".to_string(),
        };

        let token = manager.pre_issue_token(player, room).await;
        // First disconnect captures the real snapshot: authority + membership.
        manager
            .register_disconnection(player, room, true, Some(info), 5)
            .await;
        // A racing second registration carries None + a stale authority flag.
        manager
            .register_disconnection(player, room, false, None, 0)
            .await;

        let claim = manager
            .claim_reconnection(&Uuid::new_v4(), &player, &room, &token)
            .await
            .expect("claim succeeds with the preserved token");
        assert!(
            claim.disconnected.player_info.is_some(),
            "the first disconnect's player_info snapshot must survive re-registration"
        );
        assert!(
            claim.disconnected.was_authority,
            "the first disconnect's was_authority must survive re-registration"
        );
        assert_eq!(
            claim.disconnected.last_epoch, 5,
            "last_epoch preserved (max) across re-registration"
        );
    }

    /// A pre-issued token bound to a DIFFERENT room is not reused (the player
    /// joined elsewhere without a clean leave): the disconnect mints fresh.
    #[tokio::test]
    async fn pre_issued_token_for_another_room_is_not_reused() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player = Uuid::new_v4();

        let stale = manager.pre_issue_token(player, Uuid::new_v4()).await;
        let armed = manager
            .register_disconnection(player, Uuid::new_v4(), false, None, 0)
            .await;
        assert_ne!(armed, stale, "a wrong-room pre-issue must not be armed");
    }

    /// Voluntary leave discards the pre-issued token: it never becomes
    /// claimable, and the map stays bounded by currently-joined players.
    #[tokio::test]
    async fn discarded_pre_issued_token_is_not_reused() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player = Uuid::new_v4();
        let room = Uuid::new_v4();

        let wire_token = manager.pre_issue_token(player, room).await;
        manager.discard_pre_issued(&player).await;
        let armed = manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        assert_ne!(armed, wire_token, "a discarded token must never be armed");
    }

    /// Review-found (Bugbot): a second same-room disconnection while still
    /// pending must NOT advance `last_sequence` — the player never reconnected,
    /// so it still has not seen the events buffered since its first
    /// disconnect. Advancing it would drop those events from a later replay
    /// while `replay` still reported `complete`.
    #[tokio::test]
    async fn same_room_re_registration_preserves_the_original_replay_snapshot() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player = Uuid::new_v4();
        let room = Uuid::new_v4();

        // First disconnect: snapshot taken here. Then a control event is
        // buffered (the player is pending and never sees it).
        manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        manager.record_room_event(&room, &control_event()).await;

        // Second disconnect for the SAME room while still pending (e.g. a
        // replaced/racing connection): the snapshot must not jump past the
        // buffered event.
        manager
            .register_disconnection(player, room, false, None, 0)
            .await;
        manager.record_room_event(&room, &control_event()).await;

        // Both events must still replay — neither is silently excluded — and
        // the replay is honestly complete.
        let missed = manager.get_missed_events(&room, {
            let state = manager.replay_state.read().await;
            state.disconnected_players[&player]
                .disconnected
                .last_sequence
        });
        let missed = missed.await;
        assert_eq!(
            missed.events.len(),
            2,
            "both buffered control events must survive a same-room re-registration"
        );
        assert!(
            !missed.truncated,
            "nothing was evicted, so the replay is complete"
        );
    }

    /// Fuzz-found (fuzz_reconnect_tokens): re-registering a player from a NEW
    /// room replaces its pending record, and if it was the old room's last
    /// pending reconnector nothing else ever cleans that room's buffer — it
    /// would keep capturing control events forever and replay ghosts to a
    /// room with nobody pending.
    #[tokio::test]
    async fn re_registration_from_a_new_room_releases_the_orphaned_old_room_buffer() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player = Uuid::new_v4();
        let other_player = Uuid::new_v4();
        let room_a = Uuid::new_v4();
        let room_b = Uuid::new_v4();

        // P pends in room A; the gate opens and captures events there.
        manager
            .register_disconnection(player, room_a, false, None, 0)
            .await;
        manager.record_room_event(&room_a, &control_event()).await;

        // P disconnects again from room B: the re-registration REPLACES its
        // record. Nobody pends in room A anymore, so its buffer must go.
        manager
            .register_disconnection(player, room_b, false, None, 0)
            .await;
        assert!(
            !manager
                .replay_state
                .read()
                .await
                .event_buffers
                .contains_key(&room_a),
            "re-registering the last pending player from a new room must \
             release the old room's buffer"
        );
        let missed = manager.get_missed_events(&room_a, 0).await;
        assert!(
            missed.events.is_empty() && !missed.truncated,
            "an orphaned room must replay nothing"
        );

        // But while ANOTHER player still pends in the old room, a sibling's
        // re-registration must leave that room's buffer untouched.
        manager
            .register_disconnection(other_player, room_a, false, None, 0)
            .await;
        manager.record_room_event(&room_a, &control_event()).await;
        manager
            .register_disconnection(player, room_a, false, None, 0)
            .await;
        manager
            .register_disconnection(player, room_b, false, None, 0)
            .await;
        assert!(
            manager
                .replay_state
                .read()
                .await
                .event_buffers
                .contains_key(&room_a),
            "the old room's buffer must survive while another player pends there"
        );
        assert_eq!(manager.get_missed_events(&room_a, 0).await.events.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn expired_cleanup_releases_the_room_buffer_when_last_pending_player_expires() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(1, 100, metrics);
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        manager
            .register_disconnection(player_id, room_id, false, None, 0)
            .await;
        assert!(
            manager
                .replay_state
                .read()
                .await
                .event_buffers
                .contains_key(&room_id),
            "registering a disconnection opens the room's replay gate"
        );
        tokio::time::advance(StdDuration::from_secs(5)).await;

        assert_eq!(manager.cleanup_expired().await, 1);
        assert!(
            !manager
                .replay_state
                .read()
                .await
                .event_buffers
                .contains_key(&room_id),
            "expiring the last pending player must release the room buffer"
        );
        manager.record_room_event(&room_id, &control_event()).await;
        assert!(
            manager
                .get_missed_events(&room_id, 0)
                .await
                .events
                .is_empty(),
            "an expired-out room must stop capturing events"
        );
    }

    #[tokio::test]
    async fn only_room_uniform_control_events_are_replayable() {
        use crate::protocol::{LobbyState, PlayerInfo, SpectatorInfo};

        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(300, 100, metrics);
        let room_id = Uuid::new_v4();
        let player_id = Uuid::new_v4();
        manager
            .register_disconnection(Uuid::new_v4(), room_id, false, None, 0)
            .await;

        let player_info = PlayerInfo {
            id: player_id,
            name: "Player".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: Utc::now(),
            connection_info: None,
            epoch: None,
            seq: None,
            region_id: "test".to_string(),
        };
        let spectator = SpectatorInfo {
            id: player_id,
            name: "Spectator".to_string(),
            connected_at: Utc::now(),
        };

        // Every room-uniform control event buffers.
        let replayable = [
            ServerMessage::PlayerJoined {
                player: player_info,
            },
            ServerMessage::PlayerLeft {
                player_id,
                epoch: None,
                final_seq: None,
            },
            ServerMessage::PlayerReconnected {
                player_id,
                epoch: None,
            },
            ServerMessage::NewSpectatorJoined {
                spectator: spectator.clone(),
                current_spectators: vec![spectator.clone()],
                reason: None,
            },
            ServerMessage::SpectatorDisconnected {
                spectator_id: player_id,
                reason: None,
                current_spectators: Vec::new(),
            },
            ServerMessage::LobbyStateChanged {
                lobby_state: LobbyState::Lobby,
                ready_players: Vec::new(),
                all_ready: false,
            },
            ServerMessage::AuthorityChanged {
                authority_player: Some(player_id),
                you_are_authority: false,
            },
        ];
        for message in &replayable {
            manager.record_room_event(&room_id, message).await;
        }
        assert_eq!(
            manager.get_missed_events(&room_id, 0).await.events.len(),
            replayable.len(),
            "every replayable control variant must buffer"
        );

        // High-rate data-path, per-recipient, and directed messages never do.
        let not_replayable = [
            ServerMessage::GameData {
                from_player: player_id,
                data: serde_json::json!({ "tick": 1 }),
                seq: None,
                epoch: None,
                class: None,
                key: None,
            },
            ServerMessage::GameStarting {
                peer_connections: Vec::new(),
            },
            ServerMessage::Signal {
                from: player_id,
                generation: uuid::Uuid::nil(),
                signal: serde_json::Value::Null,
            },
            ServerMessage::Pong,
            ServerMessage::Error {
                message: "directed".to_string(),
                error_code: None,
            },
        ];
        for message in &not_replayable {
            manager.record_room_event(&room_id, message).await;
        }
        assert_eq!(
            manager.get_missed_events(&room_id, 0).await.events.len(),
            replayable.len(),
            "GameData/GameStarting/Signal/directed messages must never buffer"
        );
    }
}
