//! Logging configuration types.

use super::defaults::{
    default_enable_file_logging, default_log_dir, default_log_filename, default_log_format,
    default_rotation,
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Logging configuration.
#[derive(Debug, Serialize, Clone)]
pub struct LoggingConfig {
    /// Directory path for log files
    #[serde(default = "default_log_dir")]
    pub dir: String,
    /// Log file base name
    #[serde(default = "default_log_filename")]
    pub filename: String,
    /// Rotation policy: "daily" (default), "hourly", or "never"
    #[serde(default = "default_rotation")]
    pub rotation: String,
    /// Optional tracing level; read from JSON as a string and converted to enum
    #[serde(default)]
    pub level: Option<LogLevel>,
    /// Enable rolling file logging in addition to stdout JSON logs
    #[serde(default = "default_enable_file_logging")]
    pub enable_file_logging: bool,
    /// Format for rendered logs
    #[serde(default = "default_log_format")]
    pub format: LogFormat,
}

// Custom implementation of Deserialize for LoggingConfig to handle various input formats
impl<'de> Deserialize<'de> for LoggingConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct LoggingConfigHelper {
            #[serde(default = "default_log_dir")]
            dir: String,
            #[serde(default = "default_log_filename")]
            filename: String,
            #[serde(default = "default_rotation")]
            rotation: String,
            #[serde(default)]
            level: Option<serde_json::Value>,
            #[serde(default = "default_enable_file_logging")]
            enable_file_logging: bool,
            #[serde(default = "default_log_format")]
            format: LogFormat,
        }

        let helper = LoggingConfigHelper::deserialize(deserializer)?;

        // Process level field to handle different formats
        let level = helper.level.and_then(|value| {
            if value.is_string() {
                // SAFETY: `is_string()` is true, so `as_str()` always returns `Some`.
                #[allow(clippy::unwrap_used)]
                let level_str = value.as_str().unwrap();
                match level_str.trim().to_lowercase().as_str() {
                    "trace" => Some(LogLevel::Trace),
                    "debug" => Some(LogLevel::Debug),
                    "info" => Some(LogLevel::Info),
                    "warn" | "warning" => Some(LogLevel::Warn),
                    "error" | "err" => Some(LogLevel::Error),
                    _ => {
                        // Invalid level string - use default (None)
                        eprintln!("Invalid log level '{level_str}', using default");
                        None
                    }
                }
            } else if value.is_array() {
                // Handle array case - take first string element if available
                value
                    .as_array()
                    .and_then(|arr| arr.first())
                    .and_then(|first| first.as_str())
                    .and_then(|s| match s.trim().to_lowercase().as_str() {
                        "trace" => Some(LogLevel::Trace),
                        "debug" => Some(LogLevel::Debug),
                        "info" => Some(LogLevel::Info),
                        "warn" | "warning" => Some(LogLevel::Warn),
                        "error" | "err" => Some(LogLevel::Error),
                        _ => None,
                    })
            } else {
                // Unknown format - use default (None)
                None
            }
        });

        Ok(Self {
            dir: helper.dir,
            filename: helper.filename,
            rotation: helper.rotation,
            level,
            enable_file_logging: helper.enable_file_logging,
            format: helper.format,
        })
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            dir: default_log_dir(),
            filename: default_log_filename(),
            rotation: default_rotation(),
            level: None,
            enable_file_logging: default_enable_file_logging(),
            format: default_log_format(),
        }
    }
}

/// Log level enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl<'de> Deserialize<'de> for LogLevel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let s = s.trim().to_lowercase();
        match s.as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" | "err" => Ok(Self::Error),
            other => Err(serde::de::Error::custom(format!(
                "invalid log level '{other}', expected one of: trace, debug, info, warn, error"
            ))),
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Log format enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Json,
    Text,
}
