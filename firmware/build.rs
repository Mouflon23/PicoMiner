//! Links the memory layout of the target chip and bakes the settings from `../.env`
//! into the firmware, so credentials live in one ignored file instead of the source.

use std::{env, fs, path::PathBuf};

const SETTINGS: [(&str, &str); 5] = [
    ("WIFI_SSID", ""),
    ("WIFI_PASSWORD", ""),
    ("BTC_ADDRESS", ""),
    ("POOL_HOST", "pool.solomining.de"),
    ("POOL_PORT", "3333"),
];

/// Settings that differ per chip, with their RP2040 and RP2350 defaults. On the
/// RP2350 they are read from `<KEY>_RP2350`: an overclock found on one chip says
/// nothing about the other.
const CHIP_SETTINGS: [(&str, &str, &str); 3] = [
    ("WORKER", "picominer", "picominer2"),
    ("SYS_MHZ", "200", "150"),
    ("CORE_MV", "1150", "1100"),
];

fn lookup<'a>(file: &'a str, key: &str) -> Option<&'a str> {
    file.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| name.trim() == key)
        .map(|(_, value)| value.trim().trim_matches('"'))
        .filter(|value| !value.is_empty())
}

/// The process environment wins, which is how scripts sweep clock speeds.
fn bake(key: &str, value: &str) {
    println!("cargo:rerun-if-env-changed=PICOMINER_OVERRIDE_{key}");
    let overridden = env::var(format!("PICOMINER_OVERRIDE_{key}")).ok();
    let value = overridden.as_deref().unwrap_or(value);
    println!("cargo:rustc-env=PICOMINER_{key}={value}");
}

fn main() {
    let rp2350 = env::var("TARGET").unwrap().starts_with("thumbv8m");
    println!("cargo::rustc-check-cfg=cfg(rp2350)");
    if rp2350 {
        println!("cargo:rustc-cfg=rp2350");
    }

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let memory: &[u8] = if rp2350 { include_bytes!("memory-rp2350.x") } else { include_bytes!("memory.x") };
    fs::write(out.join("memory.x"), memory).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=memory-rp2350.x");
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    if !rp2350 {
        println!("cargo:rustc-link-arg-bins=-Tlink-rp.x");
    }

    println!("cargo:rerun-if-changed=../.env");
    let file = fs::read_to_string("../.env").unwrap_or_default();
    for (key, default) in SETTINGS {
        bake(key, lookup(&file, key).unwrap_or(default));
    }
    for (key, rp2040, rp2350_default) in CHIP_SETTINGS {
        let value = if rp2350 {
            lookup(&file, &format!("{key}_RP2350")).unwrap_or(rp2350_default)
        } else {
            lookup(&file, key).unwrap_or(rp2040)
        };
        bake(key, value);
    }
}
