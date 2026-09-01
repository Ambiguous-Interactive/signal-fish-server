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

#[cfg(test)]
mod tests {
    use super::*;
    use rmp::encode::{
        write_bin, write_bin_len, write_map_len, write_sint, write_str, write_u32, write_uint,
    };

    /// A canonical five-field envelope: bin-marked 16-byte UUID, string
    /// encoding token, bin payload, positive u64 seq, positive u32 epoch.
    fn canonical(seq: u64, epoch: u32) -> Vec<u8> {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 5).unwrap();
        write_str(&mut wire, "from_player").unwrap();
        write_bin(&mut wire, &[0x11; 16]).unwrap();
        write_str(&mut wire, "encoding").unwrap();
        write_str(&mut wire, "rkyv").unwrap();
        write_str(&mut wire, "payload").unwrap();
        write_bin(&mut wire, &[1, 2, 3]).unwrap();
        write_str(&mut wire, "seq").unwrap();
        write_uint(&mut wire, seq).unwrap();
        write_str(&mut wire, "epoch").unwrap();
        write_u32(&mut wire, epoch).unwrap();
        wire
    }

    fn assert_rejected(wire: &[u8], expected_phrase: &str) {
        let error = match decode_v3_binary_game_data(wire) {
            Ok(frame) => panic!("envelope must be rejected, decoded: {frame:?}"),
            Err(error) => error,
        };
        assert!(
            error.contains(expected_phrase),
            "expected error containing {expected_phrase:?}, got: {error}"
        );
        assert!(
            error.starts_with("v3 binary GameData"),
            "every rejection must carry the envelope prefix, got: {error}"
        );
    }

    #[test]
    fn accepts_a_canonical_envelope() {
        let frame = decode_v3_binary_game_data(&canonical(7, 3)).expect("canonical decodes");
        assert_eq!(frame.from_player, PlayerId::from_bytes([0x11; 16]));
        assert_eq!(frame.encoding, GameDataEncoding::Rkyv);
        assert_eq!(frame.payload, vec![1, 2, 3]);
        assert_eq!(frame.seq, 7);
        assert_eq!(frame.epoch, 3);
    }

    #[test]
    fn accepts_the_boundary_delivery_stamps() {
        let frame = decode_v3_binary_game_data(&canonical(u64::MAX, u32::MAX))
            .expect("u64::MAX/u32::MAX stamps decode");
        assert_eq!(frame.seq, u64::MAX);
        assert_eq!(frame.epoch, u32::MAX);
    }

    #[test]
    fn rejects_a_non_map_envelope() {
        let mut wire = Vec::new();
        write_uint(&mut wire, 1).unwrap();
        assert_rejected(&wire, "is not a map");
    }

    #[test]
    fn rejects_duplicate_fields() {
        // A six-field map whose `seq` appears twice: the second occurrence must
        // be refused before its value is even read.
        let mut wire = Vec::new();
        write_map_len(&mut wire, 6).unwrap();
        write_str(&mut wire, "seq").unwrap();
        write_uint(&mut wire, 7).unwrap();
        write_str(&mut wire, "seq").unwrap();
        write_uint(&mut wire, 9).unwrap();
        write_str(&mut wire, "epoch").unwrap();
        write_u32(&mut wire, 3).unwrap();
        write_str(&mut wire, "from_player").unwrap();
        write_bin(&mut wire, &[0x11; 16]).unwrap();
        write_str(&mut wire, "encoding").unwrap();
        write_str(&mut wire, "rkyv").unwrap();
        write_str(&mut wire, "payload").unwrap();
        write_bin(&mut wire, &[1, 2, 3]).unwrap();
        assert_rejected(&wire, "duplicate field");
    }

    #[test]
    fn rejects_unknown_fields() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "bogus").unwrap();
        write_uint(&mut wire, 1).unwrap();
        assert_rejected(&wire, "unknown field");
    }

    #[test]
    fn rejects_missing_fields() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 0).unwrap();
        assert_rejected(&wire, "missing field \"from_player\"");

        // One field alone is still incomplete.
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "from_player").unwrap();
        write_bin(&mut wire, &[0x11; 16]).unwrap();
        assert_rejected(&wire, "missing field");
    }

    #[test]
    fn rejects_zero_delivery_stamps() {
        assert_rejected(&canonical(0, 3), "seq must be non-zero");
        assert_rejected(&canonical(7, 0), "epoch must be non-zero");
    }

    #[test]
    fn rejects_a_non_uuid_from_player() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "from_player").unwrap();
        write_bin(&mut wire, &[0x11; 15]).unwrap();
        assert_rejected(&wire, "16-byte binary UUID");

        // A string-marked value is not a binary UUID either.
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "from_player").unwrap();
        write_str(&mut wire, "0123456789abcdef").unwrap();
        assert_rejected(&wire, "from_player is not binary data");
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut wire = canonical(7, 3);
        wire.push(0x01);
        assert_rejected(&wire, "trailing bytes");
    }

    #[test]
    fn rejects_a_truncated_payload() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "payload").unwrap();
        write_bin_len(&mut wire, 10).unwrap();
        wire.extend_from_slice(&[1, 2]);
        assert_rejected(&wire, "is truncated");
    }

    #[test]
    fn rejects_unknown_encoding_tokens() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "encoding").unwrap();
        write_str(&mut wire, "protobuf").unwrap();
        assert_rejected(&wire, "unknown token");
    }

    #[test]
    fn rejects_non_string_keys() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_uint(&mut wire, 1).unwrap();
        write_uint(&mut wire, 1).unwrap();
        assert_rejected(&wire, "envelope key is not a string");
    }

    #[test]
    fn rejects_non_integer_delivery_stamps() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "seq").unwrap();
        write_str(&mut wire, "7").unwrap();
        assert_rejected(&wire, "seq is not a u64 integer");

        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "epoch").unwrap();
        write_str(&mut wire, "3").unwrap();
        assert_rejected(&wire, "epoch is not a u32 integer");
    }

    #[test]
    fn rejects_out_of_range_integer_delivery_stamps() {
        // A narrowing `as`-cast regression would silently wrap these; the
        // decoder must refuse them instead.
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "epoch").unwrap();
        write_uint(&mut wire, u64::from(u32::MAX) + 1).unwrap();
        assert_rejected(&wire, "epoch is not a u32 integer");

        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "seq").unwrap();
        write_sint(&mut wire, -1).unwrap();
        assert_rejected(&wire, "seq is not a u64 integer");
    }

    #[test]
    fn rejects_non_string_encoding_values() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "encoding").unwrap();
        write_uint(&mut wire, 1).unwrap();
        assert_rejected(&wire, "encoding is not a string");
    }

    #[test]
    fn rejects_non_binary_payload_values() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 1).unwrap();
        write_str(&mut wire, "payload").unwrap();
        write_str(&mut wire, "bytes").unwrap();
        assert_rejected(&wire, "payload is not binary data");
    }

    #[test]
    fn accepts_an_empty_payload() {
        // The encoder pins zero-length payloads at the bin8 boundary
        // (sending.rs `bin_boundaries`), so the decoder must keep accepting
        // them.
        let mut wire = Vec::new();
        write_map_len(&mut wire, 5).unwrap();
        write_str(&mut wire, "from_player").unwrap();
        write_bin(&mut wire, &[0x11; 16]).unwrap();
        write_str(&mut wire, "encoding").unwrap();
        write_str(&mut wire, "json").unwrap();
        write_str(&mut wire, "payload").unwrap();
        write_bin(&mut wire, &[]).unwrap();
        write_str(&mut wire, "seq").unwrap();
        write_uint(&mut wire, 1).unwrap();
        write_str(&mut wire, "epoch").unwrap();
        write_u32(&mut wire, 1).unwrap();

        let frame = decode_v3_binary_game_data(&wire).expect("empty payload decodes");
        assert!(frame.payload.is_empty());
        assert_eq!(frame.encoding, GameDataEncoding::Json);
    }
}
