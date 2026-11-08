# Firmware workspace

Bare-metal Rust miner firmware. The product boards are the Braiins
Forge Miner x4 (`firmware/hashnode`), an RP2350B driving a 4-chip
BM1370 hashboard over Ethernet, and the Forge Axe
(`firmware/forge-axe`), an RP2350B driving the same
hashboard; alongside them the tree carries four evaluation
and adapter boards across three MCU parts — RP2040, RP2350, and
ESP32-S3 — which is what keeps the OS portable rather
than shaped to one part. Each board serves a small HTTP dashboard that
polls `/api/info` and renders uptime, DHCP-assigned IP, current
hashrate from the on-board BM13xx ASIC via the `ubos-mining` driver,
on-board temperature, fan speed and PWM duty, and UTC time via NTP.

## Naming

This repository has two names. On our GitLab instance it is `ubos-main`.
A push mirror publishes it on GitHub as `forge-micro`. Both names are
real, and both name this tree.

No other file names either one. Prose in this tree calls it the firmware
workspace, and calls the private repository that consumes it the internal
repository.

## Start here

Docs live in `docs/`:

- `docs/OVERVIEW.md` -- workspace map, board table, firmware architecture
- `docs/SETUP.md` -- probes, wiring, pinouts, building and flashing
- `docs/BOARDS.md` -- per-board pin assignments, size budget, OTA specifics
- `docs/MINING.md` -- BM1370 hashboard backend: chip serial, PSU, tuning
- `docs/METRICS.md` -- Prometheus exposition, CPU-busy ratio model
- `docs/DEVFEE.md` -- the fee slot: the build switch, the values, the API and the metrics
- `docs/TESTING.md` -- layered test architecture and coverage strategy
- `docs/DEBUGGING.md` -- probe-rs, binary size, stack analysis

## Third-party material

A few files in the tree are third-party material redistributed under
their own licenses, each kept next to its own license text: the IBM
Carbon icon set in `ubos/ubos-webui/web/icons.svg` (Apache-2.0, full
text in
[`ubos/ubos-webui/web/LICENSE-CARBON`](ubos/ubos-webui/web/LICENSE-CARBON)),
the probe-rs udev rules at `assets/69-probe-rs.rules` (GPL-2.0, full
text in [`assets/LICENSE-PROBE-RS`](assets/LICENSE-PROBE-RS)), and the
upstream sources the `patches/` series modify (see
[`patches/README.md`](patches/README.md)).

## License

This program is free software licensed under the terms of the GNU
General Public License, version 3 or (at your option) any later version.
See [`COPYING`](COPYING) for the full text.

One bundled file is licensed differently: `assets/69-probe-rs.rules`
is upstream probe-rs material under the GNU General Public License,
version 2.0 (full text in
[`assets/LICENSE-PROBE-RS`](assets/LICENSE-PROBE-RS)). It is a standalone
configuration file and is not combined with the GPL-3.0-or-later sources.

Braiins Systems s.r.o. reserves the right to license this program, or
any part thereof, under separate terms.

If you want to contribute, read [`CONTRIBUTING.md`](CONTRIBUTING.md)
first. It states what that reservation means for your contribution.
