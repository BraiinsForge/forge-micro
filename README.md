# Ethernet Examples

Bare-metal Rust Ethernet across six boards. Each serves a small HTTP
dashboard that polls `/api/info` and renders uptime, DHCP-assigned IP,
current hashrate (from the dummy-asic peer, an on-MCU CPU miner, or
the on-board mmm ASIC backend), on-board temperature, fan speed and
PWM duty, and UTC time via NTP.

## Start here

Docs live in `docs/`:

- `docs/OVERVIEW.md` -- workspace map, board table, mining-backend matrix
- `docs/SETUP.md` -- probes, wiring, pinouts, building and flashing
- `docs/DUMMY_ASIC.md` -- UART protocol, solver internals, flashing
- `docs/MMM.md` -- BM1362 hashboard backend on the BMM Adapter
- `docs/METRICS.md` -- Prometheus exposition, CPU-busy ratio model
- `docs/DEBUGGING.md` -- probe-rs, binary size, stack analysis

Support node (NUC): see `redacted/README.md`.
