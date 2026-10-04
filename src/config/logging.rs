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
    /// Optional tracing level; parsed by `LogLevel`'s strict deserializer
    /// (case/whitespace-tolerant, `warning`/`err` aliases); an unrecognized
    /// or non-string value is a load-time error
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
            /// Parsed by `LogLevel`'s own strict deserializer (case- and
            /// whitespace-tolerant, with the `warning`/`err` aliases). An
            /// unrecognized string or a non-string value is a hard error —
            /// the same present-but-invalid treatment every other knob gets
            /// from the loader — never a silent revert to the default level.
            #[serde(default)]
            level: Option<LogLevel>,
            #[serde(default = "default_enable_file_logging")]
            enable_file_logging: bool,
            #[serde(default = "default_log_format")]
            format: LogFormat,
        }

        let helper = LoggingConfigHelper::deserialize(deserializer)?;

        Ok(Self {
            dir: helper.dir,
            filename: helper.filename,
            rotation: helper.rotation,
            level: helper.level,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Json,
    Text,
}

impl LogFormat {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Text => "text",
        }
    }
}

impl<'de> Deserialize<'de> for LogFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let s = s.trim().to_lowercase();
        match s.as_str() {
            "json" => Ok(Self::Json),
            "text" => Ok(Self::Text),
            other => Err(serde::de::Error::custom(format!(
                "invalid log format '{other}', expected one of: json, text"
            ))),
        }
    }
}

impl fmt::Display for LogFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logging_from_json(json: &str) -> Result<LoggingConfig, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// An unrecognized level string is a hard error naming the knob value —
    /// the same treatment `logging.rotation` gets — never a silent revert to
    /// the default level (the loader's present-but-invalid contract).
    #[test]
    fn invalid_log_level_string_is_a_hard_error() {
        let error = logging_from_json(r#"{"level":"warng"}"#)
            .expect_err("unknown level string is a hard error");
        assert!(
            error.to_string().contains("invalid log level"),
            "error must name the invalid level: {error}"
        );
    }

    /// A non-string level value is a type error, not a silent `None` with no
    /// diagnostic.
    #[test]
    fn non_string_log_level_is_a_type_error() {
        logging_from_json(r#"{"level":7}"#).expect_err("numeric level is a type error");
        logging_from_json(r#"{"level":true}"#).expect_err("boolean level is a type error");
    }

    /// Case/whitespace tolerance and the documented aliases survive the
    /// strict parse; absent and explicit `null` both stay `None`.
    #[test]
    fn log_level_aliases_and_case_are_still_accepted() {
        assert_eq!(
            logging_from_json(r#"{"level":" WARNING "}"#)
                .expect("trimmed alias parses")
                .level,
            Some(LogLevel::Warn)
        );
        assert_eq!(
            logging_from_json(r#"{"level":"err"}"#)
                .expect("err alias parses")
                .level,
            Some(LogLevel::Error)
        );
        assert_eq!(
            logging_from_json(r#"{}"#)
                .expect("absent level parses")
                .level,
            None
        );
        assert_eq!(
            logging_from_json(r#"{"level":null}"#)
                .expect("explicit null parses")
                .level,
            None
        );
    }
}
