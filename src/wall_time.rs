//! Formatting for timestamps already exposed by the server.

use std::fmt;

use jiff::{tz::Offset, Timestamp};
use serde::{Serialize, Serializer};

pub(crate) struct TimestampDisplay(pub Timestamp, pub TimestampSuffix);

pub(crate) enum TimestampSuffix {
    Zulu,
    Offset,
}

impl fmt::Display for TimestampDisplay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let nanos = self.0.subsec_nanosecond();
        let precision = if nanos == 0 {
            0
        } else if nanos % 1_000_000 == 0 {
            3
        } else if nanos % 1_000 == 0 {
            6
        } else {
            9
        };
        match self.1 {
            TimestampSuffix::Zulu => write!(formatter, "{:.precision$}", self.0),
            TimestampSuffix::Offset => {
                write!(
                    formatter,
                    "{:.precision$}",
                    self.0.display_with_offset(Offset::UTC)
                )
            }
        }
    }
}

impl Serialize for TimestampDisplay {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

pub(crate) fn serialize_timestamp<S: Serializer>(
    timestamp: &Timestamp,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    TimestampDisplay(*timestamp, TimestampSuffix::Zulu).serialize(serializer)
}

pub(crate) fn serialize_optional_timestamp<S: Serializer>(
    timestamp: &Option<Timestamp>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    timestamp
        .map(|timestamp| TimestampDisplay(timestamp, TimestampSuffix::Zulu))
        .serialize(serializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_display_preserves_wire_precision_and_suffix() {
        for (input, zulu, offset) in [
            (
                "2024-01-02T03:04:05Z",
                "2024-01-02T03:04:05Z",
                "2024-01-02T03:04:05+00:00",
            ),
            (
                "2024-01-02T03:04:05.1Z",
                "2024-01-02T03:04:05.100Z",
                "2024-01-02T03:04:05.100+00:00",
            ),
            (
                "1969-12-31T23:59:59.1234Z",
                "1969-12-31T23:59:59.123400Z",
                "1969-12-31T23:59:59.123400+00:00",
            ),
            (
                "2024-01-02T03:04:05.1234567Z",
                "2024-01-02T03:04:05.123456700Z",
                "2024-01-02T03:04:05.123456700+00:00",
            ),
        ] {
            let timestamp: Timestamp = input.parse().unwrap();
            assert_eq!(
                TimestampDisplay(timestamp, TimestampSuffix::Offset).to_string(),
                offset
            );
            let json =
                serde_json::to_string(&TimestampDisplay(timestamp, TimestampSuffix::Zulu)).unwrap();
            assert_eq!(json, format!("\"{zulu}\""));
            assert_eq!(serde_json::from_str::<Timestamp>(&json).unwrap(), timestamp);
        }
    }
}
