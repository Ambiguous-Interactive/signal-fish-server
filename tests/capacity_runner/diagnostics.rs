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
/// amplification pair), the scrape-time outbound-queue posture gauges, the
/// scrape-time live-state gauges (rooms, occupants, pending publications,
/// pending reconnections, replay rings), and the maintenance-sweep pair
/// (same names the strict delivery suites assert on, parsed leniently
/// here).
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
    "signal_fish_rooms_live",
    "signal_fish_room_occupants_live",
    "signal_fish_cleanup_pending_publications",
    "signal_fish_reconnection_pending",
    "signal_fish_replay_rooms_retained",
    "signal_fish_replay_events_retained",
    "signal_fish_maintenance_sweeps_total",
    "signal_fish_maintenance_last_duration_milliseconds",
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

/// Cgroup v2 memory usage in bytes at the conventional Linux mount.
/// Returns `None` for v1, unreadable counters, or an unavailable mount.
#[cfg(target_os = "linux")]
pub fn cgroup_memory_bytes(pid: u32) -> Option<u64> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    cgroup_memory_bytes_from(&cgroup, Path::new("/sys/fs/cgroup"))
}

#[cfg(target_os = "linux")]
fn cgroup_memory_bytes_from(cgroup: &str, root: &Path) -> Option<u64> {
    let relative = cgroup.lines().find_map(|line| line.strip_prefix("0::/"))?;
    let text = std::fs::read_to_string(root.join(relative).join("memory.current")).ok()?;
    text.trim().parse().ok()
}

/// Cgroup memory usage of a process in bytes (`None` off-Linux).
#[cfg(not(target_os = "linux"))]
pub fn cgroup_memory_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Cumulative cgroup v2 CPU counters. Usage includes descendant groups;
/// bandwidth counters describe this group's limit, not ancestor throttling.
/// These values describe the group, which may contain other processes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CgroupCpuStats {
    pub path: String,
    pub usage_usec: u64,
    pub nr_periods: Option<u64>,
    pub nr_throttled: Option<u64>,
    pub throttled_usec: Option<u64>,
}

/// Read CPU counters at the conventional Linux cgroup v2 mount.
/// Missing bandwidth counters remain unavailable when the controller is off.
#[cfg(target_os = "linux")]
pub fn cgroup_cpu_stats(pid: u32) -> Option<CgroupCpuStats> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    cgroup_cpu_stats_from(&cgroup, Path::new("/sys/fs/cgroup"))
}

#[cfg(target_os = "linux")]
fn cgroup_cpu_stats_from(cgroup: &str, root: &Path) -> Option<CgroupCpuStats> {
    let path = cgroup.lines().find_map(|line| line.strip_prefix("0::/"))?;
    let text = std::fs::read_to_string(root.join(path).join("cpu.stat")).ok()?;
    let mut usage_usec = None;
    let mut nr_periods = None;
    let mut nr_throttled = None;
    let mut throttled_usec = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let target = match fields.next()? {
            "usage_usec" => &mut usage_usec,
            "nr_periods" => &mut nr_periods,
            "nr_throttled" => &mut nr_throttled,
            "throttled_usec" => &mut throttled_usec,
            _ => continue,
        };
        if target.is_some() {
            return None;
        }
        *target = Some(fields.next()?.parse().ok()?);
        if fields.next().is_some() {
            return None;
        }
    }
    Some(CgroupCpuStats {
        path: format!("/{path}"),
        usage_usec: usage_usec?,
        nr_periods,
        nr_throttled,
        throttled_usec,
    })
}

/// Cgroup CPU counters are unavailable off Linux.
#[cfg(not(target_os = "linux"))]
pub fn cgroup_cpu_stats(_pid: u32) -> Option<CgroupCpuStats> {
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

/// A non-atomic snapshot of the generator's OS threads. Match a thread by
/// `(tid, start_time_ticks)` between snapshots; exited threads disappear.
/// Compare counters only when both snapshots have known start-time ticks.
/// Counters are raw kernel values, not proof that a Tokio task was runnable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcessScheduling {
    /// Kernel switch when exposed; `None` means unknown, not enabled.
    pub schedstats_enabled: Option<bool>,
    pub threads: Vec<ThreadScheduling>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ThreadScheduling {
    pub tid: u32,
    pub name: Option<String>,
    pub start_time_ticks: Option<u64>,
    /// Instantaneous kernel state, such as R (runnable) or S (sleeping).
    pub state: Option<String>,
    /// Kernel wait channel, absent when unreadable or hidden (`0`).
    pub wait_channel: Option<String>,
    /// Raw schedstat counters; an unreadable or malformed file stays null.
    pub counters: Option<ThreadSchedulingCounters>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ThreadSchedulingCounters {
    pub runtime_ns: u64,
    pub runqueue_wait_ns: u64,
    pub timeslices: u64,
}

/// Read every current thread, rather than only the process leader. A thread
/// can exit between enumeration and file reads; retain its TID and nulls.
#[cfg(target_os = "linux")]
pub fn process_scheduling(pid: u32) -> Option<ProcessScheduling> {
    process_scheduling_from(pid, Path::new("/proc"))
}

#[cfg(target_os = "linux")]
fn process_scheduling_from(pid: u32, proc_root: &Path) -> Option<ProcessScheduling> {
    let task_root = proc_root.join(pid.to_string()).join("task");
    let mut tids = std::fs::read_dir(&task_root)
        .ok()?
        .map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .collect::<Option<Vec<_>>>()?;
    tids.sort_unstable();
    let threads = tids
        .into_iter()
        .map(|tid| {
            let task = task_root.join(tid.to_string());
            let identity = std::fs::read_to_string(task.join("stat"))
                .ok()
                .and_then(|text| thread_identity_from_stat(&text));
            ThreadScheduling {
                tid,
                name: std::fs::read_to_string(task.join("comm"))
                    .ok()
                    .map(|name| name.trim_end_matches('\n').to_string()),
                start_time_ticks: identity.as_ref().map(|(_, start)| *start),
                state: identity.map(|(state, _)| state),
                wait_channel: std::fs::read_to_string(task.join("wchan"))
                    .ok()
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty() && text != "0"),
                counters: std::fs::read_to_string(task.join("schedstat"))
                    .ok()
                    .and_then(|text| thread_scheduling_counters(&text)),
            }
        })
        .collect();
    let schedstats_enabled = std::fs::read_to_string(proc_root.join("sys/kernel/sched_schedstats"))
        .ok()
        .and_then(|text| match text.trim() {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        });
    Some(ProcessScheduling {
        schedstats_enabled,
        threads,
    })
}

#[cfg(target_os = "linux")]
fn thread_identity_from_stat(stat: &str) -> Option<(String, u64)> {
    let (_, rest) = stat.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?;
    if state.len() != 1 {
        return None;
    }
    // starttime is field 22; state is field 3.
    let start_time_ticks = fields.nth(18)?.parse().ok()?;
    Some((state.to_string(), start_time_ticks))
}

#[cfg(target_os = "linux")]
fn thread_scheduling_counters(text: &str) -> Option<ThreadSchedulingCounters> {
    let mut fields = text.split_whitespace();
    let counters = ThreadSchedulingCounters {
        runtime_ns: fields.next()?.parse().ok()?,
        runqueue_wait_ns: fields.next()?.parse().ok()?,
        timeslices: fields.next()?.parse().ok()?,
    };
    fields.next().is_none().then_some(counters)
}

#[cfg(not(target_os = "linux"))]
pub fn process_scheduling(_pid: u32) -> Option<ProcessScheduling> {
    None
}

/// Kernel socket-memory accounting for a process's network namespace
/// ([`SocketMemoryPages`], from `/proc/<pid>/net/sockstat`), summed across
/// IPv4 and IPv6. Units are kernel memory pages — the kernel's own
/// accounting unit for socket-buffer memory; the capacity host records the
/// page size as an environment fact beside the run. `None` off-Linux or
/// when the file is unreadable — recorded as unavailable, never guessed.
#[cfg(target_os = "linux")]
pub fn socket_memory_pages(pid: u32) -> Option<SocketMemoryPages> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/net/sockstat")).ok()?;
    parse_socket_memory_pages(&text)
}

/// Socket-memory accounting of a process (`None` off-Linux — recorded as
/// unavailable).
#[cfg(not(target_os = "linux"))]
pub fn socket_memory_pages(_pid: u32) -> Option<SocketMemoryPages> {
    None
}

/// TCP and UDP socket-buffer memory of a process's network namespace, in
/// kernel memory pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketMemoryPages {
    /// `mem` pages summed over the `TCP:` and `TCP6:` sockstat lines.
    pub tcp: u64,
    /// `mem` pages summed over the `UDP:` and `UDP6:` sockstat lines.
    pub udp: u64,
}

/// Parse the TCP and UDP `mem` page totals out of a `sockstat` body. Each
/// protocol family line carries `mem <pages>`; the `FRAG:` line uses
/// `memory` (fragmentation queues, not socket buffers) and a bare `mem` key
/// never matches it.
#[cfg(target_os = "linux")]
pub fn parse_socket_memory_pages(text: &str) -> Option<SocketMemoryPages> {
    let mut tcp: Option<u64> = None;
    let mut udp: Option<u64> = None;
    for line in text.lines() {
        let family = if line.starts_with("TCP:") || line.starts_with("TCP6:") {
            &mut tcp
        } else if line.starts_with("UDP:") || line.starts_with("UDP6:") {
            &mut udp
        } else {
            continue;
        };
        let mut fields = line.split_whitespace();
        while let Some(field) = fields.next() {
            if field == "mem" {
                if let Some(pages) = fields.next().and_then(|raw| raw.parse::<u64>().ok()) {
                    *family = Some(family.unwrap_or(0).saturating_add(pages));
                }
                break;
            }
        }
    }
    Some(SocketMemoryPages {
        tcp: tcp?,
        udp: udp?,
    })
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

    #[cfg(target_os = "linux")]
    #[test]
    fn thread_scheduling_preserves_threads_waits_and_disabled_or_missing_counters() {
        let proc_root = tempfile::tempdir().unwrap();
        let tasks = proc_root.path().join("42/task");
        std::fs::create_dir_all(&tasks).unwrap();
        for tid in [30, 10, 20] {
            std::fs::create_dir(tasks.join(tid.to_string())).unwrap();
        }
        // Include parentheses and spaces in comm; starttime is field 22.
        let stat = "10 (tokio ) worker) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345 23";
        std::fs::write(tasks.join("10/stat"), stat).unwrap();
        std::fs::write(tasks.join("10/comm"), "tokio ) worker\n").unwrap();
        std::fs::write(tasks.join("10/schedstat"), "0 0 0\n").unwrap();
        std::fs::write(tasks.join("10/wchan"), "futex_wait_queue\n").unwrap();
        std::fs::write(tasks.join("20/schedstat"), "12 34 56\n").unwrap();
        std::fs::write(tasks.join("20/wchan"), "0\n").unwrap();
        std::fs::write(tasks.join("30/schedstat"), "12 malformed 56\n").unwrap();
        let switch = proc_root.path().join("sys/kernel");
        std::fs::create_dir_all(&switch).unwrap();
        for (setting, expected) in [("0\n", Some(false)), ("1\n", Some(true)), ("x", None)] {
            std::fs::write(switch.join("sched_schedstats"), setting).unwrap();
            let snapshot = process_scheduling_from(42, proc_root.path()).unwrap();
            assert_eq!(snapshot.schedstats_enabled, expected);
            assert_eq!(
                snapshot
                    .threads
                    .iter()
                    .map(|thread| thread.tid)
                    .collect::<Vec<_>>(),
                [10, 20, 30]
            );
            let sleeping = &snapshot.threads[0];
            assert_eq!(sleeping.name.as_deref(), Some("tokio ) worker"));
            assert_eq!(sleeping.state.as_deref(), Some("S"));
            assert_eq!(sleeping.start_time_ticks, Some(12345));
            assert_eq!(sleeping.wait_channel.as_deref(), Some("futex_wait_queue"));
            assert_eq!(sleeping.counters.as_ref().unwrap().runqueue_wait_ns, 0);
            let measured = &snapshot.threads[1];
            assert_eq!(measured.counters.as_ref().unwrap().runtime_ns, 12);
            assert_eq!(measured.counters.as_ref().unwrap().runqueue_wait_ns, 34);
            assert_eq!(measured.counters.as_ref().unwrap().timeslices, 56);
            assert_eq!(measured.state, None);
            assert_eq!(measured.start_time_ticks, None);
            assert_eq!(measured.wait_channel, None);
            assert_eq!(snapshot.threads[2].counters, None);
        }
        std::fs::remove_file(switch.join("sched_schedstats")).unwrap();
        assert_eq!(
            process_scheduling_from(42, proc_root.path())
                .unwrap()
                .schedstats_enabled,
            None
        );
        assert_eq!(process_scheduling_from(43, proc_root.path()), None);
        std::fs::remove_dir_all(tasks).unwrap();
        assert_eq!(process_scheduling_from(42, proc_root.path()), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn thread_scheduling_rejects_malformed_and_overflowed_kernel_fields() {
        for text in [
            "",
            "1 2",
            "1 2 3 4",
            "1 x 3",
            "-1 2 3",
            "1 18446744073709551616 3",
        ] {
            assert_eq!(thread_scheduling_counters(text), None, "{text:?}");
        }
        for text in ["", "1 (worker) S 1", "1 (worker) RUNNING 1 2 3"] {
            assert_eq!(thread_identity_from_stat(text), None, "{text:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn thread_scheduling_live_snapshot_includes_a_nonleader_thread_and_measures_read_cost() {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("sched-control".into())
            .spawn(move || {
                let thread_path = std::fs::read_link("/proc/thread-self").unwrap();
                let tid: u32 = thread_path
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .parse()
                    .unwrap();
                ready_tx.send(tid).unwrap();
                stop_rx.recv().unwrap();
            })
            .unwrap();
        let tid = ready_rx.recv().unwrap();
        let start = std::time::Instant::now();
        for _ in 0..20 {
            let snapshot = process_scheduling(std::process::id()).unwrap();
            let observed = snapshot
                .threads
                .iter()
                .find(|thread| thread.tid == tid)
                .unwrap();
            assert_eq!(observed.name.as_deref(), Some("sched-control"));
            assert!(observed.start_time_ticks.is_some());
            assert!(observed.counters.is_some());
        }
        eprintln!("20 live thread snapshots: {:?}", start.elapsed());
        stop_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(process_scheduling(u32::MAX), None);
    }

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

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_cpu_reads_root_and_nested_counters_with_explicit_missing_fields() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("game")).unwrap();
        let full = "usage_usec 123\nuser_usec 100\nsystem_usec 23\nnr_periods 10\nnr_throttled 2\nthrottled_usec 45\nfuture_counter 99\n";
        std::fs::write(root.path().join("cpu.stat"), full).unwrap();
        std::fs::write(root.path().join("game/cpu.stat"), "usage_usec 0\n").unwrap();
        let measured = cgroup_cpu_stats_from("0::/\n", root.path()).unwrap();
        assert_eq!(measured.path, "/");
        assert_eq!(measured.usage_usec, 123);
        assert_eq!(measured.nr_periods, Some(10));
        assert_eq!(measured.nr_throttled, Some(2));
        assert_eq!(measured.throttled_usec, Some(45));
        let nested = cgroup_cpu_stats_from("5:cpu:/legacy\n0::/game\n", root.path()).unwrap();
        assert_eq!(nested.path, "/game");
        assert_eq!(nested.usage_usec, 0);
        assert_eq!(nested.nr_periods, None);
        assert_eq!(nested.nr_throttled, None);
        assert_eq!(nested.throttled_usec, None);
        for cgroup in ["", "5:cpu:/game\n", "0::\n", "0::/missing\n"] {
            assert_eq!(
                cgroup_cpu_stats_from(cgroup, root.path()),
                None,
                "{cgroup:?}"
            );
        }
        for malformed in [
            "",
            "nr_periods 1\n",
            "usage_usec x\n",
            "usage_usec -1\n",
            "usage_usec 18446744073709551616\n",
            "usage_usec 1\nusage_usec 2\n",
            "usage_usec 1 2\n",
            "usage_usec 1\nnr_throttled x\n",
            "usage_usec 1\nthrottled_usec\n",
        ] {
            std::fs::write(root.path().join("cpu.stat"), malformed).unwrap();
            assert_eq!(
                cgroup_cpu_stats_from("0::/\n", root.path()),
                None,
                "{malformed:?}"
            );
        }
        assert_eq!(cgroup_cpu_stats(u32::MAX), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_memory_reads_v2_root_and_nested_groups_without_inventing_absent_values() {
        let root = tempfile::tempdir().expect("cgroup fixture");
        std::fs::write(root.path().join("memory.current"), "123456\n").unwrap();
        std::fs::create_dir(root.path().join("game")).unwrap();
        std::fs::write(root.path().join("game/memory.current"), "654321\n").unwrap();
        for (body, expected) in [
            ("0::/\n", Some(123456)),
            ("0::/game\n", Some(654321)),
            ("0::/missing\n", None),
            ("5:memory:/legacy\n0::/game\n", Some(654321)),
            ("5:memory:/\n", None),
            ("0:memory:/\n", None),
            ("0::\n", None),
            ("invalid\n", None),
            ("", None),
        ] {
            assert_eq!(
                cgroup_memory_bytes_from(body, root.path()),
                expected,
                "{body:?}"
            );
        }
        std::fs::write(root.path().join("memory.current"), "0\n").unwrap();
        assert_eq!(cgroup_memory_bytes_from("0::/\n", root.path()), Some(0));
        std::fs::write(root.path().join("memory.current"), "unavailable\n").unwrap();
        assert_eq!(cgroup_memory_bytes_from("0::/\n", root.path()), None);
        assert_eq!(cgroup_memory_bytes(u32::MAX), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_memory_reads_the_live_root_counter_when_available() {
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap();
        if cgroup.lines().any(|line| line == "0::/")
            && std::fs::read_to_string("/sys/fs/cgroup/memory.current")
                .ok()
                .and_then(|text| text.trim().parse::<u64>().ok())
                .is_some()
        {
            let measured = cgroup_memory_bytes(std::process::id());
            assert!(measured.is_some(), "readable root cgroup must be sampled");
            eprintln!("live root cgroup memory sample: {measured:?} bytes");
        }
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

    // A realistic `/proc/<pid>/net/sockstat` body: IPv4 and IPv6 lines each
    // carry `mem <pages>`; the `FRAG:` line uses `memory` (fragmentation
    // queues, not socket buffers) and must not be swept into the totals.
    #[cfg(target_os = "linux")]
    #[test]
    fn parse_socket_memory_pages_sums_both_families_and_skips_frag_memory() {
        let text = "sockets: used 351\n\
                    TCP: inuse 9 orphan 0 tw 2 alloc 11 mem 3\n\
                    UDP: inuse 5 mem 2\n\
                    UDPLITE: inuse 0\n\
                    RAW: inuse 0\n\
                    FRAG: inuse 0 memory 7\n\
                    TCP6: inuse 2 mem 4\n\
                    UDP6: inuse 4 mem 1\n";
        let parsed = parse_socket_memory_pages(text).expect("both families present");
        assert_eq!(parsed.tcp, 7, "TCP 3 + TCP6 4");
        assert_eq!(parsed.udp, 3, "UDP 2 + UDP6 1");
        // A truncated body without a family line is unavailable, never zero.
        assert_eq!(parse_socket_memory_pages("sockets: used 1\n"), None);
        assert_eq!(
            parse_socket_memory_pages("TCP: inuse 1 alloc 1 mem x\n"),
            None
        );
        // The live sampler reads the calling process's namespace; a pid the
        // kernel can never have assigned is unavailable.
        assert_eq!(socket_memory_pages(u32::MAX), None);
    }

    // The calling process always has a network namespace on Linux, so the
    // live read answers with both families recorded (values may be zero).
    #[cfg(target_os = "linux")]
    #[test]
    fn socket_memory_pages_answers_for_the_calling_process() {
        let parsed = socket_memory_pages(std::process::id()).expect("own sockstat is readable");
        let _ = parsed.tcp;
        let _ = parsed.udp;
    }
}
