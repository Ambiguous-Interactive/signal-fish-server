/// Reconnection support module
///
/// Handles player reconnection after network disruptions including:
/// - Authentication token generation and validation
/// - Event buffering for missed messages
/// - Player disconnection tracking
/// - Reconnection window management
use crate::metrics::ServerMetrics;
use crate::protocol::{PlayerId, RoomId, ServerMessage};
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

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
}

impl DisconnectedPlayer {
    /// Check if reconnection window has expired
    pub fn is_expired(&self, window_seconds: i64) -> bool {
        let expiry = self.disconnected_at + Duration::seconds(window_seconds);
        Utc::now() > expiry
    }
}

/// Reconnection manager
pub struct ReconnectionManager {
    /// Disconnected players awaiting reconnection
    disconnected_players: RwLock<HashMap<PlayerId, DisconnectedPlayer>>,
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
        };

        let mut players = self.disconnected_players.write().await;
        let previous = players.insert(player_id, disconnected);
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
    pub async fn validate_reconnection(
        &self,
        player_id: &PlayerId,
        room_id: &RoomId,
        token: &str,
    ) -> Result<DisconnectedPlayer, String> {
        let disconnected = self.disconnected_players.read().await;

        let Some(player) = disconnected.get(player_id) else {
            self.metrics.increment_reconnection_validation_failure();
            return Err("No disconnection record found".to_string());
        };

        if player.token.token != token {
            self.metrics.increment_reconnection_validation_failure();
            return Err("Invalid reconnection token".to_string());
        }

        if !player.token.is_valid(player_id, room_id) {
            self.metrics.increment_reconnection_validation_failure();
            return Err("Reconnection token is invalid or expired".to_string());
        }

        if player.is_expired(self.reconnection_window) {
            self.metrics.increment_reconnection_validation_failure();
            return Err("Reconnection window has expired".to_string());
        }

        Ok(player.clone())
    }

    /// Complete reconnection and remove from disconnected players
    pub async fn complete_reconnection(&self, player_id: &PlayerId) {
        let mut players = self.disconnected_players.write().await;
        let removed = players.remove(player_id);
        let room_to_clear = removed.as_ref().and_then(|player| {
            let room_id = player.room_id;
            let others_waiting = players
                .values()
                .any(|p| p.player_id != player.player_id && p.room_id == room_id);
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

        disconnected.retain(|player_id, player| {
            let expired = player.is_expired(self.reconnection_window);
            if expired {
                tracing::info!(%player_id, "Removing expired reconnection record");
                expired_ids.push(*player_id);
            }
            !expired
        });
        let removed = initial_count - disconnected.len();
        drop(disconnected);
        if removed > 0 {
            tracing::info!(count = removed, "Cleaned up expired reconnection records");
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
            .filter(|p| p.room_id == *room_id)
            .map(|p| p.player_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::ServerMetrics;
    use std::sync::Arc;

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
            .register_disconnection(player_id, room_id, false)
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
