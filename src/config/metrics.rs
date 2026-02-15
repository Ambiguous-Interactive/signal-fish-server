//! Metrics configuration for signal-fish-server.

use super::defaults::{
    default_dashboard_cache_history_window_secs, default_dashboard_cache_refresh_interval_secs,
    default_dashboard_cache_ttl_secs, default_dashboard_history_fields, DashboardHistoryField,
};
use serde::{Deserialize, Serialize};

/// Metrics configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    pub dashboard_cache_refresh_interval_secs: u64,
    pub dashboard_cache_ttl_secs: u64,
    pub dashboard_cache_history_window_secs: u64,
    pub dashboard_cache_history_fields: Vec<DashboardHistoryField>,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            dashboard_cache_refresh_interval_secs: default_dashboard_cache_refresh_interval_secs(),
            dashboard_cache_ttl_secs: default_dashboard_cache_ttl_secs(),
            dashboard_cache_history_window_secs: default_dashboard_cache_history_window_secs(),
            dashboard_cache_history_fields: default_dashboard_history_fields(),
        }
    }
}
