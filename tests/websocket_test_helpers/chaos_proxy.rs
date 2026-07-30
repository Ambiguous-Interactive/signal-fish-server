//! Hand-rolled TCP chaos proxy for fault injection between test WebSocket
//! clients and the server under test.
//!
//! A [`ChaosProxy`] listens on an ephemeral loopback port and pumps bytes
//! bidirectionally to a fixed upstream address. Its control surface injects
//! transport-level faults that in-process channel tests cannot express:
//!
//! - [`pause`](ChaosProxy::pause) / [`resume`](ChaosProxy::resume): park a
//!   direction's pump so bytes accumulate in kernel socket buffers (a stalled
//!   reader, without touching the client's task).
//! - [`throttle`](ChaosProxy::throttle): pace a direction to a byte rate
//!   (chunked writes released against a virtual clock — see
//!   [`next_chunk_release`] — so the achieved rate converges on nominal instead
//!   of drifting below it on a loaded machine; the injected workload shape, not
//!   a synchronization primitive).
//! - [`fragment_writes`](ChaosProxy::fragment_writes): forward one byte per
//!   write so frames arrive maximally fragmented.
//! - [`rst_all`](ChaosProxy::rst_all): abort every proxied connection with a
//!   TCP RST (`SO_LINGER = 0` before drop).
//! - [`kill_mid_frame`](ChaosProxy::kill_mid_frame): drop every proxied
//!   connection immediately (FIN), discarding any half-forwarded chunk.
//!
//! Faults apply to **all** of a proxy's connections; tests that need
//! per-client faults spawn one proxy per client. A killed proxy stays killed
//! (subsequent accepts are dropped immediately) — spawn a fresh proxy for a
//! fresh link. On half-close or a socket error in either direction the whole
//! proxied connection is torn down (a full close on half-close): the chaos
//! suites only ever end proxied connections by injected kill or test end, so
//! graceful half-close fidelity is deliberately out of scope.
//!
//! The proxy's own unit tests live at the bottom of this file (echo-server
//! upstream). They carry `#[serial_test::serial]` because this module is
//! compiled into several test binaries whose sibling tests assert on
//! process-wide state (`/proc/self/fd` baselines) under plain `cargo test`;
//! serializing keeps this file's socket churn out of those baselines.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// Which pump a fault applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    ClientToServer,
    ServerToClient,
}

/// Why one direction of a proxied connection ended before the proxy tore down
/// the other half. Retained for fault-test diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PumpTermination {
    pub direction: Direction,
    pub cause: String,
}

const MAX_RETAINED_TERMINATIONS: usize = 64;

/// How the proxy was told to sever its connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KillMode {
    None,
    /// Drop sockets as-is: the kernel sends FIN (or RST if unread data is
    /// pending). Any half-forwarded chunk is discarded, not flushed.
    Fin,
    /// `SO_LINGER = 0` then drop: the kernel sends RST.
    Rst,
}

/// Per-direction fault switches shared by every connection's pump.
struct DirectionControls {
    paused: watch::Sender<bool>,
    /// Bytes per second; 0 means unlimited.
    throttle_bytes_per_sec: AtomicU64,
    fragment_writes: AtomicBool,
}

impl DirectionControls {
    fn new() -> Self {
        Self {
            paused: watch::Sender::new(false),
            throttle_bytes_per_sec: AtomicU64::new(0),
            fragment_writes: AtomicBool::new(false),
        }
    }
}

/// Both sockets of one proxied connection, retained weakly so the pumps'
/// `Arc`s stay the owners: when the pumps end, the sockets close.
struct ConnectionSockets {
    client: Weak<TcpStream>,
    upstream: Weak<TcpStream>,
}

struct ProxyControl {
    client_to_server: DirectionControls,
    server_to_client: DirectionControls,
    kill: watch::Sender<KillMode>,
    connections: Mutex<Vec<ConnectionSockets>>,
    terminations: Mutex<Vec<PumpTermination>>,
}

impl ProxyControl {
    fn direction(&self, direction: Direction) -> &DirectionControls {
        match direction {
            Direction::ClientToServer => &self.client_to_server,
            Direction::ServerToClient => &self.server_to_client,
        }
    }
}

/// See the module docs. Dropping the proxy kills its connections (FIN) and
/// stops accepting.
pub struct ChaosProxy {
    addr: SocketAddr,
    control: Arc<ProxyControl>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl ChaosProxy {
    /// Bind a listener on `127.0.0.1:0` and start proxying every accepted
    /// connection to `upstream`.
    pub async fn spawn(upstream: SocketAddr) -> Self {
        Self::spawn_with_upstream_recv_buffer(upstream, None).await
    }

    /// Spawn a proxy whose server-facing socket requests a bounded receive
    /// window before connecting.
    ///
    /// This keeps localhost TCP autotuning from absorbing an entire
    /// bandwidth-fault experiment in the proxy's kernel buffer. `None`
    /// preserves [`Self::spawn`]'s normal socket defaults; `Some(bytes)` is
    /// intended for tests that need a constrained downstream to become
    /// visible to the server's outbound queue within a bounded time.
    pub async fn spawn_with_upstream_recv_buffer(
        upstream: SocketAddr,
        recv_buffer_bytes: Option<u32>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind chaos proxy listener");
        let addr = listener.local_addr().expect("read chaos proxy address");

        let control = Arc::new(ProxyControl {
            client_to_server: DirectionControls::new(),
            server_to_client: DirectionControls::new(),
            kill: watch::Sender::new(KillMode::None),
            connections: Mutex::new(Vec::new()),
            terminations: Mutex::new(Vec::new()),
        });

        let accept_control = Arc::clone(&control);
        let accept_task = tokio::spawn(async move {
            loop {
                let Ok((client, _peer)) = listener.accept().await else {
                    // Listener error: stop accepting; existing pumps live on.
                    return;
                };
                // Match production accepted sockets (issue #197).
                let _ = client.set_nodelay(true);
                // A killed proxy stays killed: never pump a late connection.
                if *accept_control.kill.borrow() != KillMode::None {
                    drop(client);
                    continue;
                }
                let Ok(server) = connect_upstream(upstream, recv_buffer_bytes).await else {
                    // Upstream refused: drop the client so it observes EOF
                    // instead of a silent stall.
                    drop(client);
                    continue;
                };
                spawn_connection(&accept_control, client, server);
            }
        });

        Self {
            addr,
            control,
            accept_task,
        }
    }

    /// The loopback address clients should connect to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Park `direction`: no bytes are forwarded (they accumulate in kernel
    /// buffers) until [`resume`](Self::resume).
    pub fn pause(&self, direction: Direction) {
        self.control.direction(direction).paused.send_replace(true);
    }

    /// Un-park `direction`.
    pub fn resume(&self, direction: Direction) {
        self.control.direction(direction).paused.send_replace(false);
    }

    /// Pace `direction` to `bytes_per_sec` (`None` lifts the throttle).
    pub fn throttle(&self, direction: Direction, bytes_per_sec: Option<u64>) {
        self.control
            .direction(direction)
            .throttle_bytes_per_sec
            .store(bytes_per_sec.unwrap_or(0), Ordering::Relaxed);
    }

    /// Forward `direction` one byte per socket write when enabled.
    pub fn fragment_writes(&self, direction: Direction, enabled: bool) {
        self.control
            .direction(direction)
            .fragment_writes
            .store(enabled, Ordering::Relaxed);
    }

    /// Snapshot the 64 most recent completed-pump diagnostics, oldest first.
    pub fn terminations(&self) -> Vec<PumpTermination> {
        self.control
            .terminations
            .lock()
            .expect("chaos proxy termination registry poisoned")
            .clone()
    }

    /// Abort every proxied connection with a TCP RST: `SO_LINGER = 0` is set
    /// on both sockets of every live connection, then the pumps drop them.
    pub fn rst_all(&self) {
        let connections = self
            .control
            .connections
            .lock()
            .expect("chaos proxy connection registry poisoned");
        for connection in connections.iter() {
            for socket in [&connection.client, &connection.upstream] {
                if let Some(socket) = socket.upgrade() {
                    // `set_linger` is deprecated in tokio because a POSITIVE
                    // linger blocks the closing thread for the timeout; the
                    // ZERO linger used here never blocks — it converts the
                    // close into an immediate RST, which is exactly the fault
                    // this helper injects. The suggested replacement
                    // (socket2::SockRef) would add a direct dependency for
                    // identical behavior.
                    #[allow(deprecated)]
                    socket
                        .set_linger(Some(Duration::ZERO))
                        .expect("set SO_LINGER=0 for RST close");
                }
            }
        }
        drop(connections);
        self.control.kill.send_replace(KillMode::Rst);
    }

    /// Drop every proxied connection immediately without flushing whatever
    /// chunk a pump is currently forwarding (FIN-close, torn mid-frame).
    pub fn kill_mid_frame(&self) {
        self.control.kill.send_replace(KillMode::Fin);
    }
}

async fn connect_upstream(
    upstream: SocketAddr,
    recv_buffer_bytes: Option<u32>,
) -> std::io::Result<TcpStream> {
    let socket = if upstream.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    if let Some(bytes) = recv_buffer_bytes {
        socket.set_recv_buffer_size(bytes)?;
    }
    let stream = socket.connect(upstream).await?;
    // Match production accepted sockets: disable Nagle so the proxy hop does not
    // inject delayed-ACK stalls into latency measurements (issue #197).
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

impl Drop for ChaosProxy {
    fn drop(&mut self) {
        // Stop accepting and end every pump so the proxy's file descriptors
        // are released promptly (the churn suites assert fd baselines).
        self.control.kill.send_replace(KillMode::Fin);
        self.accept_task.abort();
    }
}

/// Wire one accepted connection: register both sockets (weakly) and run one
/// pump per direction; when either pump ends, the other is aborted and both
/// sockets close (see the module docs on full-close-on-half-close).
fn spawn_connection(control: &Arc<ProxyControl>, client: TcpStream, server: TcpStream) {
    let client = Arc::new(client);
    let server = Arc::new(server);
    control
        .connections
        .lock()
        .expect("chaos proxy connection registry poisoned")
        .push(ConnectionSockets {
            client: Arc::downgrade(&client),
            upstream: Arc::downgrade(&server),
        });

    let mut client_to_server = tokio::spawn(pump(
        Arc::clone(control),
        Direction::ClientToServer,
        Arc::clone(&client),
        Arc::clone(&server),
    ));
    let mut server_to_client = tokio::spawn(pump(
        Arc::clone(control),
        Direction::ServerToClient,
        server,
        client,
    ));
    let completion_control = Arc::clone(control);
    tokio::spawn(async move {
        tokio::select! {
            result = &mut client_to_server => {
                record_pump_termination(
                    &completion_control,
                    Direction::ClientToServer,
                    result,
                );
                server_to_client.abort();
            }
            result = &mut server_to_client => {
                record_pump_termination(
                    &completion_control,
                    Direction::ServerToClient,
                    result,
                );
                client_to_server.abort();
            }
        }
    });
}

fn record_pump_termination(
    control: &ProxyControl,
    direction: Direction,
    result: Result<String, tokio::task::JoinError>,
) {
    let cause = result.unwrap_or_else(|error| format!("pump task failed: {error}"));
    let mut terminations = control
        .terminations
        .lock()
        .expect("chaos proxy termination registry poisoned");
    if terminations.len() == MAX_RETAINED_TERMINATIONS {
        terminations.remove(0);
    }
    terminations.push(PumpTermination { direction, cause });
}

/// When a throttled pump may release its next chunk.
///
/// Pacing is a **virtual clock**, not a fixed sleep per chunk. Sleeping
/// `bytes / rate` after every read adds the pump's own read/write/scheduling
/// latency to every period, so the achieved rate is always *below* nominal and
/// drifts further the busier the machine is. That made the injected fault
/// inaccurate in exactly the direction that breaks experiments: on a loaded CI
/// runner a nominal "32 KiB/s" link delivered materially less, the server's
/// sojourn bound tripped, and `mixed_encoding_relay_e2e`'s throttled recipient
/// was evicted as a slow consumer — the outcome that experiment exists to prove
/// does *not* happen. It failed this way on `main` in run 30187497311 and again
/// on PR #208.
///
/// Scheduling each chunk against a running virtual clock lets a late iteration
/// be absorbed by the next one, so the long-run rate converges on nominal.
/// Catch-up credit is capped at one chunk period: a pump that fell far behind
/// (a `pause()`, or the process being descheduled) restarts the clock instead
/// of bursting to "make up" arbitrary lost time.
fn next_chunk_release(
    previous: Option<tokio::time::Instant>,
    now: tokio::time::Instant,
    pacing: Duration,
) -> tokio::time::Instant {
    let target = previous.unwrap_or(now) + pacing;
    // Comparing `target + pacing` against `now` keeps this in Instant addition
    // only — subtracting a Duration from an Instant can underflow.
    if target + pacing < now {
        now
    } else {
        target
    }
}

/// Copy bytes `from` -> `to`, honoring the direction's pause / throttle /
/// fragmentation switches and ending immediately on a kill signal, EOF, or a
/// socket error (all of which tear down the whole connection).
async fn pump(
    control: Arc<ProxyControl>,
    direction: Direction,
    from: Arc<TcpStream>,
    to: Arc<TcpStream>,
) -> String {
    let controls = control.direction(direction);
    let mut pause_rx = controls.paused.subscribe();
    let mut kill_rx = control.kill.subscribe();
    let mut buffer = vec![0u8; 8 * 1024];
    // Virtual clock for throttled pacing; see `next_chunk_release`.
    let mut release_at: Option<tokio::time::Instant> = None;

    loop {
        // Park while paused; a kill preempts the park.
        while *pause_rx.borrow_and_update() {
            tokio::select! {
                _ = kill_rx.changed() => {
                    if *kill_rx.borrow() != KillMode::None {
                        return "proxy killed while paused".to_string();
                    }
                }
                changed = pause_rx.changed() => {
                    if changed.is_err() {
                        return "pause control closed".to_string();
                    }
                }
            }
        }

        // Throttled pumps read small chunks so pacing stays fine-grained.
        let throttle = controls.throttle_bytes_per_sec.load(Ordering::Relaxed);
        let read_limit = if throttle > 0 {
            buffer.len().min(1024)
        } else {
            buffer.len()
        };

        // `biased` so a kill or pause that is already pending always wins
        // over readability: a `pause()` issued before the peer's bytes were
        // written must deterministically park the pump instead of racing a
        // simultaneously-ready read branch (the pause-change branch re-loops
        // into the park above).
        let received = tokio::select! {
            biased;
            _ = kill_rx.changed() => {
                if *kill_rx.borrow() != KillMode::None {
                    return "proxy killed while awaiting readability".to_string();
                }
                continue;
            }
            changed = pause_rx.changed() => {
                if changed.is_err() {
                    return "pause control closed".to_string();
                }
                continue;
            }
            readable = from.readable() => {
                if readable.is_err() {
                    return "source readiness failed".to_string();
                }
                match from.try_read(&mut buffer[..read_limit]) {
                    Ok(0) => return "source reached EOF".to_string(),
                    Ok(received) => received,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(error) => return format!("source read failed: {error}"),
                }
            }
        };

        // Pacing sleeps BEFORE the write so the forwarded byte rate is
        // genuinely bounded (a post-write sleep would let a whole chunk race
        // ahead of its budget). This interval is the injected fault's
        // workload shape, not a synchronization wait.
        //
        // Re-load the throttle AFTER the read: the iteration-start load above
        // only sizes the read chunk. A `throttle()` issued while this pump
        // was parked awaiting readability must pace the very first chunk it
        // releases — deciding from the stale pre-park load let that chunk
        // forward unpaced (a real race: 2 KiB "throttled" to 2 KiB/s
        // forwarded in under a millisecond).
        let throttle = controls.throttle_bytes_per_sec.load(Ordering::Relaxed);
        if throttle > 0 {
            let pacing = Duration::from_secs_f64(received as f64 / throttle as f64);
            let release = next_chunk_release(release_at, tokio::time::Instant::now(), pacing);
            release_at = Some(release);
            tokio::select! {
                _ = kill_rx.changed() => {
                    if *kill_rx.borrow() != KillMode::None {
                        return "proxy killed while pacing".to_string();
                    }
                }
                () = tokio::time::sleep_until(release) => {}
            }
        } else {
            // Restart the virtual clock when the throttle is lifted so a later
            // re-throttle does not inherit stale credit.
            release_at = None;
        }

        let fragment = controls.fragment_writes.load(Ordering::Relaxed);
        let chunk_size = if fragment { 1 } else { received };
        for piece in buffer[..received].chunks(chunk_size) {
            if let Err(cause) = write_fully(&to, piece, &mut kill_rx).await {
                return cause;
            }
        }
    }
}

/// Write `data` completely to `to`, ending early (`Err`) on kill or error.
async fn write_fully(
    to: &TcpStream,
    data: &[u8],
    kill_rx: &mut watch::Receiver<KillMode>,
) -> Result<(), String> {
    let mut written = 0;
    while written < data.len() {
        tokio::select! {
            _ = kill_rx.changed() => {
                if *kill_rx.borrow() != KillMode::None {
                    return Err("proxy killed while writing".to_string());
                }
            }
            writable = to.writable() => {
                if writable.is_err() {
                    return Err("destination readiness failed".to_string());
                }
                match to.try_write(&data[written..]) {
                    Ok(sent) => written += sent,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(format!("destination write failed: {error}")),
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Generous ceiling for every expected event in these tests: a starved
    /// runner only spends it when something is genuinely broken.
    const EVENT_DEADLINE: Duration = Duration::from_secs(20);

    /// Spawn a TCP echo server on an ephemeral loopback port.
    async fn spawn_echo_upstream() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind echo upstream");
        let addr = listener.local_addr().expect("read echo upstream address");
        tokio::spawn(async move {
            while let Ok((mut socket, _peer)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut read_half, mut write_half) = socket.split();
                    // Echo until EOF/error; the connection ending is the
                    // expected terminal state, not a failure to report.
                    let _copied_until_close =
                        tokio::io::copy(&mut read_half, &mut write_half).await;
                });
            }
        });
        addr
    }

    async fn connect_through(proxy: &ChaosProxy) -> TcpStream {
        tokio::time::timeout(EVENT_DEADLINE, TcpStream::connect(proxy.addr()))
            .await
            .expect("connect through proxy timed out")
            .expect("connect through proxy failed")
    }

    async fn write_all(stream: &mut TcpStream, data: &[u8]) {
        tokio::time::timeout(EVENT_DEADLINE, stream.write_all(data))
            .await
            .expect("proxied write timed out")
            .expect("proxied write failed");
    }

    async fn read_exactly(stream: &mut TcpStream, len: usize, context: &str) -> Vec<u8> {
        let mut data = vec![0u8; len];
        tokio::time::timeout(EVENT_DEADLINE, stream.read_exact(&mut data))
            .await
            .unwrap_or_else(|_elapsed| panic!("{context}: timed out reading {len} bytes"))
            .unwrap_or_else(|error| panic!("{context}: read failed: {error}"));
        data
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn proxy_forwards_bidirectionally() {
        let upstream = spawn_echo_upstream().await;
        let proxy = ChaosProxy::spawn(upstream).await;
        let mut client = connect_through(&proxy).await;

        write_all(&mut client, b"ping-through-proxy").await;
        let echoed = read_exactly(&mut client, 18, "echo through proxy").await;
        assert_eq!(&echoed, b"ping-through-proxy");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn pause_parks_bytes_until_resume() {
        let upstream = spawn_echo_upstream().await;
        let proxy = ChaosProxy::spawn(upstream).await;
        let mut client = connect_through(&proxy).await;

        // Healthy first: the link demonstrably works before the fault.
        write_all(&mut client, b"warmup").await;
        let echoed = read_exactly(&mut client, 6, "pre-pause echo").await;
        assert_eq!(&echoed, b"warmup");

        proxy.pause(Direction::ServerToClient);
        write_all(&mut client, b"parked").await;

        // Expected silence: a paused pump forwards nothing, ever, so this
        // bounded wait is deterministic — no scheduling can deliver bytes.
        let mut sniff = [0u8; 6];
        let stalled_read =
            tokio::time::timeout(Duration::from_millis(300), client.read_exact(&mut sniff)).await;
        assert!(
            stalled_read.is_err(),
            "paused server->client pump must not forward the echo, got {stalled_read:?}"
        );

        proxy.resume(Direction::ServerToClient);
        let released = read_exactly(&mut client, 6, "post-resume echo").await;
        assert_eq!(&released, b"parked");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn throttle_bounds_forwarding_rate_loosely() {
        let upstream = spawn_echo_upstream().await;
        let proxy = ChaosProxy::spawn(upstream).await;
        let mut client = connect_through(&proxy).await;

        // 2 KiB at 2 KiB/s: pre-write pacing guarantees >= ~1s of sleep in
        // the server->client pump regardless of chunking, so the elapsed
        // lower bound is deterministic (loose upper bound via the deadline).
        proxy.throttle(Direction::ServerToClient, Some(2 * 1024));
        let payload = vec![0xA5u8; 2 * 1024];
        let started = std::time::Instant::now();
        write_all(&mut client, &payload).await;
        let echoed = read_exactly(&mut client, payload.len(), "throttled echo").await;
        let elapsed = started.elapsed();

        assert_eq!(echoed, payload, "throttling must not corrupt bytes");
        assert!(
            elapsed >= Duration::from_millis(500),
            "2 KiB through a 2 KiB/s throttle finished in {elapsed:?} — throttle not applied"
        );
    }

    /// The pacing clock must converge on the nominal rate rather than drifting
    /// below it, which is what evicted a throttled recipient in
    /// `mixed_encoding_relay_e2e` on a loaded runner. Exercised as pure
    /// arithmetic over synthetic instants so the property is deterministic
    /// instead of a timing race.
    #[tokio::test]
    async fn chunk_pacing_compensates_for_late_iterations() {
        let pacing = Duration::from_millis(32);
        let start = tokio::time::Instant::now();

        // First chunk under a fresh clock: one full period from now.
        assert_eq!(next_chunk_release(None, start, pacing), start + pacing);

        // On time: the next release is one period after the previous one, so
        // the schedule stays anchored to the virtual clock rather than to the
        // moment this iteration happened to run.
        let previous = start + pacing;
        assert_eq!(
            next_chunk_release(Some(previous), previous, pacing),
            previous + pacing
        );

        // Slightly late (this iteration ran a third of a period behind): the
        // release stays on the virtual schedule, which is now in the past, so
        // the chunk goes immediately and the lost time is absorbed. A fixed
        // per-chunk sleep would instead add the lateness to every period.
        let late = previous + pacing + pacing / 3;
        let release = next_chunk_release(Some(previous), late, pacing);
        assert_eq!(release, previous + pacing);
        assert!(release < late, "a late iteration must release immediately");

        // Far behind (a pause, or the process descheduled for many periods):
        // credit is dropped and the clock restarts, so catching up can never
        // burst an unbounded amount of traffic through the throttle.
        let stalled = previous + pacing * 50;
        assert_eq!(next_chunk_release(Some(previous), stalled, pacing), stalled);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn fragmented_writes_preserve_content() {
        let upstream = spawn_echo_upstream().await;
        let proxy = ChaosProxy::spawn(upstream).await;
        let mut client = connect_through(&proxy).await;

        proxy.fragment_writes(Direction::ClientToServer, true);
        proxy.fragment_writes(Direction::ServerToClient, true);
        let payload: Vec<u8> = (0u16..512).map(|byte| (byte % 251) as u8).collect();
        write_all(&mut client, &payload).await;
        let echoed = read_exactly(&mut client, payload.len(), "fragmented echo").await;
        assert_eq!(echoed, payload, "fragmentation must not corrupt bytes");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn kill_mid_frame_severs_the_connection() {
        let upstream = spawn_echo_upstream().await;
        let proxy = ChaosProxy::spawn(upstream).await;
        let mut client = connect_through(&proxy).await;

        write_all(&mut client, b"alive").await;
        let echoed = read_exactly(&mut client, 5, "pre-kill echo").await;
        assert_eq!(&echoed, b"alive");

        proxy.kill_mid_frame();
        let mut sniff = [0u8; 1];
        let termination = tokio::time::timeout(EVENT_DEADLINE, client.read(&mut sniff))
            .await
            .expect("killed connection never terminated client-side");
        match termination {
            Ok(0) | Err(_) => {}
            Ok(received) => panic!("expected EOF or error after kill, read {received} bytes"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn rst_all_resets_the_connection() {
        let upstream = spawn_echo_upstream().await;
        let proxy = ChaosProxy::spawn(upstream).await;
        let mut client = connect_through(&proxy).await;

        write_all(&mut client, b"alive").await;
        let echoed = read_exactly(&mut client, 5, "pre-rst echo").await;
        assert_eq!(&echoed, b"alive");

        proxy.rst_all();
        let mut sniff = [0u8; 1];
        let termination = tokio::time::timeout(EVENT_DEADLINE, client.read(&mut sniff))
            .await
            .expect("reset connection never terminated client-side");
        // Linux delivers the RST as ECONNRESET; keep other platforms honest
        // but looser (termination of any kind), since this helper compiles
        // into cross-OS suites.
        #[cfg(target_os = "linux")]
        match &termination {
            Err(error) => assert_eq!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset,
                "SO_LINGER=0 close must surface as a TCP RST, got {error:?}"
            ),
            Ok(received) => panic!("expected ECONNRESET after rst_all, read {received} bytes"),
        }
        #[cfg(not(target_os = "linux"))]
        match termination {
            Ok(0) | Err(_) => {}
            Ok(received) => panic!("expected EOF or error after rst_all, read {received} bytes"),
        }
    }
}
