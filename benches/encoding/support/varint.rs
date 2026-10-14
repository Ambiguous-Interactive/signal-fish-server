use super::model::{Delta, Entity, Input, Payload, Shape, Snapshot, Tile, Update};

fn unsigned(bytes: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        bytes.push(u8::try_from(value & 127).unwrap() | 128);
        value >>= 7;
    }
    bytes.push(u8::try_from(value).unwrap());
}
fn floats(bytes: &mut Vec<u8>, values: &[f64]) {
    unsigned(bytes, u64::try_from(values.len()).unwrap());
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}
fn entities(bytes: &mut Vec<u8>, values: &[Entity]) {
    unsigned(bytes, u64::try_from(values.len()).unwrap());
    for value in values {
        unsigned(bytes, u64::from(value.id));
        floats(bytes, &value.position);
        floats(bytes, &value.velocity);
        unsigned(bytes, u64::from(value.health));
        unsigned(bytes, u64::from(value.ammo));
        bytes.push(u8::from(value.active));
    }
}

pub fn encode(value: &Payload) -> Vec<u8> {
    let mut bytes = Vec::new();
    match value {
        Payload::Input(value) => {
            unsigned(&mut bytes, u64::from(value.tick));
            unsigned(&mut bytes, u64::from(value.buttons));
            for signed in [value.horizontal, value.vertical] {
                let signed = i64::from(signed);
                unsigned(
                    &mut bytes,
                    u64::try_from((signed << 1) ^ (signed >> 63)).unwrap(),
                );
            }
        }
        Payload::Snapshot(value) => {
            unsigned(&mut bytes, u64::from(value.tick));
            entities(&mut bytes, &value.entities);
            unsigned(&mut bytes, u64::try_from(value.map.len()).unwrap());
            bytes.extend_from_slice(value.map.as_bytes());
        }
        Payload::Delta(value) => {
            unsigned(&mut bytes, u64::from(value.tick));
            unsigned(&mut bytes, u64::from(value.base_tick));
            entities(&mut bytes, &value.spawned);
            unsigned(&mut bytes, u64::try_from(value.updated.len()).unwrap());
            for value in &value.updated {
                unsigned(&mut bytes, u64::from(value.id));
                floats(&mut bytes, &value.position);
                unsigned(&mut bytes, u64::from(value.health));
                unsigned(&mut bytes, u64::from(value.animation));
            }
            unsigned(&mut bytes, u64::try_from(value.removed.len()).unwrap());
            for id in &value.removed {
                unsigned(&mut bytes, u64::from(*id));
            }
            unsigned(&mut bytes, u64::try_from(value.tiles.len()).unwrap());
            for tile in &value.tiles {
                for field in [tile.x, tile.y, tile.tile_type] {
                    unsigned(&mut bytes, u64::from(field));
                }
            }
        }
    }
    bytes
}

struct Reader<'a> {
    bytes: &'a [u8],
}
impl Reader<'_> {
    fn take(&mut self, count: usize) -> &[u8] {
        let (first, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        first
    }
    fn unsigned(&mut self) -> u64 {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = self.take(1)[0];
            if shift == 63 {
                assert!(byte <= 1, "varint overflow");
            }
            value |= u64::from(byte & 127) << shift;
            if byte < 128 {
                return value;
            }
        }
        panic!("unterminated varint")
    }
    fn u32(&mut self) -> u32 {
        u32::try_from(self.unsigned()).unwrap()
    }
    fn len(&mut self) -> usize {
        usize::try_from(self.unsigned()).unwrap()
    }
    fn signed(&mut self) -> i32 {
        let value = self.unsigned();
        i32::try_from(i64::try_from(value >> 1).unwrap() ^ -i64::try_from(value & 1).unwrap())
            .unwrap()
    }
    fn floats(&mut self) -> Vec<f64> {
        let count = self.len();
        assert!(count <= self.bytes.len() / 8, "truncated float list");
        (0..count)
            .map(|_| f64::from_le_bytes(self.take(8).try_into().unwrap()))
            .collect()
    }
    fn entities(&mut self) -> Vec<Entity> {
        let count = self.len();
        assert!(count <= self.bytes.len(), "truncated entity list");
        (0..count)
            .map(|_| {
                let id = self.u32();
                let position = self.floats();
                let velocity = self.floats();
                let health = self.u32();
                let ammo = self.u32();
                let active = self.take(1)[0];
                assert!(active <= 1, "invalid bool");
                Entity {
                    id,
                    position,
                    velocity,
                    health,
                    ammo,
                    active: active == 1,
                }
            })
            .collect()
    }
}

pub fn decode(shape: Shape, bytes: &[u8]) -> Payload {
    let mut reader = Reader { bytes };
    let tick = reader.u32();
    let value = match shape {
        Shape::Input => Payload::Input(Input {
            tick,
            buttons: reader.u32(),
            horizontal: reader.signed(),
            vertical: reader.signed(),
        }),
        Shape::Snapshot => {
            let entities = reader.entities();
            let length = reader.len();
            let map = std::str::from_utf8(reader.take(length)).unwrap().to_owned();
            Payload::Snapshot(Snapshot {
                tick,
                entities,
                map,
            })
        }
        Shape::Delta => {
            let base_tick = reader.u32();
            let spawned = reader.entities();
            let count = reader.len();
            assert!(count <= reader.bytes.len());
            let updated = (0..count)
                .map(|_| Update {
                    id: reader.u32(),
                    position: reader.floats(),
                    health: reader.u32(),
                    animation: reader.u32(),
                })
                .collect();
            let count = reader.len();
            assert!(count <= reader.bytes.len());
            let removed = (0..count).map(|_| reader.u32()).collect();
            let count = reader.len();
            assert!(count <= reader.bytes.len());
            let tiles = (0..count)
                .map(|_| Tile {
                    x: reader.u32(),
                    y: reader.u32(),
                    tile_type: reader.u32(),
                })
                .collect();
            Payload::Delta(Delta {
                tick,
                base_tick,
                spawned,
                updated,
                removed,
                tiles,
            })
        }
    };
    assert!(reader.bytes.is_empty(), "trailing varint payload bytes");
    value
}
