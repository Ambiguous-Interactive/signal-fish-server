mod cap;
mod flat;
pub mod model;
mod varint;

use model::{Delta, Input, Payload, Shape, Snapshot};
use prost::Message;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug)]
pub enum Codec {
    MessagePackNamed,
    MessagePackCompact,
    Prost,
    Ciborium,
    Cbor4ii,
    FlatBuffers,
    Capnp,
    CapnpPacked,
    Varint,
}

impl Codec {
    pub const ALL: [Self; 9] = [
        Self::MessagePackNamed,
        Self::MessagePackCompact,
        Self::Prost,
        Self::Ciborium,
        Self::Cbor4ii,
        Self::FlatBuffers,
        Self::Capnp,
        Self::CapnpPacked,
        Self::Varint,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::MessagePackNamed => "messagepack-named",
            Self::MessagePackCompact => "messagepack-compact",
            Self::Prost => "prost",
            Self::Ciborium => "ciborium",
            Self::Cbor4ii => "cbor4ii",
            Self::FlatBuffers => "flatbuffers",
            Self::Capnp => "capnp-unpacked",
            Self::CapnpPacked => "capnp-packed",
            Self::Varint => "schema-varint",
        }
    }
    fn serde_encode<T: Serialize>(self, value: &T) -> Vec<u8> {
        match self {
            Self::MessagePackNamed => rmp_serde::to_vec_named(value).unwrap(),
            Self::MessagePackCompact => rmp_serde::to_vec(value).unwrap(),
            Self::Ciborium => {
                let mut bytes = Vec::new();
                ciborium::into_writer(value, &mut bytes).unwrap();
                bytes
            }
            Self::Cbor4ii => cbor4ii::serde::to_vec(Vec::new(), value).unwrap(),
            _ => unreachable!(),
        }
    }
    fn serde_decode<T: for<'de> Deserialize<'de>>(self, bytes: &[u8]) -> T {
        match self {
            Self::MessagePackNamed | Self::MessagePackCompact => {
                rmp_serde::from_slice(bytes).unwrap()
            }
            Self::Ciborium => ciborium::from_reader(bytes).unwrap(),
            Self::Cbor4ii => cbor4ii::serde::from_slice(bytes).unwrap(),
            _ => unreachable!(),
        }
    }
    pub fn encode(self, value: &Payload) -> Vec<u8> {
        match self {
            Self::FlatBuffers => flat::encode(value),
            Self::Capnp => cap::encode(value, false),
            Self::CapnpPacked => cap::encode(value, true),
            Self::Varint => varint::encode(value),
            Self::Prost => match value {
                Payload::Input(value) => value.encode_to_vec(),
                Payload::Snapshot(value) => value.encode_to_vec(),
                Payload::Delta(value) => value.encode_to_vec(),
            },
            _ => match value {
                Payload::Input(value) => self.serde_encode(&(
                    value.tick,
                    value.buttons,
                    value.horizontal,
                    value.vertical,
                )),
                Payload::Snapshot(value) => self.serde_encode(value),
                Payload::Delta(value) => self.serde_encode(value),
            },
        }
    }
    pub fn decode(self, shape: Shape, bytes: &[u8]) -> Payload {
        match self {
            Self::FlatBuffers => flat::decode(shape, bytes),
            Self::Capnp => cap::decode(shape, bytes, false),
            Self::CapnpPacked => cap::decode(shape, bytes, true),
            Self::Varint => varint::decode(shape, bytes),
            Self::Prost => match shape {
                Shape::Input => Payload::Input(Input::decode(bytes).unwrap()),
                Shape::Snapshot => Payload::Snapshot(Snapshot::decode(bytes).unwrap()),
                Shape::Delta => Payload::Delta(Delta::decode(bytes).unwrap()),
            },
            _ => match shape {
                Shape::Input => {
                    let (tick, buttons, horizontal, vertical) = self.serde_decode(bytes);
                    Payload::Input(Input {
                        tick,
                        buttons,
                        horizontal,
                        vertical,
                    })
                }
                Shape::Snapshot => Payload::Snapshot(self.serde_decode(bytes)),
                Shape::Delta => Payload::Delta(self.serde_decode(bytes)),
            },
        }
    }
}

pub struct Fixture {
    pub name: String,
    pub sha256: String,
    pub source_bytes: usize,
    pub value: Payload,
}

pub fn fixtures() -> Vec<Fixture> {
    #[derive(Deserialize)]
    struct Manifest {
        version: u32,
        fixtures: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        name: String,
        file: String,
        bytes: usize,
        sha256: String,
    }
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-fixtures/encoding");
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.version, 1);
    assert_eq!(manifest.fixtures.len(), 4);
    manifest
        .fixtures
        .into_iter()
        .map(|entry| {
            let bytes = std::fs::read(root.join(&entry.file)).unwrap();
            assert_eq!(bytes.len(), entry.bytes);
            let digest: String = Sha256::digest(&bytes)
                .iter()
                .map(|v| format!("{v:02x}"))
                .collect();
            assert_eq!(digest, entry.sha256);
            let value = Payload::from_json(&entry.name, &bytes);
            assert_logical_values(
                &value.logical_json(),
                &serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            );
            Fixture {
                name: entry.name,
                sha256: digest,
                source_bytes: bytes.len(),
                value,
            }
        })
        .collect()
}

pub fn validate(codec: Codec, fixture: &Fixture) -> Vec<u8> {
    let bytes = codec.encode(&fixture.value);
    let decoded = codec.decode(fixture.value.shape(), &bytes);
    assert_eq!(
        decoded,
        fixture.value,
        "{}/{} values (source {} bytes, sha256 {})",
        fixture.name,
        codec.name(),
        fixture.source_bytes,
        fixture.sha256
    );
    assert_eq!(
        decoded.floating_bits(),
        fixture.value.floating_bits(),
        "{}/{} float bits",
        fixture.name,
        codec.name()
    );
    assert_eq!(
        codec.encode(&decoded),
        bytes,
        "{}/{} deterministic wire",
        fixture.name,
        codec.name()
    );
    bytes
}

fn assert_logical_values(actual: &serde_json::Value, expected: &serde_json::Value) {
    use serde_json::Value;
    match (actual, expected) {
        (Value::Number(actual), Value::Number(expected)) => assert_eq!(
            actual.as_f64().unwrap().to_bits(),
            expected.as_f64().unwrap().to_bits()
        ),
        (Value::Array(actual), Value::Array(expected)) => {
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.iter().zip(expected) {
                assert_logical_values(actual, expected);
            }
        }
        (Value::Object(actual), Value::Object(expected)) => {
            assert_eq!(actual.len(), expected.len());
            for (key, expected) in expected {
                assert_logical_values(&actual[key], expected);
            }
        }
        _ => assert_eq!(actual, expected),
    }
}
