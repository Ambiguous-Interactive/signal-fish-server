//! Deterministic send schedules.
//!
//! The schedule is a pure function of `(seed, RunConfig)`: identical inputs
//! produce byte-identical intended-send timelines on every host, which is what
//! makes a manifest-replayable run and paired A/B comparisons possible. Every
//! sender starts inside the warm-up window (with seed-derived jitter so
//! senders do not emit in lockstep) and then walks its period through the
//! measured window.

use crate::config::{micros, RunConfig};

/// Which window a scheduled send lands in. Warm-up sends are
/// completeness-checked by the oracle but excluded from the latency
/// histogram, so measurement numbers are never polluted by ramp traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Phase {
    Warmup,
    Measured,
}

/// One scheduled send of one sender: the sequence number and the intended
/// send time in microseconds relative to the run epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScheduledSend {
    pub seq: u64,
    pub intended_us: u64,
    pub phase: Phase,
}

/// The full send plan for one sender.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SenderPlan {
    /// Peer name (`r{room}p{player}`) — the ledger sender key.
    pub name: String,
    pub room: u32,
    pub player: u32,
    pub sends: Vec<ScheduledSend>,
}

impl SenderPlan {
    /// The last intended send, in microseconds (0 when a sender has no
    /// scheduled sends).
    pub fn last_intended_us(&self) -> u64 {
        self.sends.last().map_or(0, |send| send.intended_us)
    }
}

/// splitmix64: a tiny, dependency-free, platform-stable PRNG. Only the
/// schedule consumes it, and only through `next_u64`, so the stream is
/// fixed forever by this file.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Per-sender jitter bound: at most one tenth of the send period, so jitter
/// never reorders a sender's own schedule.
fn jitter_bound_micros(period_micros: u64) -> u64 {
    (period_micros / 10).max(1)
}

/// Build every sender's schedule.
///
/// Warm-up sends are spread across the warm-up window; measured sends start
/// exactly at the window boundary plus one period (plus jitter) and then
/// advance by the period. Jitter is derived per sender from the seed, so the
/// stream each sender sees is stable across replays and hosts.
pub fn build_plans(config: &RunConfig) -> Vec<SenderPlan> {
    let period = config.period_micros();
    let warmup_sends = config.warmup_sends_per_sender();
    let measured_sends = config.measured_sends_per_sender();
    let warmup_micros = micros(config.warmup);
    let jitter_bound = jitter_bound_micros(period);
    let capacity =
        usize::try_from(u64::from(config.rooms) * u64::from(config.players_per_room)).unwrap_or(0);

    let mut plans = Vec::with_capacity(capacity);
    for room in 0..config.rooms {
        for player in 0..config.players_per_room {
            let name = RunConfig::peer_name(room, player);
            // Seed the per-sender stream from the name so renaming a sender
            // (a workload change) re-derives jitter rather than silently
            // reusing another sender's stream.
            let mut state = config
                .seed
                .wrapping_mul(0x100_0000_01B2)
                .wrapping_add(name.len() as u64)
                ^ name
                    .bytes()
                    .enumerate()
                    .map(|(index, byte)| (byte as u64).wrapping_mul(1 + index as u64))
                    .fold(config.seed, u64::wrapping_add);
            let mut sends =
                Vec::with_capacity(usize::try_from(warmup_sends + measured_sends).unwrap_or(0));
            for index in 0..warmup_sends {
                let slot = warmup_micros / (warmup_sends + 1);
                let intended = slot * (index + 1) + splitmix64(&mut state) % jitter_bound;
                sends.push(ScheduledSend {
                    seq: index,
                    intended_us: intended,
                    phase: Phase::Warmup,
                });
            }
            for index in 0..measured_sends {
                let intended =
                    warmup_micros + period * (index + 1) + splitmix64(&mut state) % jitter_bound;
                sends.push(ScheduledSend {
                    seq: warmup_sends + index,
                    intended_us: intended,
                    phase: Phase::Measured,
                });
            }
            plans.push(SenderPlan {
                name,
                room,
                player,
                sends,
            });
        }
    }
    plans
}
