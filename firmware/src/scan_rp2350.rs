//! The nonce loops of the RP2350, with the same interface as `scan.rs`. Its SHA-256
//! accelerator hashes a block far faster than the CPU, but there is only one and
//! it always starts from the initial state, so every nonce costs three blocks. It
//! goes to core 1, which nothing interrupts. Core 0 runs the Rust loop, which the
//! M33 handles well thanks to its barrel shifter. Both run from RAM, so the cores
//! do not compete for the flash cache.

use embassy_rp::pac;
use picominer_core::sha256::Work;

/// The accelerator needs the header itself, not the assembly's precomputed context.
pub type Context = Work;

pub fn context(work: &Work) -> Context {
    *work
}

/// Readies the accelerator. Must run before core 1 starts.
pub fn load() {
    pac::RESETS.reset().modify(|w| w.set_sha256(false));
    while !pac::RESETS.reset_done().read().sha256() {}
}

/// Clearing BSWAP along the way: the words here are message words already.
#[inline(always)]
fn restart() {
    pac::SHA256.csr().write(|w| w.set_start(true));
}

#[inline(always)]
fn write(word: u32) {
    pac::SHA256.wdata().write_value(word);
}

#[inline(always)]
fn wait_ready() {
    while !pac::SHA256.csr().read().wdata_rdy() {}
}

#[inline(always)]
fn wait_digest() {
    while !pac::SHA256.csr().read().sum_vld() {}
}

#[unsafe(link_section = ".data.ram_func")]
#[inline(never)]
fn scan_hardware(work: &Work, start: u32, count: u32, limit: u32) -> Option<u32> {
    let sha = pac::SHA256;
    let (first, tail) = (work.first_block(), work.tail());
    let mut nonce = start;
    for _ in 0..count {
        restart();
        for &word in first {
            write(word);
        }
        wait_ready();
        for &word in tail {
            write(word);
        }
        write(nonce.swap_bytes());
        write(0x8000_0000);
        for _ in 0..10 {
            write(0);
        }
        write(640);
        wait_digest();

        let digest = [
            sha.sum0().read(),
            sha.sum1().read(),
            sha.sum2().read(),
            sha.sum3().read(),
            sha.sum4().read(),
            sha.sum5().read(),
            sha.sum6().read(),
            sha.sum7().read(),
        ];
        restart();
        for word in digest {
            write(word);
        }
        write(0x8000_0000);
        for _ in 0..6 {
            write(0);
        }
        write(256);
        wait_digest();

        if sha.sum7().read().swap_bytes() <= limit {
            return Some(nonce);
        }
        nonce = nonce.wrapping_add(1);
    }
    None
}

#[unsafe(link_section = ".data.ram_func")]
#[inline(never)]
fn scan_software(work: &Work, start: u32, count: u32, limit: u32) -> Option<u32> {
    work.scan(start, count, limit)
}

/// A core's loop.
#[derive(Clone, Copy)]
pub struct Scanner(fn(&Work, u32, u32, u32) -> Option<u32>);

impl Scanner {
    pub fn for_core(core: usize) -> Self {
        Self(if core == 0 { scan_software } else { scan_hardware })
    }

    /// Same contract as [`Work::scan`].
    pub fn scan(self, context: &Context, start: u32, count: u32, limit: u32) -> Option<u32> {
        (self.0)(context, start, count, limit)
    }

    /// Whether the loop finds nonce `nonce` with the tightest limit that allows
    /// it and misses it with the next one, per the reference implementation.
    pub fn agrees(self, work: &Work, context: &Context, nonce: u32) -> bool {
        let digest = work.hash(nonce);
        let top = u32::from_le_bytes([digest[28], digest[29], digest[30], digest[31]]);
        self.scan(context, nonce, 1, top) == Some(nonce)
            && (top == 0 || self.scan(context, nonce, 1, top - 1).is_none())
    }
}

/// Checks both loops against the reference. Uses the accelerator, so it has to
/// run before core 1 starts.
pub fn self_test() -> bool {
    let work = Work::from_header(&[0x5a; 80]);
    (0..32).all(|nonce| {
        Scanner::for_core(0).agrees(&work, &work, nonce) && Scanner::for_core(1).agrees(&work, &work, nonce)
    })
}
