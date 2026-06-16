# Setup

## Udev rules

Debug probes need udev rules for non-root access. Copy the bundled rules
file and reload:

```sh
sudo cp ../assets/69-probe-rs.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

On NixOS, use `services.udev.extraRules` instead of copying files. The
rules file covers ST-Link, SEGGER J-Link, CMSIS-DAP, Espressif, and FTDI
probes.

A redacted logic analyzer (used for chip-side serial debugging) needs its own
rules for non-root access. Copy the bundled file and reload the same way:

```sh
sudo cp ../assets/60-redacted.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

The file covers the original Cypress-FX2 Logic (`0925:3881`) and all modern
redacted devices (vendor `21a9`, e.g. the Logic Pro 8 at `21a9:1005`). Replug
the analyzer after reloading so the new rule applies.

## redacted headless capture

The `logic` dev shell drives the analyzer without the GUI, so a UART
trace can be captured from a script:

```sh
nix develop .#logic -c \
  redacted-run python tools/redacted_capture_uart.py \
    --seconds 20 --baud 115200 --out /tmp/redacted-cap
```

`redacted-run` starts Logic 2 headless under Xvfb with its automation gRPC
server enabled (the `--automation` flag, default port 10430), waits for
the port, runs the given command, then tears Logic down. It isolates
`HOME` to a throwaway directory, so the real Logic config is untouched.

`redacted_capture_uart.py` captures ch0 (GPIO43, miner UART TX) and ch1
(GPIO44, miner UART RX), decodes both as async serial, and writes the
decoded frames to `<out>/decoded.csv`. It auto-selects the first connected
hardware device; pass `--device-id` to pick a specific one. The Logic
Pro 8 only accepts 1.2, 1.8, or 3.3 V thresholds, so `--threshold-volts`
defaults to 1.8 V for 3.3 V signaling.

`redacted-run COMMAND` works with any command, not just the capture script,
so the automation server can host an interactive Python session too.

## ST-Link V3 firmware

`probe-rs` requires ST-Link firmware V3J3 or newer. Check with:

```sh
nix shell nixpkgs#stlink -c st-info --probe
```

If the J-version is too low, update with ST's updater (STSW-LINK007). The
updater is a Java application and needs `libusb-1.0`:

```sh
nix shell nixpkgs#jdk nixpkgs#libusb1 -c bash -c \
  'LD_LIBRARY_PATH=$(nix eval --raw nixpkgs#libusb1)/lib \
   java -Djava.library.path=<path>/native/linux_x64 \
   -jar <path>/STLinkUpgrade.jar'
```

## Debug probe (Pico 2 as CMSIS-DAP)

A Raspberry Pi Pico 2 running the debugprobe firmware provides SWD and
UART passthrough for the EVB-Pico boards. Flash `debugprobe_on_pico2.uf2`
from `../assets/` via BOOTSEL mass storage.

| Probe GPIO | EVB-Pico GPIO | Function |
|------------|---------------|----------|
| GP2 | SWCLK | SWD clock |
| GP3 | SWDIO | SWD data |
| GP4 | GP0 | UART TX (probe to target) |
| GP5 | GP1 | UART RX (target to probe) |
| GND | GND | Ground |

## SWD wiring (ST-Link V3 to Pico/EVB-Pico)

The ST-Link V3 uses a 14-pin STDC14 connector. The Pico debug header has
3 pins (SWCLK, GND, SWDIO). A 4th wire is required for voltage sensing --
without it the ST-Link reads 0V and refuses to connect.

| ST-Link V3 (STDC14) | Target |
|----------------------|--------|
| Pin 1 -- T_VCC | 3V3 (from Pico GPIO header) |
| Pin 2 -- SWDIO | SWDIO |
| Pin 3 -- GND | GND |
| Pin 4 -- SWCLK | SWCLK |

T_VCC does not power the target. It is a voltage-sense input that sets the
SWD I/O levels. The target must be powered separately via USB.

## Pin assignments

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

Same pinout as the W5500-EVB-Pico, with a W6100 in place of the W5500.

### Braiins Forge Miner x4 (a.k.a. Hashnode)

RP2350B with a W5500 over SPI0. Drives a 4-chip BM1370 hashboard via
UART1 and a HashnodePsu (SIC45x) over I2C1. GPIO30 (`SIC_EN`, active-high,
10 kΩ pull-down) gates the SIC45x power stage; it is driven high by the
miner task during bring-up. GPIO23 is the third fan tachometer input
(`RP_TACH_C`). SPI0 and I2C1 pinout matches the rp2350 board.

### BMM Adapter (bmm-pico)

RP2350A on the BMM Adapter carrier board with a W5500 over SPI0 and the
chip-side resources required by the ubos-mining backend on UART1, I2C1, and GPIO.
See `firmware/bmm-pico/src/config.rs` for the full peripheral map. The
ubos-mining bring-up is in [MINING.md](MINING.md).

### DevEBox STM32F407 + LAN8720 (stm32f407)

See `firmware/stm32f407/README.md` for the full RMII and management
interface wiring.

### Nucleo-F439ZI + LAN8742A (stm32f439)

Same RMII pin mapping as the STM32F407 except TX_EN on PG11 and TXD0 on
PG13 -- MB1137 routes these to the onboard LAN8742A via solder bridges
SB183 and SB182 in their default state. The MB1137 carries an onboard
LAN8742A with its own 25 MHz crystal and drives the 50 MHz RMII reference
clock from the PHY, so no oscillator enable pin is needed. LED is on PB0
(Nucleo LD1 green). Debug via onboard ST-LINK. Full clock and pin setup is
in `firmware/stm32f439/src/config.rs`.

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

## Building and flashing

ARM targets share one dev shell:

```sh
nix develop .#arm
cd firmware/rp2040    && cargo run --release
cd firmware/rp2350    && cargo run --release
cd firmware/hashnode  && cargo run --release
cd firmware/bmm-pico  && cargo run --release
cd firmware/stm32f407 && cargo run --release
cd firmware/stm32f439 && cargo run --release
```

ESP32-S3 targets need the ESP dev shell:

```sh
nix develop .#esp
cd firmware/esp32s3 && cargo run --release
```

Each crate's `.cargo/config.toml` pins the probe serial, so `cargo run`
flashes the right board without a `--probe` flag.

Every board drives a BM1362 hashboard via the `ubos-mining` backend
unconditionally. The `display` feature is enabled by default on
boards that have a display.

## Pool configuration

Pool config lives in the `ubos-config` record store and is persisted in
the on-chip flash. The HTTP API mutates it at runtime; the `POOL_*` env
vars only seed `Config::default`, which is the value used on a fresh
board before the first store. A stored blob in flash supersedes both.

Set the env vars before `cargo build` / `cargo run` to change the
factory defaults of a freshly flashed board:

```sh
POOL_HOST=stratum.example.com POOL_PORT=3333 cargo run --release
```

| Variable | Type | Default | Notes |
|---|---|---|---|
| `POOL_HOST` | string | `stratum.braiins.com` | IPv4 literal or DNS name |
| `POOL_PORT` | u16 | `3333` | |
| `POOL_USER` | string | empty | worker identity, P2TR address + `.workersuffix` |
| `POOL_PASSWORD` | string | unset → no password | seeds the Sv1 password slot of the default `PoolEntry` |

Code: `ubos/ubos-config/src/lib.rs` (`DEFAULT_POOL_*` and `Config::default`).

## Networking (bench)

The bench NUC runs `dnsmasq` with fixed DHCP leases. Each board gets a
stable IP derived from the last octet of its locally-administered MAC
address. These assignments are shared by firmware MAC constants, dnsmasq
leases in `nuc/networking.nix`, and `nuc/boards.nix` (consumed by
the Prometheus scrape config, nftables counters, and the load-test
wrapper).

| Board | MAC | IP |
|-------|-----|----|
| rp2350 | `02:00:00:00:00:01` | `redacted101` |
| esp32s3 | `02:00:00:00:00:02` | `redacted102` |
| stm32f407 | `02:00:00:00:00:03` | `redacted103` |
| rp2040 | `02:00:00:00:00:04` | `redacted104` |
| stm32f439 | `02:00:00:00:00:05` | `redacted105` |

The NUC provides DNS forwarding and NAT to the internet via WiFi, so
boards can resolve hostnames for NTP sync and HTTPS requests.

### mDNS discovery

Each board advertises itself via three mDNS services so Prometheus,
browsers, REST clients, and ad-hoc tooling can find it without static
config. The announcement carries an `A` record for `<hostname>.local`
and one `SRV`/`TXT`/`PTR` set per service. Each TXT record carries
`path=`, `board=`, and `mcu=`.

| Service              | Port | Purpose                    |
|----------------------|------|----------------------------|
| `_http._tcp.local`   | 80   | Web UI (dashboard/login)   |
| `_ubos._tcp.local`   | 8080 | JSON REST API              |
| `_prometheus._tcp.local` | 8081 | Prometheus scrape target |

Browsing requires `avahi-daemon` on the workstation. On NixOS, enable
it in the host configuration. The `nssmdns4` switch also wires mDNS
into NSS so `ping bmm-pico-01.local` and any other hostname-based
tool resolves through the same announcements.

```nix
services.avahi = {
  enable = true;
  nssmdns4 = true;
};
```

Then browse every advertised board:

```sh
avahi-browse -rt _prometheus._tcp
avahi-browse -rt _http._tcp
avahi-browse -rt _ubos._tcp
```

Without the daemon, a unicast query against a known IP also works
because edge-mdns answers on port 5353:

```sh
dig +time=3 +tries=1 -p 5353 @<board-ip> _prometheus._tcp.local PTR
dig +time=3 +tries=1 -p 5353 @<board-ip> _http._tcp.local PTR
dig +time=3 +tries=1 -p 5353 @<board-ip> _ubos._tcp.local PTR
```

Support node (NUC): `nuc/README.md`.
