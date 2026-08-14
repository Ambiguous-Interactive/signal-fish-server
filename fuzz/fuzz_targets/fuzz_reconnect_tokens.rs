#![no_main]
//! Coverage-guided op-sequence fuzzing of the `ReconnectionManager` claim
//! protocol (src/reconnection.rs).
//!
//! `arbitrary`-derived op sequences drive a real `ReconnectionManager` over a
//! small pool of players/rooms with BOTH real tokens (captured from
//! `register_disconnection`, so the accepting paths are reachable) and
//! arbitrary token strings (the rejection paths, including
//! `constant_time_eq` over hostile lengths/bytes). The reconnection window is
//! drawn from {0s, 1s, 1h} per input, so real wall-clock expiry races are
//! fuzzed too — the invariants below are deliberately one-sided where expiry
//! makes the reference model time-dependent.
//!
//! Invariants (any violation panics = a finding):
//! - no panic anywhere (implicit);
//! - NO DOUBLE CLAIM: a claim while another claim is active on the same
//!   record fails (`AlreadyInProgress` is checked before expiry, so this is
//!   time-robust);
//! - a claim can only succeed with the record's real token;
//! - a real, room-bound token succeeds in the non-expiring (1h) mode;
//! - completing/releasing a RELEASED or SUPERSEDED claim fails; completing an
//!   active claim removes the pending record;
//! - `cleanup_expired` never removes a record with an active claim;
//! - zero-window cleanup removes every unclaimed record, while one-hour cleanup
//!   removes none;
//! - pending snapshots preserve authority, player membership, replay cursor,
//!   epoch, and token timestamps across same-room re-registration;
//! - a reference replay ring checks both directions of the buffer gate: rooms
//!   with pending players retain every recorded event up to bounded eviction,
//!   while rooms without pending players replay nothing.
//!
//! Each input runs on a fresh current-thread tokio runtime (the manager is
//! async lock-based; no background tasks are involved).
//!
//! Run via the nightly `fuzz` CI job, never on stable.
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::{PlayerInfo, ServerMessage};
use signal_fish_server::reconnection::{
    ClaimedReconnection, DisconnectedPlayer, ReconnectionError, ReconnectionManager,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use uuid::Uuid;

/// Player/room pool sizes (ops address them by index modulo these).
const PLAYERS: usize = 4;
const ROOMS: usize = 3;
/// Op budget per input.
const MAX_OPS: usize = 32;
/// Tiny replay ring so eviction/truncation paths are reachable in-input.
const EVENT_BUFFER_SIZE: usize = 2;

#[derive(Debug, Arbitrary)]
enum TokenChoice {
    /// The real token captured when the record was registered.
    Real,
    /// An arbitrary hostile string.
    Arbitrary(String),
}

#[derive(Debug, Arbitrary)]
enum Op {
    Register {
        player: u8,
        room: u8,
        was_authority: bool,
        last_epoch: u32,
    },
    Validate {
        player: u8,
        room: u8,
        token: TokenChoice,
    },
    Claim {
        claimer: u8,
        player: u8,
        room: u8,
        token: TokenChoice,
    },
    /// Complete a previously issued claim (by index into the claims this
    /// input has collected).
    CompleteClaimed {
        claim: u8,
    },
    /// Release a previously issued claim.
    Release {
        claim: u8,
    },
    /// The non-claimed completion path (server-internal room restore).
    CompleteDirect {
        player: u8,
    },
    RecordEvent {
        room: u8,
    },
    GetMissed {
        room: u8,
        last_sequence: u8,
    },
    CleanupExpired,
    ClearRoomBuffer {
        room: u8,
    },
    /// Rotate the credential intended for the player's next genuine
    /// disconnection. Kept last so existing structured corpus encodings retain
    /// their operation discriminants.
    PreIssue {
        player: u8,
        room: u8,
    },
}

#[derive(Debug, Arbitrary)]
struct Plan {
    /// Selects the reconnection window: 0 (everything expires instantly),
    /// 1s, or 1h (nothing expires within an input).
    window_choice: u8,
    ops: Vec<Op>,
}

/// Reference bookkeeping for the time-robust invariants.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingSnapshot {
    disconnected_at_micros: i64,
    token_created_at_micros: i64,
    token_expires_at_micros: i64,
    last_sequence: u64,
    last_epoch: u32,
    was_authority: bool,
    player_info: Option<PlayerInfoSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlayerInfoSnapshot {
    id: Uuid,
    name: String,
    is_authority: bool,
    is_ready: bool,
    connected_at_micros: i64,
    has_connection_info: bool,
    epoch: Option<u32>,
    seq: Option<u64>,
    region_id: String,
}

impl From<&PlayerInfo> for PlayerInfoSnapshot {
    fn from(info: &PlayerInfo) -> Self {
        Self {
            id: info.id,
            name: info.name.clone(),
            is_authority: info.is_authority,
            is_ready: info.is_ready,
            connected_at_micros: info.connected_at.timestamp_micros(),
            has_connection_info: info.connection_info.is_some(),
            epoch: info.epoch,
            seq: info.seq,
            region_id: info.region_id.clone(),
        }
    }
}

impl From<&DisconnectedPlayer> for PendingSnapshot {
    fn from(player: &DisconnectedPlayer) -> Self {
        Self {
            disconnected_at_micros: player.disconnected_at.timestamp_micros(),
            token_created_at_micros: player.token.created_at.timestamp_micros(),
            token_expires_at_micros: player.token.expires_at.timestamp_micros(),
            last_sequence: player.last_sequence,
            last_epoch: player.last_epoch,
            was_authority: player.was_authority,
            player_info: player.player_info.as_ref().map(PlayerInfoSnapshot::from),
        }
    }
}

#[derive(Default)]
struct ReplayBufferReference {
    sequences: VecDeque<u64>,
    evicted_watermark: Option<u64>,
}

#[derive(Clone)]
struct PendingReference {
    room: Uuid,
    token: String,
    claim_active: bool,
    last_epoch: u32,
    snapshot: Option<PendingSnapshot>,
}

#[derive(Default)]
struct Reference {
    pending: HashMap<Uuid, PendingReference>,
    /// player -> (room, token) for the next genuine disconnect.
    pre_issued: HashMap<Uuid, (Uuid, String)>,
    replay_buffers: HashMap<Uuid, ReplayBufferReference>,
    next_sequence: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeldClaimState {
    Active,
    Superseded,
    Spent,
}

/// A claim this input collected. Superseded and spent handles must never
/// complete or release successfully.
struct HeldClaim {
    claim: ClaimedReconnection,
    state: HeldClaimState,
}

fn player_id(index: u8) -> Uuid {
    Uuid::from_u128(0xF0BB_0000 + (index as usize % PLAYERS) as u128 + 1)
}

fn room_id(index: u8) -> Uuid {
    Uuid::from_u128(0xF0CC_0000 + (index as usize % ROOMS) as u128 + 1)
}

fn player_info(player: Uuid, was_authority: bool, last_epoch: u32) -> Option<PlayerInfo> {
    was_authority.then(|| PlayerInfo {
        id: player,
        name: format!("fuzz-{player}"),
        is_authority: true,
        is_ready: last_epoch % 2 == 0,
        connected_at: chrono::Utc::now(),
        connection_info: None,
        epoch: Some(last_epoch),
        seq: Some(u64::from(last_epoch)),
        region_id: "fuzz".to_owned(),
    })
}

fn remove_orphaned_buffer(reference: &mut Reference, room: Uuid) {
    if !reference
        .pending
        .values()
        .any(|pending| pending.room == room)
    {
        reference.replay_buffers.remove(&room);
    }
}

fn control_event() -> ServerMessage {
    ServerMessage::PlayerLeft {
        player_id: Uuid::from_u128(0xF0DD_0001),
        epoch: Some(1),
        final_seq: Some(0),
    }
}

async fn run(plan: Plan) {
    let window_seconds: u64 = match plan.window_choice % 3 {
        0 => 0,
        1 => 1,
        _ => 3600,
    };
    let manager = ReconnectionManager::new(
        window_seconds,
        EVENT_BUFFER_SIZE,
        Arc::new(ServerMetrics::new()),
    );
    let mut reference = Reference::default();
    let mut claims: Vec<HeldClaim> = Vec::new();

    for op in plan.ops.into_iter().take(MAX_OPS) {
        match op {
            Op::PreIssue { player, room } => {
                let player = player_id(player);
                let room = room_id(room);
                let token = manager.pre_issue_token(player, room).await;
                if let Some((_, previous_token)) =
                    reference.pre_issued.insert(player, (room, token.clone()))
                {
                    assert_ne!(
                        token, previous_token,
                        "pre-issuing must rotate the replacement credential"
                    );
                }
            }
            Op::Register {
                player,
                room,
                was_authority,
                last_epoch,
            } => {
                let player = player_id(player);
                let room = room_id(room);
                let new_player_info = player_info(player, was_authority, last_epoch);
                let token = manager
                    .register_disconnection(
                        player,
                        room,
                        was_authority,
                        new_player_info.clone(),
                        last_epoch,
                    )
                    .await;
                let previous = reference.pending.get(&player).cloned();
                let same_room = previous
                    .as_ref()
                    .is_some_and(|existing| existing.room == room);

                if same_room {
                    let existing = previous
                        .as_ref()
                        .expect("same-room classification requires a pending record");
                    assert_eq!(
                        token, existing.token,
                        "same-room re-registration must preserve the token"
                    );
                    let observed_snapshot = manager
                        .validate_reconnection(&player, &room, &token)
                        .await
                        .ok()
                        .map(|disconnected| PendingSnapshot::from(&disconnected));

                    if existing.claim_active {
                        // Registration that races an active reconnect claim is
                        // a strict no-op, including its captured snapshot and
                        // the pre-issued credential for the next disconnect.
                        if let (Some(expected), Some(observed)) =
                            (&existing.snapshot, &observed_snapshot)
                        {
                            assert_eq!(
                                observed, expected,
                                "active-claim re-registration changed the pending snapshot"
                            );
                        }
                    } else {
                        let preserved_epoch = existing.last_epoch.max(last_epoch);
                        let expected_snapshot = existing.snapshot.clone().map(|mut snapshot| {
                            snapshot.last_epoch = preserved_epoch;
                            if snapshot.player_info.is_none() {
                                snapshot.player_info =
                                    new_player_info.as_ref().map(PlayerInfoSnapshot::from);
                            }
                            snapshot
                        });
                        if let (Some(expected), Some(observed)) =
                            (&expected_snapshot, &observed_snapshot)
                        {
                            assert_eq!(
                                observed, expected,
                                "same-room re-registration changed preserved snapshot state"
                            );
                        }
                        reference.pending.insert(
                            player,
                            PendingReference {
                                room,
                                token,
                                claim_active: false,
                                last_epoch: preserved_epoch,
                                snapshot: expected_snapshot.or(observed_snapshot),
                            },
                        );
                    }
                } else {
                    // A genuinely new or different-room registration consumes
                    // the pre-issued slot and replaces the record wholesale.
                    let pre_issued = reference.pre_issued.remove(&player);
                    match pre_issued {
                        Some((pre_issued_room, pre_issued_token)) if pre_issued_room == room => {
                            assert_eq!(
                                token, pre_issued_token,
                                "registration must consume its matching pre-issued token"
                            );
                        }
                        Some((_, discarded_token)) => {
                            assert_ne!(
                                token, discarded_token,
                                "registration must not reuse a wrong-room pre-issued token"
                            );
                        }
                        None => {}
                    }
                    if let Some(existing) = &previous {
                        assert_ne!(
                            token, existing.token,
                            "different-room registration must replace the old credential"
                        );
                        for held in &mut claims {
                            if held.state == HeldClaimState::Active
                                && held.claim.disconnected.player_id == player
                            {
                                held.state = HeldClaimState::Superseded;
                            }
                        }
                    }
                    let snapshot = manager
                        .validate_reconnection(&player, &room, &token)
                        .await
                        .ok()
                        .map(|disconnected| PendingSnapshot::from(&disconnected));
                    reference.pending.insert(
                        player,
                        PendingReference {
                            room,
                            token,
                            claim_active: false,
                            last_epoch,
                            snapshot,
                        },
                    );
                    if let Some(existing) = previous {
                        remove_orphaned_buffer(&mut reference, existing.room);
                    }
                    reference.replay_buffers.entry(room).or_default();
                }
            }
            Op::Validate {
                player,
                room,
                token,
            } => {
                let player = player_id(player);
                let room = room_id(room);
                let token_string = match &token {
                    TokenChoice::Real => reference
                        .pending
                        .get(&player)
                        .map(|pending| pending.token.clone())
                        .unwrap_or_default(),
                    TokenChoice::Arbitrary(raw) => raw.clone(),
                };
                let result = manager
                    .validate_reconnection(&player, &room, &token_string)
                    .await;
                let must_accept = window_seconds == 3600
                    && matches!(token, TokenChoice::Real)
                    && reference
                        .pending
                        .get(&player)
                        .is_some_and(|pending| pending.room == room);
                assert!(
                    !must_accept || result.is_ok(),
                    "a real room-bound token must validate in the one-hour mode"
                );
                if let Ok(disconnected) = result {
                    let expected = reference
                        .pending
                        .get(&player)
                        .expect("a successful validation implies a pending record");
                    assert_eq!(
                        token_string, expected.token,
                        "validation accepted a token that is not the stored one"
                    );
                    assert_eq!(
                        expected.room, room,
                        "validation accepted the wrong room binding"
                    );
                    if let Some(snapshot) = &expected.snapshot {
                        assert_eq!(
                            PendingSnapshot::from(&disconnected),
                            *snapshot,
                            "validation returned changed pending snapshot state"
                        );
                    }
                }
            }
            Op::Claim {
                claimer,
                player,
                room,
                token,
            } => {
                let claimer = player_id(claimer.wrapping_add(7));
                let player = player_id(player);
                let room = room_id(room);
                let token_string = match &token {
                    TokenChoice::Real => reference
                        .pending
                        .get(&player)
                        .map(|pending| pending.token.clone())
                        .unwrap_or_default(),
                    TokenChoice::Arbitrary(raw) => raw.clone(),
                };
                let result = manager
                    .claim_reconnection(&claimer, &player, &room, &token_string)
                    .await;
                let must_accept = window_seconds == 3600
                    && matches!(token, TokenChoice::Real)
                    && reference
                        .pending
                        .get(&player)
                        .is_some_and(|pending| pending.room == room && !pending.claim_active);
                assert!(
                    !must_accept || result.is_ok(),
                    "a real unclaimed room-bound token must claim in the one-hour mode"
                );
                match result {
                    Ok(claimed) => {
                        let expected = reference
                            .pending
                            .get_mut(&player)
                            .expect("a successful claim implies a pending record");
                        assert!(
                            !expected.claim_active,
                            "DOUBLE CLAIM: a second claim succeeded while one was active"
                        );
                        assert_eq!(
                            token_string, expected.token,
                            "claim accepted a token that is not the stored one"
                        );
                        assert_eq!(expected.room, room, "claim accepted the wrong room binding");
                        assert_eq!(
                            claimed.disconnected.last_epoch, expected.last_epoch,
                            "claim must preserve the reconnect incarnation baseline"
                        );
                        if let Some(snapshot) = &expected.snapshot {
                            assert_eq!(
                                PendingSnapshot::from(&claimed.disconnected),
                                *snapshot,
                                "claim returned changed pending snapshot state"
                            );
                        }
                        expected.claim_active = true;
                        claims.push(HeldClaim {
                            claim: claimed,
                            state: HeldClaimState::Active,
                        });
                    }
                    Err(error) => {
                        // Time-robust half: while the reference knows a claim
                        // is active, the manager must refuse (checked before
                        // expiry in claim_reconnection).
                        if reference
                            .pending
                            .get(&player)
                            .is_some_and(|pending| pending.claim_active)
                        {
                            assert_eq!(
                                error,
                                ReconnectionError::AlreadyInProgress,
                                "an actively claimed record must refuse with AlreadyInProgress"
                            );
                        }
                    }
                }
            }
            Op::CompleteClaimed { claim } => {
                if claims.is_empty() {
                    continue;
                }
                let index = claim as usize % claims.len();
                let held_state = claims[index].state;
                let claimed = claims[index].claim.clone();
                let completed = manager.complete_claimed_reconnection(&claimed).await;
                match held_state {
                    HeldClaimState::Active => {
                        assert!(completed, "completing the active claim must succeed");
                        claims[index].state = HeldClaimState::Spent;
                        if let Some(removed) =
                            reference.pending.remove(&claimed.disconnected.player_id)
                        {
                            remove_orphaned_buffer(&mut reference, removed.room);
                        }
                    }
                    HeldClaimState::Superseded | HeldClaimState::Spent => {
                        assert!(
                            !completed,
                            "completing a superseded or spent claim must fail"
                        );
                    }
                }
            }
            Op::Release { claim } => {
                if claims.is_empty() {
                    continue;
                }
                let index = claim as usize % claims.len();
                let held_state = claims[index].state;
                let claimed = claims[index].claim.clone();
                let released = manager.release_reconnection_claim(&claimed).await;
                match held_state {
                    HeldClaimState::Active => {
                        assert!(released, "releasing the active claim must succeed");
                        claims[index].state = HeldClaimState::Spent;
                        if let Some(pending) =
                            reference.pending.get_mut(&claimed.disconnected.player_id)
                        {
                            pending.claim_active = false;
                        }
                    }
                    HeldClaimState::Superseded | HeldClaimState::Spent => {
                        assert!(!released, "releasing a superseded or spent claim must fail");
                    }
                }
            }
            Op::CompleteDirect { player } => {
                let player = player_id(player);
                manager.complete_reconnection(&player).await;
                if let Some(removed) = reference.pending.remove(&player) {
                    remove_orphaned_buffer(&mut reference, removed.room);
                    // Any claim held against the removed record is now dead.
                    for held in &mut claims {
                        if held.state == HeldClaimState::Active
                            && held.claim.disconnected.player_id == player
                        {
                            held.state = HeldClaimState::Superseded;
                        }
                    }
                }
            }
            Op::RecordEvent { room } => {
                let room = room_id(room);
                manager.record_room_event(&room, &control_event()).await;
                if reference.replay_buffers.contains_key(&room) {
                    reference.next_sequence = reference.next_sequence.saturating_add(1);
                    let buffer = reference
                        .replay_buffers
                        .get_mut(&room)
                        .expect("known replay buffer disappeared from reference");
                    buffer.sequences.push_back(reference.next_sequence);
                    while buffer.sequences.len() > EVENT_BUFFER_SIZE {
                        let evicted = buffer
                            .sequences
                            .pop_front()
                            .expect("oversized replay ring must contain an event");
                        buffer.evicted_watermark = Some(
                            buffer
                                .evicted_watermark
                                .map_or(evicted, |watermark| watermark.max(evicted)),
                        );
                    }
                }
            }
            Op::GetMissed {
                room,
                last_sequence,
            } => {
                let room = room_id(room);
                let missed = manager
                    .get_missed_events(&room, u64::from(last_sequence))
                    .await;
                let (expected_len, expected_truncated) = reference
                    .replay_buffers
                    .get(&room)
                    .map_or((0, false), |buffer| {
                        (
                            buffer
                                .sequences
                                .iter()
                                .filter(|sequence| **sequence > u64::from(last_sequence))
                                .count(),
                            buffer
                                .evicted_watermark
                                .is_some_and(|watermark| watermark > u64::from(last_sequence)),
                        )
                    });
                assert_eq!(
                    missed.events.len(),
                    expected_len,
                    "replay returned the wrong number of retained events"
                );
                assert_eq!(
                    missed.truncated, expected_truncated,
                    "replay reported the wrong bounded-ring truncation state"
                );
            }
            Op::CleanupExpired => {
                let unclaimed_before = reference
                    .pending
                    .values()
                    .filter(|pending| !pending.claim_active)
                    .count();
                let removed_count = manager.cleanup_expired().await;
                match window_seconds {
                    0 => assert_eq!(
                        removed_count, unclaimed_before,
                        "zero-window cleanup must remove every unclaimed record"
                    ),
                    3600 => assert_eq!(
                        removed_count, 0,
                        "one-hour cleanup must not remove a fresh fuzz record"
                    ),
                    _ => {}
                }
                // Claimed records are exempt from expiry cleanup.
                for (player, pending) in &reference.pending {
                    if pending.claim_active {
                        assert!(
                            manager.has_pending_reconnection(player).await,
                            "cleanup_expired must never remove an actively claimed record"
                        );
                    }
                }

                // Expiry is wall-clock dependent, so let the manager tell the
                // reference which unclaimed records it actually removed. A
                // stale reference entry would make a later registration look
                // like a same-room duplicate even though production correctly
                // creates a fresh record and token.
                let tracked_players: Vec<_> = reference.pending.keys().copied().collect();
                for player in tracked_players {
                    if !manager.has_pending_reconnection(&player).await {
                        let removed = reference
                            .pending
                            .remove(&player)
                            .expect("tracked player disappeared from the reference");
                        assert!(
                            !removed.claim_active,
                            "cleanup_expired removed an actively claimed reference record"
                        );
                    }
                }
                reference.replay_buffers.retain(|room, _| {
                    reference
                        .pending
                        .values()
                        .any(|pending| pending.room == *room)
                });
            }
            Op::ClearRoomBuffer { room } => {
                let room = room_id(room);
                manager.clear_room_buffer(&room).await;
            }
        }

        // Check both sides of the buffer gate after every transition. Exact
        // replay-ring state is modeled for pending rooms; empty rooms must
        // have neither retained events nor a stale truncation watermark.
        for room_index in 0..ROOMS {
            let room = room_id(room_index as u8);
            let missed = manager.get_missed_events(&room, 0).await;
            let (expected_len, expected_truncated) = reference
                .replay_buffers
                .get(&room)
                .map_or((0, false), |buffer| {
                    (
                        buffer.sequences.len(),
                        buffer.evicted_watermark.is_some_and(|watermark| watermark > 0),
                    )
                });
            assert_eq!(
                missed.events.len(),
                expected_len,
                "pending-room replay did not retain the modeled event ring"
            );
            assert_eq!(
                missed.truncated, expected_truncated,
                "pending-room replay did not preserve the modeled eviction watermark"
            );
        }
    }
}

fuzz_target!(|plan: Plan| {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime construction must not fail");
    runtime.block_on(run(plan));
});
