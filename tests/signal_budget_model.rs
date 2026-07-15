//! P10.C H8 (model, PR lane) — a 16-player WebRTC mesh's signaling cost fits the
//! default per-connection signal rate limit.
//!
//! Falsification experiment H8. Pre-registered prediction: at N=16 players in a
//! full mesh with K=10 trickle-ICE candidates per peer, each player consumes 166
//! control-plane slots during formation (165 `Signal` messages plus its one
//! `TransportStatus` report), well under the default `max_signals`
//! budget (600 per `time_window`), i.e. ≈3.6× headroom.
//!
//! This is a MODEL — arithmetic over the protocol's signaling rules — not a live
//! WebRTC run; the empirical companion (`webrtc_mesh_budget_e2e.rs`) is nightly.
//! Its job in the PR lane is a **config ↔ protocol invariant guard**: if the
//! default `max_signals` is ever lowered below what a 16-player mesh needs (a
//! plausible "tighten the rate limit for security" change), or the per-player
//! mesh-formation cost grows, this fails fast rather than silently rate-limiting
//! real P2P mesh formation at the target player count.

use signal_fish_server::config::defaults;
use signal_fish_server::rate_limit::RateLimitConfig;

/// `Signal` messages a single player SENDS to form a full mesh, per the
/// protocol's signaling rules (Appendix E glare rule + trickle ICE):
///
/// - exactly one offer-or-answer per peer — for each unordered pair, one side
///   offers and the other answers (the `local_initiates` glare rule), so every
///   player emits one SDP message toward each of its peers — plus
/// - `k` ICE candidates per peer (trickle),
///
/// across its `n - 1` peers, plus the one accepted `TransportStatus` state
/// change that shares the same rate-limit bucket ⇒ `(n - 1) * (1 + k) + 1`.
fn mesh_formation_control_messages_per_player(n: u32, k: u32) -> u32 {
    (n - 1) * (1 + k) + 1
}

/// Representative trickle-ICE candidate count per peer for the model (host +
/// server-reflexive + a relay candidate or two on a typical dual-stack client).
const TYPICAL_ICE_CANDIDATES_PER_PEER: u32 = 10;

#[test]
fn mesh_signal_budget_model_holds_and_default_config_accommodates_sixteen_players() {
    // Pin the model at representative points so any change to the formula (or to
    // the glare rule's one-SDP-per-peer assumption) is caught explicitly.
    // (n, k, expected rate-limited control messages per player)
    for (n, k, expected) in [
        (2, TYPICAL_ICE_CANDIDATES_PER_PEER, 12), // the two-player floor
        (8, TYPICAL_ICE_CANDIDATES_PER_PEER, 78),
        (16, TYPICAL_ICE_CANDIDATES_PER_PEER, 166), // the 16-player target
        (16, 0, 16),                                // SDP-only (no trickle)
        (16, 38, 586),                              // last in-budget integer K
        (16, 39, 601),                              // first over-budget integer K
    ] {
        assert_eq!(
            mesh_formation_control_messages_per_player(n, k),
            expected,
            "mesh signal model mismatch at N={n}, K={k}"
        );
    }

    // The config ↔ protocol invariant: the default per-connection signal budget
    // must accommodate a 16-player mesh (K=10) with real headroom, or mesh
    // formation would rate-limit itself at the target player count.
    //
    // Read the DEPLOYMENT-authoritative default: `defaults::default_max_signals`
    // is the `#[serde(default = ...)]` for `config::server::RateLimitConfig`
    // (`src/config/server.rs`), so it is the value an operator's "tighten the
    // rate limit" change would touch — the exact regression this guard exists to
    // catch. (The sibling admission suite reads `defaults::*` for the same
    // reason.) Cross-check that the in-process `rate_limit::RateLimitConfig`
    // default agrees, so the two parallel `600` literals cannot silently drift.
    let budget = defaults::default_max_signals();
    assert_eq!(
        RateLimitConfig::default().max_signals,
        budget,
        "the in-process and config-layer default max_signals must stay in sync"
    );
    let per_player =
        mesh_formation_control_messages_per_player(16, TYPICAL_ICE_CANDIDATES_PER_PEER);

    assert!(
        per_player < budget,
        "16-player mesh formation ({per_player} signals/player) must fit the default \
         max_signals budget ({budget}); lowering the default below this rate-limits P2P mesh"
    );
    assert!(
        per_player * 2 <= budget,
        "the default signal budget ({budget}) should keep >=2x headroom over a 16-player \
         mesh ({per_player}/player); got {:.1}x",
        f64::from(budget) / f64::from(per_player)
    );

    // Documented failure edge (comment, not a brittle assertion coupled to the
    // exact default): a 16-player mesh only exceeds the budget once trickle-ICE
    // candidates reach K≈39 — 15*(1+K)+1 > 600 ⇒ K ≥ 39 at today's default 600.
    // The headroom assertion above is the robust guard; this notes where it runs out.
}
