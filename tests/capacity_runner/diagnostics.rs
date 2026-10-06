//! Best-effort host, build, and server-process diagnostics for the manifest
//! and interval samples.
//!
//! Every diagnostic is honest about absence: a value the host cannot provide
//! is `None` (recorded as `null` in artifacts), never guessed. The server's
//! delivery counters themselves come from the scrape helper shared with the
//! multiprocess suites.

use std::path::Path;

use sha2::{Digest, Sha256};

/// The server counters and gauges the interval sampler records: the
/// delivery-contract counters, the ingress/egress byte pair (the fan-out
/// amplification pair), and the scrape-time outbound-queue posture gauges
/// (same names the strict delivery suites assert on, parsed leniently here).
pub const TRACKED_COUNTERS: &[&str] = &[
    "signal_fish_websocket_delivery_attempts_total",
    "signal_fish_websocket_deliveries_enqueued_total",
    "signal_fish_websocket_deliveries_channel_closed_total",
    "signal_fish_websocket_deliveries_canceled_total",
    "signal_fish_websocket_messages_dropped_total",
    "signal_fish_websocket_slow_consumer_disconnects_total",
    "signal_fish_websocket_backpressure_events_total",
    "signal_fish_connections_active",
    "signal_fish_relay_bytes_total",
    "signal_fish_websocket_egress_bytes_total",
    "signal_fish_websocket_queue_depth",
    "signal_fish_websocket_queue_oldest_age_milliseconds",
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

/// Cumulative CPU seconds (user + system) a process has consumed
/// (`utime + stime` from `/proc/<pid>/stat`, normalized by the kernel's
/// fixed `USER_HZ = 100` stub — Linux has reported these fields in 100
/// ticks-per-second units on every proc(5) release since 2.6). `None`
/// elsewhere or when the process is gone — recorded as unavailable, never
/// guessed.
#[cfg(target_os = "linux")]
pub fn process_cpu_seconds(pid: u32) -> Option<f64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    cpu_seconds_from_stat(&stat)
}

/// Parse `utime + stime` (CPU seconds at the kernel's fixed `USER_HZ = 100`
/// stub) out of a `/proc/<pid>/stat` body.
#[cfg(target_os = "linux")]
fn cpu_seconds_from_stat(stat: &str) -> Option<f64> {
    const USER_HZ: f64 = 100.0;
    // The second field (comm) is parenthesized and may contain spaces, so
    // scan past its closing parenthesis before splitting on whitespace.
    let after_comm = stat.rsplit_once(')').map(|(_, rest)| rest)?;
    let fields = after_comm.split_whitespace().collect::<Vec<_>>();
    // Fields after comm are 1-based in proc(5); utime is 14, stime is 15, so
    // the 0-based positions in this slice are 11 and 12.
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some((utime + stime) as f64 / USER_HZ)
}

/// Cumulative CPU seconds of a process (`None` off-Linux — recorded as
/// unavailable).
#[cfg(not(target_os = "linux"))]
pub fn process_cpu_seconds(_pid: u32) -> Option<f64> {
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

/// Lenient labeled Prometheus sample parse: `Some(value)` when a sample of
/// `name` carries exactly the wanted `{label="value"}` pairs (order does not
/// matter), `None` when absent. A malformed pair disqualifies its line, not
/// the whole scan. Records the server's per-class delivery outcomes
/// (`signal_fish_websocket_delivery_class_outcomes_total`) for the class a
/// run measures.
pub fn parse_labeled_counter(text: &str, name: &str, labels: &[(&str, &str)]) -> Option<u64> {
    let mut wanted: Vec<(String, String)> = labels
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    wanted.sort();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(sample), Some(raw_value)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Some(open) = sample.find('{') else {
            continue;
        };
        if sample[..open] != *name {
            continue;
        }
        let inner = sample[open + 1..].trim_end_matches('}');
        let Some(mut pairs) = inner
            .split("\",")
            .map(|pair| -> Option<(String, String)> {
                let (key, value) = pair.split_once('=')?;
                Some((key.to_string(), value.trim_matches('"').to_string()))
            })
            .collect::<Option<Vec<(String, String)>>>()
        else {
            continue; // malformed pair: skip the line, keep scanning
        };
        pairs.sort();
        if pairs == wanted {
            return raw_value.parse::<u64>().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_labeled_counter_matches_real_exposition_lines_order_insensitively() {
        let text = "# HELP signal_fish_websocket_delivery_class_outcomes_total outcomes\n\
                    # TYPE signal_fish_websocket_delivery_class_outcomes_total counter\n\
                    signal_fish_websocket_delivery_class_outcomes_total{class=\"latest\",outcome=\"superseded\"} 41\n\
                    signal_fish_websocket_delivery_class_outcomes_total{class=\"volatile\",outcome=\"dropped\"} 7\n";
        let name = "signal_fish_websocket_delivery_class_outcomes_total";
        assert_eq!(
            parse_labeled_counter(
                text,
                name,
                &[("class", "latest"), ("outcome", "superseded")]
            ),
            Some(41)
        );
        // The exposition format does not promise label order.
        assert_eq!(
            parse_labeled_counter(
                text,
                name,
                &[("outcome", "superseded"), ("class", "latest")]
            ),
            Some(41)
        );
        assert_eq!(
            parse_labeled_counter(text, name, &[("class", "volatile"), ("outcome", "dropped")]),
            Some(7)
        );
        // A different label set on the same sample name is not a match.
        assert_eq!(
            parse_labeled_counter(text, name, &[("class", "latest"), ("outcome", "dropped")]),
            None
        );
    }

    #[test]
    fn parse_labeled_counter_skips_a_malformed_line_without_poisoning_the_scan() {
        let text = "metric_total{broken=\"unterminated} 5\n\
                    metric_total{class=\"latest\",outcome=\"dropped\"} 9\n";
        assert_eq!(
            parse_labeled_counter(
                text,
                "metric_total",
                &[("class", "latest"), ("outcome", "dropped")]
            ),
            Some(9)
        );
        // A pair without '=' disqualifies its own line only.
        let text = "metric_total{class=latest,outcome=\"dropped\"} 3\n\
                    metric_total{class=\"latest\",outcome=\"dropped\"} 4\n";
        assert_eq!(
            parse_labeled_counter(
                text,
                "metric_total",
                &[("class", "latest"), ("outcome", "dropped")]
            ),
            Some(4)
        );
        assert_eq!(
            parse_labeled_counter("no labels here 1", "no labels here", &[]),
            None
        );
    }

    // A realistic `/proc/<pid>/stat` body: the comm field is a quoted name
    // that may contain spaces and parentheses, and the two CPU fields ride
    // the fixed proc(5) positions past it. 52 user ticks + 7 system ticks at
    // the USER_HZ=100 stub is exactly 0.59 CPU seconds.
    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_seconds_from_stat_sums_utime_and_stime_past_a_spaced_comm_name() {
        let stat = "4242 (signal-fish-serve) S 1 4242 4242 0 -1 4194560 \
                    12345 0 0 0 52 7 0 0 20 0 8 0 1234567 123456789 9999 \
                    18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0\n";
        assert_eq!(cpu_seconds_from_stat(stat), Some(0.59));
        // A body missing the CPU fields (truncated read) is unavailable.
        assert_eq!(cpu_seconds_from_stat("1 (x) S 1 1 1 0 -1 0"), None);
        // Non-numeric CPU fields are unavailable, never guessed.
        assert_eq!(
            cpu_seconds_from_stat(
                "1 (x) S 1 1 1 0 -1 0 0 0 0 0 x y 0 0 20 0 1 0 1 1 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0",
            ),
            None
        );
    }

    // The live sampler reads the calling process and only ever moves
    // forward; real CPU work advances the counter.
    #[cfg(target_os = "linux")]
    #[test]
    fn process_cpu_seconds_advances_for_the_calling_process_and_absent_pids_are_none() {
        let before = process_cpu_seconds(std::process::id()).expect("own /proc stat is readable");
        // 150 ms = 15 USER_HZ ticks: a wide multiple of the counter's 10 ms
        // granularity so even a heavily oversubscribed runner cannot
        // schedule this thread for less than one tick (zero-flake policy).
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(150);
        while std::time::Instant::now() < deadline {
            std::hint::spin_loop();
        }
        let after = process_cpu_seconds(std::process::id()).expect("own /proc stat is readable");
        assert!(
            after > before,
            "150 ms of busy work must advance the process CPU counter, got {before} -> {after}"
        );
        // A pid the kernel can never have assigned is unavailable, not zero.
        assert_eq!(process_cpu_seconds(u32::MAX), None);
    }
}
