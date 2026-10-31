//! PicoMiner: a solo Bitcoin lottery miner for the Raspberry Pi Pico W and Pico 2 W
//! with a Waveshare Pico-OLED-1.3. See the README for the wider picture.

#![no_std]
#![no_main]

mod boot;
mod config;
mod display;
mod miner;
mod panic;
mod pool;
mod portal;
#[cfg_attr(rp2350, path = "scan_rp2350.rs")]
mod scan;
mod settings;
mod stats;
mod ui;

use core::future::Future;
use core::ptr::addr_of_mut;

use cortex_m_rt::entry;
use cyw43_pio::PioSpi;
use embassy_executor::Spawner;
use embassy_net::{DhcpConfig, StackResources};
use embassy_rp::Peri;
use embassy_rp::adc::{self, Adc};
use embassy_rp::clocks::{ClockConfig, PeriClkSrc, RoscRng};
use embassy_rp::config::Config;
use embassy_rp::executor::InterruptExecutor;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::interrupt;
use embassy_rp::interrupt::{InterruptExt, Priority};
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::peripherals::{
    ADC, ADC_TEMP_SENSOR, DMA_CH0, PIN_8, PIN_9, PIN_10, PIN_11, PIN_12, PIN_15, PIN_17, PIN_23, PIN_24, PIN_25, PIN_29, PIO0,
    SPI1, USB,
};
use embassy_rp::pio::Pio;
use embassy_rp::spi::{self, Spi};
use embassy_rp::{bind_interrupts, dma, pio, usb};
use embassy_time::Timer;
use embassy_usb_logger::ReceiverHandler;
use fixed::FixedU32;
use log::{LevelFilter, error, info, warn};
use picominer_core::settings::Settings;
use picominer_core::sha256::Work;
use static_cell::StaticCell;

use crate::display::Display;
use crate::stats::Status;

bind_interrupts!(struct Irqs {
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
    PIO0_IRQ_0 => pio::InterruptHandler<PIO0>;
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
});

static EXECUTOR: InterruptExecutor = InterruptExecutor::new();

#[cfg_attr(not(rp2350), unsafe(link_section = ".miner1.stack"))]
static mut CORE1_STACK: Stack<4096> = Stack::new();

#[interrupt]
unsafe fn SWI_IRQ_1() {
    // SAFETY: this is the interrupt the executor was started on.
    unsafe { EXECUTOR.on_interrupt() }
}

/// Serial commands; `bootsel` lets `scripts/flash.ps1` reflash without the button.
struct Commands;

impl ReceiverHandler for Commands {
    fn handle_data(&self, data: &[u8]) -> impl Future<Output = ()> + Send {
        if data.starts_with(b"bootsel") {
            boot::enter_bootloader();
        }
        async {}
    }

    fn new() -> Self {
        Self
    }
}

#[embassy_executor::task]
async fn usb_logger(driver: usb::Driver<'static, USB>) {
    embassy_usb_logger::run!(1024, LevelFilter::Info, driver, Commands);
}

type WifiSpi = PioSpi<'static, PIO0, 0>;
type WifiBus = cyw43::SpiBus<Output<'static>, WifiSpi>;

#[embassy_executor::task]
async fn wifi_runner(runner: cyw43::Runner<'static, WifiBus>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_runner(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

/// Hashes a made-up header, for measuring speed without a pool.
fn publish_benchmark() {
    stats::set_status(Status::Benchmark);
    miner::set_difficulty(config::LOCAL_MIN_DIFFICULTY);
    miner::publish(Work::from_header(&[0x42; 80]));
}

/// Peripherals, split from the rest so `startup` can build everything on the
/// executor, where tasks and their arguments do not have to be `Send`.
struct Board {
    usb: Peri<'static, USB>,
    spi1: Peri<'static, SPI1>,
    oled_clk: Peri<'static, PIN_10>,
    oled_mosi: Peri<'static, PIN_11>,
    oled_dc: Peri<'static, PIN_8>,
    oled_cs: Peri<'static, PIN_9>,
    oled_rst: Peri<'static, PIN_12>,
    key0: Peri<'static, PIN_15>,
    key1: Peri<'static, PIN_17>,
    settings: &'static Settings,
    screen: bool,
    /// Runs the setup portal instead of mining.
    portal: bool,
    self_test: bool,
    wifi_pwr: Peri<'static, PIN_23>,
    wifi_cs: Peri<'static, PIN_25>,
    wifi_dio: Peri<'static, PIN_24>,
    wifi_clk: Peri<'static, PIN_29>,
    pio0: Peri<'static, PIO0>,
    dma: Peri<'static, DMA_CH0>,
    adc: Peri<'static, ADC>,
    temperature: Peri<'static, ADC_TEMP_SENSOR>,
}

async fn start_network(spawner: Spawner, board: Wifi, settings: &'static Settings, portal: bool) {
    static WIFI_STATE: StaticCell<cyw43::State> = StaticCell::new();
    static NET_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
    static FIRMWARE: &cyw43::Aligned<cyw43::A4, [u8]> = &cyw43::Aligned(*cyw43_firmware::CYW43_43439A0);
    static CLM: &cyw43::Aligned<cyw43::A4, [u8]> = &cyw43::Aligned(*cyw43_firmware::CYW43_43439A0_CLM);
    let nvram = cyw43::aligned_bytes!("../cyw43/nvram_rp2040.bin");

    let pwr = Output::new(board.pwr, Level::Low);
    let cs = Output::new(board.cs, Level::High);
    let mut pio = Pio::new(board.pio0, Irqs);
    let dma = dma::Channel::new(board.dma, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        FixedU32::from_bits(config::WIFI_DIVIDER << 8),
        pio.irq0,
        cs,
        board.dio,
        board.clk,
        dma,
    );

    let state = WIFI_STATE.init(cyw43::State::new());
    let (device, mut control, runner) = cyw43::new(state, pwr, spi, FIRMWARE, nvram).await;
    spawner.spawn(wifi_runner(runner).unwrap());
    control.init(CLM).await;
    control.set_power_management(cyw43::PowerManagementMode::None).await;
    control.gpio_set(0, false).await;
    info!("CYW43 ready, MAC {:02x?}", control.address().await);

    let seed = RoscRng.next_u64();
    let resources = NET_RESOURCES.init(StackResources::new());
    if portal {
        static NETWORKS: StaticCell<portal::Networks> = StaticCell::new();
        let networks = NETWORKS.init(portal::scan(&mut control).await);
        info!("{} Wi-Fi networks in range", networks.len());
        portal::start_access_point(&mut control).await;
        let (stack, runner) = embassy_net::new(device, portal::net_config(), resources, seed);
        spawner.spawn(net_runner(runner).unwrap());
        portal::spawn(spawner, stack, control, settings, networks);
        return;
    }
    if !settings.is_complete() {
        warn!("no Wi-Fi network or BTC address, staying in benchmark mode");
        return;
    }

    let mut dhcp = DhcpConfig::default();
    dhcp.hostname = "picominer".try_into().ok();
    // A second DHCP server on the network refuses the leases of the first, and
    // honouring that drops the address, and the pool, right after connecting.
    dhcp.ignore_naks = true;
    let (stack, runner) = embassy_net::new(device, embassy_net::Config::dhcpv4(dhcp), resources, seed);
    spawner.spawn(net_runner(runner).unwrap());
    spawner.spawn(pool::run(stack, control, settings).unwrap());
}

struct Wifi {
    pwr: Peri<'static, PIN_23>,
    cs: Peri<'static, PIN_25>,
    dio: Peri<'static, PIN_24>,
    clk: Peri<'static, PIN_29>,
    pio0: Peri<'static, PIO0>,
    dma: Peri<'static, DMA_CH0>,
}

/// Feeds the watchdog, and calls the boot good once it has survived for a while.
#[embassy_executor::task]
async fn watchdog_feeder() -> ! {
    for _ in 0..15 {
        boot::feed();
        Timer::after_secs(1).await;
    }
    boot::confirm();
    loop {
        boot::feed();
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
async fn safe_mode() -> ! {
    let mut seconds = 0u32;
    loop {
        boot::feed();
        if seconds % 3 == 2 {
            warn!("safe mode: the last boot failed, power cycle or send bootsel");
            panic::log_previous();
        }
        seconds += 1;
        if seconds == 600 {
            // Left alone, a miner should get back to work rather than wait here.
            boot::retry();
        }
        Timer::after_secs(1).await;
    }
}

#[embassy_executor::task]
async fn startup(board: Board) {
    // SAFETY: called from a task of this same executor.
    let spawner = unsafe { Spawner::for_current_executor() }.await;
    spawner.spawn(watchdog_feeder().unwrap());
    spawner.spawn(usb_logger(usb::Driver::new(board.usb, Irqs)).unwrap());

    let display = if board.screen {
        let mut config = spi::Config::default();
        config.frequency = 10_000_000;
        let bus = Spi::new_blocking_txonly(board.spi1, board.oled_clk, board.oled_mosi, config);
        let dc = Output::new(board.oled_dc, Level::Low);
        let cs = Output::new(board.oled_cs, Level::High);
        let rst = Output::new(board.oled_rst, Level::High);
        Some(Display::new(bus, dc, cs, rst).await)
    } else {
        None
    };
    let key0 = Input::new(board.key0, Pull::Up);
    let key1 = Input::new(board.key1, Pull::Up);
    let adc = Adc::new_blocking(board.adc, adc::Config::default());
    let sensor = adc::Channel::new_temp_sensor(board.temperature);
    let (settings, portal) = (board.settings, board.portal);
    spawner.spawn(ui::run(display, key0, key1, adc, sensor, settings, portal).unwrap());

    if portal {
        stats::set_status(Status::Setup);
    } else if !settings.is_complete() || cfg!(feature = "stress") {
        publish_benchmark();
    }

    // Leaves the host time to open the serial port before the first logs.
    Timer::after_secs(2).await;
    info!("{} on {} at {} kHz", config::AGENT, config::CHIP, embassy_rp::clocks::clk_sys_freq() / 1000);
    panic::log_previous();
    info!("settings from {}", if settings::stored().is_some() { "flash" } else { ".env" });
    info!("screen {}", if board.screen { "found" } else { "absent" });
    if !board.self_test {
        error!("hashing self-test failed");
    }

    let wifi = Wifi {
        pwr: board.wifi_pwr,
        cs: board.wifi_cs,
        dio: board.wifi_dio,
        clk: board.wifi_clk,
        pio0: board.pio0,
        dma: board.dma,
    };
    start_network(spawner, wifi, settings, portal).await;
}

/// Sets the flash clock divider. Runs from RAM because XIP is off while it does.
#[cfg(not(rp2350))]
#[unsafe(link_section = ".data.ram_func")]
#[inline(never)]
fn set_flash_divider(divider: u32) {
    const SSI: *mut u32 = 0x1800_0000 as *mut u32;
    const SSIENR: usize = 0x08 / 4;
    const BAUDR: usize = 0x14 / 4;
    const SR: usize = 0x28 / 4;
    // SAFETY: these are the XIP SSI registers, and nothing else is running yet.
    unsafe {
        while SSI.add(SR).read_volatile() & 1 != 0 {}
        SSI.add(SSIENR).write_volatile(0);
        SSI.add(BAUDR).write_volatile(divider);
        SSI.add(SSIENR).write_volatile(1);
    }
}

/// Sets the core voltage. `embassy_rp::init` writes it too, but it never unlocks the
/// regulator, so on the RP2350 the value reads back fine and changes nothing.
#[cfg(rp2350)]
fn set_core_voltage() {
    use embassy_rp::pac::POWMAN;
    const PASSWORD: u32 = 0x5afe << 16;
    /// More than the 2 ms the regulator takes, at the boot clock or below.
    const SETTLE: u32 = 300_000;

    POWMAN.vreg_ctrl().modify(|w| {
        w.0 = (w.0 & 0xffff) | PASSWORD;
        w.set_unlock(true);
    });
    while POWMAN.vreg().read().update_in_progress() {}
    POWMAN.vreg().modify(|w| {
        w.0 = (w.0 & 0xffff) | PASSWORD;
        w.set_vsel(config::CORE_VOLTAGE as u8);
    });
    while POWMAN.vreg().read().update_in_progress() {}
    cortex_m::asm::delay(SETTLE);
}

/// Sets the flash clock divider, scaling the read delay and the deselect time the
/// boot ROM chose for a divider of 3, as both count system clock cycles. Runs from
/// RAM because code cannot be fetched while the timing changes.
#[cfg(rp2350)]
#[unsafe(link_section = ".data.ram_func")]
#[inline(never)]
fn set_flash_divider(divider: u32) {
    use embassy_rp::pac;
    const UNCACHED_FLASH: *const u32 = 0x1400_0000 as *const u32;
    // After the two USB pads, entry 3 is the chip select, which idles high.
    while !pac::IO_QSPI.gpio(3).status().read().outtopad() {}
    pac::QMI.mem(0).timing().modify(|w| {
        w.set_clkdiv(divider as u8);
        w.set_rxdelay((divider * 2 / 3).min(7) as u8);
        w.set_min_deselect((divider * 7 / 3).min(31) as u8);
    });
    // SAFETY: a plain read of flash. It makes the new timing take effect.
    unsafe { UNCACHED_FLASH.read_volatile() };
}

#[entry]
fn main() -> ! {
    let mode = boot::enter();
    // Before the clocks go up, see the module.
    settings::commit_staged();
    if mode == boot::Mode::Safe {
        let p = embassy_rp::init(Config::default());
        interrupt::SWI_IRQ_1.set_priority(Priority::P3);
        let spawner = EXECUTOR.start(interrupt::SWI_IRQ_1);
        spawner.spawn(safe_mode().unwrap());
        spawner.spawn(usb_logger(usb::Driver::new(p.USB, Irqs)).unwrap());
        loop {
            cortex_m::asm::wfi();
        }
    }

    #[cfg(rp2350)]
    set_core_voltage();
    if config::FLASH_DIVIDER > config::STOCK_FLASH_DIVIDER {
        set_flash_divider(config::FLASH_DIVIDER);
    }
    let mut clocks = ClockConfig::system_freq(config::SYS_HZ).unwrap();
    clocks.core_voltage = config::CORE_VOLTAGE;
    // Nothing here needs a fast peripheral clock, and it has no overclock margin.
    clocks.peri_clk_src = Some(PeriClkSrc::PllUsb);
    let p = embassy_rp::init(Config::new(clocks));

    static SETTINGS: StaticCell<Settings> = StaticCell::new();
    let settings = SETTINGS.init(settings::stored().unwrap_or_else(settings::defaults));
    let mut key0 = p.PIN_15;
    let key0_held = {
        let key = Input::new(key0.reborrow(), Pull::Up);
        // Lets the pull-up charge the line.
        cortex_m::asm::delay(config::SYS_HZ / 10_000);
        key.is_low()
    };
    let (mut oled_clk, mut oled_mosi) = (p.PIN_10, p.PIN_11);
    let screen = display::detect(oled_clk.reborrow(), oled_mosi.reborrow());
    // The portal needs the screen to say how to reach it. Without one the board
    // is expected to carry its settings from `.env`.
    let portal = !cfg!(feature = "stress") && screen && (key0_held || !settings.is_complete());

    scan::load();
    let self_test = scan::self_test();
    if !portal {
        // SAFETY: this is the only reference ever made to the stack.
        let core1_stack = unsafe { &mut *addr_of_mut!(CORE1_STACK) };
        spawn_core1(p.CORE1, core1_stack, || miner::mine(1));
    }

    interrupt::SWI_IRQ_1.set_priority(Priority::P3);
    let spawner = EXECUTOR.start(interrupt::SWI_IRQ_1);
    let board = Board {
        usb: p.USB,
        spi1: p.SPI1,
        oled_clk,
        oled_mosi,
        oled_dc: p.PIN_8,
        oled_cs: p.PIN_9,
        oled_rst: p.PIN_12,
        key0,
        key1: p.PIN_17,
        settings,
        screen,
        portal,
        self_test,
        wifi_pwr: p.PIN_23,
        wifi_cs: p.PIN_25,
        wifi_dio: p.PIN_24,
        wifi_clk: p.PIN_29,
        pio0: p.PIO0,
        dma: p.DMA_CH0,
        adc: p.ADC,
        temperature: p.ADC_TEMP_SENSOR,
    };
    spawner.spawn(startup(board).unwrap());

    miner::mine(0)
}
