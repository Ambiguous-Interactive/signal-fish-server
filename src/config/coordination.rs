//! Process-local coordination configuration and future-backend seams.

use super::defaults::default_membership_snapshot_interval_secs;
use serde::{Deserialize, Serialize};

/// Configuration for the in-memory coordinator.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CoordinationConfig {
    /// Reserved interval for a future membership-snapshot backend (seconds).
    /// The shipped coordinator does not exchange snapshots between processes.
    #[serde(default = "default_membership_snapshot_interval_secs")]
    pub membership_snapshot_interval_secs: u64,
}

impl Default for CoordinationConfig {
    fn default() -> Self {
        Self {
            membership_snapshot_interval_secs: default_membership_snapshot_interval_secs(),
        }
    }
}
