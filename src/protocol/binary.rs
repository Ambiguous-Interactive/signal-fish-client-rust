// Ported from Signal Fish Server v0.4.0 (50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8).
//! Strict decoding for protocol-v2 and protocol-v3 binary game-data envelopes.

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

/// The frozen protocol-v2 MessagePack game-data envelope.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct V2BinaryGameDataFrame {
    pub from_player: PlayerId,
    pub encoding: GameDataEncoding,
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}

/// Strictly decode the frozen protocol-v2 MessagePack envelope.
pub fn decode_v2_binary_game_data(wire: &[u8]) -> Result<V2BinaryGameDataFrame, String> {
    let mut remaining = wire;
    let field_count = read_map_len(&mut remaining)
        .map_err(|error| format!("v2 binary GameData envelope is not a map: {error}"))?;
    let mut from_player = None;
    let mut encoding = None;
    let mut payload = None;

    for _ in 0..field_count {
        let key = read_string(&mut remaining, "envelope key", "v2")?;
        match key {
            "from_player" => {
                reject_duplicate(&from_player, key, "v2")?;
                let bytes = read_binary(&mut remaining, key, "v2")?;
                let bytes: [u8; 16] = bytes.try_into().map_err(|_| {
                    "v2 binary GameData from_player must be a 16-byte binary UUID".to_string()
                })?;
                from_player = Some(PlayerId::from_bytes(bytes));
            }
            "encoding" => {
                reject_duplicate(&encoding, key, "v2")?;
                encoding = Some(match read_string(&mut remaining, key, "v2")? {
                    "message_pack" => GameDataEncoding::MessagePack,
                    value => {
                        return Err(format!(
                            "v2 binary GameData encoding must be message_pack, found {value:?}"
                        ));
                    }
                });
            }
            "payload" => {
                reject_duplicate(&payload, key, "v2")?;
                payload = Some(read_binary(&mut remaining, key, "v2")?.to_vec());
            }
            unknown => {
                return Err(format!(
                    "v2 binary GameData envelope contains unknown field {unknown:?}"
                ));
            }
        }
    }
    if !remaining.is_empty() {
        return Err("v2 binary GameData envelope contains trailing bytes".to_string());
    }
    Ok(V2BinaryGameDataFrame {
        from_player: require_field(from_player, "from_player", "v2")?,
        encoding: require_field(encoding, "encoding", "v2")?,
        payload: require_field(payload, "payload", "v2")?,
    })
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
        let key = read_string(&mut remaining, "envelope key", "v3")?;
        match key {
            "from_player" => {
                reject_duplicate(&from_player, key, "v3")?;
                let bytes = read_binary(&mut remaining, key, "v3")?;
                let bytes: [u8; 16] = bytes.try_into().map_err(|_| {
                    "v3 binary GameData from_player must be a 16-byte binary UUID".to_string()
                })?;
                from_player = Some(PlayerId::from_bytes(bytes));
            }
            "encoding" => {
                reject_duplicate(&encoding, key, "v3")?;
                encoding = Some(match read_string(&mut remaining, key, "v3")? {
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
                reject_duplicate(&payload, key, "v3")?;
                payload = Some(read_binary(&mut remaining, key, "v3")?.to_vec());
            }
            "seq" => {
                reject_duplicate(&seq, key, "v3")?;
                let value: u64 = read_int(&mut remaining).map_err(|error| {
                    format!("v3 binary GameData seq is not a u64 integer: {error}")
                })?;
                if value == 0 {
                    return Err("v3 binary GameData seq must be non-zero".to_string());
                }
                seq = Some(value);
            }
            "epoch" => {
                reject_duplicate(&epoch, key, "v3")?;
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
        from_player: require_field(from_player, "from_player", "v3")?,
        encoding: require_field(encoding, "encoding", "v3")?,
        payload: require_field(payload, "payload", "v3")?,
        seq: require_field(seq, "seq", "v3")?,
        epoch: require_field(epoch, "epoch", "v3")?,
    })
}

fn read_string<'a>(
    remaining: &mut &'a [u8],
    field: &str,
    version: &str,
) -> Result<&'a str, String> {
    let (value, tail) = read_str_from_slice(*remaining)
        .map_err(|error| format!("{version} binary GameData {field} is not a string: {error}"))?;
    *remaining = tail;
    Ok(value)
}

fn read_binary<'a>(
    remaining: &mut &'a [u8],
    field: &str,
    version: &str,
) -> Result<&'a [u8], String> {
    let len = read_bin_len(remaining).map_err(|error| {
        format!("{version} binary GameData {field} is not binary data: {error}")
    })?;
    let len = usize::try_from(len)
        .map_err(|_| format!("{version} binary GameData {field} length does not fit usize"))?;
    if remaining.len() < len {
        return Err(format!(
            "{version} binary GameData {field} is truncated: declared {len} bytes, found {}",
            remaining.len()
        ));
    }
    let (value, tail) = (*remaining).split_at(len);
    *remaining = tail;
    Ok(value)
}

fn reject_duplicate<T>(slot: &Option<T>, field: &str, version: &str) -> Result<(), String> {
    if slot.is_some() {
        Err(format!(
            "{version} binary GameData envelope contains duplicate field {field:?}"
        ))
    } else {
        Ok(())
    }
}

fn require_field<T>(slot: Option<T>, field: &str, version: &str) -> Result<T, String> {
    slot.ok_or_else(|| format!("{version} binary GameData envelope is missing field {field:?}"))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    const PLAYER_ID: u128 = 0x00112233445566778899aabbccddeeff;

    #[test]
    fn decodes_every_opaque_payload_encoding() {
        let from_player = PlayerId::from_u128(PLAYER_ID);
        for encoding in [
            GameDataEncoding::Json,
            GameDataEncoding::MessagePack,
            GameDataEncoding::Rkyv,
        ] {
            let expected = V3BinaryGameDataFrame {
                from_player,
                encoding,
                payload: vec![0, 1, 2, 0xff],
                seq: 9,
                epoch: 3,
            };
            let wire = rmp_serde::to_vec_named(&expected).expect("serialize fixture");
            assert_eq!(
                decode_v3_binary_game_data(&wire).expect("decode v3 envelope"),
                expected
            );
        }
    }

    #[test]
    fn decodes_only_the_frozen_v2_message_pack_envelope() {
        let expected = V2BinaryGameDataFrame {
            from_player: PlayerId::from_u128(PLAYER_ID),
            encoding: GameDataEncoding::MessagePack,
            payload: vec![0, 1, 2, 0xff],
        };
        let canonical = rmp_serde::to_vec_named(&expected).expect("serialize v2 fixture");
        assert_eq!(
            decode_v2_binary_game_data(&canonical).expect("decode v2 envelope"),
            expected
        );

        let positional = rmp_serde::to_vec(&(
            PlayerId::from_u128(PLAYER_ID),
            GameDataEncoding::MessagePack,
            serde_bytes::ByteBuf::from(vec![1, 2]),
        ))
        .expect("serialize positional fixture");
        assert!(decode_v2_binary_game_data(&positional).is_err());

        let mut trailing = canonical.clone();
        trailing.push(0);
        assert!(decode_v2_binary_game_data(&trailing).is_err());

        let v3 = V3BinaryGameDataFrame {
            from_player: PlayerId::from_u128(PLAYER_ID),
            encoding: GameDataEncoding::MessagePack,
            payload: vec![1],
            seq: 1,
            epoch: 1,
        };
        assert!(decode_v2_binary_game_data(
            &rmp_serde::to_vec_named(&v3).expect("serialize v3 fixture")
        )
        .is_err());
    }

    #[test]
    fn rejects_noncanonical_message_pack() {
        type Entry = (Vec<u8>, Vec<u8>);

        fn encoded<T: Serialize + ?Sized>(value: &T) -> Vec<u8> {
            rmp_serde::to_vec(value).expect("serialize fixture value")
        }

        fn valid_entries() -> Vec<Entry> {
            vec![
                (
                    encoded("from_player"),
                    encoded(&PlayerId::from_u128(PLAYER_ID)),
                ),
                (encoded("encoding"), encoded(&GameDataEncoding::Json)),
                (
                    encoded("payload"),
                    encoded(&serde_bytes::Bytes::new(b"opaque")),
                ),
                (encoded("seq"), encoded(&9u64)),
                (encoded("epoch"), encoded(&3u32)),
            ]
        }

        fn map(entries: &[Entry]) -> Vec<u8> {
            let mut wire = Vec::new();
            rmp::encode::write_map_len(&mut wire, entries.len() as u32)
                .expect("serialize fixture map length");
            for (key, value) in entries {
                wire.extend_from_slice(key);
                wire.extend_from_slice(value);
            }
            wire
        }

        let canonical = map(&valid_entries());
        assert!(decode_v3_binary_game_data(&canonical).is_ok());

        let mut cases: Vec<(&str, Vec<u8>)> = vec![(
            "positional array",
            encoded(&(PlayerId::from_u128(PLAYER_ID), "json", b"opaque", 9, 3)),
        )];

        let mut entries = valid_entries();
        entries[0].0 = encoded(&7u8);
        cases.push(("non-string key", map(&entries)));

        let mut entries = valid_entries();
        entries[1].1 = encoded(&7u8);
        cases.push(("numeric encoding", map(&entries)));

        let mut entries = valid_entries();
        entries[0].1 = encoded(&vec![0u8; 16]);
        cases.push(("array UUID", map(&entries)));

        let mut entries = valid_entries();
        entries[0].1 = encoded(&serde_bytes::Bytes::new(&[0u8; 15]));
        cases.push(("short binary UUID", map(&entries)));

        let mut entries = valid_entries();
        entries[2].1 = encoded(&vec![1u8, 2, 3]);
        cases.push(("array payload", map(&entries)));

        for missing in 0..5 {
            let mut entries = valid_entries();
            entries.remove(missing);
            cases.push((
                [
                    "missing from_player",
                    "missing encoding",
                    "missing payload",
                    "missing seq",
                    "missing epoch",
                ][missing],
                map(&entries),
            ));
        }

        let mut entries = valid_entries();
        entries.push(entries[3].clone());
        cases.push(("duplicate key", map(&entries)));

        let mut entries = valid_entries();
        entries[4].0 = encoded("unexpected");
        cases.push(("unknown key", map(&entries)));

        let mut entries = valid_entries();
        entries[3].1 = encoded(&0u8);
        cases.push(("zero seq", map(&entries)));

        let mut entries = valid_entries();
        entries[4].1 = encoded(&0u8);
        cases.push(("zero epoch", map(&entries)));

        let mut entries = valid_entries();
        entries[4].1 = encoded(&(u64::from(u32::MAX) + 1));
        cases.push(("epoch overflow", map(&entries)));

        cases.push(("truncated map", canonical[..canonical.len() - 1].to_vec()));

        let mut trailing_scalar = canonical.clone();
        trailing_scalar.extend(encoded(&1u8));
        cases.push(("trailing scalar", trailing_scalar));

        let mut concatenated_map = canonical.clone();
        concatenated_map.extend(&canonical);
        cases.push(("concatenated map", concatenated_map));

        for (name, wire) in cases {
            assert!(
                decode_v3_binary_game_data(&wire).is_err(),
                "noncanonical {name} envelope was accepted: {wire:?}"
            );
        }
    }
}
