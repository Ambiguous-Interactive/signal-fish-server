//! Exact owned rejoin snapshot tails stored without per-snapshot tree nodes.

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;

type Tail = (String, u64);
type Entry = (String, Tail);
type SharedTail = (crate::records::ReceiptName, u64);
type SharedEntry = (crate::records::ReceiptName, SharedTail);

/// Sorted snapshot members with shared player identities and full sequence tails.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotTails(Box<[SharedEntry]>);

impl SnapshotTails {
    /// Share exact member names and player identities across retained snapshots.
    pub(super) fn share_names(&mut self, names: &mut crate::records::ReceiptNames) {
        for (member, (player_id, _)) in &mut self.0 {
            *member = names.intern(member.as_str());
            *player_id = names.intern(player_id.as_str());
        }
    }

    /// Return whether the snapshot contains no members.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Find a member or its sorted insertion position without allocating.
    fn position(&self, member: &str) -> Result<usize, usize> {
        self.0
            .binary_search_by(|(name, _)| name.as_str().cmp(member))
    }

    /// Find the exact recorded player identity and tail for one member.
    pub fn get(&self, member: &str) -> Option<&SharedTail> {
        self.position(member)
            .ok()
            .map(|position| &self.0[position].1)
    }

    #[cfg(test)]
    /// Mutate one fixture tail without changing member ordering.
    pub fn get_mut(&mut self, member: &str) -> Option<&mut SharedTail> {
        self.position(member)
            .ok()
            .map(|position| &mut self.0[position].1)
    }

    #[cfg(test)]
    /// Borrow the first fixture member in artifact key order.
    pub fn first_key_value(&self) -> Option<(&String, &SharedTail)> {
        self.0
            .first()
            .map(|(member, tail)| (member.owned_string(), tail))
    }

    #[cfg(test)]
    /// Insert or replace a fixture tail while retaining sorted unique members.
    pub fn insert(&mut self, member: String, tail: Tail) {
        let tail = (tail.0.into(), tail.1);
        match self.position(&member) {
            Ok(position) => self.0[position].1 = tail,
            Err(position) => {
                let mut entries = std::mem::take(&mut self.0).into_vec();
                entries.insert(position, (member.into(), tail));
                self.0 = entries.into_boxed_slice();
            }
        }
    }
}

impl From<BTreeMap<String, Tail>> for SnapshotTails {
    /// Consume a sorted snapshot map without copying its owned strings.
    fn from(tails: BTreeMap<String, Tail>) -> Self {
        tails.into_iter().collect()
    }
}

impl FromIterator<Entry> for SnapshotTails {
    /// Sort owned entries and retain the final tail for each repeated member.
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
        Self(
            entries
                .into_iter()
                .map(|(name, (player_id, sequence))| (name.into(), (player_id.into(), sequence)))
                .collect(),
        )
    }
}

impl<'a> IntoIterator for &'a SnapshotTails {
    type Item = (&'a String, &'a SharedTail);
    type IntoIter =
        std::iter::Map<std::slice::Iter<'a, SharedEntry>, fn(&'a SharedEntry) -> Self::Item>;

    /// Borrow snapshot entries in the same member order as artifact serialization.
    fn into_iter(self) -> Self::IntoIter {
        self.0
            .iter()
            .map(|(member, tail)| (member.owned_string(), tail))
    }
}

#[cfg(test)]
impl std::ops::Index<&str> for SnapshotTails {
    type Output = SharedTail;

    /// Select a fixture member and fail when the fixture lacks its required tail.
    fn index(&self, member: &str) -> &Self::Output {
        self.get(member).expect("snapshot member")
    }
}

impl Serialize for SnapshotTails {
    /// Write the historical JSON object with sorted member names.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self)
    }
}

impl<'de> Deserialize<'de> for SnapshotTails {
    /// Read historical snapshot objects while retaining exact identities and duplicate semantics.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TailsVisitor;
        impl<'de> Visitor<'de> for TailsVisitor {
            type Value = SnapshotTails;

            /// Describe the member-to-identity-and-tail object required by the decoder.
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of member names and player identity/sequence tails")
            }

            /// Collect decoded entries and preserve the final value for repeated member keys.
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
    /// Share live and replayed identities while keeping each sequence tail independent.
    fn retained_snapshot_player_identities_share_storage_and_keep_distinct_tails() {
        use crate::records::{ChurnEvent, ChurnPhase, EventLog};
        let log = EventLog::new();
        for sequence in [0, 7, u64::MAX] {
            log.push_churn(ChurnEvent {
                recipient: "receiver".into(),
                phase: ChurnPhase::Rejoined,
                at_us: sequence,
                epoch: Some(1),
                tails: [("member".into(), ("identity-未知\0".into(), sequence))]
                    .into_iter()
                    .collect(),
            });
        }
        let records = log.take_records();
        let output = tempfile::tempdir().unwrap();
        crate::artifacts::write_deliveries(output.path(), &records).unwrap();
        let replay = crate::artifacts::read_records(output.path()).unwrap();
        assert_eq!(
            serde_json::to_value(&records).unwrap(),
            serde_json::to_value(&replay).unwrap()
        );
        for events in [&records.churn, &replay.churn] {
            let first = events[0].tails.get("member").unwrap().0.as_str().as_ptr();
            for (event, sequence) in events.iter().zip([0, 7, u64::MAX]) {
                let tail = event.tails.get("member").unwrap();
                assert_eq!(tail.0.as_str(), "identity-未知\0");
                assert_eq!(tail.1, sequence);
                assert_eq!(tail.0.as_str().as_ptr(), first);
            }
        }
    }

    #[test]
    /// Compare compact storage with independent map decoding for exact values and errors.
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
                assert_eq!(
                    compact
                        .get(member)
                        .map(|(player_id, sequence)| (player_id.as_str(), sequence)),
                    reference
                        .get(member)
                        .map(|(player_id, sequence)| (player_id.as_str(), sequence))
                );
            }
            assert_eq!(
                compact
                    .into_iter()
                    .map(|(name, (id, sequence))| (name, (id.as_str(), sequence)))
                    .collect::<Vec<_>>(),
                reference
                    .iter()
                    .map(|(name, (id, sequence))| (name, (id.as_str(), sequence)))
                    .collect::<Vec<_>>()
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
    /// Compare full historical churn rows, including omitted and explicitly empty snapshots.
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

    /// Preserve shared keys and independent mutable tails through transfer and replay.
    #[test]
    fn retained_and_replayed_snapshots_share_exact_member_names_and_keep_owned_tails() {
        use crate::records::{ChurnEvent, ChurnPhase, EventLog};
        let log = EventLog::new();
        for index in 0..3 {
            log.push_churn(ChurnEvent {
                recipient: "receiver".into(),
                phase: ChurnPhase::Rejoined,
                at_us: index,
                epoch: Some(1),
                tails: [("未知\0".to_owned(), (format!("identity-{index}"), index))]
                    .into_iter()
                    .collect(),
            });
        }
        let records = log.take_records();
        let first = records.churn[0].tails.first_key_value().unwrap().0;
        for (index, event) in records.churn.iter().enumerate() {
            let (member, tail) = event.tails.first_key_value().unwrap();
            assert!(std::ptr::eq(first, member));
            assert_eq!(tail.0.as_str(), format!("identity-{index}"));
            assert_eq!(tail.1, index as u64);
        }
        let mut edited = records.churn[0].tails.clone();
        edited.insert("another".into(), ("new".into(), 99));
        edited.get_mut("未知\0").unwrap().0 = "changed".into();
        assert_eq!(
            records.churn[0].tails.get("未知\0").unwrap().0,
            "identity-0"
        );
        let output = tempfile::tempdir().unwrap();
        crate::artifacts::write_deliveries(output.path(), &records).unwrap();
        let replay = crate::artifacts::read_records(output.path()).unwrap();
        assert_eq!(
            serde_json::to_value(&replay).unwrap(),
            serde_json::to_value(&records).unwrap()
        );
        let first = replay.churn[0].tails.first_key_value().unwrap().0;
        for event in &replay.churn {
            assert!(std::ptr::eq(
                first,
                event.tails.first_key_value().unwrap().0
            ));
        }
    }

    #[test]
    #[ignore = "isolated retained snapshot-tail memory profile; run in a fresh process"]
    /// Measure retained snapshot storage and artifact hashes in isolated comparison processes.
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
        /// Retain the selected snapshot shape and report its storage sample and exact artifact hash.
        fn measure<T: FromIterator<Entry> + Serialize>(
            snapshots: usize,
            members: usize,
            share: fn(&mut T, &mut crate::records::ReceiptNames),
        ) {
            let started = std::time::Instant::now();
            let mut names = crate::records::ReceiptNames::default();
            let tails: Vec<T> = (0..snapshots)
                .map(|snapshot| {
                    let mut tails = (0..members)
                        .map(|member| {
                            (
                                format!("r{}p{member}", snapshot / members),
                                (
                                    format!("identity-{}-{member}", snapshot / members),
                                    u64::MAX,
                                ),
                            )
                        })
                        .collect();
                    share(&mut tails, &mut names);
                    tails
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
            measure::<SnapshotTails>(snapshots, members, SnapshotTails::share_names);
        } else {
            measure::<BTreeMap<String, Tail>>(snapshots, members, |_, _| {});
        }
    }
}
