//! Constants and primitives of the capture format, shared by the reader here and writers elsewhere; see
//! `docs/capture-format.md`.

pub const MAGIC: &[u8; 8] = b"PITOTCAP";
pub const VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 64;
pub const VAR_HEADER_SIZE: usize = crate::layout::VarHeader::SIZE;
pub const CHUNK_MAGIC: &[u8; 4] = b"CHNK";
pub const CHUNK_HEADER_SIZE: usize = 40;
pub const INDEX_MAGIC: &[u8; 4] = b"INDX";
pub const INDEX_ENTRY_SIZE: usize = 28;
pub const TRAILER_MAGIC: &[u8; 8] = b"PTCEND\0\0";
pub const TRAILER_SIZE: usize = 32;

/// Header flag: the capture has the player's car only, as `.ibt` files do.
pub const FLAG_PLAYER_CAR_ONLY: u32 = 1;

/// Upper bounds that keep a damaged file from asking for absurd allocations.
pub const MAX_VARS: u32 = 4096;
pub const MAX_CHUNK_BYTES: u32 = 1 << 30;

pub const KIND_FRAME: u8 = 1;
pub const KIND_SESSION_INFO: u8 = 2;
pub const KIND_GAP: u8 = 3;
pub const KIND_DISCONNECT: u8 = 4;
pub const KIND_END: u8 = 5;
pub const KIND_SESSION_CARRY: u8 = 6;

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

/// CRC-32 with the IEEE polynomial, as zip and PNG use.
pub fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |c, &b| {
        CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8)
    })
}

/// Appends `value` as unsigned LEB128.
pub fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Reads unsigned LEB128 at `*at`, advancing it. `None` when the bytes end or the number overflows 64 bits.
pub fn get_varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7F).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

pub fn u32_at(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

pub fn i32_at(bytes: &[u8], at: usize) -> i32 {
    u32_at(bytes, at) as i32
}

pub fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

pub fn put_u32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn put_u64(out: &mut [u8], at: usize, value: u64) {
    out[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn varint_round_trip() {
        let values = [0, 1, 127, 128, 166_667, u64::from(u32::MAX) + 1, u64::MAX];
        let mut bytes = Vec::new();
        for v in values {
            put_varint(&mut bytes, v);
        }
        assert_eq!(bytes[..3], [0, 1, 127]);
        let mut at = 0;
        for v in values {
            assert_eq!(get_varint(&bytes, &mut at), Some(v));
        }
        assert_eq!(at, bytes.len());
        assert_eq!(get_varint(&[0x80], &mut 0), None, "truncated");
        assert_eq!(get_varint(&[0xFF; 11], &mut 0), None, "too long");
    }
}
