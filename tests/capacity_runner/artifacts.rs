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
use std::io::{BufRead, BufReader, BufWriter, Read as _, Write as _};
use std::path::Path;

use hdrhistogram::serialization::Serializer as _;
use serde_json::Value;

use crate::diagnostics;

use crate::config::{micros, ExternalHostEvidence, RunConfig};
use crate::oracle::{summarize, OutcomeSummary};
use crate::records::{DeliveryEvent, RunRecords};
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

/// Monotonic sampler boundaries, in microseconds from the workload epoch.
/// Elapsed phases include task descheduling. Missing response boundaries mean
/// that the request failed before that phase completed. Old artifacts omit this.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SamplerTiming {
    pub scheduled_us: u64,
    pub wake_us: u64,
    pub headers_received_us: Option<u64>,
    pub body_received_us: Option<u64>,
    pub scrape_finished_us: u64,
    pub resources_finished_us: u64,
}

/// One periodic resource sample. Every counter is `Option`: a value the
/// host cannot provide is recorded as `null` (unavailable), never dropped.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IntervalSample {
    /// Sample time in microseconds relative to the run epoch.
    pub t_us: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<SamplerTiming>,
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
    /// Cgroup-wide CPU counters for each process; absent on unsupported hosts
    /// and in old artifacts. A shared cgroup includes both processes.
    #[serde(default)]
    pub server_cgroup_cpu: Option<diagnostics::CgroupCpuStats>,
    #[serde(default)]
    pub generator_cgroup_cpu: Option<diagnostics::CgroupCpuStats>,
    /// OS-thread counters and wait states. Old artifacts and unsupported
    /// hosts retain null. Reads occur within the resource timing boundary.
    #[serde(default)]
    pub generator_scheduling: Option<diagnostics::ProcessScheduling>,
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

/// Write pretty JSON through a bounded buffer instead of retaining encoded output.
pub fn write_json(path: impl AsRef<Path>, value: &impl serde::Serialize) -> Result<(), String> {
    let path = path.as_ref();
    let file =
        fs::File::create(path).map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value)
        .map_err(|error| format!("serialize {}: {error}", path.display()))?;
    writer
        .flush()
        .map_err(|error| format!("write {}: {error}", path.display()))
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
    read_manifest_with_limits(&path, ReplayLimits::from_env()?)
}

fn read_manifest_with_limits(path: &Path, limits: ReplayLimits) -> Result<Manifest, String> {
    let file = fs::File::open(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let limit = u64::try_from(limits.line_bytes)
        .map_err(|_| "replay line budget exceeds u64")?
        .min(limits.input_bytes);
    let mut raw = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut raw)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    if crate::config::count_u64(raw.len()) > limit {
        return Err(format!(
            "replay manifest exceeds byte budget {limit}: {}; set CAPACITY_RUNNER_REPLAY_MAX_LINE_BYTES and CAPACITY_RUNNER_REPLAY_MAX_BYTES for a provisioned host",
            path.display()
        ));
    }
    serde_json::from_slice(&raw).map_err(|error| format!("parse {}: {error}", path.display()))
}

#[derive(Clone, Copy)]
struct ReplayLimits {
    records: u64,
    input_bytes: u64,
    line_bytes: usize,
}

impl ReplayLimits {
    fn from_env() -> Result<Self, String> {
        Self::from_lookup(|name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(format!("{name} must be UTF-8")),
        })
    }

    fn from_lookup(
        mut lookup: impl FnMut(&str) -> Result<Option<String>, String>,
    ) -> Result<Self, String> {
        let defaults = Self::default();
        let mut value = |name, default| {
            let parsed = lookup(name)?.map_or(Ok(default), |raw| {
                raw.parse::<u64>()
                    .map_err(|_| format!("{name} must be a positive integer"))
            })?;
            if parsed == 0 {
                return Err(format!("{name} must be a positive integer"));
            }
            Ok(parsed)
        };
        Ok(Self {
            records: value("CAPACITY_RUNNER_REPLAY_MAX_RECORDS", defaults.records)?,
            input_bytes: value("CAPACITY_RUNNER_REPLAY_MAX_BYTES", defaults.input_bytes)?,
            line_bytes: usize::try_from(value(
                "CAPACITY_RUNNER_REPLAY_MAX_LINE_BYTES",
                u64::try_from(defaults.line_bytes).map_err(|_| "replay line budget exceeds u64")?,
            )?)
            .map_err(|_| "replay line budget exceeds this host")?,
        })
    }
}

impl Default for ReplayLimits {
    fn default() -> Self {
        Self {
            records: 32_000_000,
            input_bytes: 4 * 1024 * 1024 * 1024,
            line_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Visit nonempty JSONL records in file order. The input buffer holds one
/// line; parsed records retained by a visitor remain that visitor's state.
fn visit_jsonl(
    reader: impl BufRead,
    path: &Path,
    visit: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    visit_jsonl_with_limits(reader, path, ReplayLimits::from_env()?, visit)
}

fn visit_jsonl_with_limits(
    mut reader: impl BufRead,
    path: &Path,
    limits: ReplayLimits,
    mut visit: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let mut line = Vec::new();
    let mut input_bytes = 0_u64;
    let mut records = 0_u64;
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let at_end = available.is_empty();
        let newline = available.iter().position(|&byte| byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        input_bytes = input_bytes.checked_add(crate::config::count_u64(consumed))
            .filter(|&bytes| bytes <= limits.input_bytes)
            .ok_or_else(|| format!("replay byte budget {} exceeded: {}; set CAPACITY_RUNNER_REPLAY_MAX_BYTES for a provisioned host", limits.input_bytes, path.display()))?;
        let content = newline.map_or(consumed, |index| index);
        let line_len = line.len().checked_add(content)
            .filter(|&length| length <= limits.line_bytes)
            .ok_or_else(|| format!("replay line budget {} exceeded: {}; set CAPACITY_RUNNER_REPLAY_MAX_LINE_BYTES for a provisioned host", limits.line_bytes, path.display()))?;
        if line_len > line.capacity() {
            let capacity = line
                .capacity()
                .saturating_mul(2)
                .max(line_len)
                .min(limits.line_bytes);
            line.try_reserve_exact(capacity - line.len())
                .map_err(|error| format!("reserve replay line: {error}"))?;
        }
        line.extend_from_slice(&available[..content]);
        reader.consume(consumed);
        if newline.is_some() || at_end {
            if !line.is_empty() {
                records = records.checked_add(1)
                    .filter(|&count| count <= limits.records)
                    .ok_or_else(|| format!("replay record budget {} exceeded: {}; set CAPACITY_RUNNER_REPLAY_MAX_RECORDS for a provisioned host", limits.records, path.display()))?;
                visit(&line)?;
                line.clear();
            }
            if at_end {
                return Ok(());
            }
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

fn read_delivery_event(line: &[u8], path: &Path) -> Result<DeliveryEvent<String>, String> {
    /* Preserve historical last-key-wins decoding and error messages. */
    let value: Value = serde_json::from_slice(line)
        .map_err(|error| format!("parse a {} line: {error}", path.display()))?;
    let kind = value
        .get("event_kind")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{} line missing event_kind", path.display()))?
        .to_string();
    match kind.as_str() {
        "sent" => Ok(DeliveryEvent::Sent(
            serde_json::from_value(value).map_err(|error| format!("parse sent event: {error}"))?,
        )),
        "receipt" => Ok(DeliveryEvent::Receipt(
            serde_json::from_value(value)
                .map_err(|error| format!("parse receipt event: {error}"))?,
        )),
        "gap" => Ok(DeliveryEvent::Gap(
            serde_json::from_value(value).map_err(|error| format!("parse gap event: {error}"))?,
        )),
        "unsupported_notice" => Ok(DeliveryEvent::UnsupportedNotice(
            serde_json::from_value(value)
                .map_err(|error| format!("parse unsupported_notice event: {error}"))?,
        )),
        "disconnect" => Ok(DeliveryEvent::Disconnect(
            serde_json::from_value(value)
                .map_err(|error| format!("parse disconnect event: {error}"))?,
        )),
        "churn" => Ok(DeliveryEvent::Churn(
            serde_json::from_value(value).map_err(|error| format!("parse churn event: {error}"))?,
        )),
        "join_failure" => Ok(DeliveryEvent::JoinFailure {
            detail: value
                .get("detail")
                .and_then(Value::as_str)
                .ok_or_else(|| "join_failure line missing detail".to_string())?
                .to_string(),
        }),
        "fault" => Ok(DeliveryEvent::Fault(
            serde_json::from_value(value).map_err(|error| format!("parse fault event: {error}"))?,
        )),
        "registry" => {
            let senders = value
                .get("senders")
                .ok_or_else(|| "registry line missing senders".to_string())?;
            Ok(DeliveryEvent::Registry {
                senders: serde_json::from_value(senders.clone())
                    .map_err(|error| format!("parse registry senders: {error}"))?,
            })
        }
        other => Err(format!("unknown deliveries event kind {other:?}")),
    }
}

/// Borrow ordinary JSON names from the current line. Escaped JSON names own
/// their decoded value until the run-local pool shares it. The pool copies
/// each distinct identity once and does not keep the line buffer alive.
#[derive(serde::Deserialize)]
struct ReplayName<'a>(#[serde(borrow)] std::borrow::Cow<'a, str>);

impl AsRef<str> for ReplayName<'_> {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

fn append_delivery_event(
    records: &mut RunRecords,
    names: &mut crate::records::ReceiptNames,
    event: DeliveryEvent<impl AsRef<str>>,
) {
    match event {
        DeliveryEvent::Sent(event) => records.sent.push(names.share_sent(event)),
        DeliveryEvent::Receipt(event) => records.receipts.push(names.share(event)),
        DeliveryEvent::Gap(event) => records.gaps.push(names.share_gap(event)),
        DeliveryEvent::UnsupportedNotice(event) => records.unsupported_notices.push(event),
        DeliveryEvent::Disconnect(event) => records.disconnects.push(event),
        DeliveryEvent::Churn(event) => records.churn.push(event),
        DeliveryEvent::JoinFailure { detail } => records.join_failures.push(detail),
        DeliveryEvent::Fault(event) => records.faults.push(event),
        DeliveryEvent::Registry { senders } => records.registry = senders,
    }
}

/// Read the deliveries event log back from an output directory.
pub fn read_records(output_dir: &Path) -> Result<RunRecords, String> {
    read_records_with_limits(output_dir, ReplayLimits::from_env()?)
}

fn read_records_with_limits(output_dir: &Path, limits: ReplayLimits) -> Result<RunRecords, String> {
    let path = output_dir.join(DELIVERIES_FILE);
    let file =
        fs::File::open(&path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut records = RunRecords::default();
    let mut receipt_names = crate::records::ReceiptNames::default();
    visit_jsonl_with_limits(BufReader::new(file), &path, limits, |line| {
        if let Ok(event) = serde_json::from_slice::<DeliveryEvent<ReplayName<'_>>>(line) {
            append_delivery_event(&mut records, &mut receipt_names, event);
        } else {
            // Keep historical last-key-wins decoding and precise error text.
            append_delivery_event(
                &mut records,
                &mut receipt_names,
                read_delivery_event(line, &path)?,
            );
        }
        Ok(())
    })?;
    Ok(records)
}

/// Replay a run's artifacts: rebuild the plans from the recorded config and
/// re-summarize the recorded events. The result must equal the recorded
/// summary — the run summary is never authoritative over its raw events.
pub fn replay(output_dir: &Path) -> Result<OutcomeSummary, String> {
    replay_with_limits(output_dir, ReplayLimits::from_env()?)
}

fn replay_with_limits(output_dir: &Path, limits: ReplayLimits) -> Result<OutcomeSummary, String> {
    let manifest = read_manifest_with_limits(&output_dir.join(MANIFEST_FILE), limits)?;
    if manifest.schema_version != SCHEMA_VERSION {
        return Err(format!(
            "unsupported artifact schema {} (expected {SCHEMA_VERSION})",
            manifest.schema_version
        ));
    }
    validate_replay_shape(&manifest.config, limits)?;
    manifest.validate_config_provenance()?;
    let (plans, churn) = build_run_shape(&manifest.config)?;
    crate::runner::validate_payload_size(&plans, manifest.config.payload_bytes)?;
    let roster = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect::<Vec<_>>();
    let records = read_records_with_limits(output_dir, limits)?;
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

fn validate_replay_shape(config: &RunConfig, limits: ReplayLimits) -> Result<(), String> {
    let mut admission = config.clone();
    admission.planned_record_budget = limits.records;
    admission
        .validate_planned_record_budget()
        .map_err(|error| {
            format!(
                "replay admission: {error}; host CAPACITY_RUNNER_REPLAY_MAX_RECORDS is {}",
                limits.records
            )
        })?;
    let peers = u64::from(admission.rooms)
        .checked_mul(u64::from(admission.players_per_room))
        .filter(|&count| count <= limits.records)
        .ok_or_else(|| format!("replay peer count exceeds record budget {}", limits.records))?;
    if peers == 0 {
        return Err("replay needs at least one peer".into());
    }
    Ok(())
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

    #[test]
    fn replay_input_budgets_refuse_crossing_rows_before_visiting() {
        for (raw, limits, expected, visited) in [
            (
                "{}\n{}",
                ReplayLimits {
                    records: 1,
                    input_bytes: 100,
                    line_bytes: 10,
                },
                "record budget",
                1,
            ),
            (
                "{}\n{}",
                ReplayLimits {
                    records: 10,
                    input_bytes: 4,
                    line_bytes: 10,
                },
                "byte budget",
                1,
            ),
            (
                "{}\n{  }",
                ReplayLimits {
                    records: 10,
                    input_bytes: 100,
                    line_bytes: 3,
                },
                "line budget",
                1,
            ),
        ] {
            let mut actual = 0;
            let result = visit_jsonl_with_limits(
                Cursor::new(raw),
                Path::new("fixture.jsonl"),
                limits,
                |_| {
                    actual += 1;
                    Ok(())
                },
            );
            let error = result.expect_err("oversized replay must be refused");
            assert!(error.contains(expected), "{error}");
            assert_eq!(
                actual, visited,
                "crossing record must not reach the decoder"
            );
        }
    }

    #[test]
    fn replay_input_boundaries_preserve_fragmented_lines_and_blank_line_cost() {
        let raw = "\n{\"name\":\"鱼\"}\r\n{}";
        let limits = ReplayLimits {
            records: 2,
            input_bytes: crate::config::count_u64(raw.len()),
            line_bytes: raw.find('\r').unwrap(),
        };
        for capacity in [1, 2, 7, 8192] {
            let mut rows = Vec::new();
            visit_jsonl_with_limits(
                BufReader::with_capacity(capacity, Cursor::new(raw)),
                Path::new("boundary.jsonl"),
                limits,
                |line| {
                    rows.push(serde_json::from_slice::<Value>(line).expect("complete JSON"));
                    Ok(())
                },
            )
            .expect("exact budgets fit");
            assert_eq!(
                rows,
                vec![serde_json::json!({"name":"鱼"}), serde_json::json!({})]
            );
        }
        let mut visited = 0;
        let error = visit_jsonl_with_limits(
            Cursor::new("\n\n\n"),
            Path::new("blank.jsonl"),
            ReplayLimits {
                input_bytes: 2,
                ..limits
            },
            |_| {
                visited += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.contains("byte budget"), "{error}");
        assert_eq!(visited, 0);
    }

    #[test]
    fn replay_line_budget_stops_before_reading_or_decoding_the_large_tail() {
        let reads = Arc::new(AtomicUsize::new(0));
        let reader = GuardedTail {
            prefix: Cursor::new(b"12345".to_vec()),
            reads: Arc::clone(&reads),
        };
        let mut visits = 0;
        let error = visit_jsonl_with_limits(
            BufReader::with_capacity(2, reader),
            Path::new("guarded.jsonl"),
            ReplayLimits {
                line_bytes: 4,
                ..ReplayLimits::default()
            },
            |_| {
                visits += 1;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.contains("line budget 4 exceeded"), "{error}");
        assert_eq!(visits, 0);
        assert_eq!(
            reads.load(Ordering::SeqCst),
            3,
            "refuse at the first crossing chunk, before the tail read"
        );
    }

    #[test]
    fn replay_host_limits_parse_explicit_overrides_and_reject_invalid_values() {
        let defaults = ReplayLimits::from_lookup(|_| Ok(None)).unwrap();
        assert_eq!(defaults.records, 32_000_000);
        assert_eq!(defaults.input_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(defaults.line_bytes, 16 * 1024 * 1024);
        for name in [
            "CAPACITY_RUNNER_REPLAY_MAX_RECORDS",
            "CAPACITY_RUNNER_REPLAY_MAX_BYTES",
            "CAPACITY_RUNNER_REPLAY_MAX_LINE_BYTES",
        ] {
            for value in ["", "0", "-1", "1.5", "18446744073709551616"] {
                let error =
                    ReplayLimits::from_lookup(|key| Ok((key == name).then(|| value.into())))
                        .err()
                        .expect("invalid limit");
                assert!(error.contains(name), "{error}");
            }
        }
        let overridden = ReplayLimits::from_lookup(|_| Ok(Some("17".into()))).unwrap();
        assert_eq!(
            (
                overridden.records,
                overridden.input_bytes,
                overridden.line_bytes
            ),
            (17, 17, 17)
        );
        assert!(ReplayLimits::from_lookup(|_| Err("lookup failed".into())).is_err());
    }

    #[test]
    fn replay_manifest_budget_refuses_large_input_before_json_decoding() {
        let output = tempfile::tempdir().unwrap();
        let path = output.path().join(MANIFEST_FILE);
        fs::write(&path, b"{       ").unwrap();
        for limits in [
            ReplayLimits {
                line_bytes: 7,
                ..ReplayLimits::default()
            },
            ReplayLimits {
                input_bytes: 7,
                ..ReplayLimits::default()
            },
        ] {
            let error = read_manifest_with_limits(&path, limits).unwrap_err();
            assert!(error.contains("manifest exceeds byte budget 7"), "{error}");
        }
        fs::write(&path, b"{      ").unwrap();
        let error = read_manifest_with_limits(
            &path,
            ReplayLimits {
                line_bytes: 7,
                ..ReplayLimits::default()
            },
        )
        .unwrap_err();
        assert!(
            error.contains("parse"),
            "exact budget reaches JSON decoding: {error}"
        );
    }

    #[test]
    fn replay_host_budget_overrules_archived_plan_budget_before_building() {
        let mut config = crate::scenario_config(crate::config::Encoding::V3Json);
        config.planned_record_budget = u64::MAX;
        let count = config.validate_planned_record_budget().unwrap();
        validate_replay_shape(
            &config,
            ReplayLimits {
                records: count,
                ..ReplayLimits::default()
            },
        )
        .unwrap();
        let error = validate_replay_shape(
            &config,
            ReplayLimits {
                records: count - 1,
                ..ReplayLimits::default()
            },
        )
        .unwrap_err();
        assert!(
            error.contains("CAPACITY_RUNNER_REPLAY_MAX_RECORDS"),
            "{error}"
        );
        config.warmup = std::time::Duration::ZERO;
        config.duration = std::time::Duration::ZERO;
        config.players_per_room = u32::MAX;
        assert!(validate_replay_shape(&config, ReplayLimits::default())
            .unwrap_err()
            .contains("peer count"));
    }

    #[test]
    fn replay_entrypoint_applies_host_admission_to_archived_plan_overrides() {
        let output = tempfile::tempdir().unwrap();
        let mut config = crate::scenario_config(crate::config::Encoding::V3Json);
        config.endpoint = Some(crate::host_evidence_fixture().endpoint);
        config.planned_record_budget = u64::MAX;
        let planned = config.validate_planned_record_budget().unwrap();
        let (plans, _) = build_run_shape(&config).unwrap();
        let records = crate::complete_records(&plans);
        write_manifest(
            output.path(),
            "bounded-replay",
            &config,
            WorkloadShape {
                senders: 4,
                recipients: 4,
                scheduled_sends_per_sender: 28,
                warmup_sends_per_sender: 4,
                measured_sends_per_sender: 24,
            },
            ServerIdentity {
                endpoint: config.endpoint.clone().unwrap(),
                pid: None,
                binary_sha256: None,
                binary_bytes: None,
                config_overlay_sha256: diagnostics::sha256_bytes(
                    &serde_json::to_vec(&config.server_overlay).unwrap(),
                ),
                config_provenance: ConfigProvenance::UnknownExternal,
            },
            BuildIdentity {
                toolchain: None,
                os: "fixture".into(),
                arch: "fixture".into(),
                kernel: None,
                features: Features::current(),
                clock: "fixture".into(),
                generator: "fixture".into(),
            },
        )
        .unwrap();
        let error = replay_with_limits(
            output.path(),
            ReplayLimits {
                records: planned - 1,
                ..ReplayLimits::default()
            },
        )
        .unwrap_err();
        assert!(
            error.contains("CAPACITY_RUNNER_REPLAY_MAX_RECORDS"),
            "refuse the shape before opening the absent delivery file: {error}"
        );
        write_deliveries(output.path(), &records).unwrap();
        let summary = replay_with_limits(
            output.path(),
            ReplayLimits {
                records: planned + 1,
                ..ReplayLimits::default()
            },
        )
        .unwrap();
        assert!(summary.valid, "{:?}", summary.reasons);
        assert_eq!(
            summary.totals.sent,
            crate::config::count_u64(records.sent.len())
        );
        assert_eq!(
            summary.totals.receipts,
            crate::config::count_u64(records.receipts.len())
        );
    }

    #[test]
    fn replay_budgets_keep_exact_delivery_evidence_and_verdicts() {
        let context = crate::unit_context();
        let output = tempfile::tempdir().unwrap();
        for defect in [
            "none",
            "missing",
            "duplicate",
            "misroute",
            "ordering",
            "gap",
        ] {
            let mut records = crate::complete_records(&context.plans);
            match defect {
                "missing" => {
                    records.receipts.remove(0);
                }
                "duplicate" => records.receipts.push(records.receipts[0].clone()),
                "misroute" => records.receipts[0].recipient = "unknown鱼".into(),
                "ordering" => records.receipts.swap(0, 1),
                "gap" => records.gaps.push(crate::gap_for(
                    "r0p1",
                    "r0p0",
                    0,
                    signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
                )),
                _ => {}
            }
            write_deliveries(output.path(), &records).unwrap();
            let path = output.path().join(DELIVERIES_FILE);
            let raw = fs::read(&path).unwrap();
            let limits = ReplayLimits {
                records: crate::config::count_u64(records.events().count()),
                input_bytes: crate::config::count_u64(raw.len()),
                line_bytes: raw
                    .split(|&byte| byte == b'\n')
                    .map(<[u8]>::len)
                    .max()
                    .unwrap(),
            };
            let replay = read_records_with_limits(output.path(), limits).unwrap();
            assert_eq!(
                serde_json::to_value(&records).unwrap(),
                serde_json::to_value(&replay).unwrap(),
                "{defect}"
            );
            let verdict = |records| {
                summarize(
                    &context.plans,
                    &context.roster,
                    records,
                    500_000,
                    96,
                    context.delivery_class,
                    &crate::schedule::ChurnPlan::default(),
                    None,
                )
            };
            let expected = verdict(&records);
            assert_eq!(expected.valid, defect == "none", "{defect}");
            assert_eq!(
                serde_json::to_value(expected).unwrap(),
                serde_json::to_value(verdict(&replay)).unwrap(),
                "{defect}"
            );
        }
    }

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
        let output = tempfile::tempdir().expect("output");
        std::os::unix::fs::symlink("/dev/full", output.path().join(SUMMARY_FILE))
            .expect("full device fixture");
        for value in [
            serde_json::json!({}),
            serde_json::json!({"large": "x".repeat(32_768)}),
        ] {
            results.push((
                SUMMARY_FILE,
                write_json(output.path().join(SUMMARY_FILE), &value),
            ));
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
    fn pretty_json_stream_preserves_exact_bytes_and_reports_failures() {
        let output = tempfile::tempdir().expect("output");
        let path = output.path().join(SUMMARY_FILE);
        for value in [
            serde_json::Value::Null,
            serde_json::json!({"large": "x".repeat(32_768)}),
            serde_json::json!({"escaped": "quote\"\\\n", "unicode": "未知", "empty": [],
                "nested": [{"max": u64::MAX, "min": i64::MIN, "float": 1.5}]}),
        ] {
            write_json(&path, &value).expect("stream pretty JSON");
            assert_eq!(
                fs::read(&path).expect("read JSON"),
                serde_json::to_vec_pretty(&value).unwrap()
            );
        }
        assert!(write_json(output.path().join("missing/summary.json"), &())
            .unwrap_err()
            .contains("create"));
        struct RefusesSerialization;
        impl serde::Serialize for RefusesSerialization {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("injected serialization failure"))
            }
        }
        let error = write_json(&path, &RefusesSerialization).unwrap_err();
        assert!(error.contains("serialize") && error.contains("injected serialization failure"));
    }

    #[test]
    #[ignore = "isolated summary serialization memory profile; run in a fresh process"]
    fn generator_summary_write_memory_profile() {
        let rooms = std::env::var("CAPACITY_SUMMARY_PROBE_ROOMS")
            .unwrap_or_else(|_| "999".into())
            .parse::<u32>()
            .expect("room count");
        assert!((1..=999).contains(&rooms));
        let streaming = std::env::var("CAPACITY_SUMMARY_PROBE_STREAMING").as_deref() == Ok("1");
        let context = crate::unit_context();
        let records = crate::complete_records(&context.plans);
        let mut summary = summarize(
            &context.plans,
            &context.roster,
            &records,
            500_000,
            96,
            context.delivery_class,
            &crate::schedule::ChurnPlan::default(),
            None,
        );
        let base = summary.per_recipient[0].clone();
        summary.per_recipient = (0..rooms)
            .flat_map(|room| {
                let base = &base;
                (0..16).map(move |player| {
                    let mut outcome = base.clone();
                    outcome.recipient = RunConfig::peer_name(room, player);
                    outcome.room = room;
                    outcome.received = (0..16)
                        .filter(|&sender| sender != player)
                        .map(|sender| (RunConfig::peer_name(room, sender).into(), 43_200))
                        .collect();
                    outcome
                })
            })
            .collect();
        let retained_rss = crate::diagnostics::resident_memory_bytes(std::process::id());
        let output = tempfile::tempdir().expect("output");
        let path = output.path().join(SUMMARY_FILE);
        let started = std::time::Instant::now();
        if streaming {
            write_json(&path, &summary).expect("stream summary");
        } else {
            let bytes = serde_json::to_vec_pretty(&summary).expect("encode summary");
            fs::write(&path, &bytes).expect("write summary");
            std::hint::black_box(bytes);
        }
        let elapsed_us = started.elapsed().as_micros();
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        let mut artifact = fs::File::open(&path).unwrap();
        let mut buffer = [0_u8; 8192];
        loop {
            let read = artifact.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        let sha256 = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        println!(
            "SUMMARY_WRITE_PROFILE {}",
            serde_json::json!({"rooms":rooms,
            "recipients":summary.per_recipient.len(), "streaming":streaming,
            "retained_rss_bytes":retained_rss, "elapsed_us":elapsed_us,
            "artifact_bytes":fs::metadata(&path).unwrap().len(), "sha256":sha256})
        );
        std::hint::black_box(summary);
    }

    // #775/#636: repeated names must borrow the replay line before pooling,
    // while escaped identities must still decode to their exact original text.
    #[test]
    fn replay_names_borrow_plain_text_and_preserve_escaped_identities() {
        let context = crate::unit_context();
        let records = crate::complete_records(&context.plans);
        for (name, borrows, unicode_escape) in [
            ("r0p0", true, false),
            ("未知", true, false),
            ("未知", false, true),
            ("quote\"\\\n", false, false),
        ] {
            for event in [
                DeliveryEvent::Sent(records.sent[0].clone()),
                DeliveryEvent::Receipt(records.receipts[0].clone()),
                DeliveryEvent::Gap(crate::records::GapEvent {
                    recipient: "r0p1".into(),
                    sender: "r0p0".into(),
                    epoch: 1,
                    from_seq: 1,
                    to_seq: 7,
                    reason: signal_fish_server::protocol::DeliveryGapReason::LatestSuperseded,
                }),
            ] {
                let mut value = serde_json::to_value(event).expect("event");
                value["sender"] = name.into();
                if value["event_kind"] == "receipt" || value["event_kind"] == "gap" {
                    value["recipient"] = name.into();
                }
                let mut line = serde_json::to_vec(&value).expect("replay line");
                if unicode_escape {
                    line = String::from_utf8(line)
                        .expect("UTF-8 JSON")
                        .replace("未知", "\\u672a\\u77e5")
                        .into_bytes();
                }
                let borrowed: DeliveryEvent<ReplayName<'_>> =
                    serde_json::from_slice(&line).expect("borrowed event");
                let names = match &borrowed {
                    DeliveryEvent::Sent(event) => vec![&event.sender],
                    DeliveryEvent::Receipt(event) => vec![&event.sender, &event.recipient],
                    DeliveryEvent::Gap(event) => vec![&event.sender, &event.recipient],
                    _ => unreachable!("fixture only contains sends, receipts, and gaps"),
                };
                for decoded in names {
                    assert_eq!(decoded.as_ref(), name);
                    assert_eq!(matches!(decoded.0, std::borrow::Cow::Borrowed(_)), borrows);
                }
                let mut actual = RunRecords::default();
                let mut pool = crate::records::ReceiptNames::default();
                append_delivery_event(&mut actual, &mut pool, borrowed);
                // The next line may overwrite or release the entire read buffer.
                drop(line);
                assert_eq!(
                    serde_json::to_value(actual.events().next().expect("pooled event"))
                        .expect("pooled JSON"),
                    value
                );
            }
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
        for line in &lines {
            let direct: DeliveryEvent<String> =
                serde_json::from_slice(line).expect("canonical events use direct typed decoding");
            assert_eq!(
                serde_json::to_value(&direct).expect("direct event"),
                serde_json::from_slice::<Value>(line).expect("original event")
            );
        }
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
        // Repeated identity fields also retain the last value through fallback.
        for event in [
            DeliveryEvent::Sent(expected.sent[0].clone()),
            DeliveryEvent::Receipt(expected.receipts[0].clone()),
            DeliveryEvent::Gap(expected.gaps[0].clone()),
        ] {
            let mut raw = serde_json::to_string(&event).expect("event");
            raw.pop();
            raw.push_str(r#","sender":"最终","recipient":"最终"}"#);
            fs::write(output.path().join(DELIVERIES_FILE), &raw).expect("duplicate names");
            let actual = read_records(output.path()).expect("last identity wins");
            let mut value = serde_json::to_value(event).expect("expected event");
            value["sender"] = "最终".into();
            if value["event_kind"] == "receipt" || value["event_kind"] == "gap" {
                value["recipient"] = "最终".into();
            }
            assert_eq!(
                serde_json::to_value(actual.events().next().unwrap()).unwrap(),
                value
            );
        }
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
    fn interval_cpu_throttling_evidence_survives_round_trip_and_old_artifacts() {
        let old = serde_json::json!({"t_us":9,"counters":{}});
        let sample: IntervalSample = serde_json::from_value(old).unwrap();
        let old_output = serde_json::to_value(sample).unwrap();
        assert!(old_output
            .get("generator_cgroup_cpu")
            .is_none_or(Value::is_null));
        let stats = serde_json::json!({"path":"/game", "usage_usec":123,
            "nr_periods":10, "nr_throttled":2, "throttled_usec":45});
        let mut input = old_output;
        input["server_cgroup_cpu"] = stats.clone();
        input["generator_cgroup_cpu"] = stats.clone();
        let output = tempfile::tempdir().unwrap();
        let parsed: IntervalSample = serde_json::from_value(input).unwrap();
        write_intervals(output.path(), &[parsed]).unwrap();
        let restored = read_intervals(output.path()).unwrap();
        let actual = serde_json::to_value(&restored[0]).unwrap();
        assert_eq!(actual["server_cgroup_cpu"], stats);
        assert_eq!(actual["generator_cgroup_cpu"], stats);
    }

    #[test]
    fn interval_thread_scheduling_preserves_raw_counters_and_unknown_kernel_switch() {
        let mut input = serde_json::json!({"t_us":9,"counters":{}});
        let old: IntervalSample = serde_json::from_value(input.clone()).unwrap();
        assert!(old.generator_scheduling.is_none());
        let scheduling = serde_json::json!({"schedstats_enabled":null,"threads":[
            {"tid":17,"name":"tokio-runtime-w","start_time_ticks":123,"state":"S",
             "wait_channel":"futex_wait_queue","counters":{"runtime_ns":10,
             "runqueue_wait_ns":20,"timeslices":30}},
            {"tid":18,"name":null,"start_time_ticks":null,"state":null,
             "wait_channel":null,"counters":null}]});
        input["generator_scheduling"] = scheduling.clone();
        let parsed: IntervalSample = serde_json::from_value(input).unwrap();
        let output = tempfile::tempdir().unwrap();
        write_intervals(output.path(), &[parsed]).unwrap();
        let restored = read_intervals(output.path()).unwrap();
        assert_eq!(
            serde_json::to_value(&restored[0]).unwrap()["generator_scheduling"],
            scheduling
        );
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
