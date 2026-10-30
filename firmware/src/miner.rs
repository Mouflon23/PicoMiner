//! The hashing loop, run by both cores. Core 1 does nothing else; core 0 runs it
//! in thread mode, preempted by the interrupt executor that handles everything
//! asynchronous. Hits wait in a queue that the Stratum loop polls.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU32, Ordering};

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use heapless::Deque;
#[cfg(feature = "stress")]
use picominer_core::sha256::CONTEXT_WORDS;
use picominer_core::sha256::Work;
use picominer_core::target::{hash_difficulty, share_limit};

use crate::config::LOCAL_MIN_DIFFICULTY;
use crate::scan::Scanner;
use crate::stats;

/// Nonces between two looks at the job slot: a few milliseconds of work.
const CHUNK: u32 = 256;

#[derive(Clone, Copy)]
pub struct Found {
    pub generation: u32,
    pub nonce: u32,
    pub hash: [u8; 32],
    pub difficulty: f64,
}

type Shared<T> = Mutex<CriticalSectionRawMutex, T>;

static GENERATION: AtomicU32 = AtomicU32::new(0);
static LIMIT: AtomicU32 = AtomicU32::new(0);
static WORK: Shared<Cell<Option<Work>>> = Mutex::new(Cell::new(None));
static FOUND: Shared<RefCell<Deque<Found, 8>>> = Mutex::new(RefCell::new(Deque::new()));
static HASHES: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// Replaces the current work and returns its generation.
pub fn publish(work: Work) -> u32 {
    let generation = WORK.lock(|slot| {
        slot.set(Some(work));
        let generation = GENERATION.load(Ordering::Relaxed) + 1;
        GENERATION.store(generation, Ordering::Release);
        generation
    });
    drain_found();
    generation
}

/// Stops both miners until the next [`publish`].
pub fn stop() {
    if cfg!(feature = "stress") {
        return;
    }
    WORK.lock(|slot| {
        slot.set(None);
        GENERATION.store(GENERATION.load(Ordering::Relaxed) + 1, Ordering::Release);
    });
}

pub fn set_difficulty(difficulty: f64) {
    LIMIT.store(share_limit(difficulty.min(LOCAL_MIN_DIFFICULTY)), Ordering::Relaxed);
}

pub fn take_found() -> Option<Found> {
    FOUND.lock(|queue| queue.borrow_mut().pop_front())
}

fn drain_found() {
    while take_found().is_some() {}
}

/// Total nonces tried by each core, wrapping.
pub fn hashes() -> [u32; 2] {
    [HASHES[0].load(Ordering::Relaxed), HASHES[1].load(Ordering::Relaxed)]
}

fn report(generation: u32, work: &Work, nonce: u32, limit: u32) {
    let hash = work.hash(nonce);
    let top = u32::from_le_bytes([hash[28], hash[29], hash[30], hash[31]]);
    if top > limit {
        // The assembly and the Rust SHA-256 disagree: the chip is clocked too high.
        stats::update(|stats| stats.bad_hits += 1);
        return;
    }
    let difficulty = hash_difficulty(&hash);
    stats::update(|stats| stats.best = stats.best.max(difficulty));
    let found = Found { generation, nonce, hash, difficulty };
    FOUND.lock(|queue| {
        // A full queue means nobody is reading: the newest hit can go.
        let _ = queue.borrow_mut().push_back(found);
    });
}

/// Compares both SHA-256 implementations on a few nonces. Returns the hashes run.
#[cfg(feature = "stress")]
fn stress(scanner: Scanner, work: &Work, context: &[u32; CONTEXT_WORDS], nonce: u32) -> u32 {
    const NONCES: u32 = 16;
    for n in nonce..nonce.saturating_add(NONCES) {
        if !scanner.agrees(work, context, n) {
            stats::update(|stats| stats.bad_hits += 1);
        }
    }
    NONCES * 2
}

/// Mines forever. `core` is 0 or 1 and picks a half of the nonce space.
pub fn mine(core: usize) -> ! {
    let scanner = Scanner::for_core(core);
    let half = (core as u32) << 31;
    let mut generation = u32::MAX;
    let mut job = None;
    let mut nonce = half;
    loop {
        let current = GENERATION.load(Ordering::Acquire);
        if current != generation {
            job = WORK.lock(Cell::get).map(|work: Work| (work, work.context()));
            generation = current;
            nonce = half;
        }
        let Some((work, context)) = &job else {
            cortex_m::asm::nop();
            continue;
        };

        #[cfg(feature = "stress")]
        let tried = stress(scanner, work, context, nonce);
        #[cfg(not(feature = "stress"))]
        let tried = {
            let limit = LIMIT.load(Ordering::Relaxed);
            match scanner.scan(context, nonce, CHUNK, limit) {
                Some(hit) => {
                    report(generation, work, hit, limit);
                    hit.wrapping_sub(nonce) + 1
                }
                None => CHUNK,
            }
        };
        // Only this core writes its counter, so load then store is safe.
        let counter = &HASHES[core];
        counter.store(counter.load(Ordering::Relaxed).wrapping_add(tried), Ordering::Relaxed);
        nonce = half | (nonce.wrapping_add(tried) & 0x7fff_ffff);
    }
}
