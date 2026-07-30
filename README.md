# ubos-main

Bare-metal Rust miner firmware. The published board is the Braiins
Forge Miner x4 (`firmware/hashnode`), an RP2350B driving a 4-chip
BM1370 hashboard over Ethernet. Each board serves a small HTTP
dashboard that polls `/api/info` and renders uptime, DHCP-assigned IP,
current hashrate from the on-board BM13xx ASIC via the `ubos-mining`
driver, on-board temperature, fan speed and PWM duty, and UTC time via
NTP.

## Start here

Docs live in `docs/`:

- `docs/OVERVIEW.md` -- workspace map, board table, firmware architecture
- `docs/SETUP.md` -- probes, wiring, pinouts, building and flashing
- `docs/MINING.md` -- BM1370 hashboard backend: chip serial, PSU, tuning
- `docs/METRICS.md` -- Prometheus exposition, CPU-busy ratio model
- `docs/TESTING.md` -- layered test architecture and coverage strategy
- `docs/DEBUGGING.md` -- probe-rs, binary size, stack analysis

## Third-party material

A few files in the tree are third-party material redistributed under
their own licenses, each kept next to its own license text: the IBM
Carbon icon set in `ubos/ubos-webui/assets/icons.svg` (Apache-2.0, full
text in
[`ubos/ubos-webui/assets/LICENSE-CARBON`](ubos/ubos-webui/assets/LICENSE-CARBON)),
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
