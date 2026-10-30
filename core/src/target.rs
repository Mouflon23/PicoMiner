//! Difficulty and target arithmetic. Hashes and targets are 256-bit little-endian
//! values, which is how a block hash in digest byte order is compared.

/// `0xFFFF * 2^208`, the target of difficulty 1.
const DIFF1: f64 = 26959535291011309493156476344723991336010898738574164086137773096960.0;

const TWO_64: f64 = 18446744073709551616.0;

fn le_value(hash: &[u8; 32]) -> f64 {
    let mut value = 0.0;
    for limb in hash.chunks_exact(8).rev() {
        let limb = u64::from_le_bytes([
            limb[0], limb[1], limb[2], limb[3], limb[4], limb[5], limb[6], limb[7],
        ]);
        value = value * TWO_64 + limb as f64;
    }
    value
}

/// Share difficulty of a hash.
pub fn hash_difficulty(hash: &[u8; 32]) -> f64 {
    let value = le_value(hash);
    if value == 0.0 { f64::MAX } else { DIFF1 / value }
}

/// Largest top 32-bit word a hash can have and still meet `difficulty`. A cheap
/// prefilter for the nonce loop: it passes a few hashes that fail the exact check.
pub fn share_limit(difficulty: f64) -> u32 {
    let limit = 0xFFFF as f64 / 65536.0 / difficulty;
    if limit >= u32::MAX as f64 { u32::MAX } else { limit as u32 }
}

/// Expands compact `nbits` into a little-endian 256-bit target.
pub fn nbits_target(nbits: u32) -> [u8; 32] {
    let exponent = (nbits >> 24) as usize;
    let mantissa = (nbits & 0x00ff_ffff).to_le_bytes();
    let mut target = [0u8; 32];
    for (i, byte) in mantissa[..3].iter().enumerate() {
        if let Some(slot) = (i + exponent).checked_sub(3).and_then(|at| target.get_mut(at)) {
            *slot = *byte;
        }
    }
    target
}

pub fn meets_target(hash: &[u8; 32], target: &[u8; 32]) -> bool {
    for (h, t) in hash.iter().rev().zip(target.iter().rev()) {
        if h != t {
            return h < t;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex::decode_into;

    fn hash_le(display_hex: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        decode_into(display_hex, &mut out).unwrap();
        out.reverse();
        out
    }

    #[test]
    fn genesis_target_and_difficulty() {
        let target = nbits_target(0x1d00_ffff);
        assert_eq!(
            target,
            hash_le("00000000ffff0000000000000000000000000000000000000000000000000000")
        );
        assert!((hash_difficulty(&target) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn real_block_meets_its_target() {
        let genesis = hash_le("000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f");
        let target = nbits_target(0x1d00_ffff);
        assert!(meets_target(&genesis, &target));
        assert!(!meets_target(&target, &genesis));
        assert!(meets_target(&target, &target));
        assert!((hash_difficulty(&genesis) - 2536.0).abs() < 1.0);
    }

    #[test]
    fn modern_nbits() {
        // The exponent sits well inside the target for today's difficulty.
        let target = nbits_target(0x1701_e5ac);
        assert_eq!(
            target,
            hash_le("00000000000000000001e5ac0000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn share_limit_never_rejects_a_valid_share() {
        for difficulty in [0.00015, 0.001, 0.5, 1.0, 512.0, 100000.0] {
            let limit = share_limit(difficulty);
            let mut hash = [0u8; 32];
            hash[28..].copy_from_slice(&limit.to_le_bytes());
            // A hash just above the prefilter must be below the difficulty.
            let above = limit as u64 + 1;
            hash[28..].copy_from_slice(&(above as u32).to_le_bytes());
            if above <= u32::MAX as u64 {
                assert!(hash_difficulty(&hash) < difficulty, "difficulty {difficulty}");
            }
        }
        assert_eq!(share_limit(1.0), 0);
        assert_eq!(share_limit(0.00015), 6666);
        assert_eq!(share_limit(1e-12), u32::MAX);
    }
}
