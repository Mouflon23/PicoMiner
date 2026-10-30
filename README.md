# PicoMiner

A solo Bitcoin lottery miner in Rust for the **Raspberry Pi Pico W** with a
**Waveshare Pico-OLED-1.3** screen. It is a from-scratch take on the idea behind
[NerdMiner v2](https://github.com/BitMaker-hub/NerdMiner_v2), which runs on the ESP32.

Measured on real hardware: **32 kH/s** across both cores at 200 MHz.

## The odds

This is a toy. At the current network difficulty (about 148 T) a block takes
6.4 × 10²³ hashes on average, so one PicoMiner needs around 630 billion years,
some 45 times the age of the universe. The fun is watching it try. It earns
nothing unless that miracle happens, in which case the pool pays the block reward
to your address.

## Hardware

The OLED module plugs straight onto the Pico W headers.

| Function | GPIO |
| --- | --- |
| OLED DC / CS / CLK / DIN / RST | 8 / 9 / 10 / 11 / 12 |
| KEY0: next page, wakes the screen | 15 |
| KEY1: screen on or off | 17 |

## Getting started

You need Rust, then:

```bash
rustup target add thumbv6m-none-eabi
```

```bash
cargo install elf2uf2-rs --locked
```

Copy `.env.example` to `.env` and fill in your Wi-Fi network and a BTC address.
`.env` is ignored by git, but its values are baked into the firmware, so do not
share your `.uf2`. Without it the firmware stays in **benchmark mode**: it hashes
a made-up job, which is handy for measuring speed.

Then flash. The first time, hold BOOTSEL while plugging the Pico in. Afterwards
the script reboots a running PicoMiner over USB by itself.

```powershell
./scripts/flash.ps1
./scripts/monitor.ps1
```

The screen has three pages: hashrate and shares, pool details, and Wi-Fi. The
onboard LED flashes on each accepted share.

## Choosing a pool

The default is `pool.solomining.de:3333`. What matters is how low a pool lets the
difficulty go, because at 32 kH/s a difficulty 1 share takes about 37 hours:

| Pool | Result, tested October 2026 |
| --- | --- |
| `pool.solomining.de:3333` | Honours the suggested 0.00015: a share about every 20 s |
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
- **`firmware/`** is the `no_std` image, built on [Embassy](https://embassy.dev):
  - Both cores hash, each in its own half of the nonce space. Core 0 runs the
    miner in thread mode and everything async (Wi-Fi, Stratum, screen, USB) on an
    interrupt executor that preempts it, so network latency stays low while no
    cycle is wasted.
  - The nonce loop runs from SRAM. It skips what does not change between nonces:
    the midstate, the first three rounds and part of the schedule. It also stops
    at round 60 of the second hash, where the top 32 bits are already known.
  - The chip runs at 200 MHz and 1.15 V, the highest setting the RP2040 datasheet
    covers.
  - A panic reboots the board, and the message is logged on the next boot.

## Licenses

The code is under the [MIT license](LICENSE). The Wi-Fi blobs have their own
terms, see [firmware/cyw43/README.md](firmware/cyw43/README.md).
