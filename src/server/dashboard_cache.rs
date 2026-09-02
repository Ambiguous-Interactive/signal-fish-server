use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use tokio::sync::{watch, RwLock};
use tokio::time::Duration;

use crate::config::DashboardHistoryField;
use crate::database::GameDatabase;

use super::chrono_duration_from_std;

#[derive(Debug, Clone)]
pub struct DashboardMetricsView {
    pub rooms_by_game: HashMap<String, usize>,
    pub player_percentiles: HashMap<String, f64>,
    pub game_percentiles: HashMap<String, HashMap<String, f64>>,
    pub active_rooms: usize,
    pub fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    pub stale: bool,
    pub last_error: Option<String>,
    pub refresh_interval_secs: u64,
    pub history: Vec<DashboardHistoryEntry>,
}

#[derive(Debug)]
pub(super) struct DashboardMetricsCache {
    inner: RwLock<DashboardMetricsCacheState>,
    refresh_interval: Duration,
    refresh_interval_secs: u64,
    stale_after: chrono::Duration,
    metrics: Arc<crate::metrics::ServerMetrics>,
    history_fields: HistoryFields,
    shutdown_tx: watch::Sender<bool>,
    #[cfg(test)]
    refresh_gate: Arc<RefreshGate>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct RefreshGate {
    pause_next: AtomicBool,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
impl RefreshGate {
    async fn wait_if_paused(&self) {
        if self.pause_next.swap(false, Ordering::AcqRel) {
            self.started.notify_one();
            self.release.notified().await;
        }
    }
}

#[derive(Debug)]
struct DashboardMetricsCacheState {
    snapshot: Option<DashboardMetricsSnapshot>,
    last_error: Option<String>,
    history: Vec<DashboardHistoryEntry>,
    history_capacity: usize,
}

impl DashboardMetricsCacheState {
    fn new(history_capacity: usize) -> Self {
        Self {
            snapshot: None,
            last_error: None,
            history: Vec::with_capacity(history_capacity),
            history_capacity: history_capacity.max(1),
        }
    }

    fn push_history(&mut self, snapshot: &DashboardMetricsSnapshot, fields: &HistoryFields) {
        let entry = DashboardHistoryEntry::from_snapshot(snapshot, fields);
        self.history.push(entry);
        if self.history.len() > self.history_capacity {
            let overflow = self.history.len().saturating_sub(self.history_capacity);
            self.history.drain(0..overflow);
        }
    }
}

#[derive(Debug, Clone)]
struct DashboardMetricsSnapshot {
    rooms_by_game: HashMap<String, usize>,
    player_percentiles: HashMap<String, f64>,
    game_percentiles: HashMap<String, HashMap<String, f64>>,
    active_rooms: usize,
    /// Wall clock (durable record): the absolute sample stamp surfaced in the
    /// dashboard payload, history, and the prometheus last-refresh gauge.
    fetched_at: chrono::DateTime<chrono::Utc>,
    /// Monotonic capture instant: the only input to the staleness decision,
    /// so a wall-clock step cannot flag a fresh cache stale or keep a dead
    /// one fresh. Deterministic under paused tokio time.
    fetched_at_instant: tokio::time::Instant,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardHistoryEntry {
    pub fetched_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_rooms: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rooms_by_game: Option<HashMap<String, usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_percentiles: Option<HashMap<String, f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_percentiles: Option<HashMap<String, HashMap<String, f64>>>,
}

impl DashboardHistoryEntry {
    fn from_snapshot(snapshot: &DashboardMetricsSnapshot, fields: &HistoryFields) -> Self {
        Self {
            fetched_at: snapshot.fetched_at,
            active_rooms: fields.active_rooms.then_some(snapshot.active_rooms),
            rooms_by_game: fields.rooms_by_game.then(|| snapshot.rooms_by_game.clone()),
            player_percentiles: fields
                .player_percentiles
                .then(|| snapshot.player_percentiles.clone()),
            game_percentiles: fields
                .game_percentiles
                .then(|| snapshot.game_percentiles.clone()),
        }
    }
}

const DASHBOARD_CACHE_HISTORY_MAX_CAPACITY: usize =
    crate::config::defaults::DASHBOARD_CACHE_HISTORY_MAX_SAMPLES;

impl DashboardMetricsCache {
    pub(super) fn new(
        refresh_interval: Duration,
        stale_after: Duration,
        metrics: Arc<crate::metrics::ServerMetrics>,
        history_capacity: usize,
        history_fields: &[DashboardHistoryField],
    ) -> Self {
        let safe_refresh = refresh_interval.max(Duration::from_secs(1));
        let safe_stale = stale_after.max(safe_refresh);
        let history_fields = HistoryFields::from_fields(history_fields);
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            inner: RwLock::new(DashboardMetricsCacheState::new(history_capacity)),
            refresh_interval: safe_refresh,
            refresh_interval_secs: safe_refresh.as_secs().max(1),
            stale_after: chrono_duration_from_std(safe_stale),
            metrics,
            history_fields,
            shutdown_tx,
            #[cfg(test)]
            refresh_gate: Arc::new(RefreshGate::default()),
        }
    }

    pub(super) fn history_capacity_for_window(
        refresh_interval: Duration,
        history_window_secs: u64,
    ) -> usize {
        let interval_secs = refresh_interval.as_secs().max(1);
        let window_secs = history_window_secs.max(interval_secs);
        let samples = window_secs.div_ceil(interval_secs);
        let bounded =
            samples.min(u64::try_from(DASHBOARD_CACHE_HISTORY_MAX_CAPACITY).unwrap_or(u64::MAX));
        usize::try_from(bounded)
            .unwrap_or(DASHBOARD_CACHE_HISTORY_MAX_CAPACITY)
            .max(1)
    }

    /// Spawn a refresh loop bounded by the cache and database owners.
    ///
    /// Each database read races the retained cache-shutdown signal without
    /// holding a strong cache reference. A completed sample upgrades the cache
    /// only long enough to publish it, and both owners are released before the
    /// task sleeps. Dropping the cache therefore cancels an in-flight read or
    /// wakes the sleeping task instead of retaining resources until the next
    /// configured refresh.
    pub(super) fn spawn(
        self: &Arc<Self>,
        database: Arc<dyn GameDatabase>,
    ) -> tokio::task::JoinHandle<()> {
        let cache = Arc::downgrade(self);
        let database = Arc::downgrade(&database);
        let mut shutdown = self.shutdown_tx.subscribe();
        #[cfg(test)]
        let refresh_gate = Arc::clone(&self.refresh_gate);
        tokio::spawn(async move {
            loop {
                if *shutdown.borrow() {
                    break;
                }
                let Some(database) = database.upgrade() else {
                    break;
                };
                let snapshot = tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            break;
                        }
                        continue;
                    }
                    snapshot = async {
                        #[cfg(test)]
                        refresh_gate.wait_if_paused().await;
                        Self::fetch_snapshot(database).await
                    } => snapshot,
                };

                let Some(cache) = cache.upgrade() else {
                    break;
                };
                cache.record_refresh_result(snapshot).await;
                let refresh_interval = cache.refresh_interval;
                drop(cache);

                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            break;
                        }
                    }
                    () = tokio::time::sleep(refresh_interval) => {}
                }
            }
        })
    }

    async fn record_refresh_result(&self, result: Result<DashboardMetricsSnapshot>) {
        match result {
            Ok(snapshot) => {
                {
                    let mut guard = self.inner.write().await;
                    guard.snapshot = Some(snapshot.clone());
                    guard.last_error = None;
                    guard.push_history(&snapshot, &self.history_fields);
                }
                self.metrics
                    .set_dashboard_cache_last_refresh(snapshot.fetched_at);
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to refresh dashboard metrics cache");
                {
                    let mut guard = self.inner.write().await;
                    guard.last_error = Some(err.to_string());
                }
                self.metrics.increment_dashboard_cache_refresh_failures();
            }
        }
    }

    /// Fetch one dashboard sample from storage.
    ///
    /// Consistency note: each query below takes its own `rooms.read()`
    /// acquisition, so a room created or deleted between them can produce a
    /// sample whose `active_rooms` disagrees with its percentile entries by
    /// the rooms moved in that window. This is a bounded, self-healing skew in
    /// a monitoring-only surface (the next refresh re-samples); unify the
    /// three reads under one acquisition only if a consumer ever joins those
    /// fields inside a single history entry.
    async fn fetch_snapshot(database: Arc<dyn GameDatabase>) -> Result<DashboardMetricsSnapshot> {
        let rooms_by_game = database.get_rooms_by_game().await?;
        let player_percentiles = database.get_player_count_percentiles().await?;
        let game_percentiles = database.get_game_player_percentiles().await?;
        let active_rooms = rooms_by_game.values().sum();

        Ok(DashboardMetricsSnapshot {
            rooms_by_game,
            player_percentiles,
            game_percentiles,
            active_rooms,
            // Wall clock (durable record): absolute sample stamp; the
            // staleness decision reads `fetched_at_instant` instead.
            fetched_at: chrono::Utc::now(),
            fetched_at_instant: tokio::time::Instant::now(),
        })
    }

    pub(super) async fn view(&self) -> DashboardMetricsView {
        let guard = self.inner.read().await;
        let (rooms_by_game, player_percentiles, game_percentiles, active_rooms, fetched_at) =
            if let Some(snapshot) = &guard.snapshot {
                (
                    snapshot.rooms_by_game.clone(),
                    snapshot.player_percentiles.clone(),
                    snapshot.game_percentiles.clone(),
                    snapshot.active_rooms,
                    Some(snapshot.fetched_at),
                )
            } else {
                (HashMap::new(), HashMap::new(), HashMap::new(), 0, None)
            };

        let history = guard.history.clone();

        // Staleness is a monotonic elapsed decision (see the snapshot's
        // `fetched_at_instant`); the wall stamp is surfaced but never decides.
        let stale = match &guard.snapshot {
            Some(snapshot) => {
                let elapsed = snapshot.fetched_at_instant.elapsed();
                chrono::Duration::from_std(elapsed).unwrap_or(chrono::Duration::MAX)
                    > self.stale_after
            }
            None => true,
        };

        DashboardMetricsView {
            rooms_by_game,
            player_percentiles,
            game_percentiles,
            active_rooms,
            fetched_at,
            stale,
            last_error: guard.last_error.clone(),
            refresh_interval_secs: self.refresh_interval_secs,
            history,
        }
    }

    #[cfg(test)]
    fn pause_next_refresh_for_test(&self) {
        self.refresh_gate.pause_next.store(true, Ordering::Release);
    }

    #[cfg(test)]
    async fn wait_for_paused_refresh_for_test(&self) {
        self.refresh_gate.started.notified().await;
    }
}

impl Drop for DashboardMetricsCache {
    fn drop(&mut self) {
        self.shutdown_tx.send_replace(true);
    }
}

#[derive(Debug, Clone)]
struct HistoryFields {
    active_rooms: bool,
    rooms_by_game: bool,
    player_percentiles: bool,
    game_percentiles: bool,
}

impl HistoryFields {
    fn from_fields(fields: &[DashboardHistoryField]) -> Self {
        let mut settings = Self {
            active_rooms: false,
            rooms_by_game: false,
            player_percentiles: false,
            game_percentiles: false,
        };

        for field in fields {
            match field {
                DashboardHistoryField::ActiveRooms => settings.active_rooms = true,
                DashboardHistoryField::RoomsByGame => settings.rooms_by_game = true,
                DashboardHistoryField::PlayerPercentiles => {
                    settings.player_percentiles = true;
                }
                DashboardHistoryField::GamePercentiles => settings.game_percentiles = true,
                // Minimal stub variants don't track history
                DashboardHistoryField::ActiveConnections | DashboardHistoryField::RoomsCreated => {}
            }
        }

        if !(settings.active_rooms
            || settings.rooms_by_game
            || settings.player_percentiles
            || settings.game_percentiles)
        {
            settings.active_rooms = true;
        }

        settings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::InMemoryDatabase;

    /// The staleness gate is a monotonic elapsed decision: with no snapshot it
    /// reports stale, a fresh snapshot clears it, and it flips only when
    /// tokio time passes `stale_after`. Paused time would keep the gate fresh
    /// forever against a wall-clock implementation, so this test pins the
    /// decision input, not just the boundary.
    #[tokio::test(start_paused = true)]
    async fn staleness_gate_decides_on_monotonic_elapsed_time() {
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let stale_after = Duration::from_secs(120);
        let cache = DashboardMetricsCache::new(
            Duration::from_secs(60),
            stale_after,
            metrics,
            1,
            &[DashboardHistoryField::ActiveRooms],
        );

        assert!(
            cache.view().await.stale,
            "no snapshot yet: the cache must report stale"
        );

        let fresh = DashboardMetricsSnapshot {
            rooms_by_game: HashMap::new(),
            player_percentiles: HashMap::new(),
            game_percentiles: HashMap::new(),
            active_rooms: 0,
            fetched_at: chrono::Utc::now(),
            fetched_at_instant: tokio::time::Instant::now(),
        };
        cache.inner.write().await.snapshot = Some(fresh);
        assert!(
            !cache.view().await.stale,
            "a fresh snapshot must not report stale"
        );

        // Exactly at the boundary the cache is still fresh; one tick past it
        // the gate flips.
        tokio::time::advance(stale_after).await;
        assert!(
            !cache.view().await.stale,
            "staleness boundary is strict (elapsed > stale_after)"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(
            cache.view().await.stale,
            "a snapshot older than stale_after must report stale"
        );
    }

    #[tokio::test]
    async fn spawned_refresh_task_does_not_outlive_cache_owner() {
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let cache = Arc::new(DashboardMetricsCache::new(
            Duration::from_secs(60),
            Duration::from_secs(120),
            metrics,
            1,
            &[DashboardHistoryField::ActiveRooms],
        ));
        let database: Arc<dyn GameDatabase> = Arc::new(InMemoryDatabase::new());
        let cache_weak = Arc::downgrade(&cache);
        let database_weak = Arc::downgrade(&database);

        let refresh_task = cache.spawn(Arc::clone(&database));
        drop(cache);
        drop(database);

        tokio::time::timeout(Duration::from_secs(1), refresh_task)
            .await
            .expect("the refresh task should stop when its owner drops")
            .expect("the refresh task should not panic");

        assert!(
            cache_weak.upgrade().is_none(),
            "the refresh task must release the cache when its owner drops"
        );
        assert!(
            database_weak.upgrade().is_none(),
            "the refresh task must release the database when its owner drops"
        );
    }

    #[tokio::test]
    async fn spawned_refresh_task_refreshes_while_cache_is_owned() {
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let cache = Arc::new(DashboardMetricsCache::new(
            Duration::from_secs(60),
            Duration::from_secs(120),
            Arc::clone(&metrics),
            1,
            &[DashboardHistoryField::ActiveRooms],
        ));
        let database: Arc<dyn GameDatabase> = Arc::new(InMemoryDatabase::new());
        let refresh_task = cache.spawn(Arc::clone(&database));

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if metrics.snapshot().await.dashboard_cache.refresh_count > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("an owned cache should complete its initial refresh");

        drop(cache);
        drop(database);
        tokio::time::timeout(Duration::from_secs(1), refresh_task)
            .await
            .expect("the refresh task should stop after the cache drops")
            .expect("the refresh task should not panic");
    }

    #[tokio::test]
    async fn in_flight_refresh_releases_owners_when_cache_drops() {
        let metrics = Arc::new(crate::metrics::ServerMetrics::new());
        let cache = Arc::new(DashboardMetricsCache::new(
            Duration::from_secs(60),
            Duration::from_secs(120),
            metrics,
            1,
            &[DashboardHistoryField::ActiveRooms],
        ));
        let database: Arc<dyn GameDatabase> = Arc::new(InMemoryDatabase::new());
        let cache_weak = Arc::downgrade(&cache);
        let database_weak = Arc::downgrade(&database);
        cache.pause_next_refresh_for_test();
        let refresh_task = cache.spawn(Arc::clone(&database));
        cache.wait_for_paused_refresh_for_test().await;

        drop(cache);
        drop(database);
        tokio::time::timeout(Duration::from_secs(1), refresh_task)
            .await
            .expect("dropping the cache should cancel an in-flight refresh")
            .expect("the refresh task should not panic");

        assert!(
            cache_weak.upgrade().is_none(),
            "an in-flight refresh must not retain the cache"
        );
        assert!(
            database_weak.upgrade().is_none(),
            "cancelling an in-flight refresh must release the database"
        );
    }

    #[test]
    fn history_capacity_scales_with_window() {
        let refresh = Duration::from_secs(5);
        let capacity =
            DashboardMetricsCache::history_capacity_for_window(refresh, /*window*/ 300);
        assert_eq!(capacity, 60);
    }

    #[test]
    fn history_capacity_clamps_to_at_least_one_sample() {
        let refresh = Duration::from_secs(10);
        let capacity =
            DashboardMetricsCache::history_capacity_for_window(refresh, /*window*/ 3);
        assert_eq!(capacity, 1);
    }

    #[test]
    fn history_capacity_is_capped() {
        let refresh = Duration::from_secs(1);
        let capacity = DashboardMetricsCache::history_capacity_for_window(
            refresh,
            DASHBOARD_CACHE_HISTORY_MAX_CAPACITY as u64 * 10,
        );
        assert_eq!(capacity, DASHBOARD_CACHE_HISTORY_MAX_CAPACITY);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn history_entries_respect_selected_fields() {
        let snapshot = DashboardMetricsSnapshot {
            rooms_by_game: HashMap::from([("game".into(), 5usize)]),
            player_percentiles: HashMap::from([("p50".into(), 3.0)]),
            game_percentiles: HashMap::from([(
                "game".into(),
                HashMap::from([("p95".into(), 4.0)]),
            )]),
            active_rooms: 42,
            fetched_at: chrono::Utc::now(),
            fetched_at_instant: tokio::time::Instant::now(),
        };

        let fields = HistoryFields::from_fields(&[
            DashboardHistoryField::ActiveRooms,
            DashboardHistoryField::PlayerPercentiles,
        ]);

        let entry = DashboardHistoryEntry::from_snapshot(&snapshot, &fields);
        assert_eq!(entry.active_rooms, Some(42));
        assert!(entry.rooms_by_game.is_none());
        assert!(entry.game_percentiles.is_none());
        let player_data = entry
            .player_percentiles
            .expect("player percentiles should be present");
        assert_eq!(player_data.get("p50"), Some(&3.0));
    }
}
