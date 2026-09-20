# Overview

The firmware workspace is bare-metal Rust for Ethernet boards. It
holds per-board firmware crates, the libraries that back them, and a
host-side runner that mirrors the firmware tasks against a Linux
network device. The crate map and the dependency direction are in
[STRUCTURE.md](STRUCTURE.md). The cross-crate law is in
[ARCHITECTURE.md](ARCHITECTURE.md). Each crate's own surface is in its
`//!` docs.

## Boards

| Crate | MCU | Ethernet | Target |
|-------|-----|----------|--------|
| `firmware/hashnode` | RP2350B (Braiins Forge Miner x4) | W5500 (SPI) | `thumbv8m.main-none-eabihf` |
| `firmware/forge-axe` | RP2350A (Pico 2, BM1370 hashboard) | W5500 (SPI) | `thumbv8m.main-none-eabihf` |

The evaluation and adapter board table lives in
[BOARDS.md](BOARDS.md).

## Firmware architecture

Each board crate brings up its own SPI or RMII link, then hands an
`embassy_net::Stack<'static>` to the shared `ubos-*` library crates.
From that point every board runs the same above-the-link-layer tasks:
DHCP bring-up, NTP sync, the HTTP listeners, the Prometheus metrics
endpoint, the watchdog, and the display where present.

The link layer differs by board. WIZnet boards use
`embassy-net-wiznet` in MACRAW mode, so smoltcp owns the full TCP/IP
stack and one code path covers the W5500, W5100S, and W6100. The STM32
boards use the on-chip RMII MAC via `embassy-stm32::eth`; their PHY
wiring is in [BOARDS.md](BOARDS.md).

## Mining backend

Every board drives a real BM13xx hashboard via the `ubos-mining`
crate. The backend pushes shares into the
`stratum-client::AsicSession` solution channel that the V1 and V2
client tasks drain, and its accumulated hashes feed the EWMA hashrate
in `ubos-metrics`. See [MINING.md](MINING.md).
