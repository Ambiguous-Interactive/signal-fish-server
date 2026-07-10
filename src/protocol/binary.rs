//! Strict decoding for the protocol-v3 binary game-data wire envelope.

use rmp::decode::{read_bin_len, read_int, read_map_len, read_str_from_slice};
use serde::Serialize;

use super::{GameDataEncoding, PlayerId};

/// The mandatory metadata carried by every protocol-v3 binary game-data frame.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct V3BinaryGameDataFrame {
    pub from_player: PlayerId,
    pub encoding: GameDataEncoding,
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    pub seq: u64,
    pub epoch: u32,
}

/// Decode exactly one canonical protocol-v3 binary game-data envelope.
///
/// Unlike a derived Serde decoder, this validates the physical MessagePack
/// representation: a map with string keys, binary UUID/payload fields, string
/// encoding token, integer delivery stamps, and no trailing value.
pub fn decode_v3_binary_game_data(wire: &[u8]) -> Result<V3BinaryGameDataFrame, String> {
    let mut remaining = wire;
    let field_count = read_map_len(&mut remaining)
        .map_err(|error| format!("v3 binary GameData envelope is not a map: {error}"))?;

    let mut from_player = None;
    let mut encoding = None;
    let mut payload = None;
    let mut seq = None;
    let mut epoch = None;

    for _ in 0..field_count {
        let key = read_string(&mut remaining, "envelope key")?;
        match key {
            "from_player" => {
                reject_duplicate(&from_player, key)?;
                let bytes = read_binary(&mut remaining, key)?;
                let bytes: [u8; 16] = bytes.try_into().map_err(|_| {
                    "v3 binary GameData from_player must be a 16-byte binary UUID".to_string()
                })?;
                from_player = Some(PlayerId::from_bytes(bytes));
            }
            "encoding" => {
                reject_duplicate(&encoding, key)?;
                encoding = Some(match read_string(&mut remaining, key)? {
                    "json" => GameDataEncoding::Json,
                    "message_pack" => GameDataEncoding::MessagePack,
                    "rkyv" => GameDataEncoding::Rkyv,
                    value => {
                        return Err(format!(
                            "v3 binary GameData encoding has unknown token {value:?}"
                        ));
                    }
                });
            }
            "payload" => {
                reject_duplicate(&payload, key)?;
                payload = Some(read_binary(&mut remaining, key)?.to_vec());
            }
            "seq" => {
                reject_duplicate(&seq, key)?;
                let value: u64 = read_int(&mut remaining).map_err(|error| {
                    format!("v3 binary GameData seq is not a u64 integer: {error}")
                })?;
                if value == 0 {
                    return Err("v3 binary GameData seq must be non-zero".to_string());
                }
                seq = Some(value);
            }
            "epoch" => {
                reject_duplicate(&epoch, key)?;
                let value: u32 = read_int(&mut remaining).map_err(|error| {
                    format!("v3 binary GameData epoch is not a u32 integer: {error}")
                })?;
                if value == 0 {
                    return Err("v3 binary GameData epoch must be non-zero".to_string());
                }
                epoch = Some(value);
            }
            unknown => {
                return Err(format!(
                    "v3 binary GameData envelope contains unknown field {unknown:?}"
                ));
            }
        }
    }

    if !remaining.is_empty() {
        return Err("v3 binary GameData envelope contains trailing bytes".to_string());
    }

    Ok(V3BinaryGameDataFrame {
        from_player: require_field(from_player, "from_player")?,
        encoding: require_field(encoding, "encoding")?,
        payload: require_field(payload, "payload")?,
        seq: require_field(seq, "seq")?,
        epoch: require_field(epoch, "epoch")?,
    })
}

fn read_string<'a>(remaining: &mut &'a [u8], field: &str) -> Result<&'a str, String> {
    let (value, tail) = read_str_from_slice(*remaining)
        .map_err(|error| format!("v3 binary GameData {field} is not a string: {error}"))?;
    *remaining = tail;
    Ok(value)
}

fn read_binary<'a>(remaining: &mut &'a [u8], field: &str) -> Result<&'a [u8], String> {
    let len = read_bin_len(remaining)
        .map_err(|error| format!("v3 binary GameData {field} is not binary data: {error}"))?;
    let len = usize::try_from(len)
        .map_err(|_| format!("v3 binary GameData {field} length does not fit usize"))?;
    if remaining.len() < len {
        return Err(format!(
            "v3 binary GameData {field} is truncated: declared {len} bytes, found {}",
            remaining.len()
        ));
    }
    let (value, tail) = (*remaining).split_at(len);
    *remaining = tail;
    Ok(value)
}

fn reject_duplicate<T>(slot: &Option<T>, field: &str) -> Result<(), String> {
    if slot.is_some() {
        Err(format!(
            "v3 binary GameData envelope contains duplicate field {field:?}"
        ))
    } else {
        Ok(())
    }
}

fn require_field<T>(slot: Option<T>, field: &str) -> Result<T, String> {
    slot.ok_or_else(|| format!("v3 binary GameData envelope is missing field {field:?}"))
}
