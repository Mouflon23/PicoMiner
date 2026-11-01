//! The nonce loops of the RP2350, with the same interface as `scan.rs`. Its SHA-256
//! accelerator hashes a block far faster than the CPU, but there is only one and
//! it always starts from the initial state, so every nonce costs three blocks. It
//! goes to core 1, which nothing interrupts, in `scan_rp2350.s`. Core 0 runs the Rust
//! loop, which the M33 handles well thanks to its barrel shifter. Both run from RAM,
//! so the cores do not compete for the flash cache.

use core::arch::global_asm;

use embassy_rp::pac;
use picominer_core::sha256::Work;

global_asm!(include_str!("scan_rp2350.s"), options(raw));

/// The header's first block, then the three words of the second one before the nonce.
const WORDS: usize = 19;

unsafe extern "C" {
    fn picominer_scan_accelerator(words: *const u32, nonce: u32, count: u32, limit: u32) -> u64;
}

/// The accelerator needs the header itself, not the assembly's precomputed context.
#[derive(Clone, Copy)]
pub struct Context {
    work: Work,
    words: [u32; WORDS],
}

pub fn context(work: &Work) -> Context {
    let mut words = [0; WORDS];
    let (first, tail) = words.split_at_mut(work.first_block().len());
    first.copy_from_slice(work.first_block());
    tail.copy_from_slice(work.tail());
    Context { work: *work, words }
}

/// Readies the accelerator. Must run before core 1 starts.
pub fn load() {
    pac::RESETS.reset().modify(|w| w.set_sha256(false));
    while !pac::RESETS.reset_done().read().sha256() {}
    // Core 1 hashes eleven times faster than core 0, so it goes first on the bus.
    pac::BUSCTRL.bus_priority().write(|w| w.set_proc1(true));
}

#[unsafe(link_section = ".data.ram_func")]
#[inline(never)]
fn scan_hardware(context: &Context, start: u32, count: u32, limit: u32) -> Option<u32> {
    // SAFETY: `load` ran at boot, and only core 1 uses the accelerator once it runs.
    let result = unsafe { picominer_scan_accelerator(context.words.as_ptr(), start, count, limit) };
    (result as u32 != 0).then_some((result >> 32) as u32)
}

#[unsafe(link_section = ".data.ram_func")]
#[inline(never)]
fn scan_software(context: &Context, start: u32, count: u32, limit: u32) -> Option<u32> {
    context.work.scan(start, count, limit)
}

/// The word of a nonce's hash that the loops compare with the limit.
fn top_word(work: &Work, nonce: u32) -> u32 {
    let digest = work.hash(nonce);
    u32::from_le_bytes([digest[28], digest[29], digest[30], digest[31]])
}

/// A core's loop.
#[derive(Clone, Copy)]
pub struct Scanner(fn(&Context, u32, u32, u32) -> Option<u32>);

impl Scanner {
    pub fn for_core(core: usize) -> Self {
        Self(if core == 0 { scan_software } else { scan_hardware })
    }

    /// Same contract as [`Work::scan`]. `count` must not be zero.
    pub fn scan(self, context: &Context, start: u32, count: u32, limit: u32) -> Option<u32> {
        (self.0)(context, start, count, limit)
    }

    /// Whether the loop finds nonce `nonce` with the tightest limit that allows
    /// it and misses it with the next one, per the reference implementation. Also
    /// reaches it as the second nonce of a run, which the first one would not test:
    /// that path is the one that mines.
    pub fn agrees(self, work: &Work, context: &Context, nonce: u32) -> bool {
        let top = top_word(work, nonce);
        let previous = nonce.wrapping_sub(1);
        let expected = if top_word(work, previous) <= top { previous } else { nonce };
        self.scan(context, nonce, 1, top) == Some(nonce)
            && (top == 0 || self.scan(context, nonce, 1, top - 1).is_none())
            && self.scan(context, previous, 2, top) == Some(expected)
    }
}

/// Checks both loops against the reference. Uses the accelerator, so it has to
/// run before core 1 starts.
pub fn self_test() -> bool {
    let work = Work::from_header(&[0x5a; 80]);
    let context = context(&work);
    (0..32).all(|nonce| {
        Scanner::for_core(0).agrees(&work, &context, nonce) && Scanner::for_core(1).agrees(&work, &context, nonce)
    })
}
