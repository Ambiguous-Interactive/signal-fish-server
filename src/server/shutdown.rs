use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::coordination::CloseReason;
use crate::protocol::ServerMessage;

use super::EnhancedGameServer;

/// Drops when a real WebSocket handler has fully returned.
pub(crate) struct SocketTaskGuard {
    server: Arc<EnhancedGameServer>,
}

impl Drop for SocketTaskGuard {
    fn drop(&mut self) {
        if self
            .server
            .active_socket_tasks
            .fetch_sub(1, Ordering::AcqRel)
            == 1
        {
            self.server.active_socket_tasks_notify.notify_waiters();
        }
    }
}

/// Result of starting or observing the current shutdown drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownDrain {
    /// Unix epoch millisecond deadline advertised to v3 clients.
    pub deadline_ms: u64,
    /// How long this drain waits before forcing close code 4000.
    pub grace: Duration,
    /// Optional retry hint included in the v3 `GoingAway` advisory.
    pub retry_after_secs: Option<u64>,
    /// True only for the call that transitioned the server into draining.
    pub started_by_this_call: bool,
}

impl ShutdownDrain {
    /// How long the caller should wait before forcing shutdown closes.
    ///
    /// The first drain owner waits out the grace time it has not already spent
    /// announcing. Later observers use the already-advertised deadline instead
    /// of extending shutdown by another full grace window.
    pub fn wait_before_close(self, elapsed_since_start: Duration) -> Duration {
        self.wait_before_close_since(elapsed_since_start, unix_epoch_ms_now())
    }

    fn wait_before_close_since(self, elapsed_since_start: Duration, now_ms: u64) -> Duration {
        if self.started_by_this_call {
            return self
                .grace
                .checked_sub(elapsed_since_start)
                .unwrap_or(Duration::ZERO);
        }

        Duration::from_millis(self.deadline_ms.saturating_sub(now_ms))
    }
}

impl EnhancedGameServer {
    /// Track a real WebSocket handler for bounded shutdown waiting.
    pub(crate) fn track_socket_task(self: &Arc<Self>) -> SocketTaskGuard {
        self.active_socket_tasks.fetch_add(1, Ordering::AcqRel);
        SocketTaskGuard {
            server: Arc::clone(self),
        }
    }

    /// Whether this server is already draining for shutdown.
    pub fn is_draining(&self) -> bool {
        self.shutdown_drain_deadline_ms.load(Ordering::Acquire) != 0
    }

    /// Whether any WebSocket handler task is still live.
    ///
    /// Handlers are tracked for their whole lifetime, so this is true
    /// whenever a socket could still observe a shutdown close frame. During
    /// a drain new registrations are refused, so a false reading here means
    /// no connection can ever receive the coded `4000` close.
    pub fn has_active_socket_tasks(&self) -> bool {
        self.active_socket_tasks.load(Ordering::Acquire) > 0
    }

    /// Begin graceful shutdown drain, or return the already-advertised drain.
    ///
    /// The first caller fixes the deadline. Later callers observe the same
    /// deadline and do not re-announce a second drain window.
    pub fn begin_shutdown_drain(&self) -> ShutdownDrain {
        let grace = self.config.drain_grace;
        let deadline_ms = unix_deadline_ms_after(grace);
        match self.shutdown_drain_deadline_ms.compare_exchange(
            0,
            deadline_ms,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                let _ = self.shutdown_drain_tx.send(true);
                ShutdownDrain {
                    deadline_ms,
                    grace,
                    retry_after_secs: retry_after_secs(grace),
                    started_by_this_call: true,
                }
            }
            Err(existing_deadline_ms) => ShutdownDrain {
                deadline_ms: existing_deadline_ms,
                grace,
                retry_after_secs: retry_after_secs(grace),
                started_by_this_call: false,
            },
        }
    }

    pub(crate) fn shutdown_drain_receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.shutdown_drain_tx.subscribe()
    }

    /// Best-effort v3 shutdown advisory fan-out.
    ///
    /// The close frame is the authoritative signal. `GoingAway` is advisory, so
    /// this never waits behind a full queue during shutdown.
    pub async fn announce_shutdown_drain(&self, drain: ShutdownDrain) -> usize {
        if !drain.started_by_this_call {
            tracing::debug!(
                deadline_ms = drain.deadline_ms,
                "Skipping duplicate shutdown GoingAway advisory for existing drain"
            );
            return 0;
        }

        let message = Arc::new(ServerMessage::GoingAway {
            deadline_ms: drain.deadline_ms,
            retry_after_secs: drain.retry_after_secs,
        });
        let mut enqueued = 0usize;
        for player_id in self.connection_manager.client_ids() {
            if !self.client_supports_v3(&player_id) {
                continue;
            }
            match self
                .message_coordinator
                .try_send_to_player(&player_id, Arc::clone(&message))
                .await
            {
                Ok(true) => enqueued = enqueued.saturating_add(1),
                Ok(false) => {
                    tracing::debug!(
                        %player_id,
                        "Shutdown GoingAway advisory skipped: queue full or connection gone"
                    );
                }
                Err(err) => {
                    tracing::debug!(
                        %player_id,
                        error = %err,
                        "Shutdown GoingAway advisory failed"
                    );
                }
            }
        }
        enqueued
    }

    /// Request shutdown close code 4000 on every active connection.
    pub fn close_connections_for_shutdown(&self) -> usize {
        let mut requested = 0usize;
        for player_id in self.connection_manager.client_ids() {
            if self
                .connection_manager
                .request_close_for(&player_id, CloseReason::Shutdown)
            {
                requested = requested.saturating_add(1);
            }
        }
        requested
    }

    /// Wait for close-requested socket handlers to finish before process exit.
    ///
    /// The close frames are written by per-connection send tasks after
    /// [`Self::close_connections_for_shutdown`] requests `CloseReason::Shutdown`.
    /// Unregistration can happen before that bounded flush finishes, so this
    /// waits on the parent WebSocket handler lifetime instead of connection-map
    /// membership. It returns the number of handlers still active at timeout.
    pub async fn wait_for_shutdown_connections(&self, max_wait: Duration) -> usize {
        self.wait_for_shutdown_connections_after_active_check(max_wait, || {})
            .await
    }

    async fn wait_for_shutdown_connections_after_active_check(
        &self,
        max_wait: Duration,
        mut after_active_check: impl FnMut(),
    ) -> usize {
        let deadline = crate::deadline::after(tokio::time::Instant::now(), max_wait);
        loop {
            let notified = self.active_socket_tasks_notify.notified();
            tokio::pin!(notified);
            // Arm the waiter before reading the counter so the zero-task
            // transition cannot pass between the state check and the await.
            let _ = notified.as_mut().enable();

            let active_tasks = self.active_socket_tasks.load(Ordering::Acquire);
            if active_tasks == 0 {
                return 0;
            }

            let now = tokio::time::Instant::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                return active_tasks;
            }

            after_active_check();

            tokio::select! {
                () = &mut notified => {}
                _ = crate::deadline::wait_until(deadline) => {
                    return self.active_socket_tasks.load(Ordering::Acquire);
                }
            }
        }
    }
}

/// Scheduling beat granted to upgraded-but-unpolled connection handlers to
/// arm their shutdown lifetime guard before the idle drain decides the
/// grace wait protects nothing. Far below any meaningful grace, so the idle
/// restart fast path stays fast.
pub(crate) const DRAIN_IDLE_HANDLER_SETTLE: Duration = Duration::from_millis(50);

/// Everything the shutdown drain does after the shutdown signal arrives:
/// begin the drain, fan out the v3 `GoingAway` advisories, wait out the
/// grace (skipped when no socket handler can observe the close), force the
/// coded `4000` closes, and settle the handlers before process exit.
///
/// Embedded servers that run their own accept loop call this after their
/// serve future returns, instead of reimplementing the choreography.
pub async fn run_drain_choreography(
    server: &EnhancedGameServer,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
) {
    let drain_started_at = tokio::time::Instant::now();
    let drain = server.begin_shutdown_drain();
    tracing::info!(
        deadline_ms = drain.deadline_ms,
        grace_ms = u64::try_from(drain.grace.as_millis()).unwrap_or(u64::MAX),
        started_by_this_call = drain.started_by_this_call,
        "Server shutdown drain started"
    );
    let going_away_sent = server.announce_shutdown_drain(drain).await;
    tracing::info!(
        going_away_sent,
        started_by_this_call = drain.started_by_this_call,
        "Shutdown GoingAway advisories enqueued"
    );

    let _ = shutdown_tx.send(true);

    let wait_before_close = drain.wait_before_close(drain_started_at.elapsed());
    // During a drain new registrations are refused, so with no live socket
    // handler no connection can ever receive the coded `4000` close. Waiting
    // out the grace anyway would be pure restart delay that eats into the
    // operator's termination budget. An upgraded connection whose handler
    // task has not been polled yet does not count as live, so the empty
    // reading is confirmed after one scheduling beat before the wait is
    // skipped; a handler surfacing during that beat still gets its grace.
    if wait_before_close > Duration::ZERO {
        if !server.has_active_socket_tasks() {
            tokio::time::sleep(DRAIN_IDLE_HANDLER_SETTLE).await;
        }
        if server.has_active_socket_tasks() {
            tokio::time::sleep(wait_before_close).await;
        }
    }

    let close_requests = server.close_connections_for_shutdown();
    tracing::info!(close_requests, "Shutdown close requests issued");

    let settle_timeout = crate::websocket::registered_connection_shutdown_settle_timeout();
    let remaining_connections = server.wait_for_shutdown_connections(settle_timeout).await;
    if remaining_connections > 0 {
        tracing::warn!(
            remaining_connections,
            settle_ms = u64::try_from(settle_timeout.as_millis()).unwrap_or(u64::MAX),
            "Shutdown drain ended with connections still registered"
        );
    }
}

fn retry_after_secs(grace: Duration) -> Option<u64> {
    (grace > Duration::ZERO).then_some(grace.as_secs().max(1))
}

fn unix_deadline_ms_after(grace: Duration) -> u64 {
    unix_deadline_ms_after_since(unix_epoch_duration_now(), grace)
}

fn unix_epoch_ms_now() -> u64 {
    u64::try_from(unix_epoch_duration_now().as_millis()).unwrap_or(u64::MAX)
}

fn unix_epoch_duration_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
}

fn unix_deadline_ms_after_since(now: Duration, grace: Duration) -> u64 {
    let deadline = now.saturating_add(grace);
    u64::try_from(deadline.as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::super::{NegotiatedProtocol, ServerConfig};
    use super::*;
    use crate::config::{
        CoordinationConfig, MetricsConfig, ProtocolConfig, RelayTypeConfig, SessionConfig,
        TransportSecurityConfig, TurnConfig,
    };
    use crate::database::DatabaseConfig;
    use crate::protocol::{Topology, Transport};
    use tokio::sync::mpsc::error::TryRecvError;

    #[test]
    fn unix_deadline_ms_after_never_returns_drain_sentinel() {
        assert_eq!(
            unix_deadline_ms_after_since(Duration::ZERO, Duration::ZERO),
            1
        );
        assert_eq!(
            unix_deadline_ms_after_since(Duration::from_millis(41), Duration::from_millis(1)),
            42
        );
    }

    #[test]
    fn first_shutdown_drain_waits_only_unelapsed_grace() {
        let drain = ShutdownDrain {
            deadline_ms: 1_000,
            grace: Duration::from_secs(30),
            retry_after_secs: Some(30),
            started_by_this_call: true,
        };

        assert_eq!(
            drain.wait_before_close_since(Duration::from_secs(2), 0),
            Duration::from_secs(28)
        );
        assert_eq!(
            drain.wait_before_close_since(Duration::from_secs(31), 0),
            Duration::ZERO
        );
    }

    #[test]
    fn observed_shutdown_drain_waits_until_existing_deadline() {
        let drain = ShutdownDrain {
            deadline_ms: 1_100,
            grace: Duration::from_secs(30),
            retry_after_secs: Some(30),
            started_by_this_call: false,
        };

        assert_eq!(
            drain.wait_before_close_since(Duration::from_secs(2), 1_000),
            Duration::from_millis(100)
        );
        assert_eq!(
            drain.wait_before_close_since(Duration::ZERO, 1_100),
            Duration::ZERO
        );
        assert_eq!(
            drain.wait_before_close_since(Duration::ZERO, 1_200),
            Duration::ZERO
        );
    }

    #[tokio::test]
    async fn observed_shutdown_drain_does_not_reannounce_goingaway() {
        let server = EnhancedGameServer::new(
            ServerConfig::default(),
            ProtocolConfig::default(),
            RelayTypeConfig::default(),
            SessionConfig::default(),
            TurnConfig::default(),
            DatabaseConfig::InMemory,
            MetricsConfig::default(),
            CoordinationConfig::default(),
            TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("failed to construct test server");
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let player_id = server
            .register_client(sender, "127.0.0.1:48990".parse().unwrap())
            .await
            .expect("client registration succeeds");
        server.set_client_protocol(
            &player_id,
            NegotiatedProtocol {
                version: 3,
                transports: vec![Transport::Relay],
                topologies: vec![Topology::Relay],
            },
        );

        let first_drain = server.begin_shutdown_drain();
        assert!(first_drain.started_by_this_call);
        assert_eq!(server.announce_shutdown_drain(first_drain).await, 1);
        let first_message = receiver
            .recv()
            .await
            .expect("first drain should enqueue GoingAway");
        assert!(matches!(
            &*first_message,
            ServerMessage::GoingAway { deadline_ms, .. } if *deadline_ms == first_drain.deadline_ms
        ));

        let observed_drain = server.begin_shutdown_drain();
        assert!(!observed_drain.started_by_this_call);
        assert_eq!(
            observed_drain.deadline_ms, first_drain.deadline_ms,
            "later callers must observe the original advertised deadline"
        );
        assert_eq!(
            server.announce_shutdown_drain(observed_drain).await,
            0,
            "observed drains must not send a duplicate advisory"
        );
        let duplicate_advisory = receiver.try_recv();
        assert!(
            matches!(duplicate_advisory, Err(TryRecvError::Empty)),
            "duplicate drain should not enqueue another GoingAway"
        );
    }

    #[tokio::test]
    async fn active_socket_task_tracking_reflects_live_handlers() {
        let server = EnhancedGameServer::new(
            ServerConfig::default(),
            ProtocolConfig::default(),
            RelayTypeConfig::default(),
            SessionConfig::default(),
            TurnConfig::default(),
            DatabaseConfig::InMemory,
            MetricsConfig::default(),
            CoordinationConfig::default(),
            TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("failed to construct test server");

        assert!(
            !server.has_active_socket_tasks(),
            "a freshly constructed server has no live socket handlers"
        );
        let guard = server.track_socket_task();
        assert!(
            server.has_active_socket_tasks(),
            "a tracked handler must be reported as live for the drain grace skip"
        );
        drop(guard);
        assert!(
            !server.has_active_socket_tasks(),
            "a returned handler must stop counting toward the drain grace skip"
        );
    }

    /// A connection whose handler task arms during the idle settle beat must
    /// still get the grace wait: the empty active-task reading that would
    /// skip the wait is confirmed only after the beat, so a handler
    /// surfacing inside it flips the choreography back onto the full wait
    /// and receives its coded `4000` close. Red condition: an immediate
    /// skip (no settle beat) completes the choreography long before the
    /// grace elapses.
    #[tokio::test]
    async fn handler_armed_during_idle_settle_still_gets_the_grace() {
        let grace = Duration::from_millis(300);
        let server = EnhancedGameServer::new(
            ServerConfig {
                drain_grace: grace,
                ..ServerConfig::default()
            },
            ProtocolConfig::default(),
            RelayTypeConfig::default(),
            SessionConfig::default(),
            TurnConfig::default(),
            DatabaseConfig::InMemory,
            MetricsConfig::default(),
            CoordinationConfig::default(),
            TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("failed to construct test server");

        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        let started = std::time::Instant::now();
        let choreography = {
            let server = std::sync::Arc::clone(&server);
            tokio::spawn(async move {
                run_drain_choreography(&server, shutdown_tx).await;
            })
        };

        // Arm a handler inside the 50 ms settle beat. Timer deadlines are
        // ordered on one runtime, so this 20 ms sleep fires before the
        // beat's 50 ms recheck, deterministically.
        tokio::time::sleep(Duration::from_millis(20)).await;
        let guard = server.track_socket_task();

        // Release the guard well after the recheck but before the grace
        // expires, so the trailing settle wait observes zero active tasks.
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(guard);

        choreography
            .await
            .expect("choreography task must not panic");

        // The choreography began ~0 ms into this test; completing it must
        // have waited out at least the grace (settle beat + grace), not
        // skipped the wait while the handler was arming.
        assert!(
            started.elapsed() >= grace,
            "a handler that armed during the settle beat must still get the grace wait \
             (elapsed {:?}, grace {grace:?})",
            started.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_connection_wait_does_not_miss_zero_transition_after_count_check() {
        let server = EnhancedGameServer::new(
            ServerConfig::default(),
            ProtocolConfig::default(),
            RelayTypeConfig::default(),
            SessionConfig::default(),
            TurnConfig::default(),
            DatabaseConfig::InMemory,
            MetricsConfig::default(),
            CoordinationConfig::default(),
            TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("failed to construct test server");
        let mut guard = Some(server.track_socket_task());

        let remaining = tokio::select! {
            remaining = server.wait_for_shutdown_connections_after_active_check(
                Duration::from_secs(30),
                || {
                    drop(guard.take());
                },
            ) => remaining,
            () = tokio::time::sleep(Duration::from_millis(1)) => {
                panic!("shutdown wait missed the zero-active notification");
            }
        };

        assert_eq!(remaining, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn unrepresentable_shutdown_wait_does_not_expire_immediately() {
        let server = EnhancedGameServer::new(
            ServerConfig::default(),
            ProtocolConfig::default(),
            RelayTypeConfig::default(),
            SessionConfig::default(),
            TurnConfig::default(),
            DatabaseConfig::InMemory,
            MetricsConfig::default(),
            CoordinationConfig::default(),
            TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("failed to construct test server");
        let mut guard = Some(server.track_socket_task());

        let remaining = server
            .wait_for_shutdown_connections_after_active_check(Duration::from_secs(u64::MAX), || {
                drop(guard.take())
            })
            .await;

        assert_eq!(
            remaining, 0,
            "deadline overflow must still allow the active socket to settle"
        );
    }
}
