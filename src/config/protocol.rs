//! Protocol configuration types including SDK compatibility and player name validation.

use super::defaults::{
    default_allow_leading_trailing_whitespace, default_allow_spaces_in_player_names,
    default_allow_unicode_player_names, default_allowed_player_name_symbols,
    default_enable_message_pack_game_data, default_max_game_name_length,
    default_max_player_name_length, default_max_players_limit, default_max_protocol_version,
    default_min_protocol_version, default_room_code_length, default_sdk_enforce,
};
use crate::protocol::GameDataEncoding;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Highest protocol version this server build implements. Single source of truth
/// for the `max_protocol_version` default and validation ceiling. v3 is the
/// single unshipped/mutable "current" protocol: it adds the WebRTC signaling
/// surface (`Signal`/`NewPeer`/`SessionPlan`/`TransportStatus`) AND the delivery
/// reliability surface (server-stamped `GameData.seq` + incarnation `epoch` and
/// the opt-in `RelayStats` frame) over the frozen v2 floor. Deployments can clamp
/// back to 2 (pure v2) via `protocol.max_protocol_version`.
pub const SERVER_MAX_PROTOCOL_VERSION: u16 = 3;
/// Lowest protocol version this server build still accepts (the v2 floor).
pub const SERVER_MIN_PROTOCOL_VERSION: u16 = 2;

/// Protocol configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProtocolConfig {
    /// Maximum length for game names
    #[serde(default = "default_max_game_name_length")]
    pub max_game_name_length: usize,
    /// Length of room codes
    #[serde(default = "default_room_code_length")]
    pub room_code_length: usize,
    /// Maximum length for player names
    #[serde(default = "default_max_player_name_length")]
    pub max_player_name_length: usize,
    /// Maximum number of players allowed in a room
    #[serde(default = "default_max_players_limit")]
    pub max_players_limit: u8,
    /// Allow MessagePack (binary) payloads for game data transport.
    #[serde(default = "default_enable_message_pack_game_data")]
    pub enable_message_pack_game_data: bool,
    /// Lowest protocol version this deployment accepts (the v2 floor; default 2).
    #[serde(default = "default_min_protocol_version")]
    pub min_protocol_version: u16,
    /// Highest protocol version this deployment will negotiate (default 3).
    #[serde(default = "default_max_protocol_version")]
    pub max_protocol_version: u16,
    /// SDK compatibility manifest
    #[serde(default)]
    pub sdk_compatibility: SdkCompatibilityConfig,
    /// Player name validation rules
    #[serde(default)]
    pub player_name_validation: PlayerNameValidationConfig,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            max_game_name_length: default_max_game_name_length(),
            room_code_length: default_room_code_length(),
            max_player_name_length: default_max_player_name_length(),
            max_players_limit: default_max_players_limit(),
            enable_message_pack_game_data: default_enable_message_pack_game_data(),
            min_protocol_version: default_min_protocol_version(),
            max_protocol_version: default_max_protocol_version(),
            sdk_compatibility: SdkCompatibilityConfig::default(),
            player_name_validation: PlayerNameValidationConfig::default(),
        }
    }
}

impl ProtocolConfig {
    /// Return the ordered list of game data encodings that this server will advertise
    /// to clients during the authentication handshake.
    pub fn supported_game_data_formats(&self) -> Vec<GameDataEncoding> {
        let mut formats = vec![GameDataEncoding::Json];
        if self.enable_message_pack_game_data {
            formats.push(GameDataEncoding::MessagePack);
        }
        formats
    }

    /// Negotiate the protocol version for a connection.
    ///
    /// The result is clamped into `[min_protocol_version, max_protocol_version]`.
    /// A client that omits its highest supported version (`None`) is treated as a
    /// pure v2 client and negotiated to `min_protocol_version`. A client that
    /// advertises a higher version than the server speaks is clamped down to
    /// `max_protocol_version`; one that advertises below the floor is raised to
    /// `min_protocol_version`.
    pub fn negotiate_protocol_version(&self, client_max: Option<u16>) -> u16 {
        client_max
            .unwrap_or(self.min_protocol_version)
            .min(self.max_protocol_version)
            .max(self.min_protocol_version)
    }

    /// Validate protocol version bounds.
    ///
    /// Invariants: the floor must be at least [`SERVER_MIN_PROTOCOL_VERSION`]
    /// (v2 is the oldest supported wire contract), the range must be non-empty
    /// (`max >= min`), and the ceiling must not exceed what this build implements
    /// ([`SERVER_MAX_PROTOCOL_VERSION`]).
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.min_protocol_version < SERVER_MIN_PROTOCOL_VERSION {
            anyhow::bail!(
                "protocol.min_protocol_version ({}) must be >= {SERVER_MIN_PROTOCOL_VERSION}",
                self.min_protocol_version
            );
        }
        if self.max_protocol_version < self.min_protocol_version {
            anyhow::bail!(
                "protocol.max_protocol_version ({}) must be >= protocol.min_protocol_version ({})",
                self.max_protocol_version,
                self.min_protocol_version
            );
        }
        if self.max_protocol_version > SERVER_MAX_PROTOCOL_VERSION {
            anyhow::bail!(
                "protocol.max_protocol_version ({}) must be <= {SERVER_MAX_PROTOCOL_VERSION} \
                 (the highest version this server build implements)",
                self.max_protocol_version
            );
        }
        Ok(())
    }
}

/// Player name validation configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PlayerNameValidationConfig {
    /// Allow non-ASCII letters/digits (Unicode alphanumerics)
    #[serde(default = "default_allow_unicode_player_names")]
    pub allow_unicode_alphanumeric: bool,
    /// Permit spaces between words (internal spaces only by default)
    #[serde(default = "default_allow_spaces_in_player_names")]
    pub allow_spaces: bool,
    /// Permit leading or trailing whitespace (still trimmed when checking emptiness)
    #[serde(default = "default_allow_leading_trailing_whitespace")]
    pub allow_leading_trailing_whitespace: bool,
    /// Symbol characters that are always allowed in addition to alphanumeric chars
    #[serde(default = "default_allowed_player_name_symbols")]
    pub allowed_symbols: Vec<char>,
    /// Optional string of additional characters that should be accepted
    #[serde(default)]
    pub additional_allowed_characters: Option<String>,
}

impl Default for PlayerNameValidationConfig {
    fn default() -> Self {
        Self {
            allow_unicode_alphanumeric: default_allow_unicode_player_names(),
            allow_spaces: default_allow_spaces_in_player_names(),
            allow_leading_trailing_whitespace: default_allow_leading_trailing_whitespace(),
            allowed_symbols: default_allowed_player_name_symbols(),
            additional_allowed_characters: None,
        }
    }
}

impl PlayerNameValidationConfig {
    pub fn is_allowed_symbol(&self, ch: char) -> bool {
        if self.allowed_symbols.contains(&ch) {
            return true;
        }
        if let Some(extra) = &self.additional_allowed_characters {
            return extra.chars().any(|extra_ch| extra_ch == ch);
        }
        false
    }
}

/// SDK compatibility manifest with per-platform requirements.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SdkCompatibilityConfig {
    /// Enforce platform/version requirements when true.
    #[serde(default = "default_sdk_enforce")]
    pub enforce: bool,
    /// Minimum supported SDK versions per platform (semver).
    #[serde(default)]
    pub minimum_versions: HashMap<String, String>,
    /// Recommended SDK versions per platform (semver).
    #[serde(default)]
    pub recommended_versions: HashMap<String, String>,
    /// Feature capabilities available per platform (`_default` applies globally).
    #[serde(default)]
    pub capabilities: HashMap<String, Vec<String>>,
    /// Optional platform-specific notes exposed to clients.
    #[serde(default)]
    pub notes: HashMap<String, String>,
}

impl Default for SdkCompatibilityConfig {
    fn default() -> Self {
        let mut minimum_versions = HashMap::new();
        minimum_versions.insert("unity".to_string(), "1.10.0".to_string());
        minimum_versions.insert("godot".to_string(), "0.9.0".to_string());
        minimum_versions.insert("godot-rust".to_string(), "0.9.0".to_string());
        minimum_versions.insert("test".to_string(), "1.0.0".to_string());

        let mut recommended_versions = HashMap::new();
        recommended_versions.insert("unity".to_string(), "1.12.0".to_string());
        recommended_versions.insert("godot-rust".to_string(), "0.9.2".to_string());

        let mut capabilities = HashMap::new();
        capabilities.insert(
            "_default".to_string(),
            vec![
                "reconnection".to_string(),
                "spectator-mode".to_string(),
                "rate-limits-v2".to_string(),
            ],
        );
        capabilities.insert(
            "unity".to_string(),
            vec![
                "relay-matchbox".to_string(),
                "relay-mirror".to_string(),
                "relay-fishnet".to_string(),
            ],
        );

        let mut notes = HashMap::new();
        notes.insert(
            "unity".to_string(),
            "Upgrade to 1.12.x for deterministic reconnect handling and spectator patches."
                .to_string(),
        );

        Self {
            enforce: default_sdk_enforce(),
            minimum_versions,
            recommended_versions,
            capabilities,
            notes,
        }
    }
}

impl SdkCompatibilityConfig {
    /// Evaluate SDK compatibility for the provided platform/version tuple.
    pub fn evaluate(
        &self,
        platform: Option<&str>,
        version: Option<&str>,
    ) -> Result<SdkCompatibilityReport, SdkCompatibilityError> {
        let normalized_platform = platform.map(str::to_ascii_lowercase);

        if normalized_platform.is_none() && self.enforce && !self.minimum_versions.is_empty() {
            return Err(SdkCompatibilityError::PlatformMissing);
        }

        let (platform_key, platform_display) = match normalized_platform {
            Some(ref key) => (Some(key.clone()), platform.map(ToString::to_string)),
            None => (None, None),
        };

        if let Some(ref key) = platform_key {
            if !self.minimum_versions.contains_key(key)
                && !self.recommended_versions.contains_key(key)
                && !self.capabilities.contains_key(key)
                && !self.notes.contains_key(key)
                && self.enforce
                && !self.minimum_versions.is_empty()
            {
                return Err(SdkCompatibilityError::PlatformUnknown {
                    platform: platform_display.unwrap_or_else(|| key.clone()),
                });
            }

            if let Some(minimum) = self.minimum_versions.get(key) {
                let version_str = version.ok_or_else(|| SdkCompatibilityError::VersionMissing {
                    platform: platform_display.clone().unwrap_or_else(|| key.clone()),
                })?;

                let parsed_version = semver::Version::parse(version_str).map_err(|err| {
                    SdkCompatibilityError::VersionInvalid {
                        platform: platform_display.clone().unwrap_or_else(|| key.clone()),
                        version: version_str.to_string(),
                        source: err,
                    }
                })?;

                let parsed_minimum = semver::Version::parse(minimum).map_err(|err| {
                    SdkCompatibilityError::VersionInvalid {
                        platform: platform_display.clone().unwrap_or_else(|| key.clone()),
                        version: minimum.clone(),
                        source: err,
                    }
                })?;

                if parsed_version < parsed_minimum {
                    return Err(SdkCompatibilityError::VersionTooLow {
                        platform: platform_display.clone().unwrap_or_else(|| key.clone()),
                        version: version_str.to_string(),
                        minimum: minimum.clone(),
                    });
                }
            } else if self.enforce && !self.minimum_versions.is_empty() {
                return Err(SdkCompatibilityError::PlatformUnknown {
                    platform: platform_display.unwrap_or_else(|| key.clone()),
                });
            }
        }

        let mut capabilities = Vec::new();
        if let Some(default_caps) = self.capabilities.get("_default") {
            capabilities.extend(default_caps.iter().cloned());
        }
        if let Some(ref key) = platform_key {
            if let Some(platform_caps) = self.capabilities.get(key) {
                for cap in platform_caps {
                    if !capabilities.iter().any(|existing| existing == cap) {
                        capabilities.push(cap.clone());
                    }
                }
            }
        }

        let notes = platform_key
            .as_ref()
            .and_then(|key| self.notes.get(key))
            .cloned()
            .or_else(|| self.notes.get("_default").cloned());

        let minimum_version = platform_key
            .as_ref()
            .and_then(|key| self.minimum_versions.get(key).cloned());
        let recommended_version = platform_key
            .as_ref()
            .and_then(|key| self.recommended_versions.get(key).cloned());

        Ok(SdkCompatibilityReport {
            platform: platform_display,
            sdk_version: version.map(ToString::to_string),
            minimum_version,
            recommended_version,
            capabilities,
            notes,
        })
    }
}

/// SDK compatibility report.
#[derive(Debug, Clone)]
pub struct SdkCompatibilityReport {
    pub platform: Option<String>,
    pub sdk_version: Option<String>,
    pub minimum_version: Option<String>,
    pub recommended_version: Option<String>,
    pub capabilities: Vec<String>,
    pub notes: Option<String>,
}

/// SDK compatibility error.
#[derive(Debug, thiserror::Error)]
pub enum SdkCompatibilityError {
    #[error("SDK platform is required for compatibility validation")]
    PlatformMissing,
    #[error("SDK version is required for platform `{platform}`")]
    VersionMissing { platform: String },
    #[error("SDK platform `{platform}` is not recognized")]
    PlatformUnknown { platform: String },
    #[error("SDK version `{version}` for platform `{platform}` is invalid: {source}")]
    VersionInvalid {
        platform: String,
        version: String,
        #[source]
        source: semver::Error,
    },
    #[error(
        "SDK version `{version}` for platform `{platform}` is below the minimum supported version `{minimum}`"
    )]
    VersionTooLow {
        platform: String,
        version: String,
        minimum: String,
    },
}

#[cfg(test)]
mod protocol_version_tests {
    use super::*;

    #[test]
    fn default_protocol_version_bounds() {
        let cfg = ProtocolConfig::default();
        assert_eq!(cfg.min_protocol_version, 2);
        assert_eq!(cfg.max_protocol_version, 3);
        assert_eq!(cfg.min_protocol_version, SERVER_MIN_PROTOCOL_VERSION);
        assert_eq!(cfg.max_protocol_version, SERVER_MAX_PROTOCOL_VERSION);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn negotiate_clamps_into_range() {
        let cfg = ProtocolConfig::default(); // [2, 3]

        // v3 client + v3 server => 3
        assert_eq!(cfg.negotiate_protocol_version(Some(3)), 3);
        // a stale v4-era (or future) client is clamped down to the ceiling (3)
        assert_eq!(cfg.negotiate_protocol_version(Some(4)), 3);
        // client beyond server max => clamped to max
        assert_eq!(cfg.negotiate_protocol_version(Some(9)), 3);
        // v2 client (None) => floor
        assert_eq!(cfg.negotiate_protocol_version(None), 2);
        // client below floor => raised to min
        assert_eq!(cfg.negotiate_protocol_version(Some(1)), 2);
        // exact floor
        assert_eq!(cfg.negotiate_protocol_version(Some(2)), 2);
    }

    /// Issue #136 (F6): the DEFAULT configuration must accept clients that
    /// send no platform or an unregistered one (custom engines, the Rust
    /// reference client). SDK platform/version enforcement is opt-in — a
    /// deployment that flips `enforce` on regains the strict behavior.
    #[test]
    fn default_config_accepts_unregistered_and_missing_platforms() {
        let cfg = SdkCompatibilityConfig::default();
        assert!(
            cfg.evaluate(None, None).is_ok(),
            "platform: None must pass under the default (non-enforcing) config"
        );
        assert!(
            cfg.evaluate(Some("rust"), Some("0.7.0")).is_ok(),
            "an unregistered platform must pass under the default config"
        );

        let strict = SdkCompatibilityConfig {
            enforce: true,
            ..SdkCompatibilityConfig::default()
        };
        assert!(
            strict.evaluate(Some("rust"), Some("0.7.0")).is_err(),
            "opting in to enforcement restores the strict rejection"
        );
    }

    #[test]
    fn supported_game_data_formats_follow_runtime_negotiation_policy() {
        let enabled = ProtocolConfig {
            enable_message_pack_game_data: true,
            ..ProtocolConfig::default()
        };
        assert_eq!(
            enabled.supported_game_data_formats(),
            vec![GameDataEncoding::Json, GameDataEncoding::MessagePack],
            "default advertised formats are exactly the negotiable client formats"
        );

        let disabled = ProtocolConfig {
            enable_message_pack_game_data: false,
            ..ProtocolConfig::default()
        };
        assert_eq!(
            disabled.supported_game_data_formats(),
            vec![GameDataEncoding::Json],
            "JSON remains the floor when MessagePack negotiation is disabled"
        );

        for cfg in [enabled, disabled] {
            assert!(
                !cfg.supported_game_data_formats()
                    .contains(&GameDataEncoding::Rkyv),
                "Rkyv is a reserved/internal enum variant, not a ProtocolInfo-advertised format"
            );
        }
    }

    #[test]
    fn negotiate_with_server_max_two_clamps_v3_client() {
        let cfg = ProtocolConfig {
            min_protocol_version: 2,
            max_protocol_version: 2,
            ..ProtocolConfig::default()
        };
        // v3 client + server max=2 => clamped to 2
        assert_eq!(cfg.negotiate_protocol_version(Some(3)), 2);
        assert_eq!(cfg.negotiate_protocol_version(None), 2);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn custom_valid_range_accepted() {
        let cfg = ProtocolConfig {
            min_protocol_version: 3,
            max_protocol_version: 3,
            ..ProtocolConfig::default()
        };
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.negotiate_protocol_version(None), 3);
    }

    #[test]
    fn validate_rejects_min_below_floor() {
        let cfg = ProtocolConfig {
            min_protocol_version: 1,
            max_protocol_version: 3,
            ..ProtocolConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_max_below_min() {
        let cfg = ProtocolConfig {
            min_protocol_version: 3,
            max_protocol_version: 2,
            ..ProtocolConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_max_above_server_ceiling() {
        let cfg = ProtocolConfig {
            min_protocol_version: 2,
            max_protocol_version: SERVER_MAX_PROTOCOL_VERSION + 1,
            ..ProtocolConfig::default()
        };
        assert!(cfg.validate().is_err());
    }
}
