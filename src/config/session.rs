//! Session topology / transport selection configuration (Protocol v3, PLAN §P3).
//!
//! Drives the server's `choose_session_plan` selection: the preferred
//! topology per game, whether WebRTC / Direct upgrades are permitted, and the
//! ICE servers advertised to clients when a WebRTC plan is chosen. Every upgrade
//! gracefully degrades to the relay floor, so even a fully-disabled deployment
//! keeps working exactly like v2.

use super::defaults::{default_enable_direct, default_enable_webrtc, default_session_topology};
use crate::protocol::{IceServer, Topology};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Session topology/transport policy.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SessionConfig {
    /// Preferred topology for games not in [`game_topology_mappings`](Self::game_topology_mappings).
    #[serde(default = "default_session_topology")]
    pub default_topology: Topology,
    /// Per-game topology overrides (e.g. `{"FastFPS": "mesh", "BoardGame": "host"}`).
    #[serde(default)]
    pub game_topology_mappings: HashMap<String, Topology>,
    /// Permit the WebRTC transport for `mesh` / `host` upgrades.
    #[serde(default = "default_enable_webrtc")]
    pub enable_webrtc: bool,
    /// Permit the Direct (LAN / routable) transport for `host` upgrades.
    #[serde(default = "default_enable_direct")]
    pub enable_direct: bool,
    /// ICE (STUN/TURN) servers advertised in a WebRTC `SessionPlan`.
    ///
    /// Empty by default to honor the zero-dependency ethos (P4 wires in
    /// STUN/TURN and ephemeral credentials).
    #[serde(default)]
    pub ice_servers: Vec<IceServer>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            default_topology: default_session_topology(),
            game_topology_mappings: HashMap::new(),
            enable_webrtc: default_enable_webrtc(),
            enable_direct: default_enable_direct(),
            ice_servers: Vec::new(),
        }
    }
}

impl SessionConfig {
    /// Validate session policy.
    ///
    /// A malformed ICE server (no non-empty URL) is useless and rejected. A
    /// non-`Relay` desired topology with *both* transports disabled is only
    /// warned about — the selection ladder safely downgrades such a room to the
    /// relay floor, so it is a misconfiguration, not a fatal error.
    #[must_use = "validation result must be checked; a malformed ICE server is an error"]
    pub fn validate(&self) -> anyhow::Result<()> {
        for (index, server) in self.ice_servers.iter().enumerate() {
            if !server.urls.iter().any(|url| !url.trim().is_empty()) {
                anyhow::bail!("session.ice_servers[{index}] must have at least one non-empty URL");
            }
        }

        let p2p_disabled = !self.enable_webrtc && !self.enable_direct;
        if p2p_disabled {
            let mut non_relay_topologies: Vec<Topology> = Vec::new();
            if self.default_topology != Topology::Relay {
                non_relay_topologies.push(self.default_topology);
            }
            for topology in self.game_topology_mappings.values() {
                if *topology != Topology::Relay {
                    non_relay_topologies.push(*topology);
                }
            }
            if !non_relay_topologies.is_empty() {
                tracing::warn!(
                    ?non_relay_topologies,
                    "session config requests non-relay topologies but both \
                     enable_webrtc and enable_direct are false; all rooms will \
                     downgrade to the relay floor"
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stun(url: &str) -> IceServer {
        IceServer {
            urls: vec![url.to_string()],
            username: None,
            credential: None,
        }
    }

    #[test]
    fn defaults_are_relay_floor_with_p2p_enabled() {
        let cfg = SessionConfig::default();
        assert_eq!(cfg.default_topology, Topology::Relay);
        assert!(cfg.game_topology_mappings.is_empty());
        assert!(cfg.enable_webrtc);
        assert!(cfg.enable_direct);
        assert!(cfg.ice_servers.is_empty());
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_accepts_well_formed_ice_servers() {
        let cfg = SessionConfig {
            ice_servers: vec![stun("stun:stun.l.google.com:19302")],
            ..SessionConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_ice_server_without_urls() {
        let cfg = SessionConfig {
            ice_servers: vec![IceServer {
                urls: vec![],
                username: None,
                credential: None,
            }],
            ..SessionConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_ice_server_with_only_blank_urls() {
        let cfg = SessionConfig {
            ice_servers: vec![IceServer {
                urls: vec!["".to_string(), "   ".to_string()],
                username: None,
                credential: None,
            }],
            ..SessionConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_warns_but_succeeds_when_topology_requires_disabled_p2p() {
        // Non-relay desired topology while both transports are off: the ladder
        // downgrades to relay, so this is a warning, not an error.
        let mut mappings = HashMap::new();
        mappings.insert("FastFPS".to_string(), Topology::Mesh);
        let cfg = SessionConfig {
            default_topology: Topology::Host,
            game_topology_mappings: mappings,
            enable_webrtc: false,
            enable_direct: false,
            ice_servers: Vec::new(),
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_ok_when_relay_only_and_p2p_disabled() {
        let cfg = SessionConfig {
            default_topology: Topology::Relay,
            game_topology_mappings: HashMap::new(),
            enable_webrtc: false,
            enable_direct: false,
            ice_servers: Vec::new(),
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn round_trips_through_json_with_defaults() {
        let json = "{}";
        let cfg: SessionConfig = serde_json::from_str(json).expect("empty object uses defaults");
        assert_eq!(cfg.default_topology, Topology::Relay);
        assert!(cfg.enable_webrtc);
        assert!(cfg.enable_direct);
    }

    #[test]
    fn parses_full_session_block() {
        let json = r#"{
            "default_topology": "mesh",
            "game_topology_mappings": { "BoardGame": "host" },
            "enable_webrtc": true,
            "enable_direct": false,
            "ice_servers": [ { "urls": ["stun:stun.l.google.com:19302"] } ]
        }"#;
        let cfg: SessionConfig = serde_json::from_str(json).expect("valid session block");
        assert_eq!(cfg.default_topology, Topology::Mesh);
        assert_eq!(
            cfg.game_topology_mappings.get("BoardGame"),
            Some(&Topology::Host)
        );
        assert!(cfg.enable_webrtc);
        assert!(!cfg.enable_direct);
        assert_eq!(cfg.ice_servers.len(), 1);
        assert!(cfg.validate().is_ok());
    }
}
