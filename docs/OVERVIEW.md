# Overview

The firmware workspace is bare-metal Rust for Ethernet boards. It holds
per-board firmware crates, the libraries that back them, and a host-side
runner that mirrors the firmware tasks against a Linux network device.

## Boards

| Crate | MCU | Ethernet | Target |
|-------|-----|----------|--------|
| `firmware/hashnode` | RP2350B (Braiins Forge Miner x4) | W5500 (SPI) | `thumbv8m.main-none-eabihf` |

The evaluation and adapter board table lives in
[BOARDS.md](BOARDS.md).

## Firmware architecture

Each board crate brings up its own SPI / RMII link, then hands an
`embassy_net::Stack<'static>` to the shared `ubos-*` library crates. From
that point every board runs the same above-the-link-layer tasks:

- DHCP wait and bring-up orchestration
- NTP sync loop
- Prometheus metrics HTTP endpoint
- Watchdog
- Display (where present)

The link-layer differs by board:

- WIZnet boards, the hashnode among them, use `embassy-net-wiznet`
  in MACRAW mode so smoltcp owns the full TCP/IP stack. One code path covers
  W5500/W5100S/W6100.
- The STM32 boards use the on-chip RMII MAC via `embassy-stm32::eth`;
  their PHY wiring is in [BOARDS.md](BOARDS.md).

The HTTP response is hand-rolled into caller-owned buffers because the
firmware serves a single metrics page and does not justify a full web
framework.

### Display

Code: `ubos/ubos-display/src/display.rs`.

Line 3 of the 4-line display layout shows fan PWM percentage and RPM on boards
that have cleared the `FAN_PWM_PERCENT` sentinel (i.e. boards where `BoardFan`
was constructed, which stores `0` to signal that a fan is present). On boards
without a fan, `FAN_PWM_PERCENT` retains its sentinel value and line 3 is
left blank.

## Mining backend

Every board drives a real BM13xx hashboard via the `ubos-mining` crate. The
ubos-mining backend pushes shares into the `stratum-client::AsicSession`
solution channel that the V1 and V2 client tasks drain. Hashes accumulated
by the backend feed `ubos_metrics::hashrate` for the EWMA-smoothed rate the
Prometheus endpoint and display surface. The ubos-mining backend ties together
the BM13xx chip-serial actor, the board's PSU driver, and a runtime
baud-rate switch; see [MINING.md](MINING.md).

## Workspace crates

### Shared firmware libraries

- `ubos/ubos-core/` -- consts, watchdog, LED, typed units (`Frequency`,
  `Voltage`, `Temperature`).
- `ubos/ubos-config/` -- runtime `Config` watch and `ConfigHandle`, with
  flash-backed `sequential-storage` persistence behind the
  `flash-storage` feature.
- `ubos/ubos-manager/` -- supervisor that routes `Message::Halt` and
  `Message::Resume` to the mining, cooling, and thermal handles when
  pause reasons arrive (thermal trip, fan fault, operator pause, reconfigure).
- `ubos/ubos-net/` -- network bring-up, NTP, stratum connect, atomic state.
- `ubos/ubos-metrics/` -- HTTP server, metrics, CPU-load accounting,
  EWMA hashrate accounting.
- `ubos/ubos-display/` -- SH1106 driver and render task.
- `ubos/ubos-thermal/` -- sensor poll, `ThermalHandle` publisher,
  safety trip asserted as `PauseReason::DangerousTemperature` on
  `ManagerHandle`.
- `ubos/ubos-cooling/` -- fan PID controller and `FanOutput` HAL trait.
- `ubos/ubos-logging/` -- macro facade that swaps between `defmt` and
  `log` at compile time, so the same crate builds for firmware and host.

### Mining stack

- `ubos/mining-protocol/` -- protocol-agnostic adapter trait between a
  stratum client task and the work generator.
- `ubos/stratum-v1/` -- Stratum V1 message types, parser, and adapter impl.
- `ubos/stratum-v2/` -- Stratum V2 message types, noise handshake, and
  adapter impl.
- `ubos/stratum-client/` -- shared work-generator task, `JobRing`,
  `WorkRing`, and `AsicSession` ASIC abstraction; generic over the
  protocol adapter.
- `ubos/ubos-mining/` -- BM13xx chip driver: frame layouts, parser, miner
  state machine, and the single-task chip-serial actor.

### Hardware drivers

- `ubos/rp235x-sha256/` -- polled driver for the RP2350 SHA-256 accelerator.
- `ubos/ubos-eeprom/` -- hashboard identity EEPROM wire format and read
  path; generic over `embedded-hal-async::i2c`.
- `ubos/ubos-psu/` -- async PSU driver for the BMM101 miniminer hashboard
  plus the INA230 current monitor; generic over `embedded-hal-async::i2c`.
- `ubos/ubos-temperature/` -- temperature sensor abstraction and the
  LM75A driver.
- `ubos/serial-baudrate/` -- `BaudRate` trait, orthogonal to `embedded-io`,
  for runtime baud switching from a `&self` receiver.

### Host-side

- `host/host-runner/` -- Linux runner that re-uses the firmware tasks
  against a TAP device (or an `AF_PACKET` raw socket). Also wires up an
  hwmon fan controller, optional `UBOS_I2C_BUS` PSU exercise, and an
  optional real BM1362 hashboard.
- `host/embassy-net-afpacket/` -- `embassy-net` driver bound to a Linux
  NIC via `AF_PACKET`/`SOCK_RAW`, with synthetic-MAC + promiscuous-mode
  option.
- `host/embedded-serial-unix/` -- async tty driver implementing
  `embedded-io-async` and `serial-baudrate::BaudRate`.
- `host/embedded-i2c-unix/` -- async `embedded-hal-async::i2c::I2c` over
  Linux `/dev/i2c-N` via `I2C_RDWR` ioctls.
- `host/ubos-main-host/` -- integration harness wiring the firmware
  tasks to the host-runner transport.
