//! Keeps a bad boot from needing a hand on the board, which matters when trying
//! out clock speeds. A boot counts as failed unless it [`confirm`]s after running
//! for a while. The next boot then comes up in safe mode, at stock clocks with
//! only USB running, to report what happened, and retries after a while. If even
//! that fails, the board drops into the UF2 bootloader. A power cycle forgets all
//! of this.
//!
//! Once confirmed, a panic or a hang only costs a reboot, for unattended use.

use embassy_rp::pac::watchdog::regs::Load;
use embassy_rp::{pac, rom_data};

const MAGIC: u32 = 0x5049_4300;

/// The longest the counter allows, about 8 s.
const TIMEOUT: u32 = 0x00ff_ffff;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Safe,
}

/// Picks the mode for this boot and arms the watchdog. Call this first.
pub fn enter() -> Mode {
    let stored = pac::WATCHDOG.scratch2().read();
    let failed = if stored & 0xffff_ff00 == MAGIC { stored & 0xff } else { 0 };
    if failed >= 2 {
        enter_bootloader();
    }
    pac::WATCHDOG.scratch2().write_value(MAGIC | (failed + 1));

    select_reset_targets();
    pac::WATCHDOG.tick().write(|w| {
        w.set_enable(true);
        w.set_cycles(12);
    });
    feed();
    pac::WATCHDOG.ctrl().modify(|w| w.set_enable(true));
    if failed == 0 { Mode::Normal } else { Mode::Safe }
}

pub fn feed() {
    pac::WATCHDOG.load().write_value(Load(TIMEOUT));
}

/// Marks this boot as good.
pub fn confirm() {
    pac::WATCHDOG.scratch2().write_value(0);
    crate::panic::clear();
}

/// Reboots for a fresh normal boot.
pub fn retry() -> ! {
    confirm();
    reboot()
}

/// Reboots into the UF2 bootloader. The next firmware starts with a clean slate.
pub fn enter_bootloader() -> ! {
    pac::WATCHDOG.scratch2().write_value(0);
    rom_data::reset_to_usb_boot(0, 0);
    loop {
        cortex_m::asm::nop();
    }
}

/// Resets the whole chip but the oscillators, as `embassy_rp::watchdog` does.
fn select_reset_targets() {
    pac::PSM.wdsel().write_value(pac::psm::regs::Wdsel(0x0001_ffff & !0b11));
}

pub fn reboot() -> ! {
    select_reset_targets();
    pac::WATCHDOG.ctrl().write(|w| w.set_trigger(true));
    loop {
        cortex_m::asm::nop();
    }
}
