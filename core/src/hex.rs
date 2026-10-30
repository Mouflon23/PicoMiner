//! Hex helpers that work without an allocator.

use crate::sha256::Sha256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidHex;

fn nibble(c: u8) -> Result<u8, InvalidHex> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(InvalidHex),
    }
}

/// Decodes `hex`, which must be exactly twice the length of `out`.
pub fn decode_into(hex: &str, out: &mut [u8]) -> Result<(), InvalidHex> {
    let hex = hex.as_bytes();
    let clean = hex.iter().filter(|c| !c.is_ascii_whitespace());
    let mut written = 0;
    let mut high = None;
    for &c in clean {
        let value = nibble(c)?;
        match high.take() {
            None => high = Some(value),
            Some(h) => {
                *out.get_mut(written).ok_or(InvalidHex)? = (h << 4) | value;
                written += 1;
            }
        }
    }
    if high.is_some() || written != out.len() {
        return Err(InvalidHex);
    }
    Ok(())
}

pub fn parse_u32(hex: &str) -> Result<u32, InvalidHex> {
    let mut out = [0u8; 4];
    decode_into(hex, &mut out)?;
    Ok(u32::from_be_bytes(out))
}

/// Feeds decoded `hex` to `hasher` in small chunks.
pub fn hash_hex(hasher: &mut Sha256, hex: &str) -> Result<(), InvalidHex> {
    let hex = hex.as_bytes();
    if !hex.len().is_multiple_of(2) {
        return Err(InvalidHex);
    }
    let mut chunk = [0u8; 64];
    for pairs in hex.chunks(chunk.len() * 2) {
        let bytes = pairs.len() / 2;
        for (out, pair) in chunk.iter_mut().zip(pairs.chunks_exact(2)) {
            *out = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        hasher.update(&chunk[..bytes]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::sha256;

    #[test]
    fn decodes_and_rejects() {
        let mut out = [0u8; 3];
        decode_into("00fFa5", &mut out).unwrap();
        assert_eq!(out, [0x00, 0xff, 0xa5]);
        assert_eq!(decode_into("00ff", &mut out), Err(InvalidHex));
        assert_eq!(decode_into("00ffa", &mut out), Err(InvalidHex));
        assert_eq!(decode_into("00ffzz", &mut out), Err(InvalidHex));
    }

    #[test]
    fn parses_words() {
        assert_eq!(parse_u32("1d00ffff"), Ok(0x1d00_ffff));
        assert!(parse_u32("1d00").is_err());
    }

    #[test]
    fn hash_hex_matches_raw_bytes() {
        let mut hasher = Sha256::new();
        hash_hex(&mut hasher, "616263").unwrap();
        assert_eq!(hasher.finalize(), sha256(b"abc"));

        let mut hasher = Sha256::new();
        let long = "61".repeat(300);
        hash_hex(&mut hasher, &long).unwrap();
        assert_eq!(hasher.finalize(), sha256(&[0x61; 300]));
    }
}
