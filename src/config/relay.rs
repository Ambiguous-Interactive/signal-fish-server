//! Legacy integration-label configuration.

use super::defaults::default_relay_type;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Legacy integration labels emitted in room and peer protocol metadata.
///
/// These values are informational. They do not select, open, authenticate, or
/// prove a physical relay transport.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RelayTypeConfig {
    /// Map of game names to legacy labels (for example, `Chess` to
    /// `unity_netcode`).
    #[serde(default)]
    pub game_relay_mappings: HashMap<String, String>,
    /// Default legacy label for games not explicitly configured.
    #[serde(default = "default_relay_type")]
    pub default_relay_type: String,
}

impl Default for RelayTypeConfig {
    fn default() -> Self {
        Self {
            game_relay_mappings: HashMap::new(),
            default_relay_type: default_relay_type(),
        }
    }
}
