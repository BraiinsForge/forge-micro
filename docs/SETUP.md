# Setup

## Udev rules

Debug probes need udev rules for non-root access. Copy the bundled rules
file and reload:

```sh
sudo cp ../../../assets/69-probe-rs.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

On NixOS, use `services.udev.extraRules` instead of copying files. The
rules file covers ST-Link, SEGGER J-Link, CMSIS-DAP, Espressif, and FTDI
probes.

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
from `../../../assets/` via BOOTSEL mass storage.

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

## UART dummy-asic wiring

The dummy-asic (Seeed XIAO ESP32-S3) connects to the EVB-Pico UART to
receive Work frames and return Share frames. TX and RX are crossed.

| XIAO silk | XIAO GPIO | EVB-Pico GPIO | Direction |
|-----------|-----------|---------------|-----------|
| D9 | GPIO8 (TX) | GP5 (UART RX) | dummy-asic to host |
| D10 | GPIO9 (RX) | GP4 (UART TX) | host to dummy-asic |
| GND | GND | GND | ground |

Protocol and flashing: [DUMMY_ASIC.md](DUMMY_ASIC.md).

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

### BMM Adapter (bmm-pico)

RP2350A on the BMM Adapter carrier board with a W5500 over SPI0 and the
chip-side resources required by the mmm backend on UART1, I2C1, and GPIO.
See `firmware/bmm-pico/src/config.rs` for the full peripheral map. The
backend feature matrix is in [OVERVIEW.md](OVERVIEW.md) and the mmm
bring-up is in [MMM.md](MMM.md).

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
cd firmware/bmm-pico  && cargo run --release
cd firmware/stm32f407 && cargo run --release
cd firmware/stm32f439 && cargo run --release
```

ESP32-S3 targets need the ESP dev shell:

```sh
nix develop .#esp
cd firmware/esp32s3 && cargo run --release
cd dummy-asic       && cargo run --release
```

Each crate's `.cargo/config.toml` pins the probe serial, so `cargo run`
flashes the right board without a `--probe` flag.

### Backend feature selection

`firmware/{rp2040,rp2350,stm32f407,stm32f439,esp32s3}` default to
`backend-uart`; `firmware/bmm-pico` defaults to `backend-mmm`. Boards with
displays also default `display`. Override with:

```sh
# CPU mining on a board with a display
cargo run --release --no-default-features --features "display,backend-cpu"

# Real BM1362 hashboard on the BMM Adapter
cargo run --release --no-default-features --features "display,backend-mmm"
```

The features are mutually exclusive. Build-time `compile_error!`s catch any
attempt to enable more than one in the same target.

### dummy-asic first-flash

esp-hal 1.0.0 uses the ESP-IDF bootloader format. `probe-rs` cannot write
the bootloader on its own, so the first flash must go through `espflash`.
See [DUMMY_ASIC.md](DUMMY_ASIC.md) for the procedure.

## Pool configuration

Pool parameters are baked in at compile time via `option_env!` variables.
Set them in the environment before `cargo build` / `cargo run`:

```sh
POOL_HOST=stratum.example.com POOL_PORT=3333 cargo run --release
```

| Variable | Type | Default | Notes |
|---|---|---|---|
| `POOL_HOST` | string | `stratum.braiins.com` | IPv4 literal or DNS name |
| `POOL_PORT` | u16 | `3333` | |
| `POOL_PROTOCOL` | `sv1`/`sv2` | `sv1` | |
| `POOL_USER` | string | per-board | worker identity, P2TR address + `.workersuffix` |
| `POOL_SV1_PASS` | string | unset → no password | ignored under `sv2` |
| `POOL_SV2_PUBKEY` | base58check string | unset → Braiins prod key | ignored under `sv1` |
| `POOL_SV2_TRUST_ANY_KEY` | `"1"` | unset | `"1"` skips peer auth (Noise handshake still runs); any other value is a build error |

When `POOL_SV2_PUBKEY` is unset, the firmware validates the server's Noise
static key against the Braiins production endpoint key hardcoded in
`src/pool_config.rs`. Set `POOL_SV2_TRUST_ANY_KEY=1` to skip validation
for local testing.

Code: `src/pool_config.rs`.

## Networking (bench)

The bench NUC runs `dnsmasq` with fixed DHCP leases. Each board gets a
stable IP derived from the last octet of its locally-administered MAC
address. These assignments are shared by firmware MAC constants, dnsmasq
leases in `redacted/networking.nix`, and `redacted/boards.nix` (consumed by
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

Each board advertises itself via mDNS as `_prometheus._tcp.local` so
Prometheus and ad-hoc tooling can find it without static config. The
announcement carries an `A` record for `<hostname>.local`, an `SRV`
pointing at the metrics port, and TXT entries for `path`, `board`,
and `mcu`.

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
```

Without the daemon, a unicast query against a known IP also works
because edge-mdns answers on port 5353:

```sh
dig +time=3 +tries=1 -p 5353 @<board-ip> _prometheus._tcp.local PTR
```

Support node (NUC): `redacted/README.md`.
