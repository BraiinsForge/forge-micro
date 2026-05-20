# Overview

`eth-examples` is a bare-metal Rust Ethernet workspace spanning six firmware
crates, a UART-attached mining peer, the libraries that back them, and a
host-side runner that mirrors the firmware tasks against a Linux network
device.

## Boards

| Crate | MCU | Ethernet | Target |
|-------|-----|----------|--------|
| `firmware/rp2040` | RP2040 | W5500 (SPI) | `thumbv6m-none-eabi` |
| `firmware/rp2350` | RP2350A | W6100 (SPI) | `thumbv8m.main-none-eabihf` |
| `firmware/bmm-pico` | RP2350A (BMM Adapter) | W5500 (SPI) | `thumbv8m.main-none-eabihf` |
| `firmware/stm32f407` | STM32F407VGT6 | LAN8720 (RMII, WT32-ETH01 cannibalized) | `thumbv7em-none-eabihf` |
| `firmware/stm32f439` | STM32F439ZIT6 | LAN8742A (RMII, onboard) | `thumbv7em-none-eabihf` |
| `firmware/esp32s3` | ESP32-S3 | W5100S (SPI) | `xtensa-esp32s3-none-elf` |
| `dummy-asic` | ESP32-S3 (XIAO) | (UART peer) | `xtensa-esp32s3-none-elf` |

## Firmware architecture

Each board crate brings up its own SPI / RMII link, then hands an
`embassy_net::Stack<'static>` to the shared `src/` library. From that
point every board runs the same above-the-link-layer tasks:

- DHCP wait and bring-up orchestration
- NTP sync loop
- BTC price fetch loop (Binance ticker over TLS)
- Prometheus metrics HTTP endpoint
- Watchdog
- Display (where present)

The link-layer differs by board:

- WIZnet boards (rp2040, rp2350, bmm-pico, esp32s3) use `embassy-net-wiznet`
  in MACRAW mode so smoltcp owns the full TCP/IP stack. One code path covers
  W5500/W5100S/W6100.
- STM32 boards use the on-chip RMII MAC via `embassy-stm32::eth` with a
  LAN8720 (F407) or LAN8742A (F439) PHY.

The HTTP response is hand-rolled into caller-owned buffers because the
firmware serves a single metrics page and does not justify a full web
framework.

### Display

Code: `src/display.rs`.

Line 3 of the 4-line display layout shows fan PWM percentage and RPM on boards
that have cleared the `FAN_PWM_PERCENT` sentinel (i.e. boards where `BoardFan`
was constructed, which stores `0` to signal that a fan is present). On boards
without a fan, `FAN_PWM_PERCENT` retains its sentinel value and line 3 falls
back to the BTC price.

## Mining backends

Boards that mine select a backend at compile time. The features are mutually
exclusive:

| Feature | Backend |
|---------|---------|
| `backend-cpu` | CPU mock via `asic-cpu-mock::run_asic_cpu` (every board) |
| `backend-uart` | UART-attached `dummy-asic` peer via `asic-uart-peer::run_asic_uart` |
| `backend-mmm` | Real BM1362 hashboard via the `mmm` crate (bmm-pico only) |

Every backend pushes shares into the same `stratum-client::AsicSession`
solution channel that the V1 and V2 client tasks drain. Hashes accumulated
by the backend feed `eth-examples::hashrate` for the EWMA-smoothed rate
the Prometheus endpoint and display surface. The mmm backend ties
together the BM1362 chip-serial actor, the BMM100 PSU driver, and a
runtime baud-rate switch; see [MMM.md](MMM.md).

## Workspace crates

### Shared firmware library

- `src/` (`eth-examples`) -- above-the-link-layer tasks shared by every
  firmware crate: HTTP server, metrics, NTP, BTC, display, CPU-load
  accounting, EWMA hashrate accounting, watchdog.

### Mining stack

- `mining-protocol/` -- protocol-agnostic adapter trait between a stratum
  client task and the work generator.
- `stratum-v1/` -- Stratum V1 message types, parser, and adapter impl.
- `stratum-v2/` -- Stratum V2 message types, noise handshake, and adapter
  impl.
- `stratum-client/` -- shared work-generator task, `JobRing`, `WorkRing`,
  and `AsicSession` ASIC abstraction; generic over the protocol adapter.
- `asic-cpu-mock/` -- in-process CPU-side mock that polls `AsicSession`,
  hashes nonces, and pushes solutions back.
- `asic-uart-peer/` -- UART codec that bridges `AsicSession` to an
  external `dummy-asic` peer over the legacy XOR-checksummed frame
  protocol.
- `mmm/` -- BM1362 chip driver: frame layouts, parser, miner state machine,
  and the single-task chip-serial actor.

### Hardware drivers

- `rp235x-sha256/` -- polled driver for the RP2350 SHA-256 accelerator.
- `esp32s3-tsens/` -- ESP32-S3 internal temperature sensor (esp-hal 1.0.0
  does not expose it).
- `ubos-psu/` -- async PSU driver for the BMM100 miniminer hashboard plus
  the INA230 current monitor; generic over `embedded-hal-async::i2c`.
- `ubos-logging/` -- macro facade that swaps between `defmt` and `log` at
  compile time, so the same crate builds for firmware and host.
- `serial-baudrate/` -- `BaudRate` trait, orthogonal to `embedded-io`, for
  runtime baud switching from a `&self` receiver.

### Host-side

- `host-runner/` -- Linux runner that re-uses the firmware tasks against a
  TAP device (or an `AF_PACKET` raw socket). Also wires up an hwmon fan
  controller, optional `MMM_I2C_BUS` PSU exercise, and an optional real
  BM1362 hashboard.
- `embassy-net-afpacket/` -- `embassy-net` driver bound to a Linux NIC via
  `AF_PACKET`/`SOCK_RAW`, with synthetic-MAC + promiscuous-mode option.
- `embedded-serial-unix/` -- async tty driver implementing
  `embedded-io-async` and `serial-baudrate::BaudRate`.
- `embedded-i2c-unix/` -- async `embedded-hal-async::i2c::I2c` over Linux
  `/dev/i2c-N` via `I2C_RDWR` ioctls.
- `redacted/` -- NixOS host configuration for the bench NUC and an HTTP
  load generator under `pkgs/load-test/`.

## Dummy ASIC peer

The `dummy-asic` crate runs on a Seeed XIAO ESP32-S3 and pretends to be a
real ASIC over UART. The two cores split work: `PRO_CPU` handles UART
reader and writer tasks, `APP_CPU` runs the nonce-scanning solver against
the hardware SHA peripheral. Protocol, runtime model, solver internals, and
flashing live in [DUMMY_ASIC.md](DUMMY_ASIC.md).
