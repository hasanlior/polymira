//! PS4 system functions are imported by "NID": 11 characters derived from the function name.
//! NID = base64 (custom alphabet, no padding) of the first 8 bytes of SHA-1(name + key), reversed.

use sha1::{Digest, Sha1};

const KEY: [u8; 16] = [0x51, 0x8D, 0x64, 0xA6, 0x35, 0xDE, 0xD8, 0xC1, 0xE6, 0xB0, 0x39, 0xB1, 0xC3, 0xE5, 0x52, 0x30];
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";

pub fn nid(name: &str) -> String {
    let mut h = Sha1::new();
    h.update(name.as_bytes());
    h.update(KEY);
    let digest = h.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    bytes.reverse();
    encode(&bytes)
}

/// Base64 with the NID alphabet and no padding (8 bytes → 11 characters).
pub fn encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// Library / module ids in import names ("NID#L#M") use the same alphabet.
pub fn decode_id(s: &str) -> u32 {
    s.bytes().fold(0, |acc, c| (acc << 6) | ALPHABET.iter().position(|&a| a == c).unwrap_or(0) as u32)
}

#[allow(dead_code)]
pub fn encode_id(mut id: u32) -> String {
    let mut out = Vec::new();
    loop {
        out.push(ALPHABET[(id & 63) as usize]);
        id >>= 6;
        if id == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_nid() {
        assert_eq!(nid("printf"), "hcuQgD53UxM");
    }
    #[test]
    fn ids_roundtrip() {
        for id in [0, 1, 2, 63, 64, 300] {
            assert_eq!(decode_id(&encode_id(id)), id);
        }
    }
}
