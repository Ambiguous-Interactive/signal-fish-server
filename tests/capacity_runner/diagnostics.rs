//! Best-effort host, build, and server-process diagnostics for the manifest
//! and interval samples.
//!
//! Every diagnostic is honest about absence: a value the host cannot provide
//! is `None` (recorded as `null` in artifacts), never guessed. The server's
//! delivery counters themselves come from the scrape helper shared with the
//! multiprocess suites.

use std::path::Path;

use sha2::{Digest, Sha256};

/// The server counters the interval sampler records (delivery-contract
/// counters plus the active-connections gauge; same names the strict
/// delivery suites assert on, parsed leniently here).
pub const TRACKED_COUNTERS: &[&str] = &[
    "signal_fish_websocket_delivery_attempts_total",
    "signal_fish_websocket_deliveries_enqueued_total",
    "signal_fish_websocket_deliveries_channel_closed_total",
    "signal_fish_websocket_deliveries_canceled_total",
    "signal_fish_websocket_messages_dropped_total",
    "signal_fish_websocket_slow_consumer_disconnects_total",
    "signal_fish_websocket_backpressure_events_total",
    "signal_fish_connections_active",
];

/// SHA-256 of a file, hex-encoded (server binary and config overlay hashes).
pub fn sha256_file(path: impl AsRef<Path>) -> Result<(String, u64), String> {
    let path = path.as_ref();
    let raw = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok((sha256_bytes(&raw), raw.len() as u64))
}

/// SHA-256 of in-memory bytes, hex-encoded (config overlay hash).
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `rustc --version` of the toolchain that runs the generator (best effort;
/// resolved from PATH like an operator would).
pub fn toolchain_version() -> Option<String> {
    let output = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
}

/// Kernel release (`uname -r` on Unix; `None` elsewhere — recorded as
/// unavailable rather than guessed).
pub fn kernel_release() -> Option<String> {
    if !cfg!(unix) {
        return None;
    }
    let output = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()?;
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
}

/// Resident set size of a process in bytes (VmRSS on Linux; `None`
/// elsewhere or when the process is gone).
#[cfg(target_os = "linux")]
pub fn resident_memory_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kilobytes = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
    Some(kilobytes * 1024)
}

/// Resident set size of a process in bytes (`None` off-Linux — recorded as
/// unavailable).
#[cfg(not(target_os = "linux"))]
pub fn resident_memory_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Cgroup memory usage of a process in bytes (cgroup v2 then v1, best
/// effort; `None` when the process's cgroup cannot be read).
#[cfg(target_os = "linux")]
pub fn cgroup_memory_bytes(pid: u32) -> Option<u64> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let relative = cgroup
        .lines()
        .find_map(|line| {
            // v2 lines are `0::<path>`; v1 memory lines are `N:memory:<path>`.
            let mut fields = line.split(':');
            let hierarchy = fields.next()?;
            let controller = fields.next()?;
            let path = fields.next()?.trim_start_matches('/').to_string();
            if hierarchy == "0" || controller == "memory" {
                Some(path)
            } else {
                None
            }
        })
        .filter(|path| !path.is_empty())?;

    let root = Path::new("/sys/fs/cgroup");
    let v2 = root.join(&relative).join("memory.current");
    if let Ok(text) = std::fs::read_to_string(&v2) {
        if let Ok(bytes) = text.trim().parse::<u64>() {
            return Some(bytes);
        }
    }
    // v1 exposes usage through the memory controller mount, which this
    // best-effort probe does not resolve; absence is recorded as None.
    let _ = root;
    None
}

/// Cgroup memory usage of a process in bytes (`None` off-Linux).
#[cfg(not(target_os = "linux"))]
pub fn cgroup_memory_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Lenient Prometheus sample parse: `Some(value)` when the named
/// un-labelled sample exists, `None` when absent — the sampler records
/// unavailable counters instead of failing the way the strict delivery-suite
/// parser does (a capacity run must survive a server that stopped answering
/// mid-run and record the gap).
pub fn parse_counter(text: &str, name: &str) -> Option<u64> {
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(sample_name), Some(raw_value)) = (parts.next(), parts.next()) else {
            continue;
        };
        if sample_name != name {
            continue;
        }
        return raw_value.parse::<u64>().ok();
    }
    None
}
