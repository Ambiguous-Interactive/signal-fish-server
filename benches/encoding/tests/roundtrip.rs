#[allow(warnings, clippy::all)]
#[path = "../generated/game_capnp.rs"]
pub mod game_capnp;
#[path = "../support/mod.rs"]
mod support;
use support::model::{Delta, Entity, Input, Payload, Snapshot, Tile, Update};
use support::{fixtures, validate, Codec};

#[test]
fn every_codec_preserves_all_frozen_values_and_float_bits() {
    for fixture in fixtures() {
        for codec in Codec::ALL {
            validate(codec, &fixture);
        }
    }
}

#[test]
fn every_codec_preserves_empty_lists_unicode_and_numeric_boundaries() {
    let values = [
        Payload::Input(Input {
            tick: 0,
            buttons: u32::MAX,
            horizontal: i32::MIN,
            vertical: i32::MAX,
        }),
        Payload::Snapshot(Snapshot {
            tick: u32::MAX,
            entities: Vec::new(),
            map: "雪🌊".to_owned(),
        }),
        Payload::Snapshot(Snapshot {
            tick: 0,
            map: String::new(),
            entities: vec![Entity {
                id: 0,
                position: vec![-0.0, f64::MIN_POSITIVE, f64::MAX],
                velocity: vec![0.0, -1.0, 1.0],
                health: 0,
                ammo: u32::MAX,
                active: false,
            }],
        }),
        Payload::Delta(Delta {
            tick: 0,
            base_tick: u32::MAX,
            spawned: vec![],
            updated: vec![],
            removed: vec![],
            tiles: vec![],
        }),
        Payload::Delta(Delta {
            tick: u32::MAX,
            base_tick: 0,
            spawned: vec![],
            updated: vec![Update {
                id: u32::MAX,
                position: vec![-0.0, 0.0, -0.25],
                health: 0,
                animation: u32::MAX,
            }],
            removed: vec![u32::MAX],
            tiles: vec![Tile {
                x: 0,
                y: u32::MAX,
                tile_type: u32::MAX,
            }],
        }),
    ];
    for value in values {
        for codec in Codec::ALL {
            let bytes = codec.encode(&value);
            let decoded = codec.decode(value.shape(), &bytes);
            assert_eq!(decoded, value, "{} values", codec.name());
            assert_eq!(
                decoded.floating_bits(),
                value.floating_bits(),
                "{} float bits",
                codec.name()
            );
        }
    }
}

#[test]
fn roundtrip_oracle_detects_signed_zero_loss() {
    let mut fixture = fixtures().remove(1);
    let Payload::Snapshot(snapshot) = &mut fixture.value else {
        panic!("expected snapshot")
    };
    let original_bits = snapshot.entities[0].position[2].to_bits();
    assert_eq!(original_bits, (-0.0f64).to_bits());
    let mut damaged = fixture.value.clone();
    let Payload::Snapshot(snapshot) = &mut damaged else {
        unreachable!()
    };
    snapshot.entities[0].position[2] = 0.0;
    assert_eq!(
        damaged, fixture.value,
        "ordinary equality cannot detect this loss"
    );
    assert_ne!(damaged.floating_bits(), fixture.value.floating_bits());
}

#[test]
fn schema_varint_rejects_truncation_and_trailing_bytes() {
    for fixture in fixtures() {
        let bytes = validate(Codec::Varint, &fixture);
        for end in 0..bytes.len() {
            assert!(
                std::panic::catch_unwind(
                    || Codec::Varint.decode(fixture.value.shape(), &bytes[..end])
                )
                .is_err(),
                "{} truncation at {end}",
                fixture.name
            );
        }
        let mut extra = bytes;
        extra.push(0);
        assert!(
            std::panic::catch_unwind(|| Codec::Varint.decode(fixture.value.shape(), &extra))
                .is_err()
        );
    }
}

#[test]
fn schema_varint_rejects_integer_overflow() {
    for bytes in [vec![0xff; 10], vec![0x80, 0x80, 0x80, 0x80, 0x10, 0, 0, 0]] {
        assert!(std::panic::catch_unwind(
            || Codec::Varint.decode(support::model::Shape::Input, &bytes)
        )
        .is_err());
    }
}
