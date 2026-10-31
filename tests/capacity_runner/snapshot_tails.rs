//! Exact owned rejoin snapshot tails stored without per-snapshot tree nodes.

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;

type Tail = (String, u64);
type Entry = (String, Tail);

/// Sorted snapshot members with owned player identities and full sequence tails.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotTails(Box<[Entry]>);

impl SnapshotTails {
    /// Return whether the snapshot contains no members.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn position(&self, member: &str) -> Result<usize, usize> {
        self.0
            .binary_search_by(|(name, _)| name.as_str().cmp(member))
    }

    /// Find the exact recorded player identity and tail for one member.
    pub fn get(&self, member: &str) -> Option<&Tail> {
        self.position(member)
            .ok()
            .map(|position| &self.0[position].1)
    }

    #[cfg(test)]
    /// Mutate one fixture tail without changing member ordering.
    pub fn get_mut(&mut self, member: &str) -> Option<&mut Tail> {
        self.position(member)
            .ok()
            .map(|position| &mut self.0[position].1)
    }

    #[cfg(test)]
    /// Borrow the first fixture member in artifact key order.
    pub fn first_key_value(&self) -> Option<(&String, &Tail)> {
        self.0.first().map(|(member, tail)| (member, tail))
    }

    #[cfg(test)]
    /// Insert or replace a fixture tail while retaining sorted unique members.
    pub fn insert(&mut self, member: String, tail: Tail) {
        match self.position(&member) {
            Ok(position) => self.0[position].1 = tail,
            Err(position) => {
                let mut entries = std::mem::take(&mut self.0).into_vec();
                entries.insert(position, (member, tail));
                self.0 = entries.into_boxed_slice();
            }
        }
    }
}

impl From<BTreeMap<String, Tail>> for SnapshotTails {
    fn from(tails: BTreeMap<String, Tail>) -> Self {
        Self(tails.into_iter().collect())
    }
}

impl FromIterator<Entry> for SnapshotTails {
    fn from_iter<T: IntoIterator<Item = Entry>>(entries: T) -> Self {
        let mut entries: Vec<Entry> = entries.into_iter().collect();
        if !entries.is_sorted_by(|left, right| left.0 <= right.0) {
            entries.sort_by(|left, right| left.0.cmp(&right.0));
        }
        entries.dedup_by(|later, earlier| {
            if later.0 != earlier.0 {
                return false;
            }
            earlier.1 = std::mem::take(&mut later.1);
            true
        });
        Self(entries.into_boxed_slice())
    }
}

impl<'a> IntoIterator for &'a SnapshotTails {
    type Item = (&'a String, &'a Tail);
    type IntoIter = std::iter::Map<std::slice::Iter<'a, Entry>, fn(&'a Entry) -> Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter().map(|(member, tail)| (member, tail))
    }
}

#[cfg(test)]
impl std::ops::Index<&str> for SnapshotTails {
    type Output = Tail;

    fn index(&self, member: &str) -> &Self::Output {
        self.get(member).expect("snapshot member")
    }
}

impl Serialize for SnapshotTails {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self)
    }
}

impl<'de> Deserialize<'de> for SnapshotTails {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TailsVisitor;
        impl<'de> Visitor<'de> for TailsVisitor {
            type Value = SnapshotTails;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of member names and player identity/sequence tails")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries: Vec<Entry> = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(entries.into_iter().collect())
            }
        }
        deserializer.deserialize_map(TailsVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_snapshot_tails_preserve_exact_map_json_and_duplicate_keys() {
        for json in [
            "{}",
            r#"{"a":["id",0]}"#,
            r#"{"z":["old",1],"a":["\u672a\u77e5\n\\\"",18446744073709551615],"z":["last",2],"a":["replacement",7]}"#,
            r#"{"\u0000":["",0],"":["\u0000",18446744073709551615]}"#,
        ] {
            let reference: BTreeMap<String, Tail> = serde_json::from_str(json).unwrap();
            let compact: SnapshotTails = serde_json::from_str(json).unwrap();
            let expected = serde_json::to_string(&reference).unwrap();
            assert_eq!(serde_json::to_string(&compact).unwrap(), expected);
            assert_eq!(SnapshotTails::from(reference.clone()), compact);
            assert_eq!(compact.is_empty(), reference.is_empty());
            for member in ["", "a", "z", "\0", "missing"] {
                assert_eq!(compact.get(member), reference.get(member));
            }
            assert_eq!(
                compact.into_iter().collect::<Vec<_>>(),
                reference.iter().collect::<Vec<_>>()
            );
            let mut reversed: Vec<_> = reference.into_iter().collect();
            reversed.reverse();
            assert_eq!(reversed.into_iter().collect::<SnapshotTails>(), compact);
        }
        for invalid in [
            "null",
            "[]",
            r#"{"a":null}"#,
            r#"{"a":["id",-1]}"#,
            r#"{"a":["id",18446744073709551616]}"#,
            r#"{"a":["id",1,2]}"#,
            r#"{"a":[7,1]}"#,
            r#"{"a":["id"]}"#,
        ] {
            assert!(
                serde_json::from_str::<SnapshotTails>(invalid).is_err(),
                "{invalid}"
            );
            assert!(
                serde_json::from_str::<BTreeMap<String, Tail>>(invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn snapshot_tail_storage_preserves_historical_churn_rows_and_empty_omission() {
        #[derive(Serialize, Deserialize)]
        struct HistoricalChurn {
            recipient: String,
            phase: crate::records::ChurnPhase,
            at_us: u64,
            epoch: Option<u32>,
            #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
            tails: BTreeMap<String, Tail>,
        }
        for tails in [
            "",
            r#","tails":{}"#,
            r#","tails":{"z":["old",1],"a":["unknown",18446744073709551615],"z":["last",7]}"#,
        ] {
            let json = format!(
                r#"{{"recipient":"foreign","phase":"Rejoined","at_us":17,"epoch":2{tails}}}"#
            );
            let reference: HistoricalChurn = serde_json::from_str(&json).unwrap();
            let compact: crate::records::ChurnEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(
                serde_json::to_string(&compact).unwrap(),
                serde_json::to_string(&reference).unwrap()
            );
        }
    }

    #[test]
    #[ignore = "isolated retained snapshot-tail memory profile; run in a fresh process"]
    fn generator_snapshot_tail_storage_profile() {
        let snapshots = std::env::var("CAPACITY_SNAPSHOTS")
            .unwrap_or_else(|_| "15984".into())
            .parse::<usize>()
            .unwrap();
        let members = std::env::var("CAPACITY_SNAPSHOT_MEMBERS")
            .unwrap_or_else(|_| "16".into())
            .parse::<usize>()
            .unwrap();
        assert!((1..=1_000_000).contains(&snapshots));
        assert!((1..=64).contains(&members));
        fn measure<T: FromIterator<Entry> + Serialize>(snapshots: usize, members: usize) {
            let started = std::time::Instant::now();
            let tails: Vec<T> = (0..snapshots)
                .map(|snapshot| {
                    (0..members)
                        .map(|member| {
                            (
                                format!("r{}p{member}", snapshot / members),
                                (
                                    format!("identity-{}-{member}", snapshot / members),
                                    u64::MAX,
                                ),
                            )
                        })
                        .collect()
                })
                .collect();
            let build_us = started.elapsed().as_micros();
            let retained_rss = crate::diagnostics::resident_memory_bytes(std::process::id());
            let output = tempfile::tempdir().unwrap();
            let path = output.path().join("tails.json");
            crate::artifacts::write_json(&path, &tails).unwrap();
            use sha2::{Digest, Sha256};
            use std::io::Read;
            let mut digest = Sha256::new();
            let mut file = std::fs::File::open(&path).unwrap();
            let mut buffer = [0_u8; 8192];
            loop {
                let read = file.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                digest.update(&buffer[..read]);
            }
            println!(
                "SNAPSHOT_TAIL_STORAGE {}",
                serde_json::json!({
                    "snapshots": snapshots, "members": members, "rows": snapshots * members,
                    "retained_rss_bytes": retained_rss, "build_us": build_us,
                    "json_bytes": std::fs::metadata(path).unwrap().len(),
                    "sha256": digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
                })
            );
            std::hint::black_box(tails);
        }
        if std::env::var_os("CAPACITY_SNAPSHOT_COMPACT").is_some() {
            measure::<SnapshotTails>(snapshots, members);
        } else {
            measure::<BTreeMap<String, Tail>>(snapshots, members);
        }
    }
}
