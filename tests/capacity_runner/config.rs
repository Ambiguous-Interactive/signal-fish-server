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
//! Slice boundary: [`ChurnSchedule`] carries the reconnect-burst storm (the
//! C3 reconnect cell) as of the third runner PR; [`DeliveryClass`] carries
//! the full latest/volatile contract. The input surface exists so later
//! slices (room-replacement churn) extend the enums instead of reshaping
//! every call site.

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
    /// Keyed newest-value fan-out: a newer value for the same key supersedes
    /// the still-undelivered predecessor. Every omitted sequence must arrive
    /// as an exact server-stamped gap report (`latest_superseded` or
    /// `latest_dropped_full`), never as silence.
    Latest,
    /// Opportunistic fan-out with no sender backpressure: under pressure the
    /// oldest queued volatile message is evicted. Every omitted sequence must
    /// arrive as an exact `volatile_dropped` gap report, never as silence.
    Volatile,
}

impl DeliveryClass {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "reliable" => Ok(DeliveryClass::Reliable),
            "latest" => Ok(DeliveryClass::Latest),
            "volatile" => Ok(DeliveryClass::Volatile),
            other => Err(format!(
                "unsupported delivery class {other:?} (expected \"reliable\", \"latest\", \
                 or \"volatile\")"
            )),
        }
    }
}

/// Churn / reconnect schedule a run layers over the steady workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChurnSchedule {
    /// No churn: every client joins once and stays connected.
    None,
    /// Reconnect storm (the C3 reconnect-burst cell): at `start` into the
    /// run, the sockets of a seed-chosen `fraction_percent` of all peers
    /// close; each victim rejoins with a fresh connection at a seed-staggered
    /// instant inside `[start, start + window)`. A rejoin seats a fresh
    /// incarnation, so every victim's relay stream resumes as a new stream —
    /// the delivery contract the oracle validates across the storm.
    ReconnectBurst {
        /// Percentage of peers victimized (1-100; C3 cells use 10 and 50).
        fraction_percent: u32,
        /// Time into the run (from the epoch, warm-up included) when the
        /// victims' sockets close.
        #[serde(with = "duration_micros")]
        start: Duration,
        /// Reconnect stagger window. Every victim is back inside it, and the
        /// whole storm must complete inside the scheduled-send span. The
        /// default stays strictly below the default generator-lag bound
        /// (250 ms), so the default env config is runnable.
        #[serde(with = "duration_micros")]
        window: Duration,
    },
}

impl ChurnSchedule {
    /// The default burst shape: shared by the `CHURN=reconnect-burst`
    /// parser and the per-field env overrides, so the two paths cannot
    /// drift apart.
    pub(crate) fn reconnect_burst_default() -> Self {
        ChurnSchedule::ReconnectBurst {
            fraction_percent: 50,
            start: Duration::from_millis(300),
            window: Duration::from_millis(200),
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "none" => Ok(ChurnSchedule::None),
            "reconnect-burst" => Ok(ChurnSchedule::reconnect_burst_default()),
            other => Err(format!(
                "unsupported churn schedule {other:?} (expected \"none\" or \
                 \"reconnect-burst\"; shape the burst with CHURN_FRACTION_PERCENT, \
                 CHURN_START_MS, and CHURN_WINDOW_MS)"
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
    /// Negative-control hook (latest/volatile cells): the designated peer
    /// `r0p0` joins on a clamped socket, does not read for this long, then
    /// resumes and drains. While paused, the server's bounded kernel handoff
    /// and outbound queue fill, so lossy-class pressure (supersession or
    /// eviction) engages deterministically.
    #[serde(with = "duration_micros_option")]
    pub pause_reads: Option<Duration>,
    /// Distinct coalescing keys one sender round-robins (`class: latest`
    /// only; `seq % latest_keys`). `1` is the newest-value cell — every send
    /// supersedes its still-undelivered predecessor. Keys equal to or
    /// exceeding the send count never coalesce and deliver everything.
    #[serde(default = "default_latest_keys")]
    pub latest_keys_per_sender: u32,
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

/// Default coalescing-key count: one key per sender (the newest-value cell).
pub(crate) fn default_latest_keys() -> u32 {
    1
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

    /// Build the run config from `CAPACITY_RUNNER_*` environment
    /// variables so the runner is standalone on a capacity host:
    ///
    /// `ENDPOINT` (optional), `SEED`, `ROOMS`, `PLAYERS`, `ENCODING`
    /// (`v2-json` | `v3-json`), `PAYLOAD_BYTES`, `RATE_PER_SENDER`, `CLASS`
    /// (`reliable` | `latest` | `volatile`), `LATEST_KEYS`, `WARMUP_SECS`,
    /// `DURATION_SECS`, `CHURN` (`none` | `reconnect-burst`) with
    /// `CHURN_FRACTION_PERCENT`, `CHURN_START_MS`, `CHURN_WINDOW_MS`,
    /// `OUTPUT_DIR`, `LAG_BOUND_MS`, `SAMPLE_INTERVAL_MS`. Absent optional
    /// variables fall back to the small default scenario; required scalars
    /// fall back to the same defaults so a bare invocation just works. Each
    /// run needs a FRESH `OUTPUT_DIR` (a directory that already holds a run
    /// manifest is refused).
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| -> Result<Option<String>, String> {
            match std::env::var(format!("CAPACITY_RUNNER_{name}")) {
                Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
                Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
                Err(error) => Err(format!("CAPACITY_RUNNER_{name}: {error}")),
            }
        };
        let endpoint = var("ENDPOINT")?;
        let seed = var("SEED")?
            .map(|raw| raw.parse::<u64>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_SEED: {error}"))?;
        let rooms = var("ROOMS")?
            .map(|raw| raw.parse::<u32>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_ROOMS: {error}"))?;
        let players = var("PLAYERS")?
            .map(|raw| raw.parse::<u32>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_PLAYERS: {error}"))?;
        let payload = var("PAYLOAD_BYTES")?
            .map(|raw| raw.parse::<u32>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_PAYLOAD_BYTES: {error}"))?;
        let rate = var("RATE_PER_SENDER")?
            .map(|raw| raw.parse::<f64>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_RATE_PER_SENDER: {error}"))?;
        let encoding = match var("ENCODING")? {
            Some(raw) => Some(Encoding::parse(&raw)?),
            None => None,
        };
        let delivery_class = match var("CLASS")? {
            Some(raw) => Some(DeliveryClass::parse(&raw)?),
            None => None,
        };
        let churn_fraction = var("CHURN_FRACTION_PERCENT")?
            .map(|raw| raw.parse::<u32>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_CHURN_FRACTION_PERCENT: {error}"))?;
        let churn_start_ms = var("CHURN_START_MS")?
            .map(|raw| raw.parse::<u64>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_CHURN_START_MS: {error}"))?;
        let churn_window_ms = var("CHURN_WINDOW_MS")?
            .map(|raw| raw.parse::<u64>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_CHURN_WINDOW_MS: {error}"))?;
        let churn = match var("CHURN")? {
            Some(raw) => match ChurnSchedule::parse(&raw)? {
                ChurnSchedule::None => ChurnSchedule::None,
                ChurnSchedule::ReconnectBurst { .. } => {
                    let ChurnSchedule::ReconnectBurst {
                        fraction_percent,
                        start,
                        window,
                    } = ChurnSchedule::reconnect_burst_default()
                    else {
                        unreachable!("the default shape is a burst");
                    };
                    ChurnSchedule::ReconnectBurst {
                        fraction_percent: churn_fraction.unwrap_or(fraction_percent),
                        start: churn_start_ms.map(Duration::from_millis).unwrap_or(start),
                        window: churn_window_ms.map(Duration::from_millis).unwrap_or(window),
                    }
                }
            },
            None => ChurnSchedule::None,
        };
        let warmup_secs = var("WARMUP_SECS")?
            .map(|raw| raw.parse::<f64>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_WARMUP_SECS: {error}"))?;
        let latest_keys = var("LATEST_KEYS")?
            .map(|raw| raw.parse::<u32>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_LATEST_KEYS: {error}"))?;
        let duration_secs = var("DURATION_SECS")?
            .map(|raw| raw.parse::<f64>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_DURATION_SECS: {error}"))?;
        let output_dir = var("OUTPUT_DIR")?;
        let lag_bound_ms = var("LAG_BOUND_MS")?
            .map(|raw| raw.parse::<u64>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_LAG_BOUND_MS: {error}"))?;
        let sample_ms = var("SAMPLE_INTERVAL_MS")?
            .map(|raw| raw.parse::<u64>())
            .transpose()
            .map_err(|error| format!("CAPACITY_RUNNER_SAMPLE_INTERVAL_MS: {error}"))?;

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
            churn,
            output_dir: output_dir
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("signal-fish-capacity-run")),
            generator_lag_bound: Duration::from_millis(lag_bound_ms.unwrap_or(250)),
            drain_grace: Duration::from_secs(2),
            sample_interval: Duration::from_millis(sample_ms.unwrap_or(250)),
            server_overlay: Self::default_server_overlay(),
            pause_sends: None,
            pause_reads: None,
            latest_keys_per_sender: latest_keys.unwrap_or_else(default_latest_keys),
            stall_senders: None,
            slow_reader: false,
            kill_server_after: None,
            room_code_prefix: None,
        })
    }
}
