//! SH1107 driver for the Waveshare Pico-OLED-1.3, over SPI.
//!
//! The panel is mounted so a landscape row of 128 pixels is one controller
//! column: a row is sent as 16 bytes, each byte a run of 8 pixels, leftmost in
//! bit 0. Rows map to columns in reverse, `column = 63 - y`.

use core::convert::Infallible;

use embassy_rp::gpio::Output;
use embassy_rp::peripherals::SPI1;
use embassy_rp::spi::{Blocking, Spi};
use embassy_time::Timer;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;

pub const WIDTH: u32 = 128;
pub const HEIGHT: u32 = 64;
const ROW_BYTES: usize = 16;

const INIT: &[u8] = &[
    0xAE, // display off
    0x00, 0x10, // column 0
    0xB0, // page 0
    0xDC, 0x00, // start line 0
    0x81, 0x6F, // contrast
    0x21, // vertical addressing
    0xA0, // segment remap
    0xC0, // COM scan direction
    0xA4, // follow RAM content
    0xA6, // normal polarity
    0xA8, 0x3F, // multiplex ratio 1/64
    0xD3, 0x60, // display offset
    0xD5, 0x41, // oscillator
    0xD9, 0x22, // pre-charge
    0xDB, 0x35, // VCOM deselect
    0xAD, 0x8A, // DC-DC
    0xAF, // display on
];

pub struct Display {
    spi: Spi<'static, SPI1, Blocking>,
    dc: Output<'static>,
    cs: Output<'static>,
    rst: Output<'static>,
    frame: [u8; ROW_BYTES * HEIGHT as usize],
}

impl Display {
    pub async fn new(
        spi: Spi<'static, SPI1, Blocking>,
        dc: Output<'static>,
        cs: Output<'static>,
        rst: Output<'static>,
    ) -> Self {
        let mut display = Self { spi, dc, cs, rst, frame: [0; ROW_BYTES * HEIGHT as usize] };
        display.rst.set_high();
        Timer::after_millis(1).await;
        display.rst.set_low();
        Timer::after_millis(10).await;
        display.rst.set_high();
        Timer::after_millis(10).await;
        display.commands(INIT);
        display
    }

    fn commands(&mut self, commands: &[u8]) {
        self.dc.set_low();
        self.cs.set_low();
        let _ = self.spi.blocking_write(commands);
        self.cs.set_high();
    }

    pub fn set_power(&mut self, on: bool) {
        self.commands(&[if on { 0xAF } else { 0xAE }]);
    }

    pub fn clear(&mut self) {
        self.frame.fill(0);
    }

    pub fn flush(&mut self) {
        self.commands(&[0xB0]);
        for y in 0..HEIGHT as usize {
            let column = 63 - y as u8;
            self.commands(&[column & 0x0F, 0x10 | (column >> 4)]);
            self.dc.set_high();
            self.cs.set_low();
            let row = y * ROW_BYTES;
            let _ = self.spi.blocking_write(&self.frame[row..row + ROW_BYTES]);
            self.cs.set_high();
        }
    }
}

impl OriginDimensions for Display {
    fn size(&self) -> Size {
        Size::new(WIDTH, HEIGHT)
    }
}

impl DrawTarget for Display {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Infallible>
    where
        I: IntoIterator<Item = Pixel<BinaryColor>>,
    {
        for Pixel(point, color) in pixels {
            let (x, y) = (point.x, point.y);
            if x < 0 || y < 0 || x >= WIDTH as i32 || y >= HEIGHT as i32 {
                continue;
            }
            let (x, y) = (x as usize, y as usize);
            let byte = &mut self.frame[y * ROW_BYTES + x / 8];
            if color.is_on() {
                *byte |= 1 << (x % 8);
            } else {
                *byte &= !(1 << (x % 8));
            }
        }
        Ok(())
    }
}
