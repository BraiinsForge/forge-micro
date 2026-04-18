# Ethernet Examples

Bare-metal Rust Ethernet across five boards. Each serves an HTTP metrics
page with uptime, request count, BTC price via Binance over TLS, and UTC
time via NTP.

## Boards

| Crate | MCU | Ethernet | Target |
|-------|-----|----------|--------|
| rp2040 | RP2040 | W5500 (onboard SPI) | thumbv6m-none-eabi |
| rp2350 | RP2350A | W6100 (onboard SPI) | thumbv8m.main-none-eabihf |
| stm32f407 | STM32F407VGT6 | LAN8720 (RMII, WT32-ETH01 cannibalized) | thumbv7em-none-eabihf |
| stm32f439 | STM32F439ZIT6 | LAN8720 (RMII, Waveshare ETH Board) | thumbv7em-none-eabihf |
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

### Nucleo-F439ZI + LAN8720 (stm32f439)

Same RMII pin mapping as the STM32F407. The Waveshare LAN8720 ETH Board
has its own 50 MHz oscillator, so no oscillator enable pin is needed.
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
nix develop                                          # ARM dev shell
cd firmware/rp2040      && cargo run --release   # W5500-EVB-Pico
cd firmware/rp2350      && cargo run --release   # W6100-EVB-Pico-2
cd firmware/stm32f407   && cargo run --release   # DevEBox STM32F407
cd firmware/stm32f439   && cargo run --release   # Nucleo-F439ZI
```

```sh
nix develop .#esp32                                  # ESP32 dev shell
cd firmware/esp32s3     && cargo run --release   # ESP32-S3 + W5100S
```

`probe-rs list` shows connected probes. Each crate's `.cargo/config.toml`
configures the probe-rs runner for flash and monitor.

## Network

All boards use DHCP. After flashing, check your router for the assigned IP
address and open `http://<ip>/` in a browser.
