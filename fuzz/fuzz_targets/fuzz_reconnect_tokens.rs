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
//! - completing/releasing a RELEASED or SUPERSEDED claim fails; completing an
//!   active claim removes the pending record;
//! - `cleanup_expired` never removes a record with an active claim;
//! - the buffer-existence gate: a room the reference knows has NO pending
//!   players replays nothing — `get_missed_events` returns empty/untruncated
//!   and recorded events are no-ops (pending-set shrinkage by expiry only
//!   tightens this, so it is time-robust).
//!
//! Each input runs on a fresh current-thread tokio runtime (the manager is
//! async lock-based; no background tasks are involved).
//!
//! Run via the nightly `fuzz` CI job, never on stable.
use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use signal_fish_server::metrics::ServerMetrics;
use signal_fish_server::protocol::ServerMessage;
use signal_fish_server::reconnection::{
    ClaimedReconnection, ReconnectionError, ReconnectionManager,
};
use std::collections::HashMap;
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
}

#[derive(Debug, Arbitrary)]
struct Plan {
    /// Selects the reconnection window: 0 (everything expires instantly),
    /// 1s, or 1h (nothing expires within an input).
    window_choice: u8,
    ops: Vec<Op>,
}

/// Reference bookkeeping for the time-robust invariants.
#[derive(Default)]
struct Reference {
    /// player -> (room, real token, has active claim)
    pending: HashMap<Uuid, (Uuid, String, bool)>,
}

/// A claim this input collected, plus whether we already spent it
/// (completed or released) — a spent claim must never complete again.
struct HeldClaim {
    claim: ClaimedReconnection,
    spent: bool,
}

fn player_id(index: u8) -> Uuid {
    Uuid::from_u128(0xF0BB_0000 + (index as usize % PLAYERS) as u128 + 1)
}

fn room_id(index: u8) -> Uuid {
    Uuid::from_u128(0xF0CC_0000 + (index as usize % ROOMS) as u128 + 1)
}

fn control_event() -> ServerMessage {
    ServerMessage::PlayerLeft {
        player_id: Uuid::from_u128(0xF0DD_0001),
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
            Op::Register {
                player,
                room,
                was_authority,
            } => {
                let player = player_id(player);
                let room = room_id(room);
                let token = manager
                    .register_disconnection(player, room, was_authority, None)
                    .await;
                // Re-registration replaces the record (and any active claim).
                reference.pending.insert(player, (room, token, false));
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
                        .map(|(_, token, _)| token.clone())
                        .unwrap_or_default(),
                    TokenChoice::Arbitrary(raw) => raw.clone(),
                };
                let result = manager
                    .validate_reconnection(&player, &room, &token_string)
                    .await;
                if result.is_ok() {
                    let (real_room, real_token, _) = reference
                        .pending
                        .get(&player)
                        .expect("a successful validation implies a pending record");
                    assert_eq!(
                        &token_string, real_token,
                        "validation accepted a token that is not the stored one"
                    );
                    assert_eq!(
                        *real_room, room,
                        "validation accepted the wrong room binding"
                    );
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
                        .map(|(_, token, _)| token.clone())
                        .unwrap_or_default(),
                    TokenChoice::Arbitrary(raw) => raw.clone(),
                };
                let result = manager
                    .claim_reconnection(&claimer, &player, &room, &token_string)
                    .await;
                match result {
                    Ok(claimed) => {
                        let (real_room, real_token, claim_active) = reference
                            .pending
                            .get_mut(&player)
                            .expect("a successful claim implies a pending record");
                        assert!(
                            !*claim_active,
                            "DOUBLE CLAIM: a second claim succeeded while one was active"
                        );
                        assert_eq!(
                            &token_string, real_token,
                            "claim accepted a token that is not the stored one"
                        );
                        assert_eq!(*real_room, room, "claim accepted the wrong room binding");
                        *claim_active = true;
                        claims.push(HeldClaim {
                            claim: claimed,
                            spent: false,
                        });
                    }
                    Err(error) => {
                        // Time-robust half: while the reference knows a claim
                        // is active, the manager must refuse (checked before
                        // expiry in claim_reconnection).
                        if reference
                            .pending
                            .get(&player)
                            .is_some_and(|(_, _, active)| *active)
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
                let held_spent = claims[index].spent;
                let claimed = claims[index].claim.clone();
                let completed = manager.complete_claimed_reconnection(&claimed).await;
                if held_spent {
                    assert!(
                        !completed,
                        "completing an already spent (released/completed) claim must fail"
                    );
                } else if completed {
                    claims[index].spent = true;
                    reference.pending.remove(&claimed.disconnected.player_id);
                } else {
                    // The record was replaced (re-registration) or superseded;
                    // the claim is dead either way.
                    claims[index].spent = true;
                }
            }
            Op::Release { claim } => {
                if claims.is_empty() {
                    continue;
                }
                let index = claim as usize % claims.len();
                let held_spent = claims[index].spent;
                let claimed = claims[index].claim.clone();
                let released = manager.release_reconnection_claim(&claimed).await;
                if held_spent {
                    assert!(
                        !released,
                        "releasing an already spent (released/completed) claim must fail"
                    );
                } else {
                    claims[index].spent = true;
                    if released {
                        if let Some((_, _, active)) =
                            reference.pending.get_mut(&claimed.disconnected.player_id)
                        {
                            *active = false;
                        }
                    }
                }
            }
            Op::CompleteDirect { player } => {
                let player = player_id(player);
                manager.complete_reconnection(&player).await;
                if reference.pending.remove(&player).is_some() {
                    // Any claim held against the removed record is now dead.
                    for held in &mut claims {
                        if held.claim.disconnected.player_id == player {
                            held.spent = true;
                        }
                    }
                }
            }
            Op::RecordEvent { room } => {
                let room = room_id(room);
                manager.record_room_event(&room, &control_event()).await;
            }
            Op::GetMissed {
                room,
                last_sequence,
            } => {
                let room = room_id(room);
                let missed = manager
                    .get_missed_events(&room, u64::from(last_sequence))
                    .await;
                assert!(
                    missed.events.len() <= EVENT_BUFFER_SIZE,
                    "the replay can never return more events than the ring holds"
                );
            }
            Op::CleanupExpired => {
                manager.cleanup_expired().await;
                // Claimed records are exempt from expiry cleanup.
                for (player, (_, _, active)) in &reference.pending {
                    if *active {
                        assert!(
                            manager.has_pending_reconnection(player).await,
                            "cleanup_expired must never remove an actively claimed record"
                        );
                    }
                }
            }
            Op::ClearRoomBuffer { room } => {
                let room = room_id(room);
                manager.clear_room_buffer(&room).await;
            }
        }

        // Buffer-existence gate, time-robust direction: expiry only SHRINKS
        // the manager's pending set relative to the reference, so a room the
        // reference knows is empty must replay nothing (no buffer exists —
        // recording was a no-op and the replay is empty/untruncated).
        for room_index in 0..ROOMS {
            let room = room_id(room_index as u8);
            let reference_pending = reference
                .pending
                .values()
                .any(|(pending_room, _, _)| *pending_room == room);
            if !reference_pending {
                let missed = manager.get_missed_events(&room, 0).await;
                assert!(
                    missed.events.is_empty() && !missed.truncated,
                    "a room with no pending reconnection must replay nothing"
                );
            }
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
