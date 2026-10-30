//! Panics reboot the board, so a miner left alone recovers by itself. The message
//! survives the reset in uninitialized RAM and is logged on the next boot.

use core::fmt::Write;
use core::mem::MaybeUninit;
use core::panic::PanicInfo;
use core::ptr::{addr_of, addr_of_mut};

use embassy_rp::pac;

const MAGIC: u32 = 0x5049_434f;
const CAPACITY: usize = 248;

#[repr(C)]
struct Record {
    magic: u32,
    len: u32,
    text: [u8; CAPACITY],
}

#[unsafe(link_section = ".uninit.panic")]
static mut RECORD: MaybeUninit<Record> = MaybeUninit::uninit();

struct Writer {
    text: *mut u8,
    len: usize,
}

impl Write for Writer {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        for &byte in text.as_bytes() {
            if self.len < CAPACITY {
                // SAFETY: bounded by CAPACITY, and only the panicking core writes.
                unsafe { self.text.add(self.len).write_volatile(byte) };
                self.len += 1;
            }
        }
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    cortex_m::interrupt::disable();
    let record = addr_of_mut!(RECORD).cast::<Record>();
    // SAFETY: interrupts are off and the record is plain bytes.
    unsafe {
        let mut writer = Writer { text: addr_of_mut!((*record).text).cast(), len: 0 };
        let _ = write!(writer, "{info}");
        addr_of_mut!((*record).len).write_volatile(writer.len as u32);
        addr_of_mut!((*record).magic).write_volatile(MAGIC);
    }
    // Everything but the oscillators, as `embassy_rp::watchdog` configures it.
    pac::PSM.wdsel().write_value(pac::psm::regs::Wdsel(0x0001_ffff & !0b11));
    pac::WATCHDOG.ctrl().write(|w| w.set_trigger(true));
    loop {
        cortex_m::asm::nop();
    }
}

/// Logs the message left by the previous boot, if any.
pub fn log_previous() {
    let record = addr_of!(RECORD).cast::<Record>();
    // SAFETY: only read after the magic proves the panic handler wrote it, and
    // this runs once on core 0 before anything could panic again.
    unsafe {
        if addr_of!((*record).magic).read_volatile() != MAGIC {
            return;
        }
        addr_of_mut!(RECORD).cast::<Record>().cast::<u32>().write_volatile(0);
        let len = (addr_of!((*record).len).read_volatile() as usize).min(CAPACITY);
        let text = core::slice::from_raw_parts(addr_of!((*record).text).cast::<u8>(), len);
        log::error!("previous boot panicked: {}", core::str::from_utf8(text).unwrap_or("?"));
    }
}
