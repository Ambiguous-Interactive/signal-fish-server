//! Run artifacts and artifact replay.
//!
//! A run writes five machine-readable artifacts into its output directory:
//!
//! - `manifest.json` — schema version, run ID, the full [`RunConfig`],
//!   workload shape, server identity (endpoint, PID, binary hash, config
//!   overlay hash, full controlled config evidence or unknown external
//!   provenance), toolchain, host, features, and the clock method.
//! - `deliveries.jsonl` — every send, receipt, gap report, disconnect, and
//!   join failure.
//! - `intervals.jsonl` — periodic server resource samples (delivery
//!   counters, RSS, CPU time, cgroup memory) with unavailable counters
//!   recorded as null, never omitted.
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
use std::io::{BufRead, BufReader, BufWriter, Write as _};
use std::path::Path;

use hdrhistogram::serialization::Serializer as _;
use serde_json::Value;

use crate::diagnostics;

use crate::config::{micros, ExternalHostEvidence, RunConfig};
use crate::oracle::{summarize, OutcomeSummary};
use crate::records::RunRecords;
use crate::schedule::build_run_shape;

/// Bump on any breaking artifact shape change (the audit contract requires
/// every stored run to name its schema). Version 3 makes streams
/// epoch-aware: receipts carry the server's `(epoch, server_seq)` stamps,
/// sends carry the sender's incarnation epoch, and churn runs record their
/// disconnect/rejoin events with rejoin snapshot tails. Version 4 adds the
/// server and generator CPU-time pair to every interval sample. Version 5
/// adds the unsupported-format contract experiment: the config, summary,
/// and event log gain the experiment label, its advisory events, and its
/// verdict reasons. Version 6 adds the server's socket-memory page pair
/// (TCP and UDP `mem` pages from `/proc/<pid>/net/sockstat`) to every
/// interval sample. Version 7 measures latency from the scheduled send,
/// includes stalls above 60 seconds, and records an exact maximum. Version 8
/// records full controlled config evidence and labels unknown external config.
/// Version 9 records exact application and encoded message body sizes on
/// sends and receipts, validates the configured application size, and reports
/// byte totals by phase and direction. Receipt timestamps precede decoding.
/// Version 10 embeds host-declared external config and binary evidence.
pub const SCHEMA_VERSION: u64 = 10;

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
    pub config_provenance: ConfigProvenance,
}

/// Config evidence is independent of the delivery oracle's verdict.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ConfigProvenance {
    SpawnedControlled {
        defaults: Value,
        harness_base: Value,
        effective: Value,
        effective_sha256: String,
    },
    /// An endpoint alone supplies no config or binary evidence.
    UnknownExternal,
    /// Supplied by the host operator. Consistency is checked; remote process
    /// identity and unchanged settings must be verified by the operator.
    ExternalHostDeclared { evidence: Box<ExternalHostEvidence> },
}

impl ConfigProvenance {
    pub fn spawned(port: u16, effective: Value) -> Result<Self, String> {
        let bytes = serde_json::to_vec(&effective)
            .map_err(|error| format!("serialize effective server config: {error}"))?;
        Ok(Self::SpawnedControlled {
            defaults: serde_json::to_value(signal_fish_server::config::Config::default())
                .map_err(|error| format!("serialize server defaults: {error}"))?,
            harness_base: crate::websocket_test_helpers::server_process::base_config(port),
            effective,
            effective_sha256: diagnostics::sha256_bytes(&bytes),
        })
    }

    /// Unknown external provenance cannot support a capacity point.
    pub fn has_config_evidence(&self) -> bool {
        !matches!(self, Self::UnknownExternal)
    }
}

impl ExternalHostEvidence {
    pub fn validate(&self, endpoint: Option<&str>) -> Result<(), String> {
        if endpoint != Some(self.endpoint.as_str()) {
            return Err("external host evidence endpoint does not match run endpoint".into());
        }
        let url = reqwest::Url::parse(&self.endpoint)
            .map_err(|error| format!("external evidence endpoint: {error}"))?;
        if !matches!(url.scheme(), "ws" | "wss")
            || url.host_str().is_none()
            || self
                .endpoint
                .split("://")
                .nth(1)
                .and_then(|authority| authority.rsplit(':').next())
                .and_then(|port| port.parse::<u16>().ok())
                .is_none_or(|port| port == 0)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || self.endpoint.ends_with('/')
        {
            return Err(
                "external evidence endpoint requires a ws/wss origin with explicit port".into(),
            );
        }
        for (name, value) in [("host", &self.host), ("deployment", &self.deployment)] {
            if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
                return Err(format!(
                    "external host evidence {name} must be a bounded nonempty identifier"
                ));
            }
        }
        chrono::DateTime::parse_from_rfc3339(&self.collected_at_rfc3339)
            .map_err(|_| "external host evidence collection time must be RFC 3339".to_string())?;
        for (name, hash) in [
            ("effective_sha256", &self.effective_sha256),
            ("loaded_sha256", &self.loaded_sha256),
            ("binary_sha256", &self.binary_sha256),
        ] {
            if !is_sha256(hash) {
                return Err(format!(
                    "external host evidence {name} must be lowercase SHA-256 hex"
                ));
            }
        }
        if self.binary_bytes == 0 {
            return Err("external host evidence binary_bytes must be positive".into());
        }
        let typed: signal_fish_server::config::Config =
            serde_json::from_value(self.effective.clone())
                .map_err(|error| format!("parse external effective config: {error}"))?;
        let canonical = serde_json::to_value(typed.redacted_for_display())
            .map_err(|error| format!("serialize external effective config: {error}"))?;
        if canonical != self.effective {
            return Err(
                "external effective config must be complete, canonical, and redacted".into(),
            );
        }
        let bytes = serde_json::to_vec(&self.effective)
            .map_err(|error| format!("serialize external effective config: {error}"))?;
        if diagnostics::sha256_bytes(&bytes) != self.effective_sha256 {
            return Err("external effective config hash does not match recorded config".into());
        }
        let mut files = std::collections::BTreeSet::new();
        for (field, pointer) in [
            ("security.app_auth_path", "/security/app_auth_path"),
            (
                "security.connect_token.public_key_path",
                "/security/connect_token/public_key_path",
            ),
            (
                "security.transport.tls.certificate_path",
                "/security/transport/tls/certificate_path",
            ),
            (
                "security.transport.tls.private_key_path",
                "/security/transport/tls/private_key_path",
            ),
            (
                "security.transport.tls.client_ca_cert_path",
                "/security/transport/tls/client_ca_cert_path",
            ),
        ] {
            if field.starts_with("security.transport.tls.") && !typed.security.transport.tls.enabled
            {
                continue;
            }
            if let Some(path) = self.effective.pointer(pointer).and_then(Value::as_str) {
                if path.trim().is_empty() {
                    return Err(format!("external config {field} is empty"));
                }
                files.insert(field.to_string());
            }
        }
        if files != self.file_sha256.keys().cloned().collect() {
            return Err("external host evidence file hashes must match referenced auth and active TLS files".into());
        }
        for hash in self.file_sha256.values() {
            if !is_sha256(hash) {
                return Err(
                    "external host evidence file hashes must be lowercase SHA-256 hex".into(),
                );
            }
        }
        Ok(())
    }
}

fn is_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Manifest {
    /// Check internal evidence consistency. Hashes detect accidental changes;
    /// they are not a signature or proof about a remote endpoint.
    pub fn validate_config_provenance(&self) -> Result<(), String> {
        let overlay_bytes = serde_json::to_vec(&self.config.server_overlay)
            .map_err(|error| format!("serialize recorded overlay: {error}"))?;
        if diagnostics::sha256_bytes(&overlay_bytes) != self.server.config_overlay_sha256 {
            return Err("config overlay hash does not match recorded overlay".into());
        }
        match &self.server.config_provenance {
            ConfigProvenance::UnknownExternal => {
                if self.config.endpoint.as_deref() != Some(self.server.endpoint.as_str())
                    || self.config.external_host_evidence.is_some()
                    || self.server.pid.is_some()
                    || self.server.binary_sha256.is_some()
                    || self.server.binary_bytes.is_some()
                {
                    return Err(
                        "unknown external provenance has inconsistent server identity".into(),
                    );
                }
            }
            ConfigProvenance::ExternalHostDeclared { evidence } => {
                evidence.validate(self.config.endpoint.as_deref())?;
                if self.config.external_host_evidence.as_ref() != Some(evidence.as_ref())
                    || self.server.endpoint != evidence.endpoint
                    || self.server.pid.is_some()
                    || self.server.binary_sha256.as_ref() != Some(&evidence.binary_sha256)
                    || self.server.binary_bytes != Some(evidence.binary_bytes)
                {
                    return Err("external host evidence has inconsistent server identity".into());
                }
            }
            ConfigProvenance::SpawnedControlled {
                defaults,
                harness_base,
                effective,
                effective_sha256,
            } => {
                if self.config.endpoint.is_some()
                    || self.config.external_host_evidence.is_some()
                    || self.server.pid.is_none()
                    || self.server.binary_sha256.is_none()
                    || self.server.binary_bytes.is_none()
                {
                    return Err(
                        "controlled config provenance requires spawned binary identity".into(),
                    );
                }
                let bytes = serde_json::to_vec(effective)
                    .map_err(|error| format!("serialize effective config evidence: {error}"))?;
                if diagnostics::sha256_bytes(&bytes) != *effective_sha256 {
                    return Err("effective config hash does not match recorded config".into());
                }
                if !effective["security"]["app_auth_path"].is_null()
                    || !effective["security"]["connect_token"]["public_key_path"].is_null()
                {
                    return Err(
                        "controlled config provenance does not support file-backed auth sources"
                            .into(),
                    );
                }
                let port = effective["port"]
                    .as_u64()
                    .and_then(|port| u16::try_from(port).ok())
                    .ok_or("effective config port is invalid")?;
                if self.server.endpoint != format!("ws://127.0.0.1:{port}") {
                    return Err("effective config port does not match server endpoint".into());
                }
                if !defaults.is_object() || !harness_base.is_object() {
                    return Err("recorded config layers must be objects".into());
                }
                let overlay =
                    crate::websocket_test_helpers::server_process::normalize_server_overlay(
                        &self.config.server_overlay,
                    )?;
                let mut reconstructed = defaults.clone();
                crate::websocket_test_helpers::server_process::merge_config(
                    &mut reconstructed,
                    harness_base,
                );
                crate::websocket_test_helpers::server_process::merge_config(
                    &mut reconstructed,
                    &overlay,
                );
                reconstructed["port"] = Value::from(port);
                let typed: signal_fish_server::config::Config =
                    serde_json::from_value(reconstructed)
                        .map_err(|error| format!("parse recorded config layers: {error}"))?;
                let reconstructed = serde_json::to_value(typed)
                    .map_err(|error| format!("serialize recorded config layers: {error}"))?;
                if reconstructed != *effective {
                    return Err("effective config does not match declared config layers".into());
                }
            }
        }
        Ok(())
    }
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
    /// Cumulative CPU seconds (user + system) the server process consumed.
    pub server_cpu_seconds: Option<f64>,
    /// Cgroup memory usage of the server process.
    pub cgroup_memory_bytes: Option<u64>,
    /// Resident set size of the generator (this runner process).
    pub generator_rss_bytes: Option<u64>,
    /// Cumulative CPU seconds (user + system) the generator consumed — the
    /// load generator's own cost must be distinguishable from server
    /// saturation.
    pub generator_cpu_seconds: Option<f64>,
    /// Kernel TCP socket-buffer memory of the server process, in pages
    /// (TCP + TCP6 `mem` from `/proc/<pid>/net/sockstat`).
    pub server_socket_tcp_mem_pages: Option<u64>,
    /// Kernel UDP socket-buffer memory of the server process, in pages
    /// (UDP + UDP6 `mem` from `/proc/<pid>/net/sockstat`).
    pub server_socket_udp_mem_pages: Option<u64>,
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
    writer
        .flush()
        .map_err(|error| format!("write {}: {error}", path.display()))?;
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
    writer
        .flush()
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    Ok(())
}

/// Write the HdrHistogram V2 artifact for the measured latency samples.
pub fn write_histogram(output_dir: &Path, records: &RunRecords) -> Result<(), String> {
    let path = output_dir.join(HISTOGRAM_FILE);
    let histogram = crate::oracle::latency_histogram(
        crate::oracle::latency_pairs(records).map(|(_recipient, sample)| sample),
    );
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

/// Visit nonempty JSONL records in file order. The input buffer holds one
/// line; parsed records retained by a visitor remain that visitor's state.
fn visit_jsonl(
    mut reader: impl BufRead,
    path: &Path,
    mut visit: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader
            .read_until(b'\n', &mut line)
            .map_err(|error| format!("read {}: {error}", path.display()))?
            == 0
        {
            return Ok(());
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if !line.is_empty() {
            visit(&line)?;
        }
    }
}

/// Read the interval samples back from an output directory.
pub fn read_intervals(output_dir: &Path) -> Result<Vec<IntervalSample>, String> {
    let path = output_dir.join(INTERVALS_FILE);
    let file =
        fs::File::open(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut samples = Vec::new();
    visit_jsonl(BufReader::new(file), &path, |line| {
        samples.push(
            serde_json::from_slice(line)
                .map_err(|error| format!("parse an {} line: {error}", path.display()))?,
        );
        Ok(())
    })?;
    Ok(samples)
}

/// Read the deliveries event log back from an output directory.
pub fn read_records(output_dir: &Path) -> Result<RunRecords, String> {
    let path = output_dir.join(DELIVERIES_FILE);
    let file =
        fs::File::open(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut records = RunRecords::default();
    visit_jsonl(BufReader::new(file), &path, |line| {
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
            "unsupported_notice" => records.unsupported_notices.push(
                serde_json::from_value(value)
                    .map_err(|error| format!("parse unsupported_notice event: {error}"))?,
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
        Ok(())
    })?;
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
    manifest.validate_config_provenance()?;
    let (plans, churn) = build_run_shape(&manifest.config)?;
    crate::runner::validate_payload_size(&plans, manifest.config.payload_bytes)?;
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
        manifest.config.payload_bytes,
        manifest.config.delivery_class,
        &churn,
        manifest.config.experiment,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle::InvalidReason;
    use crate::records::{
        ChurnEvent, ChurnPhase, DisconnectEvent, DisconnectObservation, UnsupportedNoticeEvent,
    };
    use std::collections::BTreeMap;
    use std::io::{self, Cursor, Read};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[cfg(target_os = "linux")]
    #[test]
    fn buffered_artifact_writers_report_final_flush_failures() {
        let records = RunRecords::default();
        let sample: IntervalSample = serde_json::from_value(serde_json::json!({
            "t_us": 1, "counters": {}
        }))
        .expect("sample");
        let mut results = Vec::new();
        for (file, interval) in [(DELIVERIES_FILE, false), (INTERVALS_FILE, true)] {
            let output = tempfile::tempdir().expect("output");
            std::os::unix::fs::symlink("/dev/full", output.path().join(file))
                .expect("full device fixture");
            let result = if interval {
                write_intervals(output.path(), std::slice::from_ref(&sample))
            } else {
                write_deliveries(output.path(), &records)
            };
            results.push((file, result));
        }
        assert!(
            results.iter().all(|(_, result)| result.is_err()),
            "every writer must report its buffered failure: {results:?}"
        );
        for (file, result) in results {
            assert!(result.expect_err("write failure").contains(file));
        }
    }

    #[test]
    fn jsonl_readers_preserve_every_event_and_arrival_order() {
        let context = crate::unit_context();
        let mut expected = crate::complete_records(&context.plans);
        expected.sent.reverse();
        expected.receipts.reverse();
        expected.gaps.push(crate::gap_for(
            "r0p1",
            "r0p0",
            0,
            signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
        ));
        expected.unsupported_notices.push(UnsupportedNoticeEvent {
            recipient: "r0p1".into(),
            at_us: 123,
        });
        expected.disconnects.push(DisconnectEvent {
            recipient: "r0p1".into(),
            observation: DisconnectObservation::ServerClosed(Some(1000)),
        });
        expected.churn.push(ChurnEvent {
            recipient: "r0p1".into(),
            phase: ChurnPhase::Rejoined,
            at_us: 456,
            epoch: Some(2),
            tails: BTreeMap::from([("r0p0".into(), ("id0".into(), 3))]),
        });
        expected.join_failures.push("join refused".into());
        expected.faults.push(InvalidReason::ServerTerminated);
        expected.registry.insert("id0".into(), ("r0p0".into(), 1));
        let output = tempfile::tempdir().expect("output");
        let lines: Vec<_> = expected
            .events()
            .map(|event| serde_json::to_vec(&event).expect("event"))
            .collect();
        let kinds: std::collections::BTreeSet<_> = lines
            .iter()
            .map(|line| {
                serde_json::from_slice::<Value>(line).expect("value")["event_kind"]
                    .as_str()
                    .expect("kind")
                    .to_string()
            })
            .collect();
        assert_eq!(kinds.len(), 9);
        for newline in [b"\n".as_slice(), b"\r\n".as_slice()] {
            // Exactly empty lines are ignored. CR-only lines remain malformed.
            let mut raw = b"\n".to_vec();
            raw.extend_from_slice(br#"{"event_kind":"registry","senders":{"obsolete":["old",1]}}"#);
            raw.extend_from_slice(newline);
            for line in &lines {
                raw.extend_from_slice(line);
                raw.extend_from_slice(newline);
            }
            raw.truncate(raw.len() - newline.len());
            fs::write(output.path().join(DELIVERIES_FILE), raw).expect("fixture");
            assert_eq!(
                serde_json::to_value(read_records(output.path()).expect("read events"))
                    .expect("actual"),
                serde_json::to_value(&expected).expect("expected")
            );
        }
        // Preserve Value's last-key-wins behavior, including event_kind.
        fs::write(output.path().join(DELIVERIES_FILE),
            br#"{"event_kind":"unknown","event_kind":"join_failure","detail":"first","detail":"last"}"#).expect("duplicate keys");
        assert_eq!(
            read_records(output.path())
                .expect("duplicate keys accepted")
                .join_failures,
            vec!["last"]
        );
        let samples = [
            serde_json::json!({"t_us":9,"counters":{"x":1}}),
            serde_json::json!({"t_us":3,"counters":{"x":2}}),
        ];
        for newline in ["\n", "\r\n"] {
            let raw = format!("\n{}{newline}{}", samples[0], samples[1]);
            fs::write(output.path().join(INTERVALS_FILE), raw).expect("interval fixture");
            let actual = read_intervals(output.path()).expect("intervals");
            assert_eq!(
                actual.iter().map(|sample| sample.t_us).collect::<Vec<_>>(),
                vec![9, 3]
            );
            assert_eq!(actual[0].counters, samples[0]["counters"]);
        }
    }

    #[test]
    fn jsonl_readers_keep_empty_and_malformed_line_semantics() {
        let output = tempfile::tempdir().expect("output");
        for file in [DELIVERIES_FILE, INTERVALS_FILE] {
            for raw in [b"".as_slice(), b"\n\n".as_slice()] {
                fs::write(output.path().join(file), raw).expect("empty fixture");
                if file == DELIVERIES_FILE {
                    assert!(read_records(output.path())
                        .expect("empty records")
                        .sent
                        .is_empty());
                } else {
                    assert!(read_intervals(output.path())
                        .expect("empty intervals")
                        .is_empty());
                }
            }
            for raw in [
                b" \n".as_slice(),
                b"\r\n".as_slice(),
                b"\xff\n".as_slice(),
                b"{\n".as_slice(),
            ] {
                fs::write(output.path().join(file), raw).expect("malformed fixture");
                let error = if file == DELIVERIES_FILE {
                    read_records(output.path()).expect_err("malformed")
                } else {
                    read_intervals(output.path()).expect_err("malformed")
                };
                assert!(error.starts_with("parse "), "{error}");
            }
        }
        for (raw, expected) in [
            (
                r#"{"event_kind":"unknown"}"#,
                "unknown deliveries event kind",
            ),
            (r#"{"event_kind":7}"#, "line missing event_kind"),
            (
                r#"{"event_kind":"join_failure"}"#,
                "join_failure line missing detail",
            ),
            (
                r#"{"event_kind":"registry"}"#,
                "registry line missing senders",
            ),
            (r#"{"event_kind":"sent"}"#, "parse sent event"),
        ] {
            fs::write(output.path().join(DELIVERIES_FILE), raw).expect("fixture");
            assert!(read_records(output.path())
                .expect_err("invalid event")
                .contains(expected));
        }
    }

    /// A readable prefix followed by a virtual large tail that refuses reads.
    /// A streaming parser must report a bad prefix without touching that tail.
    struct GuardedTail {
        prefix: Cursor<Vec<u8>>,
        reads: Arc<AtomicUsize>,
    }
    impl Read for GuardedTail {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.prefix.position() < u64::try_from(self.prefix.get_ref().len()).expect("prefix")
            {
                self.prefix.read(out)
            } else {
                Err(io::Error::other("large tail was read"))
            }
        }
    }

    #[test]
    fn jsonl_visitor_stops_before_reading_a_malformed_records_tail() {
        let reads = Arc::new(AtomicUsize::new(0));
        let reader = GuardedTail {
            prefix: Cursor::new(b"{\n".to_vec()),
            reads: Arc::clone(&reads),
        };
        let error = visit_jsonl(BufReader::new(reader), Path::new("guarded.jsonl"), |line| {
            serde_json::from_slice::<Value>(line)
                .map(|_| ())
                .map_err(|error| format!("parse prefix: {error}"))
        })
        .expect_err("bad prefix");
        assert!(error.starts_with("parse prefix:"), "{error}");
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "never read the trailing input"
        );
    }

    #[test]
    fn jsonl_visitor_reuses_lines_and_propagates_late_read_errors() {
        let raw = format!(
            "\n{}\n{{}}",
            serde_json::json!({"text":"é".repeat(100_000)})
        );
        let mut sizes = Vec::new();
        visit_jsonl(
            BufReader::with_capacity(7, Cursor::new(raw.as_bytes())),
            Path::new("large.jsonl"),
            |line| {
                serde_json::from_slice::<Value>(line).expect("complete line");
                sizes.push(line.len());
                Ok(())
            },
        )
        .expect("large and final lines");
        assert_eq!(sizes, vec![raw.len() - 4, 2]);
        let reads = Arc::new(AtomicUsize::new(0));
        let reader = GuardedTail {
            prefix: Cursor::new(b"{}\n".to_vec()),
            reads: Arc::clone(&reads),
        };
        let mut visited = 0;
        let error = visit_jsonl(BufReader::new(reader), Path::new("late.jsonl"), |_| {
            visited += 1;
            Ok(())
        })
        .expect_err("late read error");
        assert_eq!(visited, 1);
        assert_eq!(error, "read late.jsonl: large tail was read");
    }
}
