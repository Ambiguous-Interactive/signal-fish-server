//! Per-session (per-room) records for the metrics surface.
//!
//! One record per room answers "where did this room live, who was in it, and
//! how long did it run" for application-owned room directories and per-session
//! telemetry (issues #708, #763). Records are captured at the room-storage
//! seam ([`crate::database::InMemoryDatabase`]), so every create, join,
//! leave, and close path is counted exactly once.
//!
//! The registry is bounded two ways: active records track live rooms (already
//! capped by room admission), and completed records live in a fixed-size ring
//! that drops the oldest entry. The surface is JSON-only; session cardinality
//! makes Prometheus labels a poor fit.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use dashmap::DashMap;
use serde::Serialize;
use uuid::Uuid;

use crate::protocol::{Room, RoomId};

/// Maximum number of completed session records retained.
///
/// Records are small (identity plus counters); 1024 entries stay well under a
/// megabyte. Evicted entries are counted, never silently forgotten.
pub const SESSION_RECORDS_COMPLETED_CAP: usize = 1024;

/// Why a room closed. Coarse by design: the storage seam knows the deleting
/// path, not the caller's intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCloseReason {
    /// Empty-room cleanup (`cleanup_empty_rooms`).
    Empty,
    /// Inactive-room cleanup (`cleanup_expired_rooms`).
    Expired,
    /// Direct deletion (rollback, drain, or explicit teardown).
    Deleted,
}

/// One room's lifecycle record.
///
/// Timestamps are epoch milliseconds. `players_joined` counts every membership
/// added, including the creator's initial membership and reconnect
/// restorations; `players_left` counts every removal. Spectators are counted
/// separately.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub room_id: Uuid,
    pub room_code: String,
    pub game_name: String,
    pub application_id: Option<Uuid>,
    pub region_id: String,
    pub max_players: u8,
    pub created_at_ms: u64,
    /// Set when the room is removed from storage.
    pub ended_at_ms: Option<u64>,
    pub close_reason: Option<SessionCloseReason>,
    /// False while the room is a hidden pending creation.
    pub published: bool,
    pub players_joined: u64,
    pub players_left: u64,
    pub spectators_joined: u64,
    pub spectators_left: u64,
}

impl SessionRecord {
    fn from_room(room: &Room, created_at_ms: u64, published: bool) -> Self {
        Self {
            room_id: room.id,
            room_code: room.code.clone(),
            game_name: room.game_name.clone(),
            application_id: room.application_id,
            region_id: room.region_id.clone(),
            max_players: room.max_players,
            created_at_ms,
            ended_at_ms: None,
            close_reason: None,
            published,
            // The creator's membership is part of the room row, so the record
            // starts with one joined player (mirroring the server-wide
            // `players.joined` counter, which counts the creator).
            players_joined: 1,
            players_left: 0,
            spectators_joined: 0,
            spectators_left: 0,
        }
    }
}

/// Bounded registry of active and completed session records.
#[derive(Default)]
pub struct SessionRecords {
    active: DashMap<RoomId, SessionRecord>,
    completed: Mutex<VecDeque<SessionRecord>>,
    completed_dropped_total: AtomicU64,
}

/// Point-in-time copy of the registry for one scrape.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsSnapshot {
    /// Records for live rooms, oldest first (ties broken by room id for a
    /// deterministic order).
    pub active: Vec<SessionRecord>,
    /// Completed records, newest first.
    pub completed: Vec<SessionRecord>,
    /// Live-room count (`active.len()`, kept adjacent to the lists for
    /// convenience).
    pub active_count: usize,
    /// Completed-record count (`completed.len()`).
    pub completed_count: usize,
    /// Ring capacity; oldest completed records are dropped beyond it.
    pub completed_cap: usize,
    /// Total completed records dropped by the ring over the process lifetime.
    pub completed_dropped_total: u64,
}

impl SessionRecords {
    pub fn new() -> Self {
        Self::default()
    }

    fn now_ms() -> u64 {
        u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
    }

    /// Record a committed room row. `published` is false for hidden pending
    /// creations that become visible through [`Self::record_published`].
    pub(crate) fn record_created(&self, room: &Room, published: bool) {
        self.active.insert(
            room.id,
            SessionRecord::from_room(room, Self::now_ms(), published),
        );
    }

    /// Mark a pending room visible.
    pub(crate) fn record_published(&self, room_id: &RoomId) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.published = true;
        }
    }

    /// Record the room code rotation so the record always carries the
    /// currently routable code.
    pub(crate) fn record_room_code_changed(&self, room_id: &RoomId, new_code: &str) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.room_code = new_code.to_string();
        }
    }

    pub(crate) fn record_player_joined(&self, room_id: &RoomId) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.players_joined = record.players_joined.saturating_add(1);
        }
    }

    pub(crate) fn record_player_left(&self, room_id: &RoomId) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.players_left = record.players_left.saturating_add(1);
        }
    }

    pub(crate) fn record_spectator_joined(&self, room_id: &RoomId) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.spectators_joined = record.spectators_joined.saturating_add(1);
        }
    }

    pub(crate) fn record_spectator_left(&self, room_id: &RoomId) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.spectators_left = record.spectators_left.saturating_add(1);
        }
    }

    /// Finalize a removed room. Published rooms move to the completed ring;
    /// rooms that were never visible are dropped without a record, because no
    /// directory ever saw them.
    pub(crate) fn record_closed(&self, room_id: &RoomId, reason: SessionCloseReason) {
        if let Some((_, mut record)) = self.active.remove(room_id) {
            if !record.published {
                return;
            }
            record.ended_at_ms = Some(Self::now_ms());
            record.close_reason = Some(reason);
            self.push_completed(record);
        }
    }

    fn push_completed(&self, record: SessionRecord) {
        let mut completed = self
            .completed
            .lock()
            .expect("session record ring lock poisoned");
        completed.push_back(record);
        while completed.len() > SESSION_RECORDS_COMPLETED_CAP {
            completed.pop_front();
            self.completed_dropped_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Copy the registry for one scrape.
    pub fn snapshot(&self) -> SessionsSnapshot {
        let mut active: Vec<SessionRecord> = self
            .active
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        active.sort_by(|a, b| {
            a.created_at_ms
                .cmp(&b.created_at_ms)
                .then_with(|| a.room_id.cmp(&b.room_id))
        });
        let completed: Vec<SessionRecord> = self
            .completed
            .lock()
            .expect("session record ring lock poisoned")
            .iter()
            .rev()
            .cloned()
            .collect();
        let active_count = active.len();
        let completed_count = completed.len();
        SessionsSnapshot {
            active,
            completed,
            active_count,
            completed_count,
            completed_cap: SESSION_RECORDS_COMPLETED_CAP,
            completed_dropped_total: self.completed_dropped_total.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room_fixture(room_id: RoomId, code: &str) -> Room {
        let creator = Uuid::new_v4();
        Room {
            id: room_id,
            code: code.to_string(),
            game_name: "test-game".to_string(),
            max_players: 4,
            supports_authority: true,
            players: {
                let mut players = std::collections::HashMap::new();
                players.insert(
                    creator,
                    crate::protocol::PlayerInfo {
                        id: creator,
                        name: "Creator".to_string(),
                        is_authority: true,
                        is_ready: false,
                        connected_at: None,
                        connection_info: None,
                        epoch: None,
                        seq: None,
                        region_id: "region-1".to_string(),
                    },
                );
                players
            },
            authority_player: Some(creator),
            lobby_state: crate::protocol::LobbyState::Waiting,
            ready_players: Vec::new(),
            lobby_started_at: None,
            game_finalized_at: None,
            relay_type: "mesh".to_string(),
            region_id: "region-1".to_string(),
            application_id: Some(Uuid::nil()),
            created_at: chrono::Utc::now(),
            last_activity: chrono::Utc::now(),
            spectators: std::collections::HashMap::new(),
            max_spectators: None,
            password: None,
            banned_players: std::collections::HashSet::new(),
        }
    }

    fn active_record(records: &SessionRecords, room_id: RoomId) -> SessionRecord {
        records
            .active
            .get(&room_id)
            .map(|entry| entry.value().clone())
            .expect("active record")
    }

    #[test]
    fn created_record_carries_identity_and_creator_membership() {
        let records = SessionRecords::new();
        let room_id = Uuid::new_v4();
        records.record_created(&room_fixture(room_id, "ABC-def"), true);

        let record = active_record(&records, room_id);
        assert_eq!(record.room_id, room_id);
        assert_eq!(record.room_code, "ABC-def");
        assert_eq!(record.game_name, "test-game");
        assert_eq!(record.region_id, "region-1");
        assert_eq!(record.application_id, Some(Uuid::nil()));
        assert_eq!(record.max_players, 4);
        assert!(record.published);
        // The creator is the room's first member.
        assert_eq!(record.players_joined, 1);
        assert_eq!(record.players_left, 0);
        assert_eq!(record.spectators_joined, 0);
        assert_eq!(record.spectators_left, 0);
        assert_eq!(record.ended_at_ms, None);
        assert_eq!(record.close_reason, None);
    }

    #[test]
    fn membership_and_spectator_counters_follow_add_and_remove_success() {
        let records = SessionRecords::new();
        let room_id = Uuid::new_v4();
        records.record_created(&room_fixture(room_id, "ABC"), true);

        records.record_player_joined(&room_id);
        records.record_spectator_joined(&room_id);
        records.record_player_left(&room_id);
        records.record_spectator_left(&room_id);
        let record = active_record(&records, room_id);
        assert_eq!(record.players_joined, 2);
        assert_eq!(record.players_left, 1);
        assert_eq!(record.spectators_joined, 1);
        assert_eq!(record.spectators_left, 1);
    }

    #[test]
    fn publish_and_code_rotation_update_the_active_record() {
        let records = SessionRecords::new();
        let room_id = Uuid::new_v4();
        records.record_created(&room_fixture(room_id, "OLD"), false);
        assert!(!active_record(&records, room_id).published);

        records.record_published(&room_id);
        records.record_room_code_changed(&room_id, "NEW");
        let record = active_record(&records, room_id);
        assert!(record.published);
        assert_eq!(record.room_code, "NEW");
    }

    #[test]
    fn closed_published_room_moves_to_the_completed_ring_with_reason() {
        let records = SessionRecords::new();
        let room_id = Uuid::new_v4();
        records.record_created(&room_fixture(room_id, "ABC"), true);
        records.record_closed(&room_id, SessionCloseReason::Expired);

        assert!(records.active.get(&room_id).is_none());
        let snapshot = records.snapshot();
        assert_eq!(snapshot.active_count, 0);
        assert_eq!(snapshot.completed_count, 1);
        let record = &snapshot.completed[0];
        assert_eq!(record.room_id, room_id);
        assert_eq!(record.close_reason, Some(SessionCloseReason::Expired));
        assert!(record.ended_at_ms.is_some());
    }

    #[test]
    fn closed_unpublished_room_is_dropped_without_a_completed_record() {
        let records = SessionRecords::new();
        let room_id = Uuid::new_v4();
        records.record_created(&room_fixture(room_id, "ABC"), false);
        records.record_closed(&room_id, SessionCloseReason::Deleted);

        let snapshot = records.snapshot();
        assert_eq!(snapshot.active_count, 0);
        assert_eq!(snapshot.completed_count, 0);
    }

    #[test]
    fn completed_ring_drops_the_oldest_and_counts_every_drop() {
        let records = SessionRecords::new();
        for _ in 0..(SESSION_RECORDS_COMPLETED_CAP + 3) {
            let room_id = Uuid::new_v4();
            records.record_created(&room_fixture(room_id, "ABC"), true);
            records.record_closed(&room_id, SessionCloseReason::Deleted);
        }

        let snapshot = records.snapshot();
        assert_eq!(snapshot.completed_count, SESSION_RECORDS_COMPLETED_CAP);
        assert_eq!(snapshot.completed_cap, SESSION_RECORDS_COMPLETED_CAP);
        assert_eq!(snapshot.completed_dropped_total, 3);
        // Newest first: the last closed room leads the list.
        let newest = &snapshot.completed[0];
        let oldest = &snapshot.completed[SESSION_RECORDS_COMPLETED_CAP - 1];
        assert!(newest.ended_at_ms >= oldest.ended_at_ms);
    }

    #[test]
    fn snapshot_orders_active_records_deterministically() {
        let records = SessionRecords::new();
        let ids: Vec<RoomId> = (0..8).map(|_| Uuid::new_v4()).collect();
        for (index, room_id) in ids.iter().enumerate() {
            let room = room_fixture(*room_id, "ABC");
            records.record_created(&room, true);
            // Same timestamp within each pair: only the id tie-break orders.
            records.active.get_mut(room_id).unwrap().created_at_ms = index as u64 / 4;
        }

        let snapshot = records.snapshot();
        let mut keys: Vec<(u64, RoomId)> = snapshot
            .active
            .iter()
            .map(|record| (record.created_at_ms, record.room_id))
            .collect();
        keys.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let listed: Vec<(u64, RoomId)> = snapshot
            .active
            .iter()
            .map(|record| (record.created_at_ms, record.room_id))
            .collect();
        assert_eq!(listed, keys, "active records must be listed oldest-first");
    }
}
