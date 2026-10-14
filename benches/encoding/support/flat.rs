use super::model::{Delta, Entity, Input, Payload, Shape, Snapshot, Tile, Update};
/* SAFETY: flatc 25.12.19 owns the generated accessors; decode uses verified roots. */
#[allow(unsafe_code, warnings, clippy::all)]
#[path = "../generated/game_generated.rs"]
mod generated;
use generated as wire;

fn entity<'a>(
    builder: &mut flatbuffers::FlatBufferBuilder<'a>,
    value: &Entity,
) -> flatbuffers::WIPOffset<wire::Entity<'a>> {
    let position = builder.create_vector(&value.position);
    let velocity = builder.create_vector(&value.velocity);
    wire::Entity::create(
        builder,
        &wire::EntityArgs {
            id: value.id,
            position: Some(position),
            velocity: Some(velocity),
            health: value.health,
            ammo: value.ammo,
            active: value.active,
        },
    )
}
fn read_entity(value: wire::Entity<'_>) -> Entity {
    Entity {
        id: value.id(),
        position: value.position().iter().collect(),
        velocity: value.velocity().iter().collect(),
        health: value.health(),
        ammo: value.ammo(),
        active: value.active(),
    }
}

pub fn encode(value: &Payload) -> Vec<u8> {
    let mut builder = flatbuffers::FlatBufferBuilder::new();
    match value {
        Payload::Input(value) => {
            let root = wire::Input::create(
                &mut builder,
                &wire::InputArgs {
                    tick: value.tick,
                    buttons: value.buttons,
                    horizontal: value.horizontal,
                    vertical: value.vertical,
                },
            );
            builder.finish(root, None);
        }
        Payload::Snapshot(value) => {
            let offsets: Vec<_> = value
                .entities
                .iter()
                .map(|value| entity(&mut builder, value))
                .collect();
            let entities = builder.create_vector(&offsets);
            let map = builder.create_string(&value.map);
            let root = wire::Snapshot::create(
                &mut builder,
                &wire::SnapshotArgs {
                    tick: value.tick,
                    entities: Some(entities),
                    map: Some(map),
                },
            );
            builder.finish(root, None);
        }
        Payload::Delta(value) => {
            let offsets: Vec<_> = value
                .spawned
                .iter()
                .map(|value| entity(&mut builder, value))
                .collect();
            let spawned = builder.create_vector(&offsets);
            let offsets: Vec<_> = value
                .updated
                .iter()
                .map(|value| {
                    let position = builder.create_vector(&value.position);
                    wire::Update::create(
                        &mut builder,
                        &wire::UpdateArgs {
                            id: value.id,
                            position: Some(position),
                            health: value.health,
                            animation: value.animation,
                        },
                    )
                })
                .collect();
            let updated = builder.create_vector(&offsets);
            let removed = builder.create_vector(&value.removed);
            let tiles: Vec<_> = value
                .tiles
                .iter()
                .map(|tile| wire::Tile::new(tile.x, tile.y, tile.tile_type))
                .collect();
            let tiles = builder.create_vector(&tiles);
            let root = wire::Delta::create(
                &mut builder,
                &wire::DeltaArgs {
                    tick: value.tick,
                    base_tick: value.base_tick,
                    spawned: Some(spawned),
                    updated: Some(updated),
                    removed: Some(removed),
                    tiles: Some(tiles),
                },
            );
            builder.finish(root, None);
        }
    }
    builder.finished_data().to_vec()
}

pub fn decode(shape: Shape, bytes: &[u8]) -> Payload {
    match shape {
        Shape::Input => {
            let value = flatbuffers::root::<wire::Input<'_>>(bytes).unwrap();
            Payload::Input(Input {
                tick: value.tick(),
                buttons: value.buttons(),
                horizontal: value.horizontal(),
                vertical: value.vertical(),
            })
        }
        Shape::Snapshot => {
            let value = flatbuffers::root::<wire::Snapshot<'_>>(bytes).unwrap();
            Payload::Snapshot(Snapshot {
                tick: value.tick(),
                entities: value.entities().iter().map(read_entity).collect(),
                map: value.map().to_owned(),
            })
        }
        Shape::Delta => {
            let value = flatbuffers::root::<wire::Delta<'_>>(bytes).unwrap();
            Payload::Delta(Delta {
                tick: value.tick(),
                base_tick: value.base_tick(),
                spawned: value.spawned().iter().map(read_entity).collect(),
                updated: value
                    .updated()
                    .iter()
                    .map(|value| Update {
                        id: value.id(),
                        position: value.position().iter().collect(),
                        health: value.health(),
                        animation: value.animation(),
                    })
                    .collect(),
                removed: value.removed().iter().collect(),
                tiles: value
                    .tiles()
                    .iter()
                    .map(|value| Tile {
                        x: value.x(),
                        y: value.y(),
                        tile_type: value.tile_type(),
                    })
                    .collect(),
            })
        }
    }
}
