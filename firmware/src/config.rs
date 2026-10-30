//! Settings baked in at build time from `.env`, plus tuning constants.

pub const WIFI_SSID: &str = env!("PICOMINER_WIFI_SSID");
pub const WIFI_PASSWORD: &str = env!("PICOMINER_WIFI_PASSWORD");
pub const BTC_ADDRESS: &str = env!("PICOMINER_BTC_ADDRESS");
pub const POOL_HOST: &str = env!("PICOMINER_POOL_HOST");
pub const POOL_PORT: &str = env!("PICOMINER_POOL_PORT");
pub const WORKER: &str = env!("PICOMINER_WORKER");

pub const AGENT: &str = concat!("PicoMiner/", env!("CARGO_PKG_VERSION"));

/// The RP2040 datasheet covers 200 MHz at the 1.15 V `system_freq` selects.
pub const SYS_HZ: u32 = 200_000_000;

/// Roughly one share every 20 s at the hashrate of two cores.
pub const SUGGESTED_DIFFICULTY: f64 = 0.00015;

/// Hits below this are not worth reporting, whatever the pool asks for. Set well
/// under any pool difficulty so the best-difficulty counter keeps moving.
pub const LOCAL_MIN_DIFFICULTY: f64 = 0.0001;

pub const fn network_configured() -> bool {
    !WIFI_SSID.is_empty() && !BTC_ADDRESS.is_empty()
}
