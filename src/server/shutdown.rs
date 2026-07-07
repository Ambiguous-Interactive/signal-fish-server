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
            Ok(_) => ShutdownDrain {
                deadline_ms,
                grace,
                retry_after_secs: retry_after_secs(grace),
                started_by_this_call: true,
            },
            Err(existing_deadline_ms) => ShutdownDrain {
                deadline_ms: existing_deadline_ms,
                grace,
                retry_after_secs: retry_after_secs(grace),
                started_by_this_call: false,
            },
        }
    }

    /// Best-effort v3 shutdown advisory fan-out.
    ///
    /// The close frame is the authoritative signal. `GoingAway` is advisory, so
    /// this never waits behind a full queue during shutdown.
    pub async fn announce_shutdown_drain(&self, drain: ShutdownDrain) -> usize {
        let message = ServerMessage::GoingAway {
            deadline_ms: drain.deadline_ms,
            retry_after_secs: drain.retry_after_secs,
        };
        let mut enqueued = 0usize;
        for player_id in self.connection_manager.client_ids() {
            if !self.client_supports_v3(&player_id) {
                continue;
            }
            match self
                .message_coordinator
                .try_send_to_player(&player_id, std::sync::Arc::new(message.clone()))
                .await
            {
                Ok(true) => enqueued += 1,
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
                requested += 1;
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
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            let notified = self.active_socket_tasks_notify.notified();
            tokio::pin!(notified);

            let active_tasks = self.active_socket_tasks.load(Ordering::Acquire);
            if active_tasks == 0 {
                return 0;
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return active_tasks;
            }

            tokio::select! {
                () = &mut notified => {}
                () = tokio::time::sleep_until(deadline) => {
                    return self.active_socket_tasks.load(Ordering::Acquire);
                }
            }
        }
    }
}

fn retry_after_secs(grace: Duration) -> Option<u64> {
    (grace > Duration::ZERO).then_some(grace.as_secs().max(1))
}

fn unix_deadline_ms_after(grace: Duration) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let deadline = now.saturating_add(grace);
    u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX)
}
