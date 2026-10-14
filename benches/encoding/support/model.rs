use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize, prost::Message)]
pub struct Entity {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(double, repeated, packed = "true", tag = "2")]
    pub position: Vec<f64>,
    #[prost(double, repeated, packed = "true", tag = "3")]
    pub velocity: Vec<f64>,
    #[prost(uint32, tag = "4")]
    pub health: u32,
    #[prost(uint32, tag = "5")]
    pub ammo: u32,
    #[prost(bool, tag = "6")]
    pub active: bool,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, prost::Message)]
pub struct Snapshot {
    #[prost(uint32, tag = "1")]
    pub tick: u32,
    #[prost(message, repeated, tag = "2")]
    pub entities: Vec<Entity>,
    #[prost(string, tag = "3")]
    pub map: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, prost::Message)]
pub struct Update {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(double, repeated, packed = "true", tag = "2")]
    pub position: Vec<f64>,
    #[prost(uint32, tag = "3")]
    pub health: u32,
    #[prost(uint32, tag = "4")]
    pub animation: u32,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, prost::Message)]
#[serde(from = "(u32, u32, u32)", into = "(u32, u32, u32)")]
pub struct Tile {
    #[prost(uint32, tag = "1")]
    pub x: u32,
    #[prost(uint32, tag = "2")]
    pub y: u32,
    #[prost(uint32, tag = "3")]
    pub tile_type: u32,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, prost::Message)]
pub struct Delta {
    #[prost(uint32, tag = "1")]
    pub tick: u32,
    #[prost(uint32, tag = "2")]
    pub base_tick: u32,
    #[prost(message, repeated, tag = "3")]
    pub spawned: Vec<Entity>,
    #[prost(message, repeated, tag = "4")]
    pub updated: Vec<Update>,
    #[prost(uint32, repeated, packed = "true", tag = "5")]
    pub removed: Vec<u32>,
    #[prost(message, repeated, tag = "6")]
    pub tiles: Vec<Tile>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Input {
    #[prost(uint32, tag = "1")]
    pub tick: u32,
    #[prost(uint32, tag = "2")]
    pub buttons: u32,
    #[prost(sint32, tag = "3")]
    pub horizontal: i32,
    #[prost(sint32, tag = "4")]
    pub vertical: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    Input(Input),
    Snapshot(Snapshot),
    Delta(Delta),
}

#[derive(Clone, Copy, Debug)]
pub enum Shape {
    Input,
    Snapshot,
    Delta,
}

impl Payload {
    pub fn shape(&self) -> Shape {
        match self {
            Self::Input(_) => Shape::Input,
            Self::Snapshot(_) => Shape::Snapshot,
            Self::Delta(_) => Shape::Delta,
        }
    }
    pub fn from_json(name: &str, bytes: &[u8]) -> Self {
        match name {
            "input-state" => {
                let (tick, buttons, horizontal, vertical) = serde_json::from_slice(bytes).unwrap();
                Self::Input(Input {
                    tick,
                    buttons,
                    horizontal,
                    vertical,
                })
            }
            "snapshot-small" | "snapshot-large" => {
                Self::Snapshot(serde_json::from_slice(bytes).unwrap())
            }
            "world-delta" => Self::Delta(serde_json::from_slice(bytes).unwrap()),
            _ => panic!("unknown corpus shape {name}"),
        }
    }
    pub fn logical_json(&self) -> serde_json::Value {
        match self {
            Self::Input(input) => {
                serde_json::json!([input.tick, input.buttons, input.horizontal, input.vertical])
            }
            Self::Snapshot(value) => serde_json::to_value(value).unwrap(),
            Self::Delta(value) => serde_json::to_value(value).unwrap(),
        }
    }
    pub fn floating_bits(&self) -> Vec<u64> {
        let entity = |value: &Entity| {
            value
                .position
                .iter()
                .chain(&value.velocity)
                .map(|v| v.to_bits())
                .collect::<Vec<_>>()
        };
        match self {
            Self::Input(_) => Vec::new(),
            Self::Snapshot(value) => value.entities.iter().flat_map(entity).collect(),
            Self::Delta(value) => value
                .spawned
                .iter()
                .flat_map(entity)
                .chain(
                    value
                        .updated
                        .iter()
                        .flat_map(|value| value.position.iter().map(|v| v.to_bits())),
                )
                .collect(),
        }
    }
}

impl From<(u32, u32, u32)> for Tile {
    fn from((x, y, tile_type): (u32, u32, u32)) -> Self {
        Self { x, y, tile_type }
    }
}
impl From<Tile> for (u32, u32, u32) {
    fn from(value: Tile) -> Self {
        (value.x, value.y, value.tile_type)
    }
}
