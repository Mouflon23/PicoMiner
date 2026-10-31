//! Settings baked in at build time from `.env`, plus tuning constants. The setup
//! portal's settings replace the network and pool ones, see `settings.rs`.

use embassy_rp::clocks::CoreVoltage;

pub const WIFI_SSID: &str = env!("PICOMINER_WIFI_SSID");
pub const WIFI_PASSWORD: &str = env!("PICOMINER_WIFI_PASSWORD");
pub const BTC_ADDRESS: &str = env!("PICOMINER_BTC_ADDRESS");
pub const POOL_HOST: &str = env!("PICOMINER_POOL_HOST");
pub const POOL_PORT: &str = env!("PICOMINER_POOL_PORT");
pub const WORKER: &str = env!("PICOMINER_WORKER");

pub const AGENT: &str = concat!("PicoMiner/", env!("CARGO_PKG_VERSION"));
pub const CHIP: &str = if cfg!(rp2350) { "RP2350" } else { "RP2040" };

const fn parse(text: &str) -> u32 {
    let bytes = text.as_bytes();
    let mut value = 0;
    let mut i = 0;
    while i < bytes.len() {
        value = value * 10 + (bytes[i] - b'0') as u32;
        i += 1;
    }
    value
}

pub const SYS_MHZ: u32 = parse(env!("PICOMINER_SYS_MHZ"));
pub const SYS_HZ: u32 = SYS_MHZ * 1_000_000;

pub const CORE_VOLTAGE: CoreVoltage = match parse(env!("PICOMINER_CORE_MV")) {
    1100 => CoreVoltage::V1_10,
    1150 => CoreVoltage::V1_15,
    1200 => CoreVoltage::V1_20,
    1250 => CoreVoltage::V1_25,
    1300 => CoreVoltage::V1_30,
    _ => panic!("CORE_MV must be 1100, 1150, 1200, 1250 or 1300"),
};

/// Keeps the CYW43 bus near 33 MHz whatever the clock. It is limited to 50 MHz,
/// and the driver is only known to work with whole dividers.
pub const WIFI_DIVIDER: u32 = SYS_MHZ.div_ceil(67);

/// Boot2 leaves the flash at clk_sys / 2, and it is rated for 133 MHz. Past 200
/// MHz this keeps it at or under 100 MHz: only cold code runs from it. The SSI
/// only takes even dividers.
#[cfg(not(rp2350))]
pub const FLASH_DIVIDER: u32 = SYS_MHZ.div_ceil(100).next_multiple_of(2);

/// Roughly one share every 20 s when overclocked, and every 40 s at stock.
pub const SUGGESTED_DIFFICULTY: f64 = 0.0005;

/// Hits below this are not worth reporting, whatever the pool asks for. Set well
/// under any pool difficulty so the best-difficulty counter keeps moving.
pub const LOCAL_MIN_DIFFICULTY: f64 = 0.0001;
