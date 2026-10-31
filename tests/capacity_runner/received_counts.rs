//! Compact owned sender counts with the same sorted JSON object as a tree map.

use crate::records::ReceiptName;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, Default)]
pub struct ReceivedCounts(Vec<(ReceiptName, u64)>);

impl ReceivedCounts {
    pub fn iter(&self) -> impl Iterator<Item = (&ReceiptName, &u64)> {
        self.0.iter().map(|(name, count)| (name, count))
    }

    pub fn keys(&self) -> impl Iterator<Item = &ReceiptName> {
        self.0.iter().map(|(name, _)| name)
    }

    pub fn values(&self) -> impl Iterator<Item = &u64> {
        self.0.iter().map(|(_, count)| count)
    }

    pub fn get_key_value(&self, name: &str) -> Option<(&ReceiptName, &u64)> {
        let position = self
            .0
            .binary_search_by(|(key, _)| key.as_str().cmp(name))
            .ok()?;
        let (name, count) = &self.0[position];
        Some((name, count))
    }

    pub fn get(&self, name: &str) -> Option<&u64> {
        self.get_key_value(name).map(|(_, count)| count)
    }
}

impl FromIterator<(ReceiptName, u64)> for ReceivedCounts {
    fn from_iter<T: IntoIterator<Item = (ReceiptName, u64)>>(iter: T) -> Self {
        let mut entries: Vec<_> = iter.into_iter().collect();
        if !entries.is_sorted_by(|left, right| left.0 <= right.0) {
            entries.sort_by(|left, right| left.0.cmp(&right.0));
        }
        entries.dedup_by(|later, earlier| {
            if later.0 != earlier.0 {
                return false;
            }
            earlier.1 = later.1;
            true
        });
        Self(entries)
    }
}

impl Serialize for ReceivedCounts {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

impl<'de> Deserialize<'de> for ReceivedCounts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CountsVisitor;
        impl<'de> Visitor<'de> for CountsVisitor {
            type Value = ReceivedCounts;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of sender names and delivery counts")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(entries.into_iter().collect())
            }
        }
        deserializer.deserialize_map(CountsVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn compact_counts_preserve_tree_map_json_and_last_duplicate_values() {
        for input in [
            vec![],
            vec![("", 0)],
            vec![
                ("z", 1),
                ("a", 2),
                ("z", u64::MAX),
                ("a", 0),
                ("未知", 7),
                ("a\0", 9),
            ],
        ] {
            for reverse in [false, true] {
                let mut entries: Vec<_> = input
                    .iter()
                    .map(|(name, count)| (ReceiptName::from(*name), *count))
                    .collect();
                if reverse {
                    entries.reverse();
                }
                let reference: BTreeMap<_, _> = entries.iter().cloned().collect();
                let compact: ReceivedCounts = entries.into_iter().collect();
                assert_eq!(
                    serde_json::to_string(&compact).unwrap(),
                    serde_json::to_string(&reference).unwrap()
                );
                for name in ["", "a", "z", "a\0", "未知", "absent"] {
                    assert_eq!(compact.get(name), reference.get(name));
                }
                assert_eq!(
                    compact.values().copied().collect::<Vec<_>>(),
                    reference.values().copied().collect::<Vec<_>>()
                );
                let decoded: ReceivedCounts =
                    serde_json::from_str(&serde_json::to_string(&compact).unwrap()).unwrap();
                assert_eq!(
                    serde_json::to_value(decoded).unwrap(),
                    serde_json::to_value(reference).unwrap()
                );
            }
        }
        let json = r#"{"z":1,"a":2,"z":3,"a":4}"#;
        let compact: ReceivedCounts = serde_json::from_str(json).unwrap();
        let reference: BTreeMap<String, u64> = serde_json::from_str(json).unwrap();
        assert_eq!(
            serde_json::to_string(&compact).unwrap(),
            serde_json::to_string(&reference).unwrap()
        );
        for invalid in [
            "[]",
            "null",
            r#"{"a":-1}"#,
            r#"{"a":18446744073709551616}"#,
            r#"{"a":"1"}"#,
        ] {
            assert!(
                serde_json::from_str::<ReceivedCounts>(invalid).is_err(),
                "{invalid}"
            );
        }
    }
}
