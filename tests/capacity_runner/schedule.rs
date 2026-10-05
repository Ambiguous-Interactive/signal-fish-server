//! Deterministic send schedules.
//!
//! The schedule is a pure function of `(seed, RunConfig)`: identical inputs
//! produce byte-identical intended-send timelines on every host, which is what
//! makes a manifest-replayable run and paired A/B comparisons possible. Every
//! sender starts inside the warm-up window (with seed-derived jitter so
//! senders do not emit in lockstep) and then walks its period through the
//! measured window.

use crate::config::{micros, ChurnSchedule, RunConfig};
use std::collections::BTreeMap;
use std::time::Duration;

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

/// One churn cycle: `peers` disconnect at `disconnect_us`, and each peer
/// rejoins at its own staggered instant (`reconnects_us`, inside the cycle's
/// window). A rejoin bumps the peer's incarnation epoch, so the peer's relay
/// stream resumes under a fresh `(epoch, seq)` pair.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChurnCycle {
    pub peers: Vec<String>,
    pub disconnect_us: u64,
    /// Per-peer reconnect instant (the map's keys are exactly `peers`).
    pub reconnects_us: BTreeMap<String, u64>,
}

/// The full churn plan of one run: a pure function of `(seed, RunConfig)`
/// plus the unshifted send plans (the storm must complete inside their
/// span). Deterministic, so a manifest rebuilds it exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChurnPlan {
    pub cycles: Vec<ChurnCycle>,
}

impl ChurnPlan {
    /// Whether this peer is victimized by any cycle.
    pub fn victim_instants(&self, peer: &str) -> Vec<(u64, u64)> {
        self.cycles
            .iter()
            .filter(|cycle| cycle.peers.iter().any(|name| name == peer))
            .map(|cycle| {
                (
                    cycle.disconnect_us,
                    cycle
                        .reconnects_us
                        .get(peer)
                        .copied()
                        .unwrap_or(cycle.disconnect_us),
                )
            })
            .collect()
    }
}

/// Build the run shape: the unshifted send plans and the churn plan, with
/// every churn victim's post-disconnect sends shifted by its offline window
/// (a scheduled offline gap is workload shape, not generator lag — the
/// generator-lag bound must stay reserved for real generator faults).
///
/// The churn plan is validated against the unshifted span before any shift
/// is applied: the whole storm (disconnects and reconnects) must complete
/// inside the scheduled-send span, so quiescence always covers every rejoin.
pub fn build_run_shape(config: &RunConfig) -> Result<(Vec<SenderPlan>, ChurnPlan), String> {
    let plans = build_plans(config);
    let churn = build_churn(config, &plans)?;
    apply_churn_shifts(plans, &churn)
}

/// Deterministic churn plan for one run. `None` produces an empty plan; the
/// burst and room-replacement shapes pick their victims and reconnect
/// offsets from seed-derived splitmix64 streams (the same discipline as the
/// send schedule), so identical configs produce identical churn.
fn build_churn(config: &RunConfig, plans: &[SenderPlan]) -> Result<ChurnPlan, String> {
    let span_us = plans
        .iter()
        .map(SenderPlan::last_intended_us)
        .max()
        .unwrap_or(0);
    match config.churn {
        ChurnSchedule::None => Ok(ChurnPlan::default()),
        ChurnSchedule::ReconnectBurst {
            fraction_percent,
            start,
            window,
        } => build_burst_churn(config, plans, span_us, fraction_percent, start, window),
        ChurnSchedule::RoomReplacement {
            fraction_percent,
            start,
            window,
            interval,
        } => build_replacement_churn(
            config,
            plans,
            span_us,
            fraction_percent,
            start,
            window,
            interval,
        ),
    }
}

/// Validate the churn shape fields the two variants share.
fn validate_churn_fraction(fraction_percent: u32) -> Result<(), String> {
    if !(1..=100).contains(&fraction_percent) {
        return Err(format!(
            "churn fraction_percent must be in 1..=100, got {fraction_percent}"
        ));
    }
    Ok(())
}

/// Validate that a churn wave's stagger window fits a span boundary.
fn validate_window_positive(window: Duration) -> Result<(), String> {
    if window.is_zero() {
        return Err("churn window must be positive".to_string());
    }
    Ok(())
}

/// The reconnect-burst storm: one cycle, seed-chosen peer victims, staggered
/// rejoins inside `[start, start + window)`.
fn build_burst_churn(
    config: &RunConfig,
    plans: &[SenderPlan],
    span_us: u64,
    fraction_percent: u32,
    start: Duration,
    window: Duration,
) -> Result<ChurnPlan, String> {
    validate_churn_fraction(fraction_percent)?;
    validate_window_positive(window)?;
    let storm_end_us = start
        .checked_add(window)
        .map(micros)
        .ok_or_else(|| "churn start + window overflows the run clock".to_string())?;
    if storm_end_us > span_us {
        return Err(format!(
            "the churn storm (start {start:?} + window {window:?}) must complete inside the \
             scheduled-send span ({span_us} µs)"
        ));
    }
    let mut roster: Vec<String> = plans.iter().map(|plan| plan.name.clone()).collect();
    // Seeded Fisher-Yates over the roster; the first `count` names of the
    // shuffled order are the victims.
    let mut state = config.seed ^ 0xC0DE_B0FF;
    for index in (1..roster.len()).rev() {
        let index = u64::try_from(index).unwrap_or(0);
        let swap = usize::try_from(splitmix64(&mut state) % (index + 1)).unwrap_or(0);
        roster.swap(usize::try_from(index).unwrap_or(0), swap);
    }
    let count = victim_count(roster.len(), fraction_percent);
    let mut reconnects_us = BTreeMap::new();
    for victim in &roster[..count] {
        let offset = splitmix64(&mut state) % micros(window);
        reconnects_us.insert(victim.clone(), micros(start) + offset);
    }
    Ok(ChurnPlan {
        cycles: vec![ChurnCycle {
            peers: roster[..count].to_vec(),
            disconnect_us: micros(start),
            reconnects_us,
        }],
    })
}

/// Victim count for a fraction of a roster, rounded up but always inside the
/// roster (a 10% wave over three rooms still replaces one room).
fn victim_count(roster_len: usize, fraction_percent: u32) -> usize {
    roster_len
        .checked_mul(usize::try_from(fraction_percent).unwrap_or(0))
        .map(|product| product.div_ceil(100))
        .unwrap_or(0)
        .clamp(1, roster_len)
}

/// Seed salt for the room-replacement victim stream — distinct from the
/// burst's salt, so the two shapes' draws never share a sub-stream.
const REPLACEMENT_SALT: u64 = 0x5EED_C0DE_0000_0001;

/// The room-replacement schedule: on every wave `start + k * interval`
/// (while the wave fits the scheduled-send span), a seed-chosen fraction of
/// whole ROOMS cycles — every member disconnects at the wave instant and
/// rejoins, staggered inside the window, into the room's next generation
/// (a fresh room code; see `RunConfig::room_code_for_generation`). The
/// rejoin-code computation itself lives with the config; the plan only
/// carries instants, so the oracle sees the same cycle shape as a burst.
fn build_replacement_churn(
    config: &RunConfig,
    plans: &[SenderPlan],
    span_us: u64,
    fraction_percent: u32,
    start: Duration,
    window: Duration,
    interval: Duration,
) -> Result<ChurnPlan, String> {
    validate_churn_fraction(fraction_percent)?;
    validate_window_positive(window)?;
    if micros(window) >= micros(interval) {
        return Err(format!(
            "the replacement window ({window:?}) must stay below the wave interval ({interval:?}) \
             so a room is whole again before the next wave can select it"
        ));
    }
    let wave_start_us =
        |k: u64| -> Option<u64> { micros(start).checked_add(k.checked_mul(micros(interval))?) };
    let first_wave_end = wave_start_us(0)
        .and_then(|at| at.checked_add(micros(window)))
        .ok_or_else(|| "churn start + window overflows the run clock".to_string())?;
    if first_wave_end > span_us {
        return Err(format!(
            "the first replacement wave (start {start:?} + window {window:?}) must complete \
             inside the scheduled-send span ({span_us} µs)"
        ));
    }
    // Every wave fits the span: waves fire while start + k*interval + window
    // stays inside it, so quiescence always covers every rejoin.
    let mut wave_instants = Vec::new();
    let mut k = 0u64;
    while let Some(wave_us) = wave_start_us(k) {
        let Some(wave_end_us) = wave_us.checked_add(micros(window)) else {
            break;
        };
        if wave_end_us > span_us {
            break;
        }
        wave_instants.push(wave_us);
        k += 1;
    }
    let mut members_of_room: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for plan in plans {
        members_of_room
            .entry(plan.room)
            .or_default()
            .push(plan.name.clone());
    }
    let room_count = members_of_room.len();
    let count = victim_count(room_count, fraction_percent);
    let mut state = config.seed ^ REPLACEMENT_SALT;
    let mut cycles = Vec::with_capacity(wave_instants.len());
    let mut replacements_per_room: BTreeMap<u32, u32> = BTreeMap::new();
    for wave_us in wave_instants {
        // Seeded Fisher-Yates over the room indices; the first `count` rooms
        // of the shuffled order are the wave's victims (whole rooms cycle).
        let mut order: Vec<u32> = (0..config.rooms).collect();
        for index in (1..order.len()).rev() {
            let index = u64::try_from(index).unwrap_or(0);
            let swap = usize::try_from(splitmix64(&mut state) % (index + 1)).unwrap_or(0);
            order.swap(usize::try_from(index).unwrap_or(0), swap);
        }
        let mut peers = Vec::new();
        let mut reconnects_us = BTreeMap::new();
        for room in &order[..count] {
            *replacements_per_room.entry(*room).or_default() += 1;
            for member in &members_of_room[room] {
                let offset = splitmix64(&mut state) % micros(window);
                reconnects_us.insert(member.clone(), wave_us + offset);
                peers.push(member.clone());
            }
        }
        cycles.push(ChurnCycle {
            peers,
            disconnect_us: wave_us,
            reconnects_us,
        });
    }
    // A room's deterministic plan may not outgrow its code space: the code
    // space holds one generation per lowercase letter. The cap rides the
    // built plan's per-room victimization count, not the wave count, so a
    // wide, low-fraction campaign (the C3 churn shape) is never refused
    // for a bound no room reaches.
    if let Some((room, replacements)) = replacements_per_room
        .iter()
        .max_by_key(|(_, replacements)| **replacements)
    {
        if *replacements > RunConfig::MAX_ROOM_GENERATION {
            return Err(format!(
                "room {room} is replaced {replacements} times, but a room's code space holds \
                 at most {} generations; lengthen the wave interval or shorten the run",
                RunConfig::MAX_ROOM_GENERATION,
            ));
        }
    }
    Ok(ChurnPlan { cycles })
}

/// Shift every victim's post-disconnect sends by its offline duration. The
/// shift preserves inter-send spacing, so the offered workload (send count
/// and rate) is unchanged — only the timeline moves.
///
/// A send is shifted by a cycle's offline duration exactly when it was
/// originally due at or past that cycle's disconnect instant (the ORIGINAL
/// timeline, not the shifted one: cycle shifts compose, so a send due
/// between two waves must not inherit the second wave's shift just because
/// the first wave moved it). Sends due past every disconnect of their
/// peer's waves absorb the peer's total offline time, which is the shape
/// the C3 churn cells schedule around.
fn apply_churn_shifts(
    mut plans: Vec<SenderPlan>,
    churn: &ChurnPlan,
) -> Result<(Vec<SenderPlan>, ChurnPlan), String> {
    for plan in &mut plans {
        let instants = churn.victim_instants(&plan.name);
        if instants.is_empty() {
            continue;
        }
        for send in &mut plan.sends {
            let original_us = send.intended_us;
            let total_shift: u64 = instants
                .iter()
                .filter(|(disconnect_us, _)| original_us >= *disconnect_us)
                .map(|(disconnect_us, reconnect_us)| reconnect_us.saturating_sub(*disconnect_us))
                .sum();
            send.intended_us = original_us.saturating_add(total_shift);
        }
    }
    Ok((plans, churn.clone()))
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
