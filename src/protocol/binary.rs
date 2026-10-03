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
                    "protobuf" => GameDataEncoding::Protobuf,
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

/// Maximum MessagePack nesting the server decodes into a recursive value.
///
/// serde_json refuses JSON deeper than 128 levels during decode; rmp-serde
/// 1.3 carries only its own internal 1024-level guard, an implementation
/// detail of one dependency version rather than a wire contract, and it
/// allows nesting JSON game data could never reach at the same boundary. 128
/// keeps both wire encodings symmetric with serde_json's default and makes
/// the conversion budget independent of the decoder's internal limit.
pub const MSGPACK_MAX_NESTING_DEPTH: usize = 128;

/// Check MessagePack nesting with an explicit stack, not recursion.
///
/// Returns `true` when the payload stays within [`MSGPACK_MAX_NESTING_DEPTH`]
/// container levels. Depth is judged wherever the structure is readable; any
/// byte-level truncation or malformed marker also returns `true`: this
/// scanner only answers the depth question, and the real decoder owns every
/// other error, so the caller never reports a shallower error than the
/// decoder would.
pub fn msgpack_depth_within(payload: &[u8], limit: usize) -> bool {
    // Remaining sibling elements per open container. Arrays push their element
    // count; maps push twice their entry count. `open.len()` is the number of
    // containers enclosing the value being read.
    let mut open: Vec<u32> = Vec::new();
    let mut cursor = payload;

    // Consume one sibling slot at the current depth, closing any containers
    // that just ran out of elements.
    macro_rules! take_element {
        () => {
            while open.last() == Some(&0) {
                open.pop();
            }
            if let Some(top) = open.last_mut() {
                *top = top.saturating_sub(1);
            }
        };
    }

    // Read one big-endian `size`-byte length prefix plus the bytes after it.
    // `None` means the prefix itself is truncated; the decoder owns that
    // error, so the caller reports no depth violation.
    fn read_len(cursor: &[u8], size: usize) -> Option<(u64, &[u8])> {
        let (bytes, tail) = cursor.split_at_checked(size)?;
        let mut value = 0u64;
        for byte in bytes {
            value = value.checked_mul(256)?.checked_add(u64::from(*byte))?;
        }
        Some((value, tail))
    }

    // Advance past `count` payload bytes. A length longer than the remaining
    // payload is a decoder error, not a depth violation.
    fn skip_bytes(cursor: &[u8], count: u64) -> Option<&[u8]> {
        cursor
            .split_at_checked(usize::try_from(count).ok()?)
            .map(|(_, tail)| tail)
    }

    while let Some((&marker, rest)) = cursor.split_first() {
        take_element!();
        cursor = match marker {
            // fixmap / map16 / map32: every entry costs a key and a value.
            0x80..=0x8f => {
                if !push_container(&mut open, u64::from(marker & 0x0f).saturating_mul(2), limit) {
                    return false;
                }
                rest
            }
            0xde | 0xdf => {
                let size = if marker == 0xde { 2 } else { 4 };
                let Some((entries, tail)) = read_len(rest, size) else {
                    return true;
                };
                if !push_container(&mut open, entries.saturating_mul(2), limit) {
                    return false;
                }
                tail
            }
            // fixarray / array16 / array32
            0x90..=0x9f => {
                if !push_container(&mut open, u64::from(marker & 0x0f), limit) {
                    return false;
                }
                rest
            }
            0xdc | 0xdd => {
                let size = if marker == 0xdc { 2 } else { 4 };
                let Some((count, tail)) = read_len(rest, size) else {
                    return true;
                };
                if !push_container(&mut open, count, limit) {
                    return false;
                }
                tail
            }
            // fixstr / str8 / str16 / str32: opaque bytes, never containers.
            0xa0..=0xbf => {
                let Some(tail) = skip_bytes(rest, u64::from(marker & 0x1f)) else {
                    return true;
                };
                tail
            }
            0xd9..=0xdb => {
                let size = match marker {
                    0xd9 => 1usize,
                    0xda => 2,
                    _ => 4,
                };
                let Some((len, tail)) = read_len(rest, size) else {
                    return true;
                };
                let Some(skipped) = skip_bytes(tail, len) else {
                    return true;
                };
                skipped
            }
            // bin8 / bin16 / bin32
            0xc4..=0xc6 => {
                let size = match marker {
                    0xc4 => 1usize,
                    0xc5 => 2,
                    _ => 4,
                };
                let Some((len, tail)) = read_len(rest, size) else {
                    return true;
                };
                let Some(skipped) = skip_bytes(tail, len) else {
                    return true;
                };
                skipped
            }
            // ext8 / ext16 / ext32: one type byte plus the declared bytes.
            0xc7..=0xc9 => {
                let size = match marker {
                    0xc7 => 1usize,
                    0xc8 => 2,
                    _ => 4,
                };
                let Some((len, tail)) = read_len(rest, size) else {
                    return true;
                };
                let Some(total) = len.checked_add(1) else {
                    return true;
                };
                let Some(skipped) = skip_bytes(tail, total) else {
                    return true;
                };
                skipped
            }
            // fixext1 / fixext2 / fixext4 / fixext8 / fixext16
            0xd4..=0xd8 => {
                let size = match marker {
                    0xd4 => 1u64,
                    0xd5 => 2,
                    0xd6 => 4,
                    0xd7 => 8,
                    _ => 16,
                };
                let Some(total) = size.checked_add(1) else {
                    return true;
                };
                let Some(skipped) = skip_bytes(rest, total) else {
                    return true;
                };
                skipped
            }
            // f32 / f64
            0xca | 0xcb => {
                let size = if marker == 0xca { 4 } else { 8 };
                if rest.len() < size {
                    return true;
                }
                let (_, tail) = rest.split_at(size);
                tail
            }
            // u8/u16/u32/u64 and i8/i16/i32/i64
            0xcc..=0xcf => {
                let size = match marker {
                    0xcc => 1usize,
                    0xcd => 2,
                    0xce => 4,
                    _ => 8,
                };
                if rest.len() < size {
                    return true;
                }
                let (_, tail) = rest.split_at(size);
                tail
            }
            0xd0..=0xd3 => {
                let size = match marker {
                    0xd0 => 1usize,
                    0xd1 => 2,
                    0xd2 => 4,
                    _ => 8,
                };
                if rest.len() < size {
                    return true;
                }
                let (_, tail) = rest.split_at(size);
                tail
            }
            // nil, bool, never-use 0xc1, fixint, negative fixint: leaf values
            // with no payload bytes.
            _ => rest,
        };
    }
    true
}

/// Enter one container level. `false` means the payload exceeds `limit`.
fn push_container(open: &mut Vec<u32>, siblings: u64, limit: usize) -> bool {
    if open.len() >= limit {
        return false;
    }
    // A sibling count above u32::MAX cannot exist in a frame the size cap
    // admits, and the decoder rejects impossible lengths anyway; the clamp
    // only keeps the walk alive until the decoder speaks.
    open.push(u32::try_from(siblings).unwrap_or(u32::MAX));
    true
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

    /// N `fixarray(1)` markers followed by one `nil` leaf: a chain exactly
    /// `levels` containers deep, one byte per level.
    fn chain(levels: usize) -> Vec<u8> {
        let mut wire = vec![0x91u8; levels];
        wire.push(0xc0);
        wire
    }

    #[test]
    fn depth_scanner_matches_the_limit_boundary() {
        for levels in 0..=MSGPACK_MAX_NESTING_DEPTH {
            assert!(
                msgpack_depth_within(&chain(levels), MSGPACK_MAX_NESTING_DEPTH),
                "depth {levels} must be accepted"
            );
        }
        for levels in MSGPACK_MAX_NESTING_DEPTH + 1..MSGPACK_MAX_NESTING_DEPTH + 32 {
            assert!(
                !msgpack_depth_within(&chain(levels), MSGPACK_MAX_NESTING_DEPTH),
                "depth {levels} must be refused"
            );
        }
    }

    /// Map entries cost two sibling slots (key + value); the boundary pins
    /// the doubled counting so a regression cannot under-count map depth.
    #[test]
    fn depth_scanner_counts_map_entries_as_two_slots() {
        // fixmap(1) chain: one entry whose key is nil and whose value is the
        // next map. Each level costs exactly one map container.
        let map_chain = |levels: usize| {
            let mut wire = Vec::new();
            for _ in 0..levels {
                wire.push(0x81); // fixmap(1)
                wire.push(0xc0); // nil key
            }
            wire.push(0xc0); // nil value at the innermost level
            wire
        };
        assert!(msgpack_depth_within(
            &map_chain(MSGPACK_MAX_NESTING_DEPTH),
            MSGPACK_MAX_NESTING_DEPTH
        ));
        assert!(!msgpack_depth_within(
            &map_chain(MSGPACK_MAX_NESTING_DEPTH + 1),
            MSGPACK_MAX_NESTING_DEPTH
        ));
        // An ext payload's bytes are opaque: a following sibling at the same
        // depth is scanned, not swallowed by the ext skip.
        let mut wire = vec![0x92]; // fixarray(2)
        wire.extend_from_slice(&[0xc7, 0x02, 0x00, 0x01, 0xff]); // ext8 len 2
        wire.push(0x90); // second element: empty array, depth 2
        assert!(msgpack_depth_within(&wire, MSGPACK_MAX_NESTING_DEPTH));
    }

    #[test]
    fn depth_scanner_accepts_shallow_wires_and_skips_payload_bytes() {
        let mut wire = Vec::new();
        write_map_len(&mut wire, 2).unwrap();
        write_str(&mut wire, "payload").unwrap();
        write_bin(&mut wire, &[0x91, 0x91, 0x91]).unwrap();
        write_str(&mut wire, "seq").unwrap();
        write_uint(&mut wire, 7).unwrap();
        wire.push(0xca); // f32 0.0
        wire.extend_from_slice(&[0; 4]);
        wire.push(0xd4); // fixext1
        wire.extend_from_slice(&[0x00, 0xff]);
        assert!(msgpack_depth_within(&wire, MSGPACK_MAX_NESTING_DEPTH));
        // Container-looking bytes inside skipped payloads never count.
        assert!(msgpack_depth_within(
            &[0xa3, 0x91, 0x91, 0x91],
            MSGPACK_MAX_NESTING_DEPTH
        ));
        // Empty and scalar roots.
        assert!(msgpack_depth_within(&[], MSGPACK_MAX_NESTING_DEPTH));
        assert!(msgpack_depth_within(&[0xc0], MSGPACK_MAX_NESTING_DEPTH));
        assert!(msgpack_depth_within(&[0x2a], MSGPACK_MAX_NESTING_DEPTH));
        assert!(msgpack_depth_within(&[0xff], MSGPACK_MAX_NESTING_DEPTH));
        // An empty container counts as one level, then closes.
        assert!(msgpack_depth_within(&[0x90], MSGPACK_MAX_NESTING_DEPTH));
    }

    #[test]
    fn depth_scanner_is_conservative_on_malformed_input() {
        // The decoder owns byte-level errors; the scanner must not reject a
        // payload it cannot parse except for depth.
        let truncated: [(&str, Vec<u8>); 5] = [
            ("array32 header without length", vec![0xdd]),
            (
                "str16 past end of payload",
                vec![0xda, 0xff, 0xff, 0x91, 0x91],
            ),
            ("bin8 past end of payload", vec![0xc4, 0x10, 0x00]),
            ("ext32 truncated", vec![0xc9, 0x00, 0x00, 0x10, 0x00]),
            ("fixext16 truncated", vec![0xd8, 0x00]),
        ];
        for (name, wire) in truncated {
            assert!(
                msgpack_depth_within(&wire, MSGPACK_MAX_NESTING_DEPTH),
                "{name} must be left to the decoder"
            );
        }
        // Depth is still judged when every length prefix is readable: a
        // truncated chain of limit+1 markers refuses at the last push.
        assert!(!msgpack_depth_within(
            &chain(MSGPACK_MAX_NESTING_DEPTH + 1)[..MSGPACK_MAX_NESTING_DEPTH + 1],
            MSGPACK_MAX_NESTING_DEPTH
        ));
    }

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
        write_str(&mut wire, "bson").unwrap();
        assert_rejected(&wire, "unknown token");
    }

    /// Issue #627: `protobuf` joined `rkyv` as a declared opaque encoding
    /// token, so the strict decoder must accept it end to end.
    #[test]
    fn accepts_every_declared_encoding_token() {
        for (token, expected) in [
            ("json", GameDataEncoding::Json),
            ("message_pack", GameDataEncoding::MessagePack),
            ("rkyv", GameDataEncoding::Rkyv),
            ("protobuf", GameDataEncoding::Protobuf),
        ] {
            let mut wire = Vec::new();
            write_map_len(&mut wire, 5).unwrap();
            write_str(&mut wire, "from_player").unwrap();
            write_bin(&mut wire, &[0x11; 16]).unwrap();
            write_str(&mut wire, "encoding").unwrap();
            write_str(&mut wire, token).unwrap();
            write_str(&mut wire, "payload").unwrap();
            write_bin(&mut wire, &[9]).unwrap();
            write_str(&mut wire, "seq").unwrap();
            write_uint(&mut wire, 1).unwrap();
            write_str(&mut wire, "epoch").unwrap();
            write_u32(&mut wire, 1).unwrap();

            let frame = decode_v3_binary_game_data(&wire)
                .unwrap_or_else(|error| panic!("{token} must decode: {error}"));
            assert_eq!(frame.encoding, expected, "token {token} maps exactly");
        }
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
