//! Shared protocol v3 multi-peer conformance helpers.
//!
//! The cross-suite seam for the N >= 3 signaling conformance suites
//! (`tests/v3_multipeer_e2e.rs`, `tests/v3_multiprocess_e2e.rs`), included
//! per test binary via `mod v3_conformance_helpers;` — the same pattern as
//! `tests/websocket_test_helpers/mod.rs`. Any binary declaring this module
//! must also declare `mod websocket_test_helpers;`, which provides the shared
//! [`WsStream`] type and the deadline-driven readers these helpers build on.
//!
//! Each integration test binary compiles this module independently, so items
//! one suite does not use would trip `dead_code`; the module-level allow below
//! mirrors `websocket_test_helpers`.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures_util::SinkExt;
use serde_json::json;
use signal_fish_server::protocol::{ClientMessage, PlayerId, ServerMessage, SessionPlanPayload};
use tokio_tungstenite::tungstenite::Message;

use super::websocket_test_helpers::{next_matching_server_message_within, WsStream};

/// Generous per-message ceiling: a CI scheduling budget, not an expected wait.
pub const SERVER_MESSAGE_TIMEOUT: Duration = Duration::from_secs(20);

pub type SessionPlan = Box<SessionPlanPayload>;

/// Serialize `msg` and send it as one WebSocket Text frame.
pub async fn send(ws: &mut WsStream, msg: &ClientMessage) {
    let json = serde_json::to_string(msg).unwrap();
    ws.send(Message::Text(json.into())).await.unwrap();
}

pub async fn ready(ws: &mut WsStream) {
    send(ws, &ClientMessage::PlayerReady).await;
}

/// Explicitly start the game. `max_players` is a ceiling, not a required count,
/// and readiness alone never finalizes — the room starts only on an explicit
/// `StartGame` sent by the authority (or, when no authority is set, any member).
/// The conformance rooms are created with `supports_authority: false`, so any
/// member may start; sending from the creator is always valid (the creator is
/// the authority when authority is enabled, and an ordinary member otherwise).
pub async fn start_game(ws: &mut WsStream) {
    send(ws, &ClientMessage::StartGame).await;
}

/// Drain messages until a `LobbyStateChanged` reports exactly `count` ready
/// players (paces the ready handshake; mirrors `v3_session_plan_e2e.rs`).
pub async fn await_ready_count(ws: &mut WsStream, count: usize) {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "lobby ready count update",
        |message| match message {
            ServerMessage::LobbyStateChanged { ready_players, .. }
                if ready_players.len() == count =>
            {
                Some(())
            }
            _ => None,
        },
    )
    .await;
}

/// Skip to `GameStarting`, then return the `SessionPlan` that must immediately
/// follow it (panics on any other interleaved `ServerMessage`).
pub async fn expect_finalize_plan(ws: &mut WsStream, who: &str) -> SessionPlan {
    next_matching_server_message_within(
        ws,
        SERVER_MESSAGE_TIMEOUT,
        "finalization GameStarting",
        |message| match message {
            ServerMessage::GameStarting { .. } => Some(()),
            ServerMessage::SessionPlan(_) => {
                panic!("{who}: SessionPlan must not arrive before GameStarting")
            }
            _ => None,
        },
    )
    .await;
    expect_session_plan_strict(ws, who).await
}

/// Assert the exact next message is a `SessionPlan` and return it.
pub async fn expect_session_plan_strict(ws: &mut WsStream, who: &str) -> SessionPlan {
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "SessionPlan", |message| {
        match message {
            ServerMessage::SessionPlan(plan) => Some(plan),
            other => panic!("{who} expected a SessionPlan, got {other:?}"),
        }
    })
    .await
}

/// Assert the exact next message is a relayed `Signal` and return its parts.
pub async fn expect_signal(ws: &mut WsStream, who: &str) -> (PlayerId, serde_json::Value) {
    next_matching_server_message_within(ws, SERVER_MESSAGE_TIMEOUT, "relayed Signal", |message| {
        match message {
            ServerMessage::Signal { from, signal, .. } => Some((from, signal)),
            other => panic!("{who} expected a relayed Signal, got {other:?}"),
        }
    })
    .await
}

/// Borrow two distinct sockets out of one slice simultaneously.
pub fn two_mut(
    sockets: &mut [WsStream],
    first: usize,
    second: usize,
) -> (&mut WsStream, &mut WsStream) {
    assert_ne!(first, second, "two_mut requires distinct indices");
    if first < second {
        let (left, right) = sockets.split_at_mut(second);
        (&mut left[first], &mut right[0])
    } else {
        let (left, right) = sockets.split_at_mut(first);
        (&mut right[0], &mut left[second])
    }
}

/// Every ordered `(from, to)` index pair below `count` (`from != to`).
pub fn ordered_pairs(count: usize) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for from in 0..count {
        for to in 0..count {
            if from != to {
                pairs.push((from, to));
            }
        }
    }
    pairs
}

/// Relay one distinct opaque signal `ids[from_idx] -> ids[to_idx]` and assert
/// byte-identical delivery with the correct `from`.
pub async fn relay_one_signal(
    sockets: &mut [WsStream],
    ids: &[PlayerId],
    from_idx: usize,
    to_idx: usize,
) {
    let payload = json!({
        "IceCandidate": format!("candidate:conformance {}->{}", ids[from_idx], ids[to_idx])
    });
    let (from_ws, to_ws) = two_mut(sockets, from_idx, to_idx);
    send(
        from_ws,
        &ClientMessage::Signal {
            to: ids[to_idx],
            generation: uuid::Uuid::nil(),
            signal: payload.clone(),
        },
    )
    .await;
    let (relayed_from, relayed_signal) = expect_signal(to_ws, "signal recipient").await;
    assert_eq!(
        relayed_from, ids[from_idx],
        "the relayed Signal must carry the sender's id"
    );
    assert_eq!(
        relayed_signal, payload,
        "the opaque signal payload must be relayed byte-identically"
    );
}

/// Assert the GLOBAL mesh glare matrix over per-recipient plans: each plan
/// lists exactly the other members, and for every unordered pair `{a, b}`
/// exactly one side has `initiate: true` — the side with the smaller UUID
/// (the documented Appendix E rule).
///
/// Mesh `SessionPeer.is_authority` mirrors the stored member flag, which in
/// turn mirrors the room's `authority_player`. The conformance suites create
/// their rooms with `supports_authority: false`, so `authority_player` is
/// `None` and the flag is `false` for every peer — creator included. The
/// matrix pins that surface too.
pub fn assert_full_mesh_glare_matrix(plans: &[(PlayerId, &SessionPlanPayload)]) {
    let ids: BTreeSet<PlayerId> = plans.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids.len(), plans.len(), "duplicate recipient ids in plans");

    let mut matrix: BTreeMap<(PlayerId, PlayerId), bool> = BTreeMap::new();
    for (recipient, plan) in plans {
        let expected_peers: BTreeSet<PlayerId> =
            ids.iter().copied().filter(|id| id != recipient).collect();
        let actual_peers: BTreeSet<PlayerId> =
            plan.peers.iter().map(|peer| peer.player_id).collect();
        assert_eq!(
            actual_peers, expected_peers,
            "plan for {recipient} must list exactly the other members"
        );
        assert_eq!(
            plan.peers.len(),
            expected_peers.len(),
            "plan for {recipient} must not repeat peers"
        );
        for peer in &plan.peers {
            assert!(
                !peer.is_authority,
                "mesh is_authority mirrors the room's authority_player, which \
                 is None in these authority-less rooms — false for everyone, \
                 creator included; plan for {recipient} lists {peer:?}"
            );
            matrix.insert((*recipient, peer.player_id), peer.initiate);
        }
    }

    let initiate_of = |local: PlayerId, remote: PlayerId| -> bool {
        matrix
            .get(&(local, remote))
            .copied()
            .unwrap_or_else(|| panic!("missing matrix entry {local} -> {remote}"))
    };
    for &smaller in &ids {
        for &larger in &ids {
            if smaller >= larger {
                continue;
            }
            assert!(
                initiate_of(smaller, larger),
                "{smaller} (smaller UUID) must offer to {larger}"
            );
            assert!(
                !initiate_of(larger, smaller),
                "{larger} (larger UUID) must answer {smaller}, not offer"
            );
        }
    }
}
