//! Settings live in the last flash sector, which `memory.x` keeps out of the
//! image. The portal does not write them itself: erasing ends by rerunning boot2,
//! which sets the flash clock back to half the system clock, far too fast once
//! overclocked. It leaves the record in RAM that survives a reset instead, and
//! the next boot writes it before raising any clock.

use core::mem::MaybeUninit;
use core::ptr::{addr_of, addr_of_mut};

use embassy_rp::flash::{Blocking, ERASE_SIZE, Flash};
use embassy_rp::pac;
use embassy_rp::peripherals::FLASH;
use picominer_core::settings::{RECORD_SIZE, Settings};

use crate::config;

const FLASH_SIZE: usize = if cfg!(rp2350) { 4 } else { 2 } * 1024 * 1024;
const OFFSET: u32 = (FLASH_SIZE - ERASE_SIZE) as u32;
const XIP_BASE: usize = 0x1000_0000;
const MAGIC: u32 = 0x5049_4353;

#[repr(C)]
struct Pending {
    magic: u32,
    record: [u8; RECORD_SIZE],
}

#[unsafe(link_section = ".uninit.settings")]
static mut PENDING: MaybeUninit<Pending> = MaybeUninit::uninit();

fn pending() -> *mut Pending {
    addr_of_mut!(PENDING).cast()
}

/// The settings from `.env`, used until the portal saves some.
pub fn defaults() -> Settings {
    Settings {
        wifi_ssid: config::WIFI_SSID.try_into().unwrap_or_default(),
        wifi_password: config::WIFI_PASSWORD.try_into().unwrap_or_default(),
        btc_address: config::BTC_ADDRESS.try_into().unwrap_or_default(),
        pool_host: config::POOL_HOST.try_into().unwrap_or_default(),
        pool_port: config::POOL_PORT.parse().unwrap_or(0),
        worker: config::WORKER.try_into().unwrap_or_default(),
    }
}

/// The settings saved by the portal, if any.
pub fn stored() -> Option<Settings> {
    // SAFETY: memory-mapped flash outside the image, and no one writes it now.
    let record = unsafe { &*((XIP_BASE + OFFSET as usize) as *const [u8; RECORD_SIZE]) };
    Settings::decode(record)
}

/// Leaves `settings` for [`commit_staged`] to write on the next boot.
pub fn stage(settings: &Settings) {
    let pending = pending();
    // SAFETY: plain bytes, and only the portal's executor writes them.
    unsafe {
        settings.encode(&mut *addr_of_mut!((*pending).record));
        addr_of_mut!((*pending).magic).write_volatile(MAGIC);
    }
}

/// Writes the settings staged before the reset, if any. Call it before the
/// clocks go up and before core 1 starts.
pub fn commit_staged() {
    let pending = pending();
    // SAFETY: the record is only used once the magic proves it was written, and
    // nothing else uses the flash peripheral this early.
    unsafe {
        if addr_of!((*pending).magic).read_volatile() != MAGIC {
            return;
        }
        addr_of_mut!((*pending).magic).write_volatile(0);
        let record = &*addr_of!((*pending).record);
        if Settings::decode(record).is_none() {
            return;
        }
        // The driver waits on the DMA channels, which are still held in reset
        // before `embassy_rp::init`.
        pac::RESETS.reset().modify(|w| w.set_dma(false));
        while !pac::RESETS.reset_done().read().dma() {}
        let mut flash = Flash::<_, Blocking, FLASH_SIZE>::new_blocking(FLASH::steal());
        if flash.blocking_erase(OFFSET, OFFSET + ERASE_SIZE as u32).is_ok() {
            let _ = flash.blocking_write(OFFSET, record);
        }
    }
}
