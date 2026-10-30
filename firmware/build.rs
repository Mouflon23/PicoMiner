//! Links the RP2040 memory layout and bakes the settings from `../.env` into the
//! firmware, so credentials live in one ignored file instead of the source.

use std::{env, fs, path::PathBuf};

const SETTINGS: [(&str, &str); 8] = [
    ("WIFI_SSID", ""),
    ("WIFI_PASSWORD", ""),
    ("BTC_ADDRESS", ""),
    ("POOL_HOST", "pool.solomining.de"),
    ("POOL_PORT", "3333"),
    ("WORKER", "picominer"),
    ("SYS_MHZ", "200"),
    ("CORE_MV", "1150"),
];

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("memory.x"), include_bytes!("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tlink-rp.x");

    println!("cargo:rerun-if-changed=../.env");
    let file = fs::read_to_string("../.env").unwrap_or_default();
    for (key, default) in SETTINGS {
        // The process environment wins, which is how scripts sweep clock speeds.
        println!("cargo:rerun-if-env-changed=PICOMINER_OVERRIDE_{key}");
        let overridden = env::var(format!("PICOMINER_OVERRIDE_{key}")).ok();
        let value = file
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once('='))
            .find(|(name, _)| name.trim() == key)
            .map(|(_, value)| value.trim().trim_matches('"'))
            .filter(|value| !value.is_empty())
            .unwrap_or(default);
        let value = overridden.as_deref().unwrap_or(value);
        println!("cargo:rustc-env=PICOMINER_{key}={value}");
    }
}
