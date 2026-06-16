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
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
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
            expires_at: now + Duration::seconds(validity_seconds),
        }
    }

    /// Check if token is expired
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }

    /// Check if token is valid for given player and room
    pub fn is_valid(&self, player_id: &PlayerId, room_id: &RoomId) -> bool {
        !self.is_expired() && self.player_id == *player_id && self.room_id == *room_id
    }
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
        Self {
            room_id,
            max_size,
            events: VecDeque::with_capacity(max_size),
        }
    }

    /// Add an event to the buffer
    pub fn push(&mut self, message: ServerMessage, sequence: u64) {
        let event = BufferedEvent {
            message,
            timestamp: Utc::now(),
            sequence,
        };

        self.events.push_back(event);

        // Remove oldest events if buffer is full
        while self.events.len() > self.max_size {
            self.events.pop_front();
        }
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
}

impl DisconnectedPlayer {
    /// Check if reconnection window has expired
    pub fn is_expired(&self, window_seconds: i64) -> bool {
        let expiry = self.disconnected_at + Duration::seconds(window_seconds);
        Utc::now() > expiry
    }
}

#[derive(Debug, Clone)]
struct ReconnectionRecord {
    disconnected: DisconnectedPlayer,
    claim: Option<ReconnectionClaimState>,
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

/// Reconnection manager
pub struct ReconnectionManager {
    /// Disconnected players awaiting reconnection
    disconnected_players: RwLock<HashMap<PlayerId, ReconnectionRecord>>,
    /// Event buffers per room
    event_buffers: RwLock<HashMap<RoomId, EventBuffer>>,
    /// Reconnection window in seconds
    reconnection_window: i64,
    /// Event buffer size per room
    event_buffer_size: usize,
    /// Next sequence number for events
    next_sequence: RwLock<u64>,
    /// Metrics sink
    metrics: Arc<ServerMetrics>,
}

impl ReconnectionManager {
    /// Create a new reconnection manager
    pub fn new(
        reconnection_window: u64,
        event_buffer_size: usize,
        metrics: Arc<ServerMetrics>,
    ) -> Self {
        Self {
            disconnected_players: RwLock::new(HashMap::new()),
            event_buffers: RwLock::new(HashMap::new()),
            reconnection_window: reconnection_window as i64,
            event_buffer_size,
            next_sequence: RwLock::new(0),
            metrics,
        }
    }

    /// Register a player disconnection
    pub async fn register_disconnection(
        &self,
        player_id: PlayerId,
        room_id: RoomId,
        was_authority: bool,
        player_info: Option<PlayerInfo>,
    ) -> String {
        let token = ReconnectionToken::new(player_id, room_id, self.reconnection_window);
        let token_string = token.token.clone();

        let last_sequence = *self.next_sequence.read().await;

        let disconnected = DisconnectedPlayer {
            player_id,
            room_id,
            disconnected_at: Utc::now(),
            token,
            last_sequence,
            was_authority,
            player_info,
        };
        let record = ReconnectionRecord {
            disconnected,
            claim: None,
        };

        let mut players = self.disconnected_players.write().await;
        let previous = players.insert(player_id, record);
        drop(players);

        self.metrics.increment_reconnection_tokens_issued();
        if previous.is_none() {
            self.metrics.increment_reconnection_sessions_active();
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
        let disconnected = self.disconnected_players.read().await;

        let Some(record) = disconnected.get(player_id) else {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::NoRecord);
        };
        let player = &record.disconnected;

        if !crate::security::constant_time_eq(&player.token.token, token) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::TokenMismatch);
        }

        if !player.token.is_valid(player_id, room_id) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::TokenInvalid);
        }

        if player.is_expired(self.reconnection_window) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::WindowExpired);
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
        let mut disconnected = self.disconnected_players.write().await;

        let Some(record) = disconnected.get_mut(player_id) else {
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

        if !player.token.is_valid(player_id, room_id) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::TokenInvalid);
        }

        if player.is_expired(self.reconnection_window) {
            self.metrics.increment_reconnection_validation_failure();
            return Err(ReconnectionError::WindowExpired);
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
        let mut players = self.disconnected_players.write().await;
        let removed = players.remove(player_id);
        let room_to_clear = removed.as_ref().and_then(|record| {
            let room_id = record.disconnected.room_id;
            let others_waiting = players.values().any(|p| {
                p.disconnected.player_id != record.disconnected.player_id
                    && p.disconnected.room_id == room_id
            });
            if others_waiting {
                None
            } else {
                Some(room_id)
            }
        });
        drop(players);

        if removed.is_some() {
            self.metrics.decrement_reconnection_sessions_active();
            self.metrics.increment_reconnection_completions();
        }

        if let Some(room_id) = room_to_clear {
            let mut buffers = self.event_buffers.write().await;
            buffers.remove(&room_id);
        }

        tracing::info!(%player_id, "Player reconnection completed");
    }

    /// Complete a reconnection record that was already reserved by
    /// [`Self::claim_reconnection`].
    pub async fn complete_claimed_reconnection(&self, claim: &ClaimedReconnection) -> bool {
        let mut players = self.disconnected_players.write().await;
        let Some(record) = players.get(&claim.disconnected.player_id) else {
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

        let Some(record) = players.remove(&claim.disconnected.player_id) else {
            tracing::warn!(
                player_id = %claim.disconnected.player_id,
                "Claimed reconnection completion lost its pending record"
            );
            return false;
        };
        let room_id = record.disconnected.room_id;
        let others_waiting = players.values().any(|p| {
            p.disconnected.player_id != record.disconnected.player_id
                && p.disconnected.room_id == room_id
        });
        drop(players);

        self.metrics.decrement_reconnection_sessions_active();
        self.metrics.increment_reconnection_completions();

        if !others_waiting {
            let mut buffers = self.event_buffers.write().await;
            buffers.remove(&room_id);
        }

        tracing::info!(
            player_id = %record.disconnected.player_id,
            "Player reconnection completed"
        );
        true
    }

    /// Release a reserved reconnection record after a failed restore attempt.
    pub async fn release_reconnection_claim(&self, claim: &ClaimedReconnection) -> bool {
        let mut players = self.disconnected_players.write().await;
        let Some(record) = players.get_mut(&claim.disconnected.player_id) else {
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

    /// Get missed events for a reconnecting player
    pub async fn get_missed_events(
        &self,
        room_id: &RoomId,
        last_sequence: u64,
    ) -> Vec<ServerMessage> {
        let buffers = self.event_buffers.read().await;
        buffers
            .get(room_id)
            .map(|buffer| buffer.get_events_after(last_sequence))
            .unwrap_or_default()
    }

    /// Buffer an event for a room
    pub async fn buffer_event(&self, room_id: &RoomId, message: ServerMessage) {
        let mut sequence = self.next_sequence.write().await;
        *sequence += 1;
        let seq = *sequence;
        drop(sequence);

        let mut buffers = self.event_buffers.write().await;
        let buffer = buffers
            .entry(*room_id)
            .or_insert_with(|| EventBuffer::new(*room_id, self.event_buffer_size));

        buffer.push(message, seq);
        drop(buffers);

        self.metrics.add_reconnection_events_buffered(1);
    }

    /// Clear event buffer for a room (when room is deleted)
    pub async fn clear_room_buffer(&self, room_id: &RoomId) {
        self.event_buffers.write().await.remove(room_id);
        tracing::debug!(%room_id, "Event buffer cleared for room");
    }

    /// Clean up expired disconnections
    pub async fn cleanup_expired(&self) -> usize {
        let mut disconnected = self.disconnected_players.write().await;
        let initial_count = disconnected.len();
        let mut expired_ids = Vec::new();

        disconnected.retain(|player_id, record| {
            let expired =
                record.claim.is_none() && record.disconnected.is_expired(self.reconnection_window);
            if expired {
                tracing::info!(%player_id, "Removing expired reconnection record");
                expired_ids.push(*player_id);
            }
            !expired
        });
        let removed = initial_count - disconnected.len();
        let remaining = disconnected.len();
        drop(disconnected);
        if removed > 0 {
            tracing::info!(count = removed, "Cleaned up expired reconnection records");
            self.metrics
                .set_reconnection_sessions_active(remaining as u64);
        }

        removed
    }

    /// Check if a player has a pending disconnection
    pub async fn has_pending_reconnection(&self, player_id: &PlayerId) -> bool {
        self.disconnected_players
            .read()
            .await
            .contains_key(player_id)
    }

    /// Get all disconnected players for a room
    pub async fn get_disconnected_players_in_room(&self, room_id: &RoomId) -> Vec<PlayerId> {
        self.disconnected_players
            .read()
            .await
            .values()
            .filter(|p| p.disconnected.room_id == *room_id)
            .map(|p| p.disconnected.player_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ServerMetrics;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use tokio::sync::Barrier;

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
        let token = ReconnectionToken::new(player_id, room_id, 300);

        assert_eq!(token.player_id, player_id);
        assert_eq!(token.room_id, room_id);
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
        let manager = ReconnectionManager::new(300, 100, metrics);
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();

        // Register disconnection
        let token = manager
            .register_disconnection(player_id, room_id, false, None)
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

    #[tokio::test]
    async fn test_reconnection_claim_is_single_use_under_concurrency() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = Arc::new(ReconnectionManager::new(300, 100, metrics));
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let token = manager
            .register_disconnection(player_id, room_id, false, None)
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
            .register_disconnection(player_id, room_id, false, None)
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

    #[tokio::test]
    async fn test_reconnection_cleanup_updates_active_session_gauge() {
        let metrics = Arc::new(ServerMetrics::new());
        let manager = ReconnectionManager::new(1, 100, Arc::clone(&metrics));
        let player_id = Uuid::new_v4();
        let room_id = Uuid::new_v4();
        let _token = manager
            .register_disconnection(player_id, room_id, false, None)
            .await;
        {
            let mut players = manager.disconnected_players.write().await;
            let record = players
                .get_mut(&player_id)
                .expect("registered disconnection record exists");
            record.disconnected.disconnected_at = Utc::now() - Duration::seconds(5);
        }

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
        let events = manager.get_missed_events(&room_id, 0).await;
        assert_eq!(events.len(), 3);
    }
}
