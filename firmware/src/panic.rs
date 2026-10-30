//! Panics and hard faults reboot the board. The message survives the reset in
//! uninitialized RAM and is logged on the next boot.

use core::fmt::{self, Write};
use core::mem::MaybeUninit;
use core::panic::PanicInfo;
use core::ptr::{addr_of, addr_of_mut};

use cortex_m_rt::{ExceptionFrame, exception};
use embassy_rp::multicore::{CoreId, current_core};

use crate::boot;

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

fn record() -> *mut Record {
    addr_of_mut!(RECORD).cast()
}

struct Writer {
    text: *mut u8,
    len: usize,
}

impl Write for Writer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for &byte in text.as_bytes() {
            if self.len < CAPACITY {
                // SAFETY: bounded by CAPACITY, and only the failing core writes.
                unsafe { self.text.add(self.len).write_volatile(byte) };
                self.len += 1;
            }
        }
        Ok(())
    }
}

fn store_and_reboot(message: fmt::Arguments<'_>) -> ! {
    cortex_m::interrupt::disable();
    let record = record();
    // SAFETY: interrupts are off and the record is plain bytes.
    unsafe {
        let mut writer = Writer { text: addr_of_mut!((*record).text).cast(), len: 0 };
        let _ = writer.write_fmt(message);
        addr_of_mut!((*record).len).write_volatile(writer.len as u32);
        addr_of_mut!((*record).magic).write_volatile(MAGIC);
    }
    boot::reboot()
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    store_and_reboot(format_args!("{info}"))
}

#[exception]
unsafe fn HardFault(frame: &ExceptionFrame) -> ! {
    let core = if current_core() == CoreId::Core0 { 0 } else { 1 };
    store_and_reboot(format_args!("HardFault on core {core} at {:#010x}, lr {:#010x}", frame.pc(), frame.lr()))
}

/// Logs the message left by a previous boot, if any.
pub fn log_previous() {
    let record = record();
    // SAFETY: the text is only read once the magic proves it was written, and
    // the length is bounded before use.
    unsafe {
        if addr_of!((*record).magic).read_volatile() != MAGIC {
            return;
        }
        let len = (addr_of!((*record).len).read_volatile() as usize).min(CAPACITY);
        let text = core::slice::from_raw_parts(addr_of!((*record).text).cast::<u8>(), len);
        log::error!("previous boot: {}", core::str::from_utf8(text).unwrap_or("?"));
    }
}

/// Forgets the stored message.
pub fn clear() {
    // SAFETY: a volatile write of a plain word.
    unsafe { addr_of_mut!((*record()).magic).write_volatile(0) };
}
