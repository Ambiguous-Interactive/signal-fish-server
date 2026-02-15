//! Relay type configuration.

use super::defaults::default_relay_type;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Relay type configuration for game-to-relay mappings.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RelayTypeConfig {
    /// Map of game names to relay types (e.g., "Chess" -> "unity_netcode")
    #[serde(default)]
    pub game_relay_mappings: HashMap<String, String>,
    /// Default relay type for games not explicitly configured
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
