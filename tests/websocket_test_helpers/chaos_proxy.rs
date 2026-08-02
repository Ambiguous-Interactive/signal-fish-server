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
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
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
const MAX_RETAINED_CONTROL_ERRORS: usize = 16;

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
    /// Linearizes exclusive pause/kill transitions with socket I/O while
    /// allowing unrelated connections to perform normal I/O concurrently.
    io_barrier: IoBarrier,
    destination_write_bytes: AtomicU64,
    /// Bytes per second; 0 means unlimited.
    throttle_bytes_per_sec: AtomicU64,
    fragment_writes: AtomicBool,
}

impl DirectionControls {
    fn new() -> Self {
        Self {
            paused: watch::Sender::new(false),
            io_barrier: IoBarrier::new(),
            destination_write_bytes: AtomicU64::new(0),
            throttle_bytes_per_sec: AtomicU64::new(0),
            fragment_writes: AtomicBool::new(false),
        }
    }

    fn set_paused(&self, paused: bool) {
        let _barrier = self.io_barrier.write();
        self.paused.send_replace(paused);
    }
}

/// A synchronous, writer-preferring barrier around nonblocking socket calls.
///
/// `std::sync::RwLock` does not specify a fairness policy, so continuous pump
/// traffic could theoretically starve a fault transition. Once a writer is
/// waiting here, new readers park until that transition has completed. The
/// mutex protects only counters; no socket syscall or async wait holds it.
struct IoBarrier {
    state: Mutex<IoBarrierState>,
    changed: Condvar,
}

#[derive(Default)]
struct IoBarrierState {
    active_readers: usize,
    waiting_writers: usize,
    writer_active: bool,
}

struct IoReadGuard<'a> {
    barrier: &'a IoBarrier,
}

struct IoWriteGuard<'a> {
    barrier: &'a IoBarrier,
}

impl IoBarrier {
    fn new() -> Self {
        Self {
            state: Mutex::new(IoBarrierState::default()),
            changed: Condvar::new(),
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, IoBarrierState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read(&self) -> IoReadGuard<'_> {
        let mut state = self.lock_state();
        while state.writer_active || state.waiting_writers > 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.active_readers += 1;
        IoReadGuard { barrier: self }
    }

    fn write(&self) -> IoWriteGuard<'_> {
        let mut state = self.lock_state();
        state.waiting_writers += 1;
        while state.writer_active || state.active_readers > 0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.waiting_writers -= 1;
        state.writer_active = true;
        IoWriteGuard { barrier: self }
    }
}

impl Drop for IoReadGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.barrier.lock_state();
        state.active_readers -= 1;
        if state.active_readers == 0 {
            self.barrier.changed.notify_all();
        }
    }
}

impl Drop for IoWriteGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.barrier.lock_state();
        state.writer_active = false;
        self.barrier.changed.notify_all();
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
    control_errors: Mutex<Vec<String>>,
}

impl ProxyControl {
    fn new() -> Self {
        Self {
            client_to_server: DirectionControls::new(),
            server_to_client: DirectionControls::new(),
            kill: watch::Sender::new(KillMode::None),
            connections: Mutex::new(Vec::new()),
            terminations: Mutex::new(Vec::new()),
            control_errors: Mutex::new(Vec::new()),
        }
    }

    fn direction(&self, direction: Direction) -> &DirectionControls {
        match direction {
            Direction::ClientToServer => &self.client_to_server,
            Direction::ServerToClient => &self.server_to_client,
        }
    }

    fn set_kill(&self, mode: KillMode) {
        let _client_to_server = self.client_to_server.io_barrier.write();
        let _server_to_client = self.server_to_client.io_barrier.write();
        self.kill.send_if_modified(|current| {
            if *current == KillMode::None || mode == KillMode::Rst {
                *current = mode;
                true
            } else {
                false
            }
        });
    }
}

/// Owns an accepted client until its upstream socket is ready. If the accept
/// task is aborted after a terminal fault is published, Drop still applies the
/// RST mode before releasing the otherwise-unregistered socket.
struct PendingClient {
    control: Arc<ProxyControl>,
    stream: Option<TcpStream>,
}

impl PendingClient {
    fn new(control: Arc<ProxyControl>, stream: TcpStream) -> Self {
        Self {
            control,
            stream: Some(stream),
        }
    }

    fn stream(&self) -> &TcpStream {
        self.stream
            .as_ref()
            .expect("pending client already consumed")
    }

    fn into_stream(mut self) -> TcpStream {
        self.stream.take().expect("pending client already consumed")
    }
}

impl Drop for PendingClient {
    fn drop(&mut self) {
        let Some(stream) = self.stream.as_ref() else {
            return;
        };
        apply_late_connection_kill(&self.control, *self.control.kill.borrow(), [stream]);
    }
}

/// Exercise the cancellation-safe pending-client RST path from the single
/// canonical helper test binary without registering another test in every
/// integration binary that embeds this module.
pub(crate) async fn drop_pending_client_after_published_rst(
) -> std::io::Result<(std::io::Result<usize>, Vec<String>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let (peer, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
    let peer = peer?;
    let accepted = accepted?.0;
    let control = Arc::new(ProxyControl::new());
    let pending = PendingClient::new(Arc::clone(&control), accepted);

    control.set_kill(KillMode::Rst);
    drop(pending);
    let control_errors = control
        .control_errors
        .lock()
        .expect("chaos proxy control-error registry poisoned")
        .clone();

    let mut sniff = [0u8; 1];
    let termination = loop {
        if let Err(error) = peer.readable().await {
            break Err(error);
        }
        match peer.try_read(&mut sniff) {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            result => break result,
        }
    };
    Ok((termination, control_errors))
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

        let control = Arc::new(ProxyControl::new());

        let accept_control = Arc::clone(&control);
        let accept_task = tokio::spawn(async move {
            let mut kill_rx = accept_control.kill.subscribe();
            loop {
                let Ok((client, _peer)) = listener.accept().await else {
                    // Listener error: stop accepting; existing pumps live on.
                    return;
                };
                // Match production accepted sockets (issue #197).
                let pending_client = PendingClient::new(Arc::clone(&accept_control), client);
                let _ = pending_client.stream().set_nodelay(true);
                // A killed proxy stays killed: never pump a late connection.
                let current_kill = *kill_rx.borrow();
                if current_kill != KillMode::None {
                    drop(pending_client);
                    continue;
                }
                let server = tokio::select! {
                    biased;
                    changed = kill_rx.changed() => {
                        let mode = if changed.is_ok() {
                            *kill_rx.borrow()
                        } else {
                            KillMode::Fin
                        };
                        debug_assert_ne!(mode, KillMode::None);
                        drop(pending_client);
                        continue;
                    }
                    result = connect_upstream(upstream, recv_buffer_bytes) => result,
                };
                let Ok(server) = server else {
                    // Upstream refused: drop the client so it observes EOF
                    // instead of a silent stall.
                    drop(pending_client);
                    continue;
                };
                let client = pending_client.into_stream();
                register_or_drop_connection(&accept_control, client, server);
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
        self.control.direction(direction).set_paused(true);
    }

    /// Un-park `direction`.
    pub fn resume(&self, direction: Direction) {
        self.control.direction(direction).set_paused(false);
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

    /// Saturating count of bytes accepted by destination socket writes in
    /// `direction` since spawn.
    ///
    /// This diagnostic lets fault tests prove that an exclusive pause or kill
    /// transition stopped every connection at the same observable frontier.
    pub fn destination_write_bytes(&self, direction: Direction) -> u64 {
        self.control
            .direction(direction)
            .destination_write_bytes
            .load(Ordering::Relaxed)
    }

    /// Snapshot the 64 most recent completed-pump diagnostics, oldest first.
    pub fn terminations(&self) -> Vec<PumpTermination> {
        self.control
            .terminations
            .lock()
            .expect("chaos proxy termination registry poisoned")
            .clone()
    }

    /// Snapshot the 16 most recent control-path errors, oldest first.
    pub fn control_errors(&self) -> Vec<String> {
        self.control
            .control_errors
            .lock()
            .expect("chaos proxy control-error registry poisoned")
            .clone()
    }

    /// Abort every proxied connection with a TCP RST: `SO_LINGER = 0` is set
    /// on both sockets of every live connection, then the pumps drop them.
    pub fn rst_all(&self) {
        let client_to_server = self.control.client_to_server.io_barrier.write();
        let server_to_client = self.control.server_to_client.io_barrier.write();
        let connections = self
            .control
            .connections
            .lock()
            .expect("chaos proxy connection registry poisoned");
        let mut first_error = None;
        for connection in connections.iter() {
            for socket in [&connection.client, &connection.upstream] {
                if let Some(socket) = socket.upgrade() {
                    if let Err(error) = set_rst_linger(&socket) {
                        first_error.get_or_insert(error);
                    }
                }
            }
        }
        drop(connections);
        self.control.kill.send_replace(KillMode::Rst);
        drop(server_to_client);
        drop(client_to_server);
        if let Some(error) = first_error {
            panic!("set SO_LINGER=0 for RST close: {error}");
        }
    }

    /// Drop every proxied connection immediately without flushing whatever
    /// chunk a pump is currently forwarding (FIN-close, torn mid-frame).
    pub fn kill_mid_frame(&self) {
        self.control.set_kill(KillMode::Fin);
    }
}

fn set_rst_linger(socket: &TcpStream) -> std::io::Result<()> {
    // `set_linger` is deprecated in tokio because a POSITIVE linger blocks the
    // closing thread for the timeout; the ZERO linger used here never blocks —
    // it converts the close into an immediate RST, which is exactly the fault
    // this helper injects. `socket2::SockRef` would add a direct dependency for
    // identical behavior.
    #[allow(deprecated)]
    socket.set_linger(Some(Duration::ZERO))
}

fn apply_late_connection_kill<'a>(
    control: &ProxyControl,
    mode: KillMode,
    sockets: impl IntoIterator<Item = &'a TcpStream>,
) {
    if mode != KillMode::Rst {
        return;
    }
    for socket in sockets {
        if let Err(error) = set_rst_linger(socket) {
            record_control_error(
                control,
                format!("late-connection RST setup failed: {error}"),
            );
        }
    }
}

/// Revalidate the terminal mode while holding both direction barriers. This
/// closes the accept/connect race with `rst_all`, `kill_mid_frame`, and Drop:
/// either the sockets are registered before the fault snapshots them, or the
/// late pair is reset/dropped without ever spawning pumps.
fn register_or_drop_connection(control: &Arc<ProxyControl>, client: TcpStream, server: TcpStream) {
    let client_to_server = control.client_to_server.io_barrier.write();
    let server_to_client = control.server_to_client.io_barrier.write();
    let mode = *control.kill.borrow();
    if mode == KillMode::None {
        spawn_connection(control, client, server);
    } else {
        apply_late_connection_kill(control, mode, [&client, &server]);
        drop(server);
        drop(client);
    }
    drop(server_to_client);
    drop(client_to_server);
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
        self.control.set_kill(KillMode::Fin);
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
    record_control_termination(control, direction, cause);
}

fn record_control_termination(control: &ProxyControl, direction: Direction, cause: String) {
    let mut terminations = control
        .terminations
        .lock()
        .expect("chaos proxy termination registry poisoned");
    if terminations.len() == MAX_RETAINED_TERMINATIONS {
        terminations.remove(0);
    }
    terminations.push(PumpTermination { direction, cause });
}

fn record_control_error(control: &ProxyControl, error: String) {
    let mut errors = control
        .control_errors
        .lock()
        .expect("chaos proxy control-error registry poisoned");
    if errors.len() == MAX_RETAINED_CONTROL_ERRORS {
        errors.remove(0);
    }
    errors.push(error);
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
        if *kill_rx.borrow() != KillMode::None {
            return "proxy killed before next I/O operation".to_string();
        }

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
                let _barrier = controls.io_barrier.read();
                if *kill_rx.borrow() != KillMode::None {
                    return "proxy killed before source read".to_string();
                }
                // `pause()` may have won the barrier after readiness resolved.
                // Leave those bytes in the source socket until resume.
                if *pause_rx.borrow() {
                    continue;
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
            if let Err(cause) = write_fully(&to, piece, controls, &mut pause_rx, &mut kill_rx).await
            {
                return cause;
            }
        }
    }
}

/// Write `data` completely to `to`, ending early (`Err`) on kill or error.
// `fetch_update` is deprecated only on the analysis nightly in favor of
// nightly-only `try_update`; retain the stable API for the supported MSRV.
#[allow(deprecated)]
async fn write_fully(
    to: &TcpStream,
    data: &[u8],
    controls: &DirectionControls,
    pause_rx: &mut watch::Receiver<bool>,
    kill_rx: &mut watch::Receiver<KillMode>,
) -> Result<(), String> {
    let mut written = 0;
    while written < data.len() {
        if *kill_rx.borrow() != KillMode::None {
            return Err("proxy killed before write".to_string());
        }

        // A pause can arrive after the source read or during a fragmented
        // write. Preserve the unwritten suffix and park until resume.
        while *pause_rx.borrow_and_update() {
            tokio::select! {
                _ = kill_rx.changed() => {
                    if *kill_rx.borrow() != KillMode::None {
                        return Err("proxy killed while paused before write".to_string());
                    }
                }
                changed = pause_rx.changed() => {
                    if changed.is_err() {
                        return Err("pause control closed before write".to_string());
                    }
                }
            }
        }

        tokio::select! {
            biased;
            _ = kill_rx.changed() => {
                if *kill_rx.borrow() != KillMode::None {
                    return Err("proxy killed while writing".to_string());
                }
            }
            changed = pause_rx.changed() => {
                if changed.is_err() {
                    return Err("pause control closed while writing".to_string());
                }
            }
            writable = to.writable() => {
                if writable.is_err() {
                    return Err("destination readiness failed".to_string());
                }
                let _barrier = controls.io_barrier.read();
                if *kill_rx.borrow() != KillMode::None {
                    return Err("proxy killed before destination write".to_string());
                }
                // `pause()` may have won the barrier after writability
                // resolved. Re-loop into the park without losing the suffix.
                if *pause_rx.borrow() {
                    continue;
                }
                match to.try_write(&data[written..]) {
                    Ok(sent) => {
                        written += sent;
                        let _previous = controls.destination_write_bytes.fetch_update(
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                            |total| Some(total.saturating_add(sent as u64)),
                        );
                    }
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
