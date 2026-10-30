//! Hardware-independent half of PicoMiner, tested on the host with `cargo test`.

#![cfg_attr(not(test), no_std)]

pub mod format;
pub mod hex;
pub mod json;
pub mod sha256;
pub mod stratum;
pub mod target;
