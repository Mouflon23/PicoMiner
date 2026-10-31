//! Hardware-independent half of PicoMiner, tested on the host with `cargo test`.

#![cfg_attr(not(test), no_std)]

pub mod dhcp;
pub mod format;
pub mod hex;
pub mod http;
pub mod json;
pub mod settings;
pub mod sha256;
pub mod stratum;
pub mod target;
