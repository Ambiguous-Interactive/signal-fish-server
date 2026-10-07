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

use std::collections::{BTreeSet, VecDeque};
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

/// Maximum number of active records one `/metrics/sessions` response lists.
///
/// Live rooms are admission-capped far above any single scrape's usefulness
/// (the server-wide ceiling is 10 000 at defaults), and the records are
/// room-cardinality data. The oldest records survive (the snapshot's
/// deterministic order), the omission is flagged `activeTruncated` with the
/// dropped count in `activeOmitted`, and every envelope counter keeps its
/// pre-truncation value.
pub const SESSION_RECORDS_ACTIVE_RESPONSE_CAP: usize = 512;

/// Whole-response byte budget for the `/metrics/sessions` JSON response.
///
/// Sized like the `/metrics` whole-response budget so the structural caps
/// compose: a full completed ring
/// ([`SESSION_RECORDS_COMPLETED_CAP`]) plus a full active response cap
/// ([`SESSION_RECORDS_ACTIVE_RESPONSE_CAP`]) of worst-case-sized records
/// stays inside the budget — pinned by
/// `full_caps_of_worst_case_records_fit_the_response_budget` — and
/// truncation stays a rare fail-visible backstop instead of a steady state.
/// 1.5 MiB: the per-session counters and encoding/version sets added with
/// the #766 fields push the full-caps worst case just past 1 MiB.
pub const SESSIONS_RESPONSE_MAX_BYTES: usize = 1536 * 1024;

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
    /// Distinct player protocol versions observed at membership add, sorted.
    /// Membership is add-only: a version seen once stays listed for the
    /// session's lifetime, so the set describes the session's client mix.
    pub protocol_versions: BTreeSet<u16>,
    /// Distinct game-data wire encodings observed on budget-admitted relayed
    /// frames, sorted (`json`, `message_pack`, `rkyv`, `protobuf`).
    pub game_data_encodings: BTreeSet<String>,
    /// Sender-side budget-admitted game-data frames relayed in this room
    /// (the per-session twin of the charge path behind
    /// `players.relay_bytes_total`).
    pub game_data_messages: u64,
    /// Sender-side budget-admitted game-data payload bytes relayed in this
    /// room (the per-session twin of `players.relay_bytes_total`).
    pub relay_bytes: u64,
    /// Authority switches inside this room (the per-session twin of
    /// `players.authorityTransfers`).
    pub authority_transfers: u64,
    /// Accepted v3 transport reports that established a peer-to-peer path
    /// (the per-session twin of `transport.p2pEstablished`; same definition).
    pub p2p_established: u64,
    /// Accepted v3 transport reports that fell back to the relay floor (the
    /// per-session twin of `transport.relayFallback`; same definition).
    pub relay_fallback: u64,
    /// TURN credentials issued for this room's session plans and ICE
    /// pre-gathers (the per-session twin of
    /// `transport.turnCredentialsIssued`; same definition).
    pub turn_credentials_issued: u64,
    /// Monotonic completion sequence, stamped when the record enters the
    /// completed ring; `null` while the room is active. The `?since=` cursor
    /// compares against this, making completed-session ingest gapless within
    /// the ring's retention window.
    pub seq: Option<u64>,
}

impl SessionRecord {
    fn from_room(room: &Room, published: bool) -> Self {
        Self {
            room_id: room.id,
            room_code: room.code.clone(),
            game_name: room.game_name.clone(),
            application_id: room.application_id,
            region_id: room.region_id.clone(),
            max_players: room.max_players,
            // The room row's own stamp is the single source of truth.
            created_at_ms: u64::try_from(room.created_at.timestamp_millis()).unwrap_or(0),
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
            protocol_versions: BTreeSet::new(),
            game_data_encodings: BTreeSet::new(),
            game_data_messages: 0,
            relay_bytes: 0,
            authority_transfers: 0,
            p2p_established: 0,
            relay_fallback: 0,
            turn_credentials_issued: 0,
            seq: None,
        }
    }
}

/// Bounded registry of active and completed session records.
pub struct SessionRecords {
    active: DashMap<RoomId, SessionRecord>,
    completed: Mutex<VecDeque<SessionRecord>>,
    completed_dropped_total: AtomicU64,
    /// Rooms removed before their first publication. No directory ever saw
    /// them, so they leave no completed record; the counter keeps the drop
    /// visible to operators.
    unpublished_dropped_total: AtomicU64,
    /// Source of the per-record completion sequence ([`SessionRecord::seq`]).
    /// Stamped in ring-insertion order so a `?since=` cursor over the
    /// completed list is gapless within the ring's retention window. Starts
    /// at 1 so a `since=0` bootstrap cursor keeps every completed record.
    next_completed_seq: AtomicU64,
}

impl Default for SessionRecords {
    fn default() -> Self {
        Self {
            active: DashMap::default(),
            completed: Mutex::default(),
            completed_dropped_total: AtomicU64::default(),
            unpublished_dropped_total: AtomicU64::default(),
            next_completed_seq: AtomicU64::new(1),
        }
    }
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
    /// Total rooms removed before their first publication. They leave no
    /// completed record because no directory ever saw them.
    pub unpublished_dropped_total: u64,
}

impl SessionRecords {
    pub fn new() -> Self {
        Self::default()
    }

    fn now_ms() -> u64 {
        // Wall clock (durable record): the ended stamp is the storage-removal
        // time on the observability surface; no deadline reads it.
        u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
    }

    /// Record a committed room row. `published` is false for hidden pending
    /// creations that become visible through [`Self::record_published`].
    pub(crate) fn record_created(&self, room: &Room, published: bool) {
        self.active
            .insert(room.id, SessionRecord::from_room(room, published));
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

    /// Follow a later application claim or clear so the record never keeps a
    /// stale creation-time owner: the admission path claims the room's owner
    /// at first claimed join, and rollback clears it.
    pub(crate) fn record_application_id_changed(
        &self,
        room_id: &RoomId,
        application_id: Option<Uuid>,
    ) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.application_id = application_id;
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

    /// Attribute one budget-admitted game-data frame (sender-side payload
    /// bytes and wire encoding) to its room's session.
    pub(crate) fn record_game_data(&self, room_id: &RoomId, bytes: u64, encoding: &str) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.game_data_messages = record.game_data_messages.saturating_add(1);
            record.relay_bytes = record.relay_bytes.saturating_add(bytes);
            // Probe before inserting: the owned insert would allocate on
            // every frame, and the relay charge path stays allocation-free
            // in steady state (one encoding per room in the common case).
            if !record.game_data_encodings.contains(encoding) {
                record.game_data_encodings.insert(encoding.to_string());
            }
        }
    }

    /// Attribute a member's negotiated protocol version to its room's
    /// session. The set is add-only (see [`SessionRecord::protocol_versions`]).
    pub(crate) fn record_member_protocol_version(&self, room_id: &RoomId, version: u16) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.protocol_versions.insert(version);
        }
    }

    /// Attribute one authority switch to the room's session.
    pub(crate) fn record_authority_transfer(&self, room_id: &RoomId) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.authority_transfers = record.authority_transfers.saturating_add(1);
        }
    }

    /// Attribute one accepted P2P-establishing transport report to the
    /// reporter's room's session (same definition as the server-wide
    /// `transport.p2pEstablished` counter).
    pub(crate) fn record_p2p_established(&self, room_id: &RoomId) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.p2p_established = record.p2p_established.saturating_add(1);
        }
    }

    /// Attribute one accepted relay-fallback transport report to the
    /// reporter's room's session (same definition as the server-wide
    /// `transport.relayFallback` counter).
    pub(crate) fn record_relay_fallback(&self, room_id: &RoomId) {
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.relay_fallback = record.relay_fallback.saturating_add(1);
        }
    }

    /// Attribute TURN credential issuances to the room whose session plan (or
    /// ICE pre-gather) minted them. Zero-count calls are no-ops so callers can
    /// forward their totals unconditionally.
    pub(crate) fn record_turn_credentials(&self, room_id: &RoomId, count: u64) {
        if count == 0 {
            return;
        }
        if let Some(mut record) = self.active.get_mut(room_id) {
            record.turn_credentials_issued = record.turn_credentials_issued.saturating_add(count);
        }
    }

    /// Finalize a removed room. Published rooms move to the completed ring;
    /// rooms that were never visible are dropped without a record (no
    /// directory ever saw them), with the drop counted.
    pub(crate) fn record_closed(&self, room_id: &RoomId, reason: SessionCloseReason) {
        if let Some((_, mut record)) = self.active.remove(room_id) {
            if !record.published {
                self.unpublished_dropped_total
                    .fetch_add(1, Ordering::Relaxed);
                return;
            }
            record.ended_at_ms = Some(Self::now_ms());
            record.close_reason = Some(reason);
            self.push_completed(record);
        }
    }

    fn push_completed(&self, mut record: SessionRecord) {
        let mut completed = self
            .completed
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Stamp only after taking the ring mutex, so sequence order equals
        // ring order by construction: a scrape takes the same mutex and can
        // never observe a stamped-but-unpushed sequence that would let a
        // `?since=` cursor skip a completion still inside the ring. (Closes
        // are additionally serialized by the storage layer's rooms write
        // lock; the invariant here must not depend on that.)
        record.seq = Some(self.next_completed_seq.fetch_add(1, Ordering::Relaxed));
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
        // Poisoning recovers the guarded data: the ring is a plain queue, so
        // a panicked writer leaves element-consistent state behind.
        let completed: Vec<SessionRecord> = self
            .completed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
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
            unpublished_dropped_total: self.unpublished_dropped_total.load(Ordering::Relaxed),
        }
    }

    /// Snapshot for one scrape with the `/metrics/sessions` query filters
    /// applied (issue #766): `application_id` restricts both lists to one
    /// application's rooms, `since` cursor-filters the completed list to
    /// records whose completion sequence exceeds it (the active list is a
    /// live snapshot and is unaffected). The envelope counts describe the
    /// filtered view — they equal the returned list lengths before response
    /// truncation — while the process-lifetime drop counters stay global.
    pub fn snapshot_view(
        &self,
        since: Option<u64>,
        application_id: Option<Uuid>,
    ) -> SessionsSnapshot {
        let mut snapshot = self.snapshot();
        if application_id.is_some() {
            snapshot
                .active
                .retain(|record| record.application_id == application_id);
            snapshot
                .completed
                .retain(|record| record.application_id == application_id);
        }
        if let Some(since) = since {
            snapshot
                .completed
                .retain(|record| record.seq.is_some_and(|seq| seq > since));
        }
        snapshot.active_count = snapshot.active.len();
        snapshot.completed_count = snapshot.completed.len();
        snapshot
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
        assert_eq!(
            snapshot.unpublished_dropped_total, 1,
            "the invisible drop must stay operator-visible as a counter"
        );
    }

    /// The wire shape is part of the contract: consumers scrape camelCase
    /// keys, so a field rename here would silently break the surface.
    #[test]
    fn record_and_snapshot_serialize_camel_case_keys() {
        let records = SessionRecords::new();
        let room_id = Uuid::new_v4();
        let mut room = room_fixture(room_id, "ABC");
        room.created_at = chrono::Utc::now();
        records.record_created(&room, true);
        records.record_closed(&room_id, SessionCloseReason::Empty);

        let value = serde_json::to_value(records.snapshot()).expect("snapshot serializes");
        let record = &value["completed"][0];
        for key in [
            "roomId",
            "roomCode",
            "gameName",
            "applicationId",
            "regionId",
            "maxPlayers",
            "createdAtMs",
            "endedAtMs",
            "closeReason",
            "published",
            "playersJoined",
            "playersLeft",
            "spectatorsJoined",
            "spectatorsLeft",
            "protocolVersions",
            "gameDataEncodings",
            "gameDataMessages",
            "relayBytes",
            "authorityTransfers",
            "p2pEstablished",
            "relayFallback",
            "turnCredentialsIssued",
            "seq",
        ] {
            assert!(
                record.get(key).is_some(),
                "missing camelCase wire key `{key}` in {record}"
            );
        }
        let envelope_keys = [
            "active",
            "completed",
            "activeCount",
            "completedCount",
            "completedCap",
            "completedDroppedTotal",
            "unpublishedDroppedTotal",
        ];
        for key in envelope_keys {
            assert!(value.get(key).is_some(), "missing envelope key `{key}`");
        }
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

    /// The structural caps must compose inside the response byte budget: a
    /// full completed ring plus a full active response cap of
    /// worst-case-sized records serializes within the `/metrics/sessions`
    /// budget. Without this pin, raising either cap independently silently
    /// turns every scrape into the truncation marker.
    #[test]
    fn full_caps_of_worst_case_records_fit_the_response_budget() {
        let records = SessionRecords::new();
        let worst_case_room = || {
            let mut room = room_fixture(Uuid::new_v4(), &"C".repeat(64));
            room.game_name = "G".repeat(64);
            room.region_id = "R".repeat(64);
            room
        };
        // A worst-case record also carries full per-record sets (the
        // encodings serialize as string arrays, the versions as a number
        // array), not just the identity strings.
        let stamp_worst_case_sets = |records: &SessionRecords, room_id: RoomId| {
            let mut record = records.active.get_mut(&room_id).expect("active record");
            for encoding in ["json", "message_pack", "rkyv", "protobuf"] {
                record.game_data_encodings.insert(encoding.to_string());
            }
            record.protocol_versions.insert(u16::MIN);
            record.protocol_versions.insert(u16::MAX);
        };

        for _ in 0..SESSION_RECORDS_COMPLETED_CAP {
            let room = worst_case_room();
            records.record_created(&room, true);
            stamp_worst_case_sets(&records, room.id);
            records.record_closed(&room.id, SessionCloseReason::Deleted);
        }
        for _ in 0..SESSION_RECORDS_ACTIVE_RESPONSE_CAP {
            let room = worst_case_room();
            records.record_created(&room, true);
            stamp_worst_case_sets(&records, room.id);
        }

        let bytes = serde_json::to_vec(&records.snapshot()).expect("snapshot serializes");
        let serialized = bytes.len();
        assert!(
            serialized <= SESSIONS_RESPONSE_MAX_BYTES,
            "full caps of worst-case records must fit the response budget: \
             {serialized} bytes > {SESSIONS_RESPONSE_MAX_BYTES}"
        );
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

    #[test]
    fn game_data_frames_accumulate_bytes_and_distinct_encodings() {
        let records = SessionRecords::new();
        let room_id = Uuid::new_v4();
        records.record_created(&room_fixture(room_id, "ABC"), true);

        records.record_game_data(&room_id, 96, "json");
        records.record_game_data(&room_id, 128, "message_pack");
        records.record_game_data(&room_id, 32, "json");
        let record = active_record(&records, room_id);
        assert_eq!(record.game_data_messages, 3);
        assert_eq!(record.relay_bytes, 96 + 128 + 32);
        assert_eq!(
            record.game_data_encodings.into_iter().collect::<Vec<_>>(),
            vec!["json".to_string(), "message_pack".to_string()],
            "encodings must be a sorted distinct set"
        );
    }

    #[test]
    fn member_protocol_versions_form_a_sorted_distinct_add_only_set() {
        let records = SessionRecords::new();
        let room_id = Uuid::new_v4();
        records.record_created(&room_fixture(room_id, "ABC"), true);

        records.record_member_protocol_version(&room_id, 3);
        records.record_member_protocol_version(&room_id, 2);
        records.record_member_protocol_version(&room_id, 3);
        let record = active_record(&records, room_id);
        assert_eq!(
            record.protocol_versions.into_iter().collect::<Vec<_>>(),
            vec![2, 3],
            "versions must be a sorted distinct set"
        );
    }

    #[test]
    fn authority_transport_and_turn_counters_accumulate_per_session() {
        let records = SessionRecords::new();
        let room_id = Uuid::new_v4();
        records.record_created(&room_fixture(room_id, "ABC"), true);

        records.record_authority_transfer(&room_id);
        records.record_authority_transfer(&room_id);
        records.record_p2p_established(&room_id);
        records.record_relay_fallback(&room_id);
        records.record_relay_fallback(&room_id);
        // Zero-count issuance calls are no-ops (callers forward totals).
        records.record_turn_credentials(&room_id, 0);
        records.record_turn_credentials(&room_id, 5);
        let record = active_record(&records, room_id);
        assert_eq!(record.authority_transfers, 2);
        assert_eq!(record.p2p_established, 1);
        assert_eq!(record.relay_fallback, 2);
        assert_eq!(record.turn_credentials_issued, 5);
    }

    #[test]
    fn attribution_for_an_unknown_room_is_a_no_op() {
        let records = SessionRecords::new();
        let missing = Uuid::new_v4();
        records.record_game_data(&missing, 10, "json");
        records.record_member_protocol_version(&missing, 3);
        records.record_authority_transfer(&missing);
        records.record_p2p_established(&missing);
        records.record_relay_fallback(&missing);
        records.record_turn_credentials(&missing, 2);
        assert_eq!(records.snapshot().active_count, 0);
    }

    /// The completion sequence is stamped in ring-insertion order so a
    /// `?since=` cursor over the completed list is gapless within the ring's
    /// retention window, and active records carry no sequence yet.
    #[test]
    fn completed_records_carry_monotonic_cursor_sequences() {
        let records = SessionRecords::new();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        records.record_created(&room_fixture(first, "AAA"), true);
        records.record_created(&room_fixture(second, "BBB"), true);

        let active = records.snapshot();
        assert!(active.active[0].seq.is_none(), "active records have no seq");

        records.record_closed(&second, SessionCloseReason::Deleted);
        records.record_closed(&first, SessionCloseReason::Empty);
        let snapshot = records.snapshot();
        let (second_seq, first_seq) = (snapshot.completed[0].seq, snapshot.completed[1].seq);
        assert!(
            second_seq > first_seq,
            "sequences follow completion order, not creation order: \
             second {second_seq:?} must exceed first {first_seq:?}"
        );
        assert!(first_seq.unwrap() < second_seq.unwrap());
    }

    /// The `?applicationId=` view restricts both lists to one application's
    /// rooms and recomputes the envelope counts for the filtered view; the
    /// process-lifetime drop counters stay global.
    #[test]
    fn application_view_restricts_both_lists_and_recomputes_counts() {
        let records = SessionRecords::new();
        let app_a = Uuid::new_v4();
        let app_b = Uuid::new_v4();
        let owned = Uuid::new_v4();
        let mut owned_room = room_fixture(owned, "OWN");
        owned_room.application_id = Some(app_a);
        let unowned = Uuid::new_v4();
        records.record_created(&owned_room, true);
        records.record_created(&room_fixture(unowned, "UNO"), true);
        // One completed record per application.
        records.record_closed(&owned, SessionCloseReason::Deleted);
        let closed_b = Uuid::new_v4();
        let mut closed_room = room_fixture(closed_b, "CLB");
        closed_room.application_id = Some(app_b);
        records.record_created(&closed_room, true);
        records.record_closed(&closed_b, SessionCloseReason::Deleted);

        let view = records.snapshot_view(None, Some(app_a));
        // Only the app-owned rooms match; the unowned room (and any other
        // application's rooms) stay out of the view.
        assert!(
            view.active
                .iter()
                .all(|record| record.application_id == Some(app_a)),
            "the active view must carry only the requested application's rooms"
        );
        assert!(view
            .completed
            .iter()
            .all(|r| r.application_id == Some(app_a)));
        assert_eq!(view.active_count, 0);
        assert_eq!(view.completed_count, 1);
        // Global counters are never filtered.
        assert_eq!(view.completed_dropped_total, 0);

        let empty_view = records.snapshot_view(None, Some(app_b));
        assert_eq!(empty_view.active_count, 0);
        assert_eq!(empty_view.completed_count, 1);
    }

    /// The `?since=` cursor filters only the completed list, keeps records
    /// strictly above the cursor, and leaves the active snapshot untouched.
    #[test]
    fn since_cursor_filters_only_completed_records_above_the_cursor() {
        let records = SessionRecords::new();
        let active_id = Uuid::new_v4();
        records.record_created(&room_fixture(active_id, "ACT"), true);
        let mut seqs = Vec::new();
        for code in ["C1", "C2", "C3"] {
            let room_id = Uuid::new_v4();
            records.record_created(&room_fixture(room_id, code), true);
            records.record_closed(&room_id, SessionCloseReason::Deleted);
            seqs.push(
                records
                    .snapshot()
                    .completed
                    .iter()
                    .find(|record| record.room_code == code)
                    .and_then(|record| record.seq)
                    .expect("completed record carries its sequence"),
            );
        }
        seqs.sort();

        let view = records.snapshot_view(Some(seqs[1]), None);
        // Completed keeps only the sequences strictly above the cursor;
        // the active record is a live snapshot and is unaffected.
        assert_eq!(view.completed_count, 1);
        assert_eq!(view.completed[0].seq, Some(seqs[2]));
        assert_eq!(
            view.active.iter().map(|r| r.room_id).collect::<Vec<_>>(),
            vec![active_id]
        );
        // Cursor zero keeps every completed record.
        assert_eq!(records.snapshot_view(Some(0), None).completed_count, 3);
    }
}
