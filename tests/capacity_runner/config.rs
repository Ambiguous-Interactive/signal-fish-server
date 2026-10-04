//! Runner inputs for the C2 capacity runner (issue #648).
//!
//! Every field is a registered contract input
//! (`docs/development/arm-capacity-audit.md`, "C2 first runner PR"): endpoint,
//! seed, room/player count, protocol/encoding, payload bytes, sender rate,
//! delivery class, warm-up, duration, churn/reconnect schedule, and output
//! directory. The config is intentionally the ONLY place a run is shaped: the
//! schedule is a pure function of the seed plus these fields, so a manifest
//! recorded with this config reproduces the exact workload.
//!
//! Slice boundary: [`DeliveryClass`] and [`ChurnSchedule`] currently accept
//! only their foundational variant. The input surface exists so later slices
//! (latest/volatile delivery classes, reconnect storms, room churn) extend the
//! enums instead of reshaping every call site.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

/// Wire protocol and encoding a run's clients speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Encoding {
    /// v2 clients: join directly over `/v2/ws`, JSON frames.
    V2Json,
    /// v3 clients: `Authenticate` + `ProtocolInfo` negotiation over `/v3/ws`,
    /// JSON frames.
    V3Json,
}

impl Encoding {
    /// The URL path of this encoding's WebSocket endpoint.
    pub fn ws_path(self) -> &'static str {
        match self {
            Encoding::V2Json => "/v2/ws",
            Encoding::V3Json => "/v3/ws",
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "v2-json" => Ok(Encoding::V2Json),
            "v3-json" => Ok(Encoding::V3Json),
            other => Err(format!(
                "unsupported encoding {other:?} (expected \"v2-json\" or \"v3-json\"; \
                 MessagePack cohorts are a later C2 slice)"
            )),
        }
    }
}

/// Delivery class under measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeliveryClass {
    /// Reliable, ordered, exactly-once fan-out: the primary capacity ceiling.
    Reliable,
}

impl DeliveryClass {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "reliable" => Ok(DeliveryClass::Reliable),
            other => Err(format!(
                "unsupported delivery class {other:?} (expected \"reliable\"; \
                 latest/volatile contract experiments are a later C2 slice)"
            )),
        }
    }
}

/// Churn / reconnect schedule a run layers over the steady workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChurnSchedule {
    /// No churn: every client joins once and stays connected.
    None,
}

impl ChurnSchedule {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "none" => Ok(ChurnSchedule::None),
            other => Err(format!(
                "unsupported churn schedule {other:?} (expected \"none\"; reconnect storms and \
                 room-replacement schedules are later C2 slices)"
            )),
        }
    }
}

/// A one-shot sender pause used by the scheduled-send-latency negative
/// control: after each sender has sent `after_seq` messages, it pauses for
/// `duration` before continuing its schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SendPause {
    pub after_seq: u64,
    #[serde(with = "duration_micros")]
    pub duration: Duration,
}

/// Complete input set for one run. Serialized into the run manifest, so a
/// replay can rebuild the exact schedule from the manifest alone.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RunConfig {
    /// Connect to an existing server (`ws://host:port`) instead of spawning
    /// the compiled binary. `None` spawns the binary on a fresh port.
    pub endpoint: Option<String>,
    /// Seed for the deterministic send schedule (jitter and phase placement).
    pub seed: u64,
    pub rooms: u32,
    pub players_per_room: u32,
    pub encoding: Encoding,
    /// Application payload bytes per relayed message (the ledger padding).
    pub payload_bytes: u32,
    /// Messages per second per sender.
    pub send_rate_per_sender: f64,
    pub delivery_class: DeliveryClass,
    /// Traffic sent before the measurement window. Warm-up deliveries are
    /// completeness-checked but excluded from the latency histogram.
    #[serde(with = "duration_micros")]
    pub warmup: Duration,
    /// Measured window length.
    #[serde(with = "duration_micros")]
    pub duration: Duration,
    pub churn: ChurnSchedule,
    /// Directory receiving the run artifacts (manifest, deliveries, intervals,
    /// summary, histogram).
    pub output_dir: PathBuf,
    /// Scheduled-send lag above which the generator (not the server) is
    /// declared saturated and the run is invalidated.
    #[serde(with = "duration_micros")]
    pub generator_lag_bound: Duration,
    /// Quiet wait after the last scheduled send before the oracle runs.
    #[serde(with = "duration_micros")]
    pub drain_grace: Duration,
    /// Interval between server resource samples (scrape + RSS).
    #[serde(with = "duration_micros")]
    pub sample_interval: Duration,
    /// Deep-merged over the shared harness base config for spawned servers.
    pub server_overlay: Value,
    /// Negative-control hook: pause senders once (scheduled-send latency).
    pub pause_sends: Option<SendPause>,
    /// Negative-control hook: stall every sender for this long from the first
    /// measured send, to trip the generator-lag bound deterministically.
    #[serde(with = "duration_micros_option")]
    pub stall_senders: Option<Duration>,
    /// Negative-control hook: recipient `r0p0` joins and then never reads
    /// again (slow-consumer eviction path).
    pub slow_reader: bool,
    /// Negative-control hook: SIGKILL the spawned server this long into the
    /// run (measured from the run epoch, warm-up included; must fall inside
    /// the scheduled sends, and the runner refuses otherwise).
    #[serde(with = "duration_micros_option")]
    pub kill_server_after: Option<Duration>,
    /// Run-scoped room-code prefix for this run so two concurrent runs
    /// against one shared external server start from distinct room
    /// namespaces (12 bits — a rare prefix collision degrades to loud join
    /// refusals, never silent cross-talk). Assigned by the runner from the
    /// run ID before any join; serialized for audit only.
    #[serde(default)]
    pub room_code_prefix: Option<String>,
}

/// `Duration` as whole microseconds (u64) for artifact-stable serialization.
pub mod duration_micros {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        u64::try_from(value.as_micros())
            .unwrap_or(u64::MAX)
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_micros(u64::deserialize(deserializer)?))
    }
}

/// `Option<Duration>` as whole microseconds (u64), `None` serialized as null.
pub mod duration_micros_option {
    use serde::{Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(
        value: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(duration) => super::duration_micros::serialize(duration, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        use serde::Deserialize as _;
        let raw = Option::<u64>::deserialize(deserializer)?;
        Ok(raw.map(Duration::from_micros))
    }
}

/// Whole microseconds of `duration`, saturating at `u64::MAX`. The explicit
/// narrowing helper for every artifact timestamp and schedule offset (the
/// crate forbids silent `as` casts that can truncate).
pub fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Explicit widening of a collection count (usize -> u64).
pub fn count_u64(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

/// Explicit narrowing of a bounded wire quantity (u32 -> usize).
pub fn count_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(0)
}

impl RunConfig {
    /// The runner's spawned-server posture: relay sessions, admission off,
    /// and the ceilings a capacity run must not trip. The connection caps
    /// stay coherent under the server's validation (a per-IP cap may not
    /// exceed the server-wide ceiling): both rise together so a many-client
    /// run — where every generator shares the server host's loopback address
    /// — never trips the per-IP budget. Room-creation budget rises for
    /// multi-room cells. Deep-merged over the shared harness base config by
    /// `spawn_server`.
    pub fn default_server_overlay() -> Value {
        serde_json::json!({
            "session": { "default_topology": "relay" },
            "rate_limit": { "max_room_creations": 1_000_000 },
            "security": {
                "max_connections": 100_000,
                "max_connections_per_ip": 100_000
            }
        })
    }

    /// Six-character room code for one run's room (the server pins
    /// `room_code_length` to 6). The prefix is run-scoped, so concurrent
    /// runs against one shared external server start from distinct room
    /// namespaces (a rare prefix collision degrades to loud join refusals,
    /// never silent cross-talk). Rooms are limited to 999 per run (three
    /// decimal digits).
    pub fn room_code(&self, room: u32) -> String {
        let prefix = self.room_code_prefix.as_deref().unwrap_or("FFF");
        format!("{prefix}{room:03}")
    }

    /// Globally unique peer name: room-scoped sender keys are what make a
    /// cross-room delivery observable as a misroute.
    pub fn peer_name(room: u32, player: u32) -> String {
        format!("r{room}p{player}")
    }

    /// Message period for one sender, in microseconds (rounded down to at
    /// least 1). The rate is a float input, but the conversion goes through
    /// std's checked `Duration::from_secs_f64`, so no float-to-integer cast
    /// exists in the schedule's critical path.
    pub fn period_micros(&self) -> u64 {
        micros(Duration::from_secs_f64(1.0 / self.send_rate_per_sender)).max(1)
    }

    /// Scheduled sends per sender inside the warm-up window (floor).
    pub fn warmup_sends_per_sender(&self) -> u64 {
        micros(self.warmup) / self.period_micros()
    }

    /// Scheduled sends per sender inside the measured window (ceil).
    pub fn measured_sends_per_sender(&self) -> u64 {
        micros(self.duration).div_ceil(self.period_micros())
    }

    /// Build the run config from `SIGNAL_FISH_CAPACITY_*` environment
    /// variables so the runner is standalone on a capacity host:
    ///
    /// `ENDPOINT` (optional), `SEED`, `ROOMS`, `PLAYERS`, `ENCODING`
    /// (`v2-json` | `v3-json`), `PAYLOAD_BYTES`, `RATE_PER_SENDER`, `CLASS`
    /// (`reliable`), `WARMUP_SECS`, `DURATION_SECS`, `CHURN` (`none`),
    /// `OUTPUT_DIR`, `LAG_BOUND_MS`, `SAMPLE_INTERVAL_MS`. Absent optional
    /// variables fall back to the small default scenario; required scalars
    /// fall back to the same defaults so a bare invocation just works. Each
    /// run needs a FRESH `OUTPUT_DIR` (a directory that already holds a run
    /// manifest is refused).
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| -> Result<Option<String>, String> {
            match std::env::var(format!("SIGNAL_FISH_CAPACITY_{name}")) {
                Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
                Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
                Err(error) => Err(format!("SIGNAL_FISH_CAPACITY_{name}: {error}")),
            }
        };
        let endpoint = var("ENDPOINT")?;
        let seed = var("SEED")?
            .map(|raw| raw.parse::<u64>())
            .transpose()
            .map_err(|error| format!("SIGNAL_FISH_CAPACITY_SEED: {error}"))?;
        let rooms = var("ROOMS")?
            .map(|raw| raw.parse::<u32>())
            .transpose()
            .map_err(|error| format!("SIGNAL_FISH_CAPACITY_ROOMS: {error}"))?;
        let players = var("PLAYERS")?
            .map(|raw| raw.parse::<u32>())
            .transpose()
            .map_err(|error| format!("SIGNAL_FISH_CAPACITY_PLAYERS: {error}"))?;
        let payload = var("PAYLOAD_BYTES")?
            .map(|raw| raw.parse::<u32>())
            .transpose()
            .map_err(|error| format!("SIGNAL_FISH_CAPACITY_PAYLOAD_BYTES: {error}"))?;
        let rate = var("RATE_PER_SENDER")?
            .map(|raw| raw.parse::<f64>())
            .transpose()
            .map_err(|error| format!("SIGNAL_FISH_CAPACITY_RATE_PER_SENDER: {error}"))?;
        let encoding = match var("ENCODING")? {
            Some(raw) => Some(Encoding::parse(&raw)?),
            None => None,
        };
        let delivery_class = match var("CLASS")? {
            Some(raw) => Some(DeliveryClass::parse(&raw)?),
            None => None,
        };
        let churn = match var("CHURN")? {
            Some(raw) => Some(ChurnSchedule::parse(&raw)?),
            None => None,
        };
        let warmup_secs = var("WARMUP_SECS")?
            .map(|raw| raw.parse::<f64>())
            .transpose()
            .map_err(|error| format!("SIGNAL_FISH_CAPACITY_WARMUP_SECS: {error}"))?;
        let duration_secs = var("DURATION_SECS")?
            .map(|raw| raw.parse::<f64>())
            .transpose()
            .map_err(|error| format!("SIGNAL_FISH_CAPACITY_DURATION_SECS: {error}"))?;
        let output_dir = var("OUTPUT_DIR")?;
        let lag_bound_ms = var("LAG_BOUND_MS")?
            .map(|raw| raw.parse::<u64>())
            .transpose()
            .map_err(|error| format!("SIGNAL_FISH_CAPACITY_LAG_BOUND_MS: {error}"))?;
        let sample_ms = var("SAMPLE_INTERVAL_MS")?
            .map(|raw| raw.parse::<u64>())
            .transpose()
            .map_err(|error| format!("SIGNAL_FISH_CAPACITY_SAMPLE_INTERVAL_MS: {error}"))?;

        Ok(RunConfig {
            endpoint,
            seed: seed.unwrap_or(0xC0FFEE),
            rooms: rooms.unwrap_or(1),
            players_per_room: players.unwrap_or(4),
            encoding: encoding.unwrap_or(Encoding::V3Json),
            payload_bytes: payload.unwrap_or(96),
            send_rate_per_sender: rate.unwrap_or(20.0),
            delivery_class: delivery_class.unwrap_or(DeliveryClass::Reliable),
            warmup: Duration::from_secs_f64(warmup_secs.unwrap_or(0.2)),
            duration: Duration::from_secs_f64(duration_secs.unwrap_or(1.2)),
            churn: churn.unwrap_or(ChurnSchedule::None),
            output_dir: output_dir
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("signal-fish-capacity-run")),
            generator_lag_bound: Duration::from_millis(lag_bound_ms.unwrap_or(250)),
            drain_grace: Duration::from_secs(2),
            sample_interval: Duration::from_millis(sample_ms.unwrap_or(250)),
            server_overlay: Self::default_server_overlay(),
            pause_sends: None,
            stall_senders: None,
            slow_reader: false,
            kill_server_after: None,
            room_code_prefix: None,
        })
    }
}
