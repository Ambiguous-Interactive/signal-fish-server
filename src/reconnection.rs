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
use std::borrow::Cow;
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
    /// The seat behind this record was removed by the room's authority
    /// (`KickPlayer`, issue #525). Reconnection is never offered for a
    /// kicked seat.
    Kicked,
}

impl ReconnectionError {
    /// The client-facing [`ErrorCode`] for this rejection.
    ///
    /// A bad/mismatched token is a `RECONNECTION_TOKEN_INVALID`; only an elapsed
    /// *window* is `RECONNECTION_EXPIRED`; only an authority-kicked seat is
    /// `KICKED`; everything else is the generic `RECONNECTION_FAILED`.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::NoRecord | Self::AlreadyInProgress => ErrorCode::ReconnectionFailed,
            Self::TokenMismatch | Self::TokenInvalid => ErrorCode::ReconnectionTokenInvalid,
            Self::WindowExpired => ErrorCode::ReconnectionExpired,
            Self::Kicked => ErrorCode::Kicked,
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
            Self::Kicked => "The seat was removed by the room authority",
        };
        f.write_str(reason)
    }
}

impl std::error::Error for ReconnectionError {}

/// Authentication token for reconnection
#[derive(Clone)]
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

impl std::fmt::Debug for ReconnectionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconnectionToken")
            .field("token", &crate::config::REDACTED_SECRET)
            .field("player_id", &self.player_id)
            .field("room_id", &self.room_id)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl ReconnectionToken {
    /// Create a new reconnection token
    pub fn new(player_id: PlayerId, room_id: RoomId, validity_seconds: i64) -> Self {
        // Wall clock (durable record): `created_at`/`expires_at` are absolute
        // instants the token must keep across process restarts.
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
        // Wall clock (durable record): same cross-restart semantics as `new`.
        let now = Utc::now();
        Self {
            token: Uuid::new_v4().to_string(),
            player_id,
            room_id,
            created_at: now,
            expires_at: expiration_from_unsigned(now, validity_seconds),
        }
    }

    /// Check if token is expired.
    ///
    /// Wall clock (embedder convenience): the server's own admission runs on
    /// the manager's monotonic deadline captured at the disconnect; this
    /// absolute-time answer exists for embedders inspecting a token.
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
            // Wall clock (durable record): replay/diagnostic stamp on the
            // buffered event.
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
    /// The authority kick tombstone, preserved across a defensive merge (see
    /// `ReconnectionRecord::kicked`).
    kicked: bool,
}

impl DisconnectedPlayer {
    /// Whether `window_seconds` has elapsed since the captured wall-clock
    /// disconnect instant.
    ///
    /// Wall clock (embedder convenience): diagnostic only. The manager decides
    /// real reconnect eligibility from a monotonic deadline captured at the
    /// genuine disconnect, so this answer can differ from the server's after a
    /// wall-clock adjustment.
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
    /// Authority tombstone (issue #525): the seat behind this record was
    /// removed by the room's authority (`KickPlayer`). A claim against a
    /// kicked record must be refused — including a claim already in flight
    /// when the kick landed — because the reconnection restore path would
    /// otherwise resurrect the removed seat from its membership snapshot.
    /// Set under the room event mutation gate together with the durable seat
    /// removal, so a restore decision serialized after the kick always sees
    /// it.
    kicked: bool,
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
        // Wall clock (durable record): crash-recovery/diagnostic stamp for
        // the two-phase claim; claim validity is the manager's monotonic
        // window.
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

/// Scrape-time live-state counts from [`ReconnectionManager::replay_sample`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplaySample {
    /// Players with a pending reconnection record (disconnected, inside the
    /// reconnect window, not yet claimed).
    pub pending_reconnections: u64,
    /// Rooms holding a replay ring.
    pub replay_rooms: u64,
    /// Buffered replayable events across all rings (bounded by
    /// `event_buffer_size` per room).
    pub replay_events: u64,
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

    /// Scrape-time live-state counts for the metrics endpoint: pending
    /// reconnection records and the retained replay rings. Sampled when the
    /// endpoint is hit under one short read lock, never maintained on the
    /// write path.
    pub async fn replay_sample(&self) -> ReplaySample {
        let state = self.replay_state.read().await;
        ReplaySample {
            pending_reconnections: u64::try_from(state.disconnected_players.len())
                .unwrap_or(u64::MAX),
            replay_rooms: u64::try_from(state.event_buffers.len()).unwrap_or(u64::MAX),
            replay_events: u64::try_from(
                state
                    .event_buffers
                    .values()
                    .map(|buffer| buffer.events.len())
                    .sum::<usize>(),
            )
            .unwrap_or(u64::MAX),
        }
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
        // Same claim invariant as every expiry surface: a claimed record is
        // owned by its in-flight reconnect transaction, and removing it here
        // would strand that transaction (its completion would silently fail
        // while the player observed a successful reconnect).
        if state
            .disconnected_players
            .get(player_id)
            .is_some_and(|record| record.claim.is_some())
        {
            return false;
        }
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

    /// Discard the pending reconnection record for `player_id` only when it
    /// would restore into `room_id` (and is unclaimed).
    ///
    /// A fresh seat commit for the same player and room (a join, not a token
    /// claim) supersedes the dropped prior membership. The stale record must
    /// not survive it: the next disconnect would merge the old record —
    /// keeping its token, deadline, and epoch/sequence view — and destroy the
    /// token the join just issued, dead-ending the client's credential with a
    /// misleading `TokenMismatch`. Cross-room records keep the replacement
    /// semantics of [`Self::register_disconnection`]; claimed records stay
    /// owned by their in-flight restore (same invariant as
    /// [`Self::discard_pending_reconnection`]). Returns whether a record was
    /// discarded.
    pub async fn discard_pending_reconnection_in_room(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
    ) -> bool {
        let mut state = self.replay_state.write().await;
        let same_room_unclaimed = state
            .disconnected_players
            .get(player_id)
            .is_some_and(|record| {
                record.disconnected.room_id == *room_id && record.claim.is_none()
            });
        if !same_room_unclaimed {
            return false;
        }
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

    /// Tombstone a pending reconnection record as authority-kicked
    /// (issue #525).
    ///
    /// Unlike [`Self::discard_pending_reconnection`], this also marks a
    /// CLAIMED record: its in-flight reconnect transaction re-checks the
    /// tombstone under the room mutation gate before restoring membership,
    /// so a kick that races the claim still wins. Returns whether a record
    /// was marked.
    pub async fn mark_reconnection_kicked(&self, player_id: &PlayerId) -> bool {
        let mut state = self.replay_state.write().await;
        match state.disconnected_players.get_mut(player_id) {
            Some(record) => {
                record.kicked = true;
                true
            }
            None => false,
        }
    }

    /// Whether the pending record for `player_id` carries the authority-kick
    /// tombstone. Consulted by the reconnect restore path under the room
    /// mutation gate (see [`Self::mark_reconnection_kicked`]).
    pub async fn is_reconnection_kicked(&self, player_id: &PlayerId) -> bool {
        self.replay_state
            .read()
            .await
            .disconnected_players
            .get(player_id)
            .is_some_and(|record| record.kicked)
    }

    /// The room a pending reconnection record would restore into, if one
    /// exists. Used by the kick path to tombstone only the record that
    /// belongs to the room being moderated (issue #525).
    pub async fn pending_reconnection_room(&self, player_id: &PlayerId) -> Option<RoomId> {
        self.replay_state
            .read()
            .await
            .disconnected_players
            .get(player_id)
            .map(|record| record.disconnected.room_id)
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
                kicked: existing.kicked,
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
        // human-readable durable record (wall clock), while the monotonic
        // instant anchors the only deadline that decides eligibility.
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
        let mut player_info = match &existing_same_room {
            Some(existing) if existing.player_info.is_some() => existing.player_info.clone(),
            _ => player_info,
        };
        // Peer metadata describes the old socket's reachable endpoint. A new
        // socket may be on another network, so it must advertise fresh info
        // before peers or a Direct SessionPlan can use it again.
        if let Some(info) = player_info.as_mut() {
            info.connection_info = None;
        }

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
            // A re-registration observes a seated snapshot under the
            // disconnect path's connection lifecycle gate. A concurrent kick
            // must revalidate and hold that current gate through removal;
            // the kick's room-gated pending-record check covers a target that
            // has no live socket. The preserved flag handles re-registration
            // of a record that is already tombstoned.
            kicked: existing_same_room
                .as_ref()
                .is_some_and(|existing| existing.kicked),
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

        if record.kicked {
            self.metrics.increment_reconnection_validation_failure();
            tracing::info!(
                %player_id,
                "Refusing reconnection: the seat was removed by the room authority"
            );
            return Err(ReconnectionError::Kicked);
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
        self.push_event(room_id, Cow::Borrowed(message), false)
            .await;
    }

    /// Buffer an event for a room unconditionally (no replayable filter, no
    /// pending-reconnection gate; creates the room's ring if absent). Retained
    /// for embedders that wire their own delivery layer; the server itself
    /// records via [`Self::record_room_event`]. GameData/Signal/GameStarting
    /// are never replayed on the production path — reconnectors resync via the
    /// `Reconnected` snapshot and, for started sessions, the late-join
    /// `SessionPlan` flow.
    pub async fn buffer_event(&self, room_id: &RoomId, message: ServerMessage) {
        self.push_event(room_id, Cow::Owned(message), true).await;
    }

    /// Shared push path for [`Self::record_room_event`] and
    /// [`Self::buffer_event`]: assigns the next global sequence number, pushes
    /// into the room's ring, and advances the buffered/evicted metrics.
    async fn push_event(
        &self,
        room_id: &RoomId,
        message: Cow<'_, ServerMessage>,
        create_if_missing: bool,
    ) {
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
        let evicted = buffer.push(message.into_owned(), sequence);
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

    /// Keep an expired same-room record stable through durable seat cleanup.
    /// The caller holds the room mutation gate before entering this method.
    /// The write lock prevents a new disconnect from replacing the record
    /// while the storage operation is suspended. Storage errors retain it.
    /// `None` means ineligible. `Some` carries the removed seat, if one existed.
    pub(crate) async fn cleanup_expired_membership<F>(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        cleanup: F,
    ) -> anyhow::Result<Option<Option<PlayerInfo>>>
    where
        F: std::future::Future<Output = anyhow::Result<Option<PlayerInfo>>>,
    {
        let mut state = self.replay_state.write().await;
        let now = Instant::now();
        let eligible = state
            .disconnected_players
            .get(player_id)
            .is_some_and(|record| {
                record.disconnected.room_id == *room_id
                    && record.claim.is_none()
                    && record.window_closed_at(now)
            });
        if !eligible {
            return Ok(None);
        }
        let removed = cleanup.await?;
        self.remove_expired_reconnection_locked(&mut state, player_id);
        Ok(Some(removed))
    }

    /// Remove one record only if it is still expired and unclaimed after the
    /// caller completed its durable cleanup. Returns whether it was removed.
    pub async fn remove_expired_reconnection(&self, player_id: &PlayerId) -> bool {
        let mut state = self.replay_state.write().await;
        self.remove_expired_reconnection_locked(&mut state, player_id)
    }

    fn remove_expired_reconnection_locked(
        &self,
        state: &mut ReplayState,
        player_id: &PlayerId,
    ) -> bool {
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
#[path = "reconnection_tests.rs"]
mod tests;
