use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default constants for validation (can be overridden by config)
/// These are used when no config is available
#[allow(dead_code)]
pub const DEFAULT_MAX_GAME_NAME_LENGTH: usize = 64;
#[allow(dead_code)]
pub const DEFAULT_ROOM_CODE_LENGTH: usize = 6;
#[allow(dead_code)]
pub const DEFAULT_MAX_PLAYER_NAME_LENGTH: usize = 32;
#[allow(dead_code)]
pub const DEFAULT_MAX_PLAYERS_LIMIT: u8 = 100;
/// Default deployment region identifier when one is not configured.
pub const DEFAULT_REGION_ID: &str = "default";

/// Unique identifier for players
pub type PlayerId = Uuid;
/// Unique identifier for rooms
pub type RoomId = Uuid;

/// Relay transport protocol selection
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Default,
    Archive,
    RkyvSerialize,
    RkyvDeserialize,
)]
#[rkyv(compare(PartialEq))]
#[serde(rename_all = "lowercase")]
pub enum RelayTransport {
    /// TCP transport (reliable, ordered delivery)
    /// Recommended for: Turn-based games, lobby systems, RPGs
    Tcp,
    /// UDP transport (low-latency, unreliable)
    /// Recommended for: FPS, racing games, real-time action
    Udp,
    /// WebSocket transport (reliable, browser-compatible)
    /// Recommended for: WebGL builds, browser games, cross-platform
    Websocket,
    /// Automatic selection based on room size and game type
    /// Default: UDP for 2-4 players, TCP for 5+ players, WebSocket for browser builds
    #[default]
    Auto,
}

/// Encoding format for sequenced game data payloads.
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Default,
    Archive,
    RkyvSerialize,
    RkyvDeserialize,
)]
#[rkyv(compare(PartialEq))]
#[serde(rename_all = "snake_case")]
pub enum GameDataEncoding {
    /// JSON payloads delivered over text frames.
    #[default]
    Json,
    /// MessagePack payloads delivered over binary frames.
    #[serde(rename = "message_pack")]
    MessagePack,
    /// Rkyv zero-copy binary format for maximum performance.
    /// Recommended for: High-frequency updates, large player counts, latency-sensitive games.
    #[serde(rename = "rkyv")]
    Rkyv,
}

/// Data-path transport a client can use for game traffic.
///
/// `Relay` is the universal floor (WebSocket fan-out through the server) and is
/// always available. `Direct` is a LAN / routable IP:port path and `WebRtc` is a
/// peer-to-peer WebRTC data channel; both are opt-in upgrades negotiated in
/// `Authenticate`. Wire tokens (Appendix A): `relay`, `direct`, `webrtc`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// Server WebSocket fan-out — the mandatory floor every client supports.
    Relay,
    /// Direct IP:port connection (LAN / routable host).
    Direct,
    /// Peer-to-peer WebRTC data channel.
    ///
    /// `rename_all = "snake_case"` would emit `web_rtc`; Appendix A requires the
    /// token `webrtc`, so the variant is renamed explicitly.
    #[serde(rename = "webrtc")]
    WebRtc,
}

/// Session topology negotiated for a room.
///
/// `Relay` keeps the v2 server-relay hub. `Host` is a star topology around a
/// single authoritative peer, and `Mesh` is everyone-to-everyone. Wire tokens
/// (Appendix A): `relay`, `host`, `mesh`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Topology {
    /// Server relay hub (v2 behavior, always available).
    Relay,
    /// Star topology around a single host/authority.
    Host,
    /// Full mesh: every peer connects to every other peer.
    Mesh,
}

/// A single ICE (STUN/TURN) server entry delivered inside a [`SessionPlanPayload`].
///
/// Mirrors the browser `RTCIceServer` shape (Appendix A/B): `urls` holds one or
/// more STUN/TURN endpoints, and `username`/`credential` carry short-lived TURN
/// credentials when present. Both auth fields are omitted from the wire when
/// absent (public STUN needs no credentials). These are v3-only control payloads,
/// so — like [`ProtocolInfoPayload`] — no rkyv derives are added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceServer {
    /// STUN/TURN URLs for this server (e.g. `stun:stun.l.google.com:19302`).
    pub urls: Vec<String>,
    /// TURN username (omitted for credential-less STUN servers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// TURN credential (omitted for credential-less STUN servers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// One peer a recipient should connect to under the chosen session topology.
///
/// Part of a per-recipient [`SessionPlanPayload`] (Appendix B/E): the same room
/// produces a different `peers` list (and `initiate` flags) for each member.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPeer {
    /// The other peer's id.
    pub player_id: PlayerId,
    /// The other peer's display name.
    pub player_name: String,
    /// True if this peer is the session's authoritative host.
    ///
    /// In `host` topology this marks the elected host (the session authority): a
    /// client's single peer (the host) is `true`, and the host's client peers are
    /// `false`. In `mesh` it reflects the room's designated authority flag and is
    /// informational only (mesh has no central host).
    pub is_authority: bool,
    /// Whether the recipient sends the WebRTC offer to this peer ("you send the
    /// offer to this peer"), so exactly one side of each pair offers.
    ///
    /// In `mesh` topology this follows the deterministic glare rule (Appendix E:
    /// the lesser UUID initiates); in `host` topology the direction is fixed —
    /// each client initiates to the host and the host answers.
    pub initiate: bool,
}

/// The per-recipient session directive emitted at lobby finalization (v3 only).
///
/// Sent alongside the unchanged `GameStarting` to each v3-capable member of a
/// room whose negotiated plan is *not* the relay floor (Appendix A/B/D/E). It
/// tells the recipient which topology/transport to use, who (if anyone) the host
/// is, which peers to connect to (with per-recipient `initiate` flags), the ICE
/// servers to gather against, and the universal `fallback` transport.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPlanPayload {
    /// Chosen session topology (`relay`, `host`, or `mesh`).
    pub topology: Topology,
    /// Chosen data-path transport (`relay`, `direct`, or `webrtc`).
    pub transport: Transport,
    /// The elected host, present only for `host` topology.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<PlayerId>,
    /// Peers this recipient should connect to (excludes the recipient itself).
    pub peers: Vec<SessionPeer>,
    /// ICE (STUN/TURN) servers for WebRTC; empty (and omitted) for non-WebRTC plans.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ice_servers: Vec<IceServer>,
    /// The universal fallback transport — always [`Transport::Relay`], the floor.
    pub fallback: Transport,
}

/// Legacy, self-declared peer metadata carried in `GameStarting`.
///
/// This is preserved for the v2/back-compat handoff surface. It is not protocol
/// v3 transport negotiation and must not be treated as proof of direct/WebRTC
/// reachability.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConnectionInfo {
    /// Self-declared direct IP:port endpoint metadata.
    #[serde(rename = "direct")]
    Direct { host: String, port: u16 },
    /// Self-declared Unity Relay allocation metadata.
    #[serde(rename = "unity_relay")]
    UnityRelay {
        allocation_id: String,
        connection_data: String,
        key: String,
    },
    /// Self-declared relay server metadata.
    #[serde(rename = "relay")]
    Relay {
        /// Relay server host
        host: String,
        /// Relay server port (TCP or UDP depending on transport)
        port: u16,
        /// Transport protocol (TCP, UDP, or Auto)
        #[serde(default)]
        transport: RelayTransport,
        /// Allocation ID (room ID)
        allocation_id: String,
        /// Client authentication token (opaque server-issued value)
        token: String,
        /// Assigned client ID (set by server after connection)
        #[serde(skip_serializing_if = "Option::is_none")]
        client_id: Option<u16>,
    },
    /// Self-declared WebRTC metadata.
    #[serde(rename = "webrtc")]
    WebRTC {
        sdp: Option<String>,
        ice_candidates: Vec<String>,
    },
    /// Custom connection data (extensible for other types)
    #[serde(rename = "custom")]
    Custom { data: serde_json::Value },
}

/// Information about a player in a room
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    pub is_authority: bool,
    pub is_ready: bool,
    pub connected_at: chrono::DateTime<chrono::Utc>,
    /// Legacy self-declared peer metadata for `GameStarting`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<ConnectionInfo>,
    /// Deployment region that currently hosts this player (internal only).
    #[serde(skip_serializing, skip_deserializing, default)]
    pub region_id: String,
}

/// Information about a spectator watching a room
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectatorInfo {
    pub id: PlayerId,
    pub name: String,
    pub connected_at: chrono::DateTime<chrono::Utc>,
}

/// Describes why a spectator state change occurred.
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Default,
    Archive,
    RkyvSerialize,
    RkyvDeserialize,
)]
#[rkyv(compare(PartialEq))]
#[serde(rename_all = "snake_case")]
pub enum SpectatorStateChangeReason {
    #[default]
    Joined,
    VoluntaryLeave,
    Disconnected,
    Removed,
    RoomClosed,
}

/// Legacy peer metadata included in `GameStarting`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConnectionInfo {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub relay_type: String,
    /// Self-declared metadata provided by the peer, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_info: Option<ConnectionInfo>,
}

impl PeerConnectionInfo {
    /// Build wire-level legacy peer metadata from a room member snapshot.
    pub fn from_player(player: &PlayerInfo, relay_type: &str) -> Self {
        Self {
            player_id: player.id,
            player_name: player.name.clone(),
            is_authority: player.is_authority,
            relay_type: relay_type.to_owned(),
            connection_info: player.connection_info.clone(),
        }
    }

    /// Build wire-level legacy peer metadata for each room member.
    pub fn from_players<'a>(
        players: impl IntoIterator<Item = &'a PlayerInfo>,
        relay_type: &str,
    ) -> Vec<Self> {
        players
            .into_iter()
            .map(|player| Self::from_player(player, relay_type))
            .collect()
    }
}

/// Rate limit information for an application
#[derive(Debug, Clone, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct RateLimitInfo {
    /// Requests allowed per minute
    pub per_minute: u32,
    /// Requests allowed per hour
    pub per_hour: u32,
    /// Requests allowed per day
    pub per_day: u32,
}

/// Describes negotiated protocol capabilities for a specific SDK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolInfoPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_version: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub game_data_formats: Vec<GameDataEncoding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_name_rules: Option<PlayerNameRulesPayload>,
    /// Protocol version negotiated for this connection (v3+ only).
    ///
    /// `None` for negotiated v2 connections so the v2 wire contract stays
    /// byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u16>,
    /// Lowest protocol version this deployment accepts (v3+ only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_protocol_version: Option<u16>,
    /// Highest protocol version this deployment speaks (v3+ only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_protocol_version: Option<u16>,
}

/// Describes the characters your deployment allows inside `player_name`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerNameRulesPayload {
    pub max_length: usize,
    pub min_length: usize,
    pub allow_unicode_alphanumeric: bool,
    pub allow_spaces: bool,
    pub allow_leading_trailing_whitespace: bool,
    #[serde(default)]
    pub allowed_symbols: Vec<char>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_allowed_characters: Option<String>,
}

impl PlayerNameRulesPayload {
    pub fn from_protocol_config(config: &crate::config::ProtocolConfig) -> Self {
        let rules = &config.player_name_validation;
        Self {
            max_length: config.max_player_name_length,
            min_length: 1,
            allow_unicode_alphanumeric: rules.allow_unicode_alphanumeric,
            allow_spaces: rules.allow_spaces,
            allow_leading_trailing_whitespace: rules.allow_leading_trailing_whitespace,
            allowed_symbols: rules.allowed_symbols.clone(),
            additional_allowed_characters: rules.additional_allowed_characters.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    struct PeerConnectionCase {
        name: &'static str,
        player: PlayerInfo,
        relay_type: &'static str,
        expected_connection_info: serde_json::Value,
    }

    fn player_fixture(
        id: PlayerId,
        name: &str,
        is_authority: bool,
        connection_info: Option<ConnectionInfo>,
    ) -> PlayerInfo {
        PlayerInfo {
            id,
            name: name.to_string(),
            is_authority,
            is_ready: true,
            connected_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            connection_info,
            region_id: "test-region".to_string(),
        }
    }

    #[test]
    fn peer_connection_info_from_player_maps_wire_fields() {
        let cases = [
            PeerConnectionCase {
                name: "authority without connection metadata",
                player: player_fixture(
                    PlayerId::from_u128(0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa),
                    "Authority",
                    true,
                    None,
                ),
                relay_type: "matchbox",
                expected_connection_info: serde_json::Value::Null,
            },
            PeerConnectionCase {
                name: "peer with direct connection metadata",
                player: player_fixture(
                    PlayerId::from_u128(0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb),
                    "Peer",
                    false,
                    Some(ConnectionInfo::Direct {
                        host: "10.0.0.5".to_string(),
                        port: 7777,
                    }),
                ),
                relay_type: "custom-relay",
                expected_connection_info: json!({
                    "type": "direct",
                    "host": "10.0.0.5",
                    "port": 7777
                }),
            },
        ];

        for case in cases {
            let peer = PeerConnectionInfo::from_player(&case.player, case.relay_type);

            assert_eq!(
                peer.player_id, case.player.id,
                "{}: player_id must come from the room member snapshot",
                case.name
            );
            assert_eq!(
                peer.player_name, case.player.name,
                "{}: player_name must come from the room member snapshot",
                case.name
            );
            assert_eq!(
                peer.is_authority, case.player.is_authority,
                "{}: authority flag must come from the room member snapshot",
                case.name
            );
            assert_eq!(
                peer.relay_type, case.relay_type,
                "{}: relay_type must come from the finalized room",
                case.name
            );
            assert_eq!(
                serde_json::to_value(&peer.connection_info).unwrap(),
                case.expected_connection_info,
                "{}: connection_info must preserve the player's P2P metadata",
                case.name
            );
        }
    }

    #[test]
    fn peer_connection_info_from_players_preserves_member_order() {
        let players = [
            player_fixture(
                PlayerId::from_u128(0x11111111111111111111111111111111),
                "First",
                true,
                None,
            ),
            player_fixture(
                PlayerId::from_u128(0x22222222222222222222222222222222),
                "Second",
                false,
                None,
            ),
        ];

        let peers = PeerConnectionInfo::from_players(players.iter(), "matchbox");

        assert_eq!(peers.len(), players.len());
        assert_eq!(peers[0].player_id, players[0].id);
        assert_eq!(peers[1].player_id, players[1].id);
        assert!(peers.iter().all(|peer| peer.relay_type == "matchbox"));
    }
}
