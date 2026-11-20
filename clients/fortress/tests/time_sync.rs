//! Match the released browser fixture at the Fortress time-sync boundary.

#[allow(dead_code)]
#[path = "../src/workload.rs"]
mod workload;

use fortress_rollback::{
    DesyncDetection, FortressEvent, Message, NonBlockingSocket, P2PSession, ProtocolConfig,
    SessionBuilder, SessionState,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;
use workload::{apply_requests, input_for_frame, GameConfig, GameState};

struct Link {
    callback: Arc<AtomicU64>,
    delays: [u64; 2],
    pending: [VecDeque<(u64, Message)>; 2],
    sent: [usize; 2],
    delivered: [usize; 2],
}

struct Socket {
    side: usize,
    link: Rc<RefCell<Link>>,
}

fn peer_id(side: usize) -> Uuid {
    Uuid::from_u128(side as u128 + 1)
}

impl NonBlockingSocket<Uuid> for Socket {
    fn send_to(&mut self, message: &Message, destination: &Uuid) {
        let mut link = self.link.borrow_mut();
        let recipient = 1 - self.side;
        assert_eq!(*destination, peer_id(recipient));

        let callback = link.callback.load(Ordering::Relaxed);
        // Keep startup identical and vary delivery only after synchronization.
        let delay = if callback >= 60 {
            link.delays[self.side]
        } else {
            1
        };
        link.sent[self.side] += 1;
        link.pending[recipient].push_back((callback + delay, message.clone()));
    }

    fn receive_all_messages(&mut self) -> Vec<(Uuid, Message)> {
        let mut link = self.link.borrow_mut();
        let mut ready = Vec::new();
        let callback = link.callback.load(Ordering::Relaxed);
        while link.pending[self.side]
            .front()
            .is_some_and(|(due, _)| *due <= callback)
        {
            let (_, message) = link.pending[self.side].pop_front().unwrap();
            link.delivered[self.side] += 1;
            ready.push((peer_id(1 - self.side), message));
        }
        ready
    }
}

fn session(
    side: usize,
    link: Rc<RefCell<Link>>,
    protocol: ProtocolConfig,
) -> P2PSession<GameConfig> {
    SessionBuilder::<GameConfig>::new()
        .with_num_players(2)
        .unwrap()
        .with_fps(workload::NOMINAL_FPS)
        .unwrap()
        .with_protocol_config(protocol)
        .with_desync_detection_mode(DesyncDetection::On { interval: 60 })
        .add_local_player(side)
        .unwrap()
        .add_remote_player(1 - side, peer_id(1 - side))
        .unwrap()
        .start_p2p_session(Socket { side, link })
        .unwrap()
}

fn time_sync_case(delays: [u64; 2], paused_peer: Option<usize>, expected_waits: [u64; 2]) {
    let callback = Arc::new(AtomicU64::new(0));
    let link = Rc::new(RefCell::new(Link {
        callback: Arc::clone(&callback),
        delays,
        pending: Default::default(),
        sent: [0; 2],
        delivered: [0; 2],
    }));
    let start = Instant::now();
    let clock_callback = Arc::clone(&callback);
    let protocol = ProtocolConfig {
        clock: Some(Arc::new(move || {
            start + Duration::from_micros(clock_callback.load(Ordering::Relaxed) * 16_667)
        })),
        protocol_rng_seed: Some(12),
        ..ProtocolConfig::default()
    };
    let mut peers = [
        session(0, Rc::clone(&link), protocol.clone()),
        session(1, Rc::clone(&link), protocol),
    ];
    let mut states = [GameState::default(), GameState::default()];
    let mut skips = [0; 2];
    let mut waits = [0; 2];
    let mut max_lag = [0; 2];
    let target = workload::TARGET_CONFIRMED_FRAMES + 10;

    for tick in 1..720 {
        callback.store(tick, Ordering::Relaxed);
        for (side, peer) in peers.iter_mut().enumerate() {
            peer.poll_remote_clients();
            for event in peer.events() {
                match event {
                    FortressEvent::WaitRecommendation { skip_frames } => {
                        assert_eq!(skip_frames, 3, "delay {delays:?}, peer {side}");
                        assert_eq!(skips[side], 0, "peer overlaps wait recommendations");
                        waits[side] += 1;
                        skips[side] = skip_frames;
                    }
                    FortressEvent::DesyncDetected { .. } | FortressEvent::Disconnected { .. } => {
                        panic!("unexpected disconnect or desync, delay {delays:?}, peer {side}");
                    }
                    _ => {}
                }
            }
            // Keep receiving while one peer misses four simulation advances.
            if peer.current_state() != SessionState::Running
                || (paused_peer == Some(side) && (60..64).contains(&tick))
            {
                continue;
            }
            if skips[side] > 0 {
                skips[side] -= 1;
                continue;
            }
            let current_frame = peer.current_frame().as_i32();
            if current_frame < target {
                let handle = peer.local_player_handles()[0];
                peer.add_local_input(handle, input_for_frame(current_frame, side))
                    .unwrap();
                apply_requests(&mut states[side], peer.advance_frame().unwrap());
                let lag = peer.current_frame().as_i32() - peer.confirmed_frame().as_i32();
                max_lag[side] = max_lag[side].max(lag);
            }
        }
    }

    assert_eq!(
        waits, expected_waits,
        "one-way callback delays {delays:?}, paused peer {paused_peer:?}"
    );
    assert_eq!(skips, [0, 0], "the peers applied all requested pauses");
    for (side, peer) in peers.iter().enumerate() {
        assert!(peer.confirmed_frame().as_i32() >= workload::TARGET_CONFIRMED_FRAMES);
        let metrics = peer.metrics();
        assert_eq!(metrics.wait_recommendations, waits[side]);
        assert!(metrics.checksums_compared >= workload::MIN_CHECKSUM_SAMPLES);
        assert_eq!(metrics.checksums_matched, metrics.checksums_compared);
        assert_eq!(metrics.checksums_mismatched, 0);
        assert!((max_lag[side] as u64) <= workload::MAX_CONFIRMATION_LAG);
    }
    assert_eq!(states[0].frame, target);
    assert_eq!(states[1].frame, target);

    let link = link.borrow();
    assert!(link.pending.iter().all(VecDeque::is_empty));
    assert_eq!(link.sent, [link.delivered[1], link.delivered[0]]);
    assert!(link.sent.iter().all(|sent| *sent >= 1_200));
}

#[test]
fn matched_rtt_separates_symmetric_delivery_from_legitimate_waits() {
    time_sync_case([4, 4], None, [0, 0]);
    time_sync_case([7, 1], None, [0, 1]);
    time_sync_case([1, 7], None, [1, 0]);
}

#[test]
fn simulation_pause_can_trigger_waits_with_symmetric_delivery() {
    time_sync_case([1, 1], None, [0, 0]);
    time_sync_case([1, 1], Some(0), [0, 1]);
    time_sync_case([1, 1], Some(1), [1, 0]);
}
