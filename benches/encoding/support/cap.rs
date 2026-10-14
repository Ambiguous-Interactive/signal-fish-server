use super::model::{Delta, Entity, Input, Payload, Shape, Snapshot, Tile, Update};
use crate::game_capnp as wire;

fn set_floats(mut builder: capnp::primitive_list::Builder<'_, f64>, values: &[f64]) {
    for (index, value) in values.iter().enumerate() {
        builder.set(u32::try_from(index).unwrap(), *value);
    }
}
fn set_entity(mut builder: wire::entity::Builder<'_>, value: &Entity) {
    builder.set_id(value.id);
    set_floats(
        builder
            .reborrow()
            .init_position(u32::try_from(value.position.len()).unwrap()),
        &value.position,
    );
    set_floats(
        builder
            .reborrow()
            .init_velocity(u32::try_from(value.velocity.len()).unwrap()),
        &value.velocity,
    );
    builder.set_health(value.health);
    builder.set_ammo(value.ammo);
    builder.set_active(value.active);
}
fn read_entity(value: wire::entity::Reader<'_>) -> Entity {
    Entity {
        id: value.get_id(),
        position: value.get_position().unwrap().iter().collect(),
        velocity: value.get_velocity().unwrap().iter().collect(),
        health: value.get_health(),
        ammo: value.get_ammo(),
        active: value.get_active(),
    }
}
pub fn encode(value: &Payload, packed: bool) -> Vec<u8> {
    let mut message = capnp::message::Builder::new_default();
    match value {
        Payload::Input(value) => {
            let mut root = message.init_root::<wire::input::Builder>();
            root.set_tick(value.tick);
            root.set_buttons(value.buttons);
            root.set_horizontal(value.horizontal);
            root.set_vertical(value.vertical);
        }
        Payload::Snapshot(value) => {
            let mut root = message.init_root::<wire::snapshot::Builder>();
            root.set_tick(value.tick);
            root.set_map(value.map.as_str());
            let mut list = root.init_entities(u32::try_from(value.entities.len()).unwrap());
            for (index, value) in value.entities.iter().enumerate() {
                set_entity(list.reborrow().get(u32::try_from(index).unwrap()), value);
            }
        }
        Payload::Delta(value) => {
            let mut root = message.init_root::<wire::delta::Builder>();
            root.set_tick(value.tick);
            root.set_base_tick(value.base_tick);
            let mut list = root
                .reborrow()
                .init_spawned(u32::try_from(value.spawned.len()).unwrap());
            for (index, value) in value.spawned.iter().enumerate() {
                set_entity(list.reborrow().get(u32::try_from(index).unwrap()), value);
            }
            let mut list = root
                .reborrow()
                .init_updated(u32::try_from(value.updated.len()).unwrap());
            for (index, value) in value.updated.iter().enumerate() {
                let mut builder = list.reborrow().get(u32::try_from(index).unwrap());
                builder.set_id(value.id);
                builder.set_health(value.health);
                builder.set_animation(value.animation);
                set_floats(
                    builder.init_position(u32::try_from(value.position.len()).unwrap()),
                    &value.position,
                );
            }
            let mut list = root
                .reborrow()
                .init_removed(u32::try_from(value.removed.len()).unwrap());
            for (index, value) in value.removed.iter().enumerate() {
                list.set(u32::try_from(index).unwrap(), *value);
            }
            let mut list = root
                .reborrow()
                .init_tiles(u32::try_from(value.tiles.len()).unwrap());
            for (index, value) in value.tiles.iter().enumerate() {
                let mut builder = list.reborrow().get(u32::try_from(index).unwrap());
                builder.set_x(value.x);
                builder.set_y(value.y);
                builder.set_tile_type(value.tile_type);
            }
        }
    }
    if packed {
        let mut bytes = Vec::new();
        capnp::serialize_packed::write_message(&mut bytes, &message).unwrap();
        bytes
    } else {
        capnp::serialize::write_message_to_words(&message)
    }
}

pub fn decode(shape: Shape, bytes: &[u8], packed: bool) -> Payload {
    let mut slice = bytes;
    if packed {
        let message =
            capnp::serialize_packed::read_message(&mut slice, capnp::message::ReaderOptions::new())
                .unwrap();
        assert!(slice.is_empty());
        read_payload(shape, &message)
    } else {
        let message = capnp::serialize::read_message_from_flat_slice(
            &mut slice,
            capnp::message::ReaderOptions::new(),
        )
        .unwrap();
        assert!(slice.is_empty());
        read_payload(shape, &message)
    }
}

fn read_payload<S: capnp::message::ReaderSegments>(
    shape: Shape,
    message: &capnp::message::Reader<S>,
) -> Payload {
    match shape {
        Shape::Input => {
            let value = message.get_root::<wire::input::Reader>().unwrap();
            Payload::Input(Input {
                tick: value.get_tick(),
                buttons: value.get_buttons(),
                horizontal: value.get_horizontal(),
                vertical: value.get_vertical(),
            })
        }
        Shape::Snapshot => {
            let value = message.get_root::<wire::snapshot::Reader>().unwrap();
            Payload::Snapshot(Snapshot {
                tick: value.get_tick(),
                entities: value
                    .get_entities()
                    .unwrap()
                    .iter()
                    .map(read_entity)
                    .collect(),
                map: value.get_map().unwrap().to_str().unwrap().to_owned(),
            })
        }
        Shape::Delta => {
            let value = message.get_root::<wire::delta::Reader>().unwrap();
            Payload::Delta(Delta {
                tick: value.get_tick(),
                base_tick: value.get_base_tick(),
                spawned: value
                    .get_spawned()
                    .unwrap()
                    .iter()
                    .map(read_entity)
                    .collect(),
                updated: value
                    .get_updated()
                    .unwrap()
                    .iter()
                    .map(|value| Update {
                        id: value.get_id(),
                        position: value.get_position().unwrap().iter().collect(),
                        health: value.get_health(),
                        animation: value.get_animation(),
                    })
                    .collect(),
                removed: value.get_removed().unwrap().iter().collect(),
                tiles: value
                    .get_tiles()
                    .unwrap()
                    .iter()
                    .map(|value| Tile {
                        x: value.get_x(),
                        y: value.get_y(),
                        tile_type: value.get_tile_type(),
                    })
                    .collect(),
            })
        }
    }
}
