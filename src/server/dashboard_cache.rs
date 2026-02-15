use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;
use tokio::sync::RwLock;
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
            let overflow = self.history.len() - self.history_capacity;
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
    fetched_at: chrono::DateTime<chrono::Utc>,
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

const DASHBOARD_CACHE_HISTORY_MAX_CAPACITY: usize = 720;

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
        Self {
            inner: RwLock::new(DashboardMetricsCacheState::new(history_capacity)),
            refresh_interval: safe_refresh,
            refresh_interval_secs: safe_refresh.as_secs().max(1),
            stale_after: chrono_duration_from_std(safe_stale),
            metrics,
            history_fields,
        }
    }

    pub(super) fn history_capacity_for_window(
        refresh_interval: Duration,
        history_window_secs: u64,
    ) -> usize {
        let interval_secs = refresh_interval.as_secs().max(1);
        let window_secs = history_window_secs.max(interval_secs);
        let samples = window_secs.div_ceil(interval_secs);
        samples
            .max(1)
            .min(DASHBOARD_CACHE_HISTORY_MAX_CAPACITY as u64) as usize
    }

    pub(super) fn spawn(self: &Arc<Self>, database: Arc<dyn GameDatabase>) {
        let cache = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                cache.refresh_once(database.clone()).await;
                tokio::time::sleep(cache.refresh_interval).await;
            }
        });
    }

    async fn refresh_once(&self, database: Arc<dyn GameDatabase>) {
        match Self::fetch_snapshot(database).await {
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
            fetched_at: chrono::Utc::now(),
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

        let stale = if let Some(ts) = fetched_at {
            chrono::Utc::now().signed_duration_since(ts) > self.stale_after
        } else {
            true
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
