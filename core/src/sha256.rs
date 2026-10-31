//! SHA-256 tuned for the Cortex-M0+: a generic streaming hasher for the coinbase
//! and merkle tree, plus [`Work::scan`], the nonce loop that dominates runtime.

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub const IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[inline(always)]
fn big0(x: u32) -> u32 {
    x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)
}

#[inline(always)]
fn big1(x: u32) -> u32 {
    x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)
}

#[inline(always)]
fn small0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}

#[inline(always)]
fn small1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

#[inline(always)]
fn ch(e: u32, f: u32, g: u32) -> u32 {
    g ^ (e & (f ^ g))
}

#[inline(always)]
fn maj(a: u32, b: u32, c: u32) -> u32 {
    (a & b) | (c & (a | b))
}

/// One round, written so the eight state words never move: callers rotate the
/// argument order instead, which saves eight stack moves per round on the M0+.
macro_rules! round {
    ($a:ident, $b:ident, $c:ident, $d:ident, $e:ident, $f:ident, $g:ident, $h:ident, $ki:expr, $wi:expr) => {
        $h = $h
            .wrapping_add(big1($e))
            .wrapping_add(ch($e, $f, $g))
            .wrapping_add($ki)
            .wrapping_add($wi);
        $d = $d.wrapping_add($h);
        $h = $h.wrapping_add(big0($a)).wrapping_add(maj($a, $b, $c));
    };
}

/// Eight consecutive rounds from `$i`; the state is back in its original order after.
macro_rules! eight_rounds {
    ($s:ident, $w:ident, $i:expr) => {
        let [ref mut a, ref mut b, ref mut c, ref mut d, ref mut e, ref mut f, ref mut g, ref mut h] = $s;
        let (mut a_, mut b_, mut c_, mut d_, mut e_, mut f_, mut g_, mut h_) =
            (*a, *b, *c, *d, *e, *f, *g, *h);
        round!(a_, b_, c_, d_, e_, f_, g_, h_, K[$i], $w[$i]);
        round!(h_, a_, b_, c_, d_, e_, f_, g_, K[$i + 1], $w[$i + 1]);
        round!(g_, h_, a_, b_, c_, d_, e_, f_, K[$i + 2], $w[$i + 2]);
        round!(f_, g_, h_, a_, b_, c_, d_, e_, K[$i + 3], $w[$i + 3]);
        round!(e_, f_, g_, h_, a_, b_, c_, d_, K[$i + 4], $w[$i + 4]);
        round!(d_, e_, f_, g_, h_, a_, b_, c_, K[$i + 5], $w[$i + 5]);
        round!(c_, d_, e_, f_, g_, h_, a_, b_, K[$i + 6], $w[$i + 6]);
        round!(b_, c_, d_, e_, f_, g_, h_, a_, K[$i + 7], $w[$i + 7]);
        (*a, *b, *c, *d, *e, *f, *g, *h) = (a_, b_, c_, d_, e_, f_, g_, h_);
    };
}

#[inline(always)]
fn expand(w: &mut [u32; 64], from: usize, to: usize) {
    let mut i = from;
    while i < to {
        w[i] = small1(w[i - 2])
            .wrapping_add(w[i - 7])
            .wrapping_add(small0(w[i - 15]))
            .wrapping_add(w[i - 16]);
        i += 1;
    }
}

pub fn compress(state: &mut [u32; 8], block: &[u32; 16]) {
    let mut w = [0u32; 64];
    w[..16].copy_from_slice(block);
    expand(&mut w, 16, 64);
    let mut s = *state;
    let mut i = 0;
    while i < 64 {
        eight_rounds!(s, w, i);
        i += 8;
    }
    for (out, add) in state.iter_mut().zip(s) {
        *out = out.wrapping_add(add);
    }
}

fn be_words(block: &[u8; 64]) -> [u32; 16] {
    let mut w = [0u32; 16];
    for (word, bytes) in w.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    w
}

/// Streaming SHA-256. The coinbase is hashed through this while its hex is
/// decoded, so it never has to fit in RAM as raw bytes.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    used: usize,
    total: u64,
}

impl Sha256 {
    pub const fn new() -> Self {
        Self { state: IV, buf: [0; 64], used: 0, total: 0 }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u64;
        while !data.is_empty() {
            let take = (64 - self.used).min(data.len());
            self.buf[self.used..self.used + take].copy_from_slice(&data[..take]);
            self.used += take;
            data = &data[take..];
            if self.used == 64 {
                compress(&mut self.state, &be_words(&self.buf));
                self.used = 0;
            }
        }
    }

    pub fn finalize(mut self) -> [u8; 32] {
        let bits = self.total * 8;
        self.update(&[0x80]);
        while self.used != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (bytes, word) in out.chunks_exact_mut(4).zip(self.state) {
            bytes.copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize()
}

pub fn sha256d(data: &[u8]) -> [u8; 32] {
    sha256(&sha256(data))
}

/// A header reduced to what changes nothing between nonces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Work {
    /// The first 64 header bytes as big-endian words. A hardware SHA-256 that
    /// cannot resume from the midstate has to hash them again for every nonce.
    first: [u32; 16],
    /// SHA-256 state after the first 64 header bytes.
    midstate: [u32; 8],
    /// State after rounds 0..=2 of the second block, which precede the nonce.
    state3: [u32; 8],
    /// Header bytes 64..76 as big-endian words.
    tail: [u32; 3],
    /// Schedule words 16 and 17, which do not depend on the nonce.
    w16: u32,
    w17: u32,
}

impl Work {
    pub fn from_header(header: &[u8; 80]) -> Self {
        let mut first = [0u8; 64];
        first.copy_from_slice(&header[..64]);
        let first = be_words(&first);
        let mut midstate = IV;
        compress(&mut midstate, &first);

        let word = |i: usize| {
            u32::from_be_bytes([header[64 + i], header[65 + i], header[66 + i], header[67 + i]])
        };
        let tail = [word(0), word(4), word(8)];

        let [a, mut b, mut c, mut d, e, mut f, mut g, mut h] = midstate;
        round!(a, b, c, d, e, f, g, h, K[0], tail[0]);
        round!(h, a, b, c, d, e, f, g, K[1], tail[1]);
        round!(g, h, a, b, c, d, e, f, K[2], tail[2]);

        // Message words 5..=14 are zero, word 15 is the 640 bit length.
        let w16 = small0(tail[1]).wrapping_add(tail[0]);
        let w17 = small1(640).wrapping_add(small0(tail[2])).wrapping_add(tail[1]);

        Self { first, midstate, state3: [f, g, h, a, b, c, d, e], tail, w16, w17 }
    }

    /// The first block of the header, as message words.
    pub fn first_block(&self) -> &[u32; 16] {
        &self.first
    }

    /// Header bytes 64..76, which start the second block, as message words.
    pub fn tail(&self) -> &[u32; 3] {
        &self.tail
    }

    /// Full double SHA-256 of the header with `nonce`, in digest byte order.
    pub fn hash(&self, nonce: u32) -> [u8; 32] {
        let mut w = [0u32; 16];
        w[..3].copy_from_slice(&self.tail);
        w[3] = nonce.swap_bytes();
        w[4] = 0x8000_0000;
        w[15] = 640;
        let mut first = self.midstate;
        compress(&mut first, &w);

        let mut w = [0u32; 16];
        w[..8].copy_from_slice(&first);
        w[8] = 0x8000_0000;
        w[15] = 256;
        let mut second = IV;
        compress(&mut second, &w);

        let mut out = [0u8; 32];
        for (bytes, word) in out.chunks_exact_mut(4).zip(second) {
            bytes.copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    /// Tries `count` nonces from `start` and returns the first whose hash has a
    /// top little-endian word at most `limit`. The hit still has to be confirmed
    /// against the exact target with [`Work::hash`]: only the top word is computed.
    #[inline]
    pub fn scan(&self, start: u32, count: u32, limit: u32) -> Option<u32> {
        let mut w = [0u32; 64];
        w[0] = self.tail[0];
        w[1] = self.tail[1];
        w[2] = self.tail[2];
        w[4] = 0x8000_0000;
        w[15] = 640;
        w[16] = self.w16;
        w[17] = self.w17;

        let mut w2 = [0u32; 64];
        w2[8] = 0x8000_0000;
        w2[15] = 256;

        let mut nonce = start;
        for _ in 0..count {
            w[3] = nonce.swap_bytes();
            expand(&mut w, 18, 64);

            // Named as they sit after round 2; five rounds bring them back in order.
            let [mut f, mut g, mut h, mut a, mut b, mut c, mut d, mut e] = self.state3;
            round!(f, g, h, a, b, c, d, e, K[3], w[3]);
            round!(e, f, g, h, a, b, c, d, K[4], w[4]);
            round!(d, e, f, g, h, a, b, c, K[5], w[5]);
            round!(c, d, e, f, g, h, a, b, K[6], w[6]);
            round!(b, c, d, e, f, g, h, a, K[7], w[7]);
            // Constant indices let the round constants become immediates in the code.
            let mut s = [a, b, c, d, e, f, g, h];
            eight_rounds!(s, w, 8);
            eight_rounds!(s, w, 16);
            eight_rounds!(s, w, 24);
            eight_rounds!(s, w, 32);
            eight_rounds!(s, w, 40);
            eight_rounds!(s, w, 48);
            eight_rounds!(s, w, 56);

            for i in 0..8 {
                w2[i] = self.midstate[i].wrapping_add(s[i]);
            }
            // Word 16 onward of the second pass.
            expand(&mut w2, 16, 61);

            let mut s = IV;
            eight_rounds!(s, w2, 0);
            eight_rounds!(s, w2, 8);
            eight_rounds!(s, w2, 16);
            eight_rounds!(s, w2, 24);
            eight_rounds!(s, w2, 32);
            eight_rounds!(s, w2, 40);
            eight_rounds!(s, w2, 48);
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = s;
            round!(a, b, c, d, e, f, g, h, K[56], w2[56]);
            round!(h, a, b, c, d, e, f, g, K[57], w2[57]);
            round!(g, h, a, b, c, d, e, f, K[58], w2[58]);
            round!(f, g, h, a, b, c, d, e, K[59], w2[59]);
            round!(e, f, g, h, a, b, c, d, K[60], w2[60]);
            // The h leaving round 63 is the e leaving round 60, which sits in
            // the variable `h` here, so the last three rounds are never run.
            let _ = (a, b, c, d, e, f, g);
            let top = h.wrapping_add(IV[7]).swap_bytes();
            if top <= limit {
                return Some(nonce);
            }
            nonce = nonce.wrapping_add(1);
        }
        None
    }
}

include!("scan_context.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex::decode_into;

    fn hex32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        decode_into(s, &mut out).unwrap();
        out
    }

    #[test]
    fn nist_vectors() {
        assert_eq!(
            sha256(b""),
            hex32("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(
            sha256(b"abc"),
            hex32("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            hex32("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
        );
    }

    #[test]
    fn streaming_matches_oneshot_across_block_boundaries() {
        let data = [0x5au8; 200];
        for split in [0, 1, 55, 56, 63, 64, 65, 119, 120, 199, 200] {
            let mut hasher = Sha256::new();
            hasher.update(&data[..split]);
            hasher.update(&data[split..]);
            assert_eq!(hasher.finalize(), sha256(&data), "split {split}");
        }
    }

    fn genesis_header() -> [u8; 80] {
        let mut header = [0u8; 80];
        decode_into(
            "0100000000000000000000000000000000000000000000000000000000000000\
             000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa\
             4b1e5e4a29ab5f49ffff001d1dac2b7c",
            &mut header,
        )
        .unwrap();
        header
    }

    #[test]
    fn work_hash_matches_generic_sha256d() {
        let header = genesis_header();
        let work = Work::from_header(&header);
        assert_eq!(work.hash(0x7c2b_ac1d), sha256d(&header));
        let mut reversed = sha256d(&header);
        reversed.reverse();
        assert_eq!(
            reversed,
            hex32("000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f")
        );
    }

    #[test]
    fn scan_finds_the_genesis_nonce() {
        let work = Work::from_header(&genesis_header());
        let nonce = 0x7c2b_ac1d;
        assert_eq!(work.scan(nonce - 500, 1000, 0), Some(nonce));
        assert_eq!(work.scan(nonce + 1, 1000, 0), None);
    }

    #[test]
    fn scan_top_word_agrees_with_full_hash() {
        let work = Work::from_header(&genesis_header());
        for nonce in 0..2_000u32 {
            let digest = work.hash(nonce);
            let top = u32::from_le_bytes([digest[28], digest[29], digest[30], digest[31]]);
            assert_eq!(work.scan(nonce, 1, top), Some(nonce), "nonce {nonce}");
            if top > 0 {
                assert_eq!(work.scan(nonce, 1, top - 1), None, "nonce {nonce}");
            }
        }
    }
}
