# ubos-main

Bare-metal Rust Ethernet across six boards. Each serves a small HTTP
dashboard that polls `/api/info` and renders uptime, DHCP-assigned IP,
current hashrate from the on-board BM1362 ASIC via the `ubos-mining` driver, on-board
temperature, fan speed and PWM duty, and UTC time via NTP.

## Start here

Docs live in `docs/`:

- `docs/OVERVIEW.md` -- workspace map, board table, firmware architecture
- `docs/SETUP.md` -- probes, wiring, pinouts, building and flashing
- `docs/MINING.md` -- BM1362 hashboard backend on the BMM Adapter
- `docs/METRICS.md` -- Prometheus exposition, CPU-busy ratio model
- `docs/TESTING.md` -- layered test architecture and coverage strategy
- `docs/DEBUGGING.md` -- probe-rs, binary size, stack analysis

Support node (NUC): see `nuc/README.md`.

## License

This program is free software licensed under the terms of the GNU
General Public License, version 3 or (at your option) any later version.
See [`LICENSE`](LICENSE) for the full text.

Braiins Systems s.r.o. reserves the right to license this program, or
any part thereof, under separate terms.
