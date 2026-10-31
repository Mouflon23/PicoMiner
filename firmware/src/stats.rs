//! State shared between the miners and the screen.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Starting,
    /// Serving the setup portal.
    Setup,
    /// No credentials: hashing a synthetic job to measure speed.
    Benchmark,
    Joining,
    Dhcp,
    Connecting,
    Mining,
    PoolError,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Starting => "Starting",
            Status::Setup => "Setup",
            Status::Benchmark => "Benchmark",
            Status::Joining => "Wi-Fi...",
            Status::Dhcp => "DHCP...",
            Status::Connecting => "Pool...",
            Status::Mining => "Mining",
            Status::PoolError => "Pool error",
        }
    }
}

#[derive(Clone, Copy)]
pub struct Stats {
    pub status: Status,
    pub accepted: u32,
    pub rejected: u32,
    pub blocks: u32,
    pub best: f64,
    pub pool_difficulty: f64,
    pub height: u32,
    pub rssi: i32,
    pub ip: [u8; 4],
    pub temperature: f32,
    /// Times the assembly and the Rust SHA-256 disagreed.
    pub bad_hits: u32,
}

static STATS: Mutex<CriticalSectionRawMutex, RefCell<Stats>> = Mutex::new(RefCell::new(Stats {
    status: Status::Starting,
    accepted: 0,
    rejected: 0,
    blocks: 0,
    best: 0.0,
    pool_difficulty: 0.0,
    height: 0,
    rssi: 0,
    ip: [0; 4],
    temperature: 0.0,
    bad_hits: 0,
}));

pub fn get() -> Stats {
    STATS.lock(|stats| *stats.borrow())
}

pub fn update(change: impl FnOnce(&mut Stats)) {
    STATS.lock(|stats| change(&mut stats.borrow_mut()));
}

pub fn set_status(status: Status) {
    update(|stats| stats.status = status);
}
