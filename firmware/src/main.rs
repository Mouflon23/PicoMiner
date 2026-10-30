//! PicoMiner: a solo Bitcoin lottery miner for the Raspberry Pi Pico W with a
//! Waveshare Pico-OLED-1.3. See the README for the wider picture.

#![no_std]
#![no_main]

mod config;
mod display;
mod miner;
mod panic;
mod pool;
mod stats;
mod ui;

use core::future::Future;

use cortex_m_rt::entry;
use cyw43_pio::{PioSpi, RM2_CLOCK_DIVIDER};
use embassy_executor::Spawner;
use embassy_net::{DhcpConfig, StackResources};
use embassy_rp::Peri;
use embassy_rp::clocks::{ClockConfig, RoscRng};
use embassy_rp::config::Config;
use embassy_rp::executor::InterruptExecutor;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::interrupt;
use embassy_rp::interrupt::{InterruptExt, Priority};
use embassy_rp::multicore::{Stack, spawn_core1};
use embassy_rp::peripherals::{
    DMA_CH0, PIN_8, PIN_9, PIN_10, PIN_11, PIN_12, PIN_15, PIN_17, PIN_23, PIN_24, PIN_25, PIN_29, PIO0,
    SPI1, USB,
};
use embassy_rp::pio::Pio;
use embassy_rp::spi::{self, Spi};
use embassy_rp::{bind_interrupts, dma, pio, rom_data, usb};
use embassy_time::Timer;
use embassy_usb_logger::ReceiverHandler;
use log::{LevelFilter, info, warn};
use picominer_core::sha256::Work;
use static_cell::{ConstStaticCell, StaticCell};

use crate::display::Display;
use crate::stats::Status;

bind_interrupts!(struct Irqs {
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>;
    PIO0_IRQ_0 => pio::InterruptHandler<PIO0>;
    USBCTRL_IRQ => usb::InterruptHandler<USB>;
});

static EXECUTOR: InterruptExecutor = InterruptExecutor::new();
static CORE1_STACK: ConstStaticCell<Stack<4096>> = ConstStaticCell::new(Stack::new());

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
            rom_data::reset_to_usb_boot(0, 0);
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
    wifi_pwr: Peri<'static, PIN_23>,
    wifi_cs: Peri<'static, PIN_25>,
    wifi_dio: Peri<'static, PIN_24>,
    wifi_clk: Peri<'static, PIN_29>,
    pio0: Peri<'static, PIO0>,
    dma: Peri<'static, DMA_CH0>,
}

async fn start_network(spawner: Spawner, board: Wifi) {
    static WIFI_STATE: StaticCell<cyw43::State> = StaticCell::new();
    static NET_RESOURCES: StaticCell<StackResources<4>> = StaticCell::new();
    static FIRMWARE: &cyw43::Aligned<cyw43::A4, [u8]> = &cyw43::Aligned(*cyw43_firmware::CYW43_43439A0);
    static CLM: &cyw43::Aligned<cyw43::A4, [u8]> = &cyw43::Aligned(*cyw43_firmware::CYW43_43439A0_CLM);
    let nvram = cyw43::aligned_bytes!("../cyw43/nvram_rp2040.bin");

    let pwr = Output::new(board.pwr, Level::Low);
    let cs = Output::new(board.cs, Level::High);
    let mut pio = Pio::new(board.pio0, Irqs);
    let dma = dma::Channel::new(board.dma, Irqs);
    // The default divider would clock the chip at its 50 MHz limit from 200 MHz.
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        RM2_CLOCK_DIVIDER,
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

    if !config::network_configured() {
        warn!("no Wi-Fi credentials or BTC address in .env, staying in benchmark mode");
        return;
    }

    let mut dhcp = DhcpConfig::default();
    dhcp.hostname = "picominer".try_into().ok();
    let seed = RoscRng.next_u64();
    let resources = NET_RESOURCES.init(StackResources::new());
    let (stack, runner) = embassy_net::new(device, embassy_net::Config::dhcpv4(dhcp), resources, seed);
    spawner.spawn(net_runner(runner).unwrap());
    spawner.spawn(pool::run(stack, control).unwrap());
}

struct Wifi {
    pwr: Peri<'static, PIN_23>,
    cs: Peri<'static, PIN_25>,
    dio: Peri<'static, PIN_24>,
    clk: Peri<'static, PIN_29>,
    pio0: Peri<'static, PIO0>,
    dma: Peri<'static, DMA_CH0>,
}

#[embassy_executor::task]
async fn startup(board: Board) {
    // SAFETY: called from a task of this same executor.
    let spawner = unsafe { Spawner::for_current_executor() }.await;
    spawner.spawn(usb_logger(usb::Driver::new(board.usb, Irqs)).unwrap());

    let mut config = spi::Config::default();
    config.frequency = 10_000_000;
    let bus = Spi::new_blocking_txonly(board.spi1, board.oled_clk, board.oled_mosi, config);
    let display = Display::new(
        bus,
        Output::new(board.oled_dc, Level::Low),
        Output::new(board.oled_cs, Level::High),
        Output::new(board.oled_rst, Level::High),
    )
    .await;
    let key0 = Input::new(board.key0, Pull::Up);
    let key1 = Input::new(board.key1, Pull::Up);
    spawner.spawn(ui::run(display, key0, key1).unwrap());

    if !config::network_configured() {
        publish_benchmark();
    }

    // Leaves the host time to open the serial port before the first logs.
    Timer::after_secs(2).await;
    info!("{} on {} kHz", config::AGENT, embassy_rp::clocks::clk_sys_freq() / 1000);
    panic::log_previous();

    let wifi = Wifi {
        pwr: board.wifi_pwr,
        cs: board.wifi_cs,
        dio: board.wifi_dio,
        clk: board.wifi_clk,
        pio0: board.pio0,
        dma: board.dma,
    };
    start_network(spawner, wifi).await;
}

#[entry]
fn main() -> ! {
    let clocks = ClockConfig::system_freq(config::SYS_HZ).unwrap();
    let p = embassy_rp::init(Config::new(clocks));

    spawn_core1(p.CORE1, CORE1_STACK.take(), || miner::mine(1));

    interrupt::SWI_IRQ_1.set_priority(Priority::P3);
    let spawner = EXECUTOR.start(interrupt::SWI_IRQ_1);
    let board = Board {
        usb: p.USB,
        spi1: p.SPI1,
        oled_clk: p.PIN_10,
        oled_mosi: p.PIN_11,
        oled_dc: p.PIN_8,
        oled_cs: p.PIN_9,
        oled_rst: p.PIN_12,
        key0: p.PIN_15,
        key1: p.PIN_17,
        wifi_pwr: p.PIN_23,
        wifi_cs: p.PIN_25,
        wifi_dio: p.PIN_24,
        wifi_clk: p.PIN_29,
        pio0: p.PIO0,
        dma: p.DMA_CH0,
    };
    spawner.spawn(startup(board).unwrap());

    miner::mine(0)
}
