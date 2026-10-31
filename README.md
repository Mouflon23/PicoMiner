# PicoMiner

A solo Bitcoin lottery miner in Rust for the **Raspberry Pi Pico W** and **Pico 2 W**,
with an optional **Waveshare Pico-OLED-1.3** screen. It is a from-scratch take on the
idea behind [NerdMiner v2](https://github.com/BitMaker-hub/NerdMiner_v2), which runs
on the ESP32.

One `.uf2` file works on both boards: each one picks its own firmware by itself.

Measured on real hardware, across both cores:

| Board | Clock | Core voltage | Hashrate |
| --- | --- | --- | --- |
| Pico W | 200 MHz, the highest the datasheet covers | 1.15 V | 59 kH/s |
| Pico W | 432 MHz, overclocked, on one tested board | 1.30 V | 128 kH/s |
| Pico 2 W | 150 MHz, stock | 1.10 V | 356 kH/s |
| Pico 2 W | 412 MHz, overclocked, on one tested board | 1.30 V | **976 kH/s** |

## The odds

This is a toy. At the current network difficulty (about 148 T) a block takes
6.4 × 10²³ hashes on average, so one PicoMiner at 128 kH/s needs around 160
billion years, some 11 times the age of the universe. The fun is watching it try. It earns
nothing unless that miracle happens, in which case the pool pays the block reward
to your address.

## Hardware

The OLED module plugs straight onto the headers of either board. It is optional
and detected at boot: the panel never answers over SPI, but the module pulls its
clock and data lines up, and an unconnected pin does not spring back. Without it
you follow the miner on the USB log, the LED and the pool's page.

| Function | GPIO |
| --- | --- |
| OLED DC / CS / CLK / DIN / RST | 8 / 9 / 10 / 11 / 12 |
| KEY0: next page, wakes the screen; held at power-up, opens the setup portal | 15 |
| KEY1: screen on or off | 17 |

## Getting started

You need Rust and Python, then:

```bash
rustup target add thumbv6m-none-eabi thumbv8m.main-none-eabihf
```

The Wi-Fi network, the BTC address and the pool are set from a phone, in the
setup portal described below. To skip it, copy `.env.example` to `.env` and fill
them in there: they become the defaults baked into the firmware. `.env` is ignored
by git, but then do not share your `.uf2`.

Then flash. The first time, hold BOOTSEL while plugging the Pico in. Afterwards
the script reboots a running PicoMiner over USB by itself.

```powershell
./scripts/flash.ps1
./scripts/monitor.ps1
```

The script builds both firmwares and packs them into `target/picominer.uf2`, which
you can also drop by hand on the drive of either board. With several boards
plugged in, the scripts ask which one with `-Port COM7` instead of guessing.

The screen has three pages: hashrate and shares, pool details, and Wi-Fi with the
chip temperature. The onboard LED flashes on each accepted share.

## Setup portal

As with NerdMiner v2, a PicoMiner with nothing configured opens its own Wi-Fi
network instead of mining. So does any PicoMiner when you hold **KEY0** while
plugging it in. The screen shows the details and the LED blinks slowly.

The portal needs the screen, which says how to reach it. A board without one never
opens it: give it its settings through `.env`, or it stays in benchmark mode.

1. Join the network `PicoMinerAP`, password `MineYourCoins`. Your phone may warn
   that it has no internet; stay connected.
2. Open http://192.168.4.1.
3. Pick your Wi-Fi network, then enter its password, your BTC address and the
   pool. Leaving the password empty keeps the saved one.
4. Press **Save and restart**. PicoMiner restarts and starts mining.

The settings are kept in the last 4 KB of flash, survive reflashing and replace
those from `.env`. They are written right after the restart, before the clocks go
up, because writing flash while overclocked would crash the board.

## Overclocking

The firmware runs at 200 MHz on the Pico W and 150 MHz on the Pico 2 W by default.
Above that you leave the datasheet, and every chip has its own limit. `SYS_MHZ` and
`CORE_MV` in `.env` set the clock and voltage of the Pico W, `SYS_MHZ_RP2350` and
`CORE_MV_RP2350` those of the Pico 2 W, so a limit found on one chip never reaches
the other. This script tries several pairs on one board:

```powershell
./scripts/sweep.ps1 -Steps 300:1250,400:1300,432:1300 -Seconds 60 -Port COM7
```

Each step runs the `stress` build, which checks the hashing loops against the Rust
SHA-256 on every nonce and reports the mismatches as `bad`. Failing is safe: a boot
that crashes or hangs comes back in **safe mode**, at stock clocks with USB running,
and logs what happened. It tries a normal boot again after 10 minutes, and a power
cycle forgets everything.

The tested Pico W ran 18 million checked hashes at 432 MHz and 1.30 V without an
error, at 41 °C with no heatsink, and crashes from 440 MHz. The tested Pico 2 W held
412 MHz at 1.30 V, at 42 °C with no heatsink, and crashes from 416 MHz. The clock
moves in steps of a few MHz up there. 1.30 V is the regulator's ceiling before its
limit has to be disabled, and PicoMiner does not go past it.

On the RP2350, `embassy_rp::init` writes the voltage without unlocking the regulator,
so the register reads back the new value while the chip stays at 1.10 V. The firmware
unlocks it first. Check the temperature when changing this: it has to go up.

## Choosing a pool

The default is `pool.solomining.de:3333`. What matters is how low a pool lets the
difficulty go, because at 128 kH/s a difficulty 1 share takes about 9 hours:

| Pool | Result, tested October 2026 |
| --- | --- |
| `pool.solomining.de:3333` | Goes as low as 0.00015, then adapts: a share about every 20 s |
| `public-pool.io:3333` | Floors at difficulty 1 |
| `pool.nerdminer.io:3333` | Floors at difficulty 1 |
| `solo.ckpool.org:3333` | Floors at difficulty 1 |
| `pool.nerdminers.org:3333` | Refuses unknown user agents |

The chance of finding a block is the same on all of them: the difficulty only
decides how often you get visible proof that the miner works.

## How it works

- **`core/`** holds everything that does not touch hardware and is tested on the
  PC: SHA-256, the JSON reader, Stratum v1, merkle and header building, target
  arithmetic. Run `cargo test` at the repository root. The tests mine the genesis
  block and parse a real `mining.notify`.
- **`firmware/`** is the `no_std` image, built on [Embassy](https://embassy.dev)
  from the same sources for both chips. The build target picks the chip:
  - Both cores hash, each in its own half of the nonce space. Core 0 runs the
    miner in thread mode and everything async (Wi-Fi, Stratum, screen, USB) on an
    interrupt executor that preempts it, so network latency stays low while no
    cycle is wasted.
  - On the RP2040, the nonce loop is 6,750 cycles per hash, against about 12,500
    for the same logic in Rust. `tools/gen_scan.py` writes it as Thumb assembly,
    fully unrolled, and first proves it against `hashlib` on a small simulator that
    also counts the cycles. Run it after changing the loop; its header lists the
    tricks involved.
  - Each core runs its own copy of that loop, with its stack, alone in one of the
    four SRAM banks, so the cores never wait for each other. The other two banks
    hold everything else.
  - On the RP2350, core 1 drives the SHA-256 accelerator, at about 460 cycles per
    hash even though it has to hash all three blocks for every nonce: its state
    registers cannot be written, so it cannot resume from the midstate. Most of
    those cycles are the bus, at 4 to 5 per register access. There is only one
    accelerator, so core 0 runs the Rust loop, for about 8% of the total.
  - A UF2 file names a chip family on every block, and a boot ROM skips the blocks
    of the others. `tools/uf2.py` mixes both images evenly, so that either board
    finishes at the end of the file: a board restarts on its last block, and
    earlier than that Windows would report a failed copy.
  - After a good boot, a panic or a hang costs a reboot and nothing more, and the
    message is logged on the next boot.
  - The setup portal runs its own small DHCP and HTTP servers, with the request
    parsing and the settings format in `core/` so they are tested on the PC.
  - DHCP refusals are ignored on purpose. On a network with a second DHCP server,
    that server refuses the lease of the first one, which would otherwise drop the
    address and the pool right after connecting.

## Licenses

The code is under the [MIT license](LICENSE). The Wi-Fi blobs have their own
terms, see [firmware/cyw43/README.md](firmware/cyw43/README.md).
