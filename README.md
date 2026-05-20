# ubos-main

Bare-metal Rust Ethernet across six boards. Each serves a small HTTP
dashboard that polls `/api/info` and renders uptime, DHCP-assigned IP,
current hashrate from the on-board BM1362 ubos-miner ASIC backend, on-board
temperature, fan speed and PWM duty, and UTC time via NTP.

## Start here

Docs live in `docs/`:

- `docs/OVERVIEW.md` -- workspace map, board table, firmware architecture
- `docs/SETUP.md` -- probes, wiring, pinouts, building and flashing
- `docs/MINER.md` -- BM1362 hashboard backend on the BMM Adapter
- `docs/METRICS.md` -- Prometheus exposition, CPU-busy ratio model
- `docs/DEBUGGING.md` -- probe-rs, binary size, stack analysis

Support node (NUC): see `nuc/README.md`.
