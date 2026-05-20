# Ethernet Examples

Bare-metal Rust Ethernet across six boards plus a UART-attached mining
peer, a Linux host-side runner, and the libraries that back them. Each
firmware serves an HTTP metrics page with uptime, request count, current
hashrate, BTC price via Binance over TLS, and UTC time via NTP.

## Start here

Docs live in `docs/`:

- `docs/OVERVIEW.md` -- workspace map, board table, mining-backend matrix
- `docs/SETUP.md` -- probes, wiring, pinouts, building and flashing
- `docs/DUMMY_ASIC.md` -- UART protocol, solver internals, flashing
- `docs/MMM.md` -- BM1362 hashboard backend on the BMM Adapter
- `docs/METRICS.md` -- Prometheus exposition, CPU-busy ratio model
- `docs/DEBUGGING.md` -- probe-rs, binary size, stack analysis

Support node (NUC): see `redacted/README.md`.
