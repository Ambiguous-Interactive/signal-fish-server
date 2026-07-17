//! Deterministic Fortress workload shared by the native and Godot/WASM gates.

use fortress_rollback::{Config, FortressRequest, InputVec};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const TARGET_CONFIRMED_FRAMES: i32 = 600;
pub const NOMINAL_FPS: usize = 60;
pub const MIN_CHECKSUM_SAMPLES: u64 = 8;
pub const MIN_COMPLETED_MESSAGES_PER_SECOND: f64 = 120.0;
pub const MAX_PIPELINE_QUEUE_DEPTH: usize = 64;
pub const MAX_OLDEST_QUEUE_AGE_US: u64 = 500_000;
pub const MAX_CONFIRMATION_LAG: u64 = 8;
pub const MAX_ROLLBACK_DEPTH: u32 = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub buttons: u32,
}

#[derive(Debug, Clone, Default)]
pub struct GameState {
    pub frame: i32,
    pub checksum: u64,
}

pub struct GameConfig;

impl Config for GameConfig {
    type Input = Input;
    type State = GameState;
    type Address = Uuid;
}

pub fn input_for_frame(frame: i32, player_handle: usize) -> Input {
    Input {
        buttons: (frame as u32).wrapping_mul(31) ^ u32::try_from(player_handle).unwrap_or(u32::MAX),
    }
}

pub fn apply_requests(
    state: &mut GameState,
    requests: impl IntoIterator<Item = FortressRequest<GameConfig>>,
) {
    for request in requests {
        match request {
            FortressRequest::SaveGameState { cell, frame } => {
                cell.save(frame, Some(state.clone()), Some(u128::from(state.checksum)));
            }
            FortressRequest::LoadGameState { cell, .. } => {
                if let Some(saved) = cell.load() {
                    *state = saved;
                }
            }
            FortressRequest::AdvanceFrame { inputs } => advance_game(state, &inputs),
        }
    }
}

fn advance_game(state: &mut GameState, inputs: &InputVec<Input>) {
    state.frame = state.frame.saturating_add(1);
    let mut mixed = state.frame as u64;
    for (input, _status) in inputs.iter() {
        mixed = mixed
            .wrapping_mul(0x9E37_79B1_85EB_CA87)
            .wrapping_add(u64::from(input.buttons));
    }
    state.checksum = state.checksum.rotate_left(7) ^ mixed;
}
