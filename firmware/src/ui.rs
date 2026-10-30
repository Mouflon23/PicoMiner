//! Screen pages and the task that refreshes them. KEY0 cycles the pages and KEY1
//! turns the panel off, which matters on an OLED left running for days.

use core::fmt::Write;

use embassy_rp::gpio::Input;
use embassy_time::{Duration, Instant, Ticker};
use embedded_graphics::mono_font::MonoFont;
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::{FONT_6X10, FONT_10X20};
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Line, PrimitiveStyle};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use heapless::String;
use log::info;
use picominer_core::format::{Si, Uptime};

use crate::config;
use crate::display::{Display, WIDTH};
use crate::miner;
use crate::stats::{self, Stats};

const PAGES: u8 = 3;

fn left(display: &mut Display, x: i32, y: i32, text: &str) {
    let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let _ = Text::with_baseline(text, Point::new(x, y), style, Baseline::Top).draw(display);
}

fn aligned(display: &mut Display, x: i32, y: i32, text: &str, font: &MonoFont<'_>, alignment: Alignment) {
    let style = MonoTextStyle::new(font, BinaryColor::On);
    let layout = TextStyleBuilder::new().alignment(alignment).baseline(Baseline::Top).build();
    let _ = Text::with_text_style(text, Point::new(x, y), style, layout).draw(display);
}

fn title(display: &mut Display, right: &str) {
    left(display, 0, 0, "PicoMiner");
    aligned(display, WIDTH as i32 - 1, 0, right, &FONT_6X10, Alignment::Right);
    let _ = Line::new(Point::new(0, 11), Point::new(WIDTH as i32 - 1, 11))
        .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, 1))
        .draw(display);
}

fn miner_page(display: &mut Display, stats: &Stats, hashrate: f32) {
    title(display, stats.status.label());
    let mut text: String<24> = String::new();
    let _ = write!(text, "{:.1} kH/s", hashrate / 1000.0);
    aligned(display, WIDTH as i32 / 2, 15, &text, &FONT_10X20, Alignment::Center);

    text.clear();
    let _ = write!(text, "Best {}", Si(stats.best));
    left(display, 0, 41, &text);
    text.clear();
    let _ = write!(text, "Shares {}", stats.accepted);
    left(display, 0, 53, &text);
    if stats.blocks > 0 {
        aligned(display, WIDTH as i32 - 1, 53, "BLOCK!", &FONT_6X10, Alignment::Right);
    } else if stats.rejected > 0 {
        text.clear();
        let _ = write!(text, "Rej {}", stats.rejected);
        aligned(display, WIDTH as i32 - 1, 53, &text, &FONT_6X10, Alignment::Right);
    }
}

fn pool_page(display: &mut Display, stats: &Stats, uptime: u32) {
    title(display, "Pool");
    let mut text: String<24> = String::new();
    let _ = write!(text, "{}", config::POOL_HOST);
    left(display, 0, 14, &text);
    text.clear();
    let _ = write!(text, "Diff   {}", Si(stats.pool_difficulty));
    left(display, 0, 26, &text);
    text.clear();
    let _ = write!(text, "Block  {}", stats.height);
    left(display, 0, 38, &text);
    text.clear();
    let _ = write!(text, "Uptime {}", Uptime(uptime));
    left(display, 0, 50, &text);
}

fn network_page(display: &mut Display, stats: &Stats, total: u64) {
    title(display, "Wi-Fi");
    let mut text: String<24> = String::new();
    let _ = write!(text, "{}", config::WIFI_SSID);
    left(display, 0, 14, &text);
    text.clear();
    let [a, b, c, d] = stats.ip;
    let _ = write!(text, "{a}.{b}.{c}.{d}");
    left(display, 0, 26, &text);
    text.clear();
    let _ = write!(text, "Signal {} dBm", stats.rssi);
    left(display, 0, 38, &text);
    text.clear();
    let _ = write!(text, "Hashes {}", Si(total as f64));
    left(display, 0, 50, &text);
}

#[embassy_executor::task]
pub async fn run(mut display: Display, key0: Input<'static>, key1: Input<'static>) -> ! {
    let mut page = 0;
    let mut powered = true;
    let mut held = (false, false);

    let started = Instant::now();
    let mut last_time = started;
    let mut last_hashes = miner::hashes();
    let mut total = 0u64;
    let mut hashrate = 0f32;
    let mut ticks = 0u32;

    let mut ticker = Ticker::every(Duration::from_millis(50));
    loop {
        ticker.next().await;

        let pressed = (key0.is_low(), key1.is_low());
        let mut redraw = false;
        if pressed.0 && !held.0 {
            page = (page + 1) % PAGES;
            redraw = true;
            if !powered {
                powered = true;
                display.set_power(true);
            }
        }
        if pressed.1 && !held.1 {
            powered = !powered;
            display.set_power(powered);
        }
        held = pressed;

        ticks += 1;
        if ticks.is_multiple_of(20) {
            let now = Instant::now();
            let hashes = miner::hashes();
            let delta = hashes[0].wrapping_sub(last_hashes[0]) as u64
                + hashes[1].wrapping_sub(last_hashes[1]) as u64;
            total += delta;
            let micros = (now - last_time).as_micros().max(1);
            let instant = delta as f32 * 1_000_000.0 / micros as f32;
            hashrate = if hashrate == 0.0 { instant } else { hashrate * 0.8 + instant * 0.2 };
            last_time = now;
            last_hashes = hashes;
            redraw = true;
            if ticks.is_multiple_of(200) {
                let stats = stats::get();
                info!(
                    "{:.0} H/s, core0 {} core1 {}, best {:.6}, accepted {}, rejected {}, {}",
                    hashrate,
                    hashes[0],
                    hashes[1],
                    stats.best,
                    stats.accepted,
                    stats.rejected,
                    stats.status.label(),
                );
            }
        }

        if redraw && powered {
            let stats = stats::get();
            display.clear();
            match page {
                0 => miner_page(&mut display, &stats, hashrate),
                1 => pool_page(&mut display, &stats, started.elapsed().as_secs() as u32),
                _ => network_page(&mut display, &stats, total),
            }
            display.flush();
        }
    }
}
