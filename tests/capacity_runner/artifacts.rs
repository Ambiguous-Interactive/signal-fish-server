//! Run artifacts and artifact replay.
//!
//! A run writes five machine-readable artifacts into its output directory:
//!
//! - `manifest.json` — schema version, run ID, the full [`RunConfig`],
//!   workload shape, server identity (endpoint, PID, binary hash, config
//!   overlay hash), toolchain, host, features, and the clock method.
//! - `deliveries.jsonl` — every send, receipt, gap report, disconnect, and
//!   join failure.
//! - `intervals.jsonl` — periodic server resource samples (delivery
//!   counters, RSS, cgroup memory) with unavailable counters recorded as
//!   null, never omitted.
//! - `summary.json` — the oracle outcome (the run's verdict).
//! - `latency-histogram-v2.hdr` — HdrHistogram V2 encoding of the measured
//!   one-way latency samples.
//!
//! Replay is the same purity as the oracle: `replay` reads the manifest and
//! `deliveries.jsonl`, rebuilds the plans from the recorded config, and
//! re-summarizes. The result must equal the recorded summary byte-for-byte
//! at the JSON level — that is what makes an archived run auditable without
//! the host that produced it.

use std::fs;
use std::io::{BufWriter, Write as _};
use std::path::Path;

use hdrhistogram::serialization::Serializer as _;
use serde_json::Value;

use crate::diagnostics;

use crate::config::{micros, RunConfig};
use crate::oracle::{summarize, OutcomeSummary};
use crate::records::RunRecords;
use crate::schedule::build_run_shape;

/// Bump on any breaking artifact shape change (the audit contract requires
/// every stored run to name its schema). Version 3 makes streams
/// epoch-aware: receipts carry the server's `(epoch, server_seq)` stamps,
/// sends carry the sender's incarnation epoch, and churn runs record their
/// disconnect/rejoin events with rejoin snapshot tails.
pub const SCHEMA_VERSION: u64 = 3;

pub const MANIFEST_FILE: &str = "manifest.json";
pub const DELIVERIES_FILE: &str = "deliveries.jsonl";
pub const INTERVALS_FILE: &str = "intervals.jsonl";
pub const SUMMARY_FILE: &str = "summary.json";
pub const HISTOGRAM_FILE: &str = "latency-histogram-v2.hdr";

/// Server identity recorded in the manifest.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerIdentity {
    /// Endpoint the clients connected to (`ws://127.0.0.1:PORT`).
    pub endpoint: String,
    /// OS process id of a spawned server (`None` for an external endpoint).
    pub pid: Option<u32>,
    /// SHA-256 of the spawned binary (`None` for an external endpoint).
    pub binary_sha256: Option<String>,
    /// Size in bytes of the spawned binary.
    pub binary_bytes: Option<u64>,
    /// SHA-256 of the server config overlay deep-merged over the harness
    /// base config.
    pub config_overlay_sha256: String,
}

/// Static identity of the generating host and build.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BuildIdentity {
    pub toolchain: Option<String>,
    pub os: String,
    pub arch: String,
    pub kernel: Option<String>,
    pub features: Features,
    pub clock: String,
    pub generator: String,
}

impl BuildIdentity {
    /// Identity of the process generating this manifest.
    pub fn current() -> Self {
        Self {
            toolchain: diagnostics::toolchain_version(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            kernel: diagnostics::kernel_release(),
            features: Features::current(),
            clock: "tokio::time::Instant (monotonic; one shared run epoch)".to_string(),
            generator: "capacity_runner (in-tree)".to_string(),
        }
    }
}

/// Which optional build features were compiled into this runner.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Features {
    pub tls: bool,
    pub trace_validation: bool,
    pub allocation_tracking: bool,
}

impl Features {
    pub fn current() -> Self {
        Self {
            tls: cfg!(feature = "tls"),
            trace_validation: cfg!(feature = "trace-validation"),
            allocation_tracking: cfg!(feature = "allocation-tracking"),
        }
    }
}

/// One periodic resource sample. Every counter is `Option`: a value the
/// host cannot provide is recorded as `null` (unavailable), never dropped.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntervalSample {
    /// Sample time in microseconds relative to the run epoch.
    pub t_us: u64,
    pub counters: Value,
    /// Resident set size of the server process.
    pub server_rss_bytes: Option<u64>,
    /// Cgroup memory usage of the server process.
    pub cgroup_memory_bytes: Option<u64>,
    /// Resident set size of the generator (this runner process).
    pub generator_rss_bytes: Option<u64>,
    /// Scrape failure detail — a sample that could not be taken is recorded
    /// as an explicit event, not skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrape_error: Option<String>,
}

/// Everything a replay needs besides the deliveries file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub schema_version: u64,
    pub run_id: String,
    pub created_at_rfc3339: String,
    pub config: RunConfig,
    pub workload: WorkloadShape,
    pub server: ServerIdentity,
    pub build: BuildIdentity,
}

/// The derived workload shape (sanity anchor for replays).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkloadShape {
    pub senders: u64,
    pub recipients: u64,
    pub scheduled_sends_per_sender: u64,
    pub warmup_sends_per_sender: u64,
    pub measured_sends_per_sender: u64,
}

/// Write the manifest for a run about to start.
pub fn write_manifest(
    output_dir: &Path,
    run_id: &str,
    config: &RunConfig,
    workload: WorkloadShape,
    server: ServerIdentity,
    build: BuildIdentity,
) -> Result<(), String> {
    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        run_id: run_id.to_string(),
        created_at_rfc3339: chrono::Utc::now().to_rfc3339(),
        config: config.clone(),
        workload,
        server,
        build,
    };
    write_json(output_dir.join(MANIFEST_FILE), &manifest)
}

/// Append-write helper: serialize `value` as pretty JSON.
pub fn write_json(path: impl AsRef<Path>, value: &impl serde::Serialize) -> Result<(), String> {
    let path = path.as_ref();
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

/// Write the event log as JSON lines.
pub fn write_deliveries(output_dir: &Path, records: &RunRecords) -> Result<(), String> {
    let path = output_dir.join(DELIVERIES_FILE);
    let file =
        fs::File::create(&path).map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for event in records.events() {
        serde_json::to_writer(&mut writer, &event)
            .map_err(|error| format!("serialize a deliveries event: {error}"))?;
        writeln!(writer).map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    Ok(())
}

/// Write interval samples as JSON lines.
pub fn write_intervals(output_dir: &Path, samples: &[IntervalSample]) -> Result<(), String> {
    let path = output_dir.join(INTERVALS_FILE);
    let file =
        fs::File::create(&path).map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for sample in samples {
        serde_json::to_writer(&mut writer, sample)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
        writeln!(writer).map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    Ok(())
}

/// Write the HdrHistogram V2 artifact for the measured latency samples.
pub fn write_histogram(output_dir: &Path, samples: &[u64]) -> Result<(), String> {
    let path = output_dir.join(HISTOGRAM_FILE);
    let mut histogram = hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)
        .map_err(|error| format!("create latency histogram: {error}"))?;
    for sample in samples {
        // Saturate rather than fail: a sample beyond the ceiling (a >60 s
        // stall) belongs in the shape at its ceiling, and the summary
        // already carries max/percentiles over the same values.
        histogram.saturating_record(*sample);
    }
    let mut encoded = Vec::new();
    hdrhistogram::serialization::V2Serializer::new()
        .serialize(&histogram, &mut encoded)
        .map_err(|error| format!("serialize latency histogram: {error:?}"))?;
    fs::write(&path, encoded).map_err(|error| format!("write {}: {error}", path.display()))
}

/// Read the manifest back from an output directory.
pub fn read_manifest(output_dir: &Path) -> Result<Manifest, String> {
    let path = output_dir.join(MANIFEST_FILE);
    let raw = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&raw).map_err(|error| format!("parse {}: {error}", path.display()))
}

/// Read the deliveries event log back from an output directory.
pub fn read_records(output_dir: &Path) -> Result<RunRecords, String> {
    let path = output_dir.join(DELIVERIES_FILE);
    let raw = fs::read(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut records = RunRecords::default();
    for line in raw.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_slice(line)
            .map_err(|error| format!("parse a {} line: {error}", path.display()))?;
        let kind = value
            .get("event_kind")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{} line missing event_kind", path.display()))?
            .to_string();
        match kind.as_str() {
            "sent" => records.sent.push(
                serde_json::from_value(value)
                    .map_err(|error| format!("parse sent event: {error}"))?,
            ),
            "receipt" => records.receipts.push(
                serde_json::from_value(value)
                    .map_err(|error| format!("parse receipt event: {error}"))?,
            ),
            "gap" => records.gaps.push(
                serde_json::from_value(value)
                    .map_err(|error| format!("parse gap event: {error}"))?,
            ),
            "disconnect" => records.disconnects.push(
                serde_json::from_value(value)
                    .map_err(|error| format!("parse disconnect event: {error}"))?,
            ),
            "churn" => records.churn.push(
                serde_json::from_value(value)
                    .map_err(|error| format!("parse churn event: {error}"))?,
            ),
            "join_failure" => records.join_failures.push(
                value
                    .get("detail")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "join_failure line missing detail".to_string())?
                    .to_string(),
            ),
            "fault" => records.faults.push(
                serde_json::from_value(value)
                    .map_err(|error| format!("parse fault event: {error}"))?,
            ),
            "registry" => {
                let senders = value
                    .get("senders")
                    .ok_or_else(|| "registry line missing senders".to_string())?;
                records.registry = serde_json::from_value(senders.clone())
                    .map_err(|error| format!("parse registry senders: {error}"))?;
            }
            other => return Err(format!("unknown deliveries event kind {other:?}")),
        }
    }
    Ok(records)
}

/// Replay a run's artifacts: rebuild the plans from the recorded config and
/// re-summarize the recorded events. The result must equal the recorded
/// summary — the run summary is never authoritative over its raw events.
pub fn replay(output_dir: &Path) -> Result<OutcomeSummary, String> {
    let manifest = read_manifest(output_dir)?;
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported artifact schema {} (expected {SCHEMA_VERSION})",
            manifest.schema_version
        ));
    }
    let (plans, churn) = build_run_shape(&manifest.config)?;
    let roster = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect::<Vec<_>>();
    let records = read_records(output_dir)?;
    Ok(summarize(
        &plans,
        &roster,
        &records,
        micros(manifest.config.generator_lag_bound),
        manifest.config.delivery_class,
        &churn,
    ))
}
