//! Freeze the cross-language measurement inputs before codec evaluation.

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Deserialize)]
struct Manifest {
    version: u32,
    fixtures: Vec<Fixture>,
}

#[derive(Deserialize)]
struct Fixture {
    name: String,
    file: String,
    bytes: usize,
    sha256: String,
}

#[test]
fn frozen_game_payload_corpus_has_exact_bytes_and_shapes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test-fixtures/encoding");
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest.version, 1,
        "review codec results when the corpus changes"
    );
    assert_eq!(manifest.fixtures.len(), 4);
    let mut names = BTreeSet::new();
    for fixture in manifest.fixtures {
        assert!(names.insert(fixture.name.clone()), "duplicate corpus input");
        assert_eq!(fixture.file, format!("{}.json", fixture.name));
        let bytes = std::fs::read(root.join(&fixture.file)).unwrap();
        assert_eq!(bytes.len(), fixture.bytes, "{} source size", fixture.name);
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(digest, fixture.sha256, "{} source digest", fixture.name);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        match fixture.name.as_str() {
            "input-state" => {
                assert!((16..=32).contains(&bytes.len()));
                assert_eq!(value, serde_json::json!([120, 5, -127, 127]));
            }
            "snapshot-small" | "snapshot-large" => {
                assert!((256..=1024).contains(&bytes.len()));
                let entities = value["entities"].as_array().unwrap();
                let count = if fixture.name == "snapshot-small" {
                    2
                } else {
                    8
                };
                assert_eq!(entities.len(), count);
                assert!(entities.iter().all(|entity| {
                    entity["position"].as_array().unwrap().len() == 3
                        && entity["velocity"].as_array().unwrap().len() == 3
                        && entity["active"].is_boolean()
                }));
            }
            "world-delta" => {
                assert_eq!(bytes.len(), 4096);
                for (field, count) in [
                    ("spawned", 3),
                    ("updated", 54),
                    ("removed", 3),
                    ("tiles", 24),
                ] {
                    assert_eq!(value[field].as_array().unwrap().len(), count, "{field}");
                }
                assert!(value["base_tick"].as_u64().unwrap() < value["tick"].as_u64().unwrap());
            }
            name => panic!("unreviewed corpus shape: {name}"),
        }
    }
}
