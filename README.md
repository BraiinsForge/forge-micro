# Ethernet Examples

Bare-metal Rust Ethernet across six boards. Each serves an HTTP metrics
page with uptime, request count, current hashrate from the dummy-asic
peer, BTC price via Binance over TLS, and UTC time via NTP.

## Boards

| Crate | MCU | Ethernet | Target |
|-------|-----|----------|--------|
| bmm-pico | RP2350A | W5500 (onboard SPI) | thumbv8m.main-none-eabihf |
| rp2040 | RP2040 | W5500 (onboard SPI) | thumbv6m-none-eabi |
| rp2350 | RP2350A | W6100 (onboard SPI) | thumbv8m.main-none-eabihf |
| stm32f407 | STM32F407VGT6 | LAN8720 (RMII, WT32-ETH01 cannibalized) | thumbv7em-none-eabihf |
| stm32f439 | STM32F439ZIT6 | LAN8742A (RMII, onboard) | thumbv7em-none-eabihf |
| esp32s3 | ESP32-S3 | W5100S (SPI) | xtensa-esp32s3-none-elf |

## Prerequisites

### Udev rules

Debug probes need udev rules for non-root access. Copy the bundled rules
file and reload:

```sh
sudo cp ../../assets/69-probe-rs.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

The rules file covers ST-Link, SEGGER J-Link, CMSIS-DAP, Espressif, and
FTDI probes. See [probe-rs probe setup](https://probe.rs/docs/getting-started/probe-setup/)
for details.

On NixOS, use `services.udev.extraRules` instead of copying files. See
the rules file for the full list of VID:PID entries.

### ST-Link V3 firmware

probe-rs requires ST-Link firmware V3J3 or newer. Check with:

```sh
nix shell nixpkgs#stlink -c st-info --probe
```

If the J-version is too low, update using
[STSW-LINK007](https://www.st.com/en/development-tools/stsw-link007.html)
(Java GUI, works on Linux). The updater needs `libusb-1.0`:

```sh
nix shell nixpkgs#jdk nixpkgs#libusb1 -c bash -c \
  'LD_LIBRARY_PATH=$(nix eval --raw nixpkgs#libusb1)/lib \
   java -Djava.library.path=<path>/native/linux_x64 \
   -jar <path>/STLinkUpgrade.jar'
```

### Debug probe (Pico 2 as CMSIS-DAP)

A Raspberry Pi Pico 2 running the debugprobe firmware provides SWD and
UART passthrough for the EVB-Pico boards. Flash `debugprobe_on_pico2.uf2`
from `assets/` via BOOTSEL mass storage (hold BOOTSEL, plug USB, copy
the UF2 file). The original Pico variant is `debugprobe_on_pico.uf2`.

| Probe GPIO | EVB-Pico GPIO | Function |
|------------|---------------|----------|
| GP2 | SWCLK | SWD clock |
| GP3 | SWDIO | SWD data |
| GP4 | GP0 | UART TX (probe → target) |
| GP5 | GP1 | UART RX (target → probe) |
| GND | GND | Ground |

### UART dummy-asic wiring

The dummy-asic (Seeed XIAO ESP32-S3) connects to the EVB-Pico UART to
receive Work frames and return Share frames. TX and RX are crossed.

| XIAO silk | XIAO GPIO | EVB-Pico GPIO | Direction |
|-----------|-----------|---------------|-----------|
| D9 | GPIO8 (TX) | GP5 (UART RX) | dummy-asic → host |
| D10 | GPIO9 (RX) | GP4 (UART TX) | host → dummy-asic |
| GND | GND | GND | ground |

### SWD wiring (ST-Link V3 to Pico/EVB-Pico)

The ST-Link V3 uses a 14-pin STDC14 connector. The Pico debug header
has 3 pins (SWCLK, GND, SWDIO). A 4th wire is required for voltage
sensing — without it the ST-Link reads 0V and refuses to connect.

| ST-Link V3 (STDC14) | Target |
|----------------------|--------|
| Pin 1 — T_VCC | 3V3 (from Pico GPIO header) |
| Pin 2 — SWDIO | SWDIO |
| Pin 3 — GND | GND |
| Pin 4 — SWCLK | SWCLK |

T_VCC does not power the target. It is a voltage sense input that sets
the SWD I/O levels. The target must be powered separately via USB.

## Pin Assignments

### W5500-EVB-Pico (rp2040)

| Function | GPIO | SPI0 |
|----------|------|------|
| MISO | 16 | SPI0 RX |
| CS | 17 | GPIO |
| CLK | 18 | SPI0 SCK |
| MOSI | 19 | SPI0 TX |
| RST | 20 | GPIO |
| INT | 21 | GPIO (Pull-Up) |
| LED | 25 | GPIO |

### W6100-EVB-Pico-2 (rp2350)

Same pinout as the W5500-EVB-Pico. The board uses a W6100 instead of W5500.

| Function | GPIO | SPI0 |
|----------|------|------|
| MISO | 16 | SPI0 RX |
| CS | 17 | GPIO |
| CLK | 18 | SPI0 SCK |
| MOSI | 19 | SPI0 TX |
| RST | 20 | GPIO |
| INT | 21 | GPIO (Pull-Up) |
| LED | 25 | GPIO |

### DevEBox STM32F407 + LAN8720 (stm32f407)

See [firmware/stm32f407/README.md](firmware/stm32f407/README.md)
for the full RMII and management interface wiring.

### Nucleo-F439ZI + LAN8742A (stm32f439)

Same RMII pin mapping as the STM32F407 except TX_EN on PG11 and TXD0
on PG13 — MB1137 routes these to the onboard LAN8742A via solder
bridges SB183 and SB182 in their default state. The MB1137 carries an
onboard LAN8742A with its own 25 MHz crystal and drives the 50 MHz RMII
reference clock from the PHY, so no oscillator enable pin is needed.
LED is on PB0 (Nucleo LD1 green). Debug via onboard ST-LINK.

### ESP32-S3 + W5100S (esp32s3)

Repurposes the LCD header pins on an ESP32-S3 dev board.

| Function | GPIO |
|----------|------|
| SCLK | 1 |
| MOSI | 2 |
| MISO | 42 |
| CS | 39 |
| INT | 41 (Pull-Up) |
| RST | 40 |

## Building and Flashing

```sh
nix develop .#arm                                    # ARM dev shell
cd firmware/rp2040      && cargo run --release   # W5500-EVB-Pico
cd firmware/rp2350      && cargo run --release   # W6100-EVB-Pico-2
cd firmware/stm32f407   && cargo run --release   # DevEBox STM32F407
cd firmware/stm32f439   && cargo run --release   # Nucleo-F439ZI
```

```sh
nix develop .#esp                                    # ESP32 dev shell
cd firmware/esp32s3     && cargo run --release   # ESP32-S3 + W5100S
```

`probe-rs list` shows connected probes. Each crate's `.cargo/config.toml`
configures the probe-rs runner for flash and monitor.

### Pool configuration (rp2350)

The rp2350 firmware reads pool config from compile-time env vars
(`POOL_HOST`, `POOL_PORT`, `POOL_PROTOCOL`, `POOL_USER`,
`POOL_SV1_PASS`, `POOL_SV2_PUBKEY`, `POOL_SV2_TRUST_ANY_KEY`).
Unset variables fall through to the defaults in
`eth_examples::pool_config`. `POOL_HOST` accepts a DNS name or IPv4
literal; resolution happens at boot via the smoltcp DNS query.

`POOL_SV2_PUBKEY` defaults to the Braiins prod authority key
(`DEFAULT_POOL_SV2_PUBKEY`) when unset, so an unconfigured firmware
connects to `stratum.braiins.com` with peer authentication enabled.
Set `POOL_SV2_TRUST_ANY_KEY=1` to disable peer auth for testing
against an SV2 pool whose authority key does not match the Braiins
prod key. Any other value for `POOL_SV2_TRUST_ANY_KEY` is a
build-time error.

`backend-uart` is the default (UART peer via the dummy-asic board);
`backend-cpu` swaps in on-MCU CPU mining. The two are mutually
exclusive. Both SV1 and SV2 are always built; protocol selection is
purely runtime via `POOL_PROTOCOL=sv1|sv2`.

Stratum V1, UART peer backend (default):

```sh
nix develop .#arm -c bash -c '
  cd firmware/rp2350 && \
  POOL_HOST=172.65.65.63 \
  POOL_PORT=3333 \
  POOL_PROTOCOL=sv1 \
  POOL_USER=<braiins-account>.<worker> \
  POOL_SV1_PASS=x \
  cargo run --release
'
```

Stratum V1, on-MCU CPU backend:

```sh
nix develop .#arm -c bash -c '
  cd firmware/rp2350 && \
  POOL_HOST=172.65.65.63 \
  POOL_PORT=3333 \
  POOL_PROTOCOL=sv1 \
  POOL_USER=<braiins-account>.<worker> \
  POOL_SV1_PASS=x \
  cargo run --release --no-default-features --features "display,backend-cpu"
'
```

Stratum V2, UART peer backend:

```sh
nix develop .#arm -c bash -c '
  cd firmware/rp2350 && \
  POOL_HOST=<braiins-sv2-ipv4> \
  POOL_PORT=<braiins-sv2-port> \
  POOL_PROTOCOL=sv2 \
  POOL_USER=<braiins-account>.<worker> \
  POOL_SV2_PUBKEY=<braiins-sv2-pubkey-base58check> \
  cargo run --release
'
```

Stratum V2, on-MCU CPU backend:

```sh
nix develop .#arm -c bash -c '
  cd firmware/rp2350 && \
  POOL_HOST=<braiins-sv2-ipv4> \
  POOL_PORT=<braiins-sv2-port> \
  POOL_PROTOCOL=sv2 \
  POOL_USER=<braiins-account>.<worker> \
  POOL_SV2_PUBKEY=<braiins-sv2-pubkey-base58check> \
  cargo run --release --no-default-features --features "display,backend-cpu"
'
```

### Dummy-ASIC (ESP32-S3 XIAO)

The dummy-asic uses esp-hal 1.0.0 which requires the ESP-IDF bootloader
format. probe-rs cannot write the bootloader on its own, so the first
flash must go through espflash:

```sh
nix-shell -p espflash --run \
  "espflash flash --chip esp32s3 --port /dev/ttyACM<N> target/xtensa-esp32s3-none-elf/release/dummy-asic"
```

Put the XIAO into download mode first (hold BOOT, press RST). After
flashing, do a full power cycle — disconnect all power sources, not
just USB, since the XIAO may be back-powered through the UART wiring.

Once the bootloader and partition table are in flash, subsequent
flashes work with probe-rs:

```sh
nix develop .#esp
cd dummy-asic && cargo run --release
```

When multiple ESP-JTAG probes are connected, use the per-main-board
aliases to select the right dummy-asic peer by probe serial:

```sh
cd dummy-asic && cargo run-rp2040 --release      # rp2040 peer
cd dummy-asic && cargo run-rp2350 --release      # rp2350 peer
cd dummy-asic && cargo run-stm32f407 --release   # stm32f407 peer
cd dummy-asic && cargo run-stm32f439 --release   # stm32f439 peer
cd dummy-asic && cargo run-esp32s3 --release     # esp32s3 peer
```

See `dummy-asic/.cargo/config.toml` for the full alias list and probe
serials.

The espflash step only needs to be repeated if the flash is erased.

## Size and stack analysis

### Binary size

`cargo size --release -- -A` (from `cargo-binutils`, pre-installed in
the dev shells) prints per-section sizes from the ELF. It builds first,
so it is a drop-in replacement for `cargo build --release` when you
want size output. The default Berkeley output (`cargo size --release`)
adds a `dec`/`hex` total; `-A` gives the per-section breakdown. Budgets:

- Flash = `.vector_table + .text + .rodata + .data` (plus `.boot2`
  on RP2040; plus `.start_block`, `.bi_entries`, `.end_block`, and
  `.gnu.sgstubs` on RP2350)
- Static RAM = `.data + .bss + .uninit`

`cargo bloat --release` attributes flash usage to functions and crates,
useful for tracking down why a build grew.

### Stack

The executor model determines what "stack per task" means:

- **ARM boards (`embassy-executor`)**: a single shared stack is used by
  whichever task is polling. Task state lives in a static arena sized at
  compile time, not on the stack. Worst-case stack usage is the deepest
  poll path plus ISR nesting.
- **ESP32-S3 (`esp-rtos`)**: FreeRTOS threads with per-thread stacks,
  sized at spawn. The embassy executor runs on one of them.

Analysis options:

- `cargo-call-stack` — whole-program static upper bound per entry point.
  Cortex-M only, known limitations around async state machines and
  function pointers.
- `-Z emit-stack-sizes` (nightly rustc) — emits per-function sizes to a
  linker section. No call graph, so pair with external analysis.
- Runtime watermark — paint the stack with a known pattern at boot, read
  high-water via probe-rs. On ESP32-S3, FreeRTOS exposes
  `uxTaskGetStackHighWaterMark` directly.

## Network

The NUC runs dnsmasq on end0 with fixed DHCP leases. Each board gets a
stable IP derived from the last octet of its locally-administered MAC
address.

The IPs below are the source-of-truth assignments shared by
firmware MAC consts, dnsmasq leases in `redacted/networking.nix`,
and `redacted/boards.nix`. The Nix file is consumed by the
Prometheus scrape config, nftables traffic counters, and the
load-test wrapper.

| Board | MAC | IP |
|-------|-----|----|
| rp2350 | `02:00:00:00:00:01` | `redacted101` |
| esp32s3 | `02:00:00:00:00:02` | `redacted102` |
| stm32f407 | `02:00:00:00:00:03` | `redacted103` |
| rp2040 | `02:00:00:00:00:04` | `redacted104` |
| stm32f439 | `02:00:00:00:00:05` | `redacted105` |

The NUC provides DNS forwarding and NAT to the internet via WiFi, so
boards can resolve hostnames for NTP sync and HTTPS requests.

## Support Node (NUC)

A Raspberry Pi 4 running NixOS that hosts the target boards, joins the
redacted redacted mesh, and exposes node_exporter for remote monitoring.

See [redacted/README.md](redacted/README.md) for build, flash, and enrollment
instructions. Host definition is in `redacted/nuc.nix`.
