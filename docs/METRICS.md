# Metrics

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

Each firmware exposes a Prometheus text endpoint on
`http://<board-ip>:<Metrics::PORT>/`. A Prometheus server scrapes it on
an interval.

## Endpoint model

Code: `ubos/ubos-metrics/src/metrics.rs`.

Any async context pushes a `Metric` value into a static `Channel` via
`push`. Each board declares one `MetricsStatics` with `metrics_statics!`,
which holds the series map and the response buffer. `serve_all!` sizes
it from the `MAX_METRICS` and `METRICS_RESPONSE_BYTES` constants in the
board's `config.rs`. The `run_ingest` task is the only writer of the
map. It drains the channel and upserts by name and label
set. On each TCP connection, the exposition service reads the map. It
renders the full map and a fixed set of derived metrics (uptime,
request count, NTP sync state) in Prometheus exposition format 0.0.4.

Every push is one sample of one value, and it carries the time of the
push. A sample says nothing about the value between two pushes, so no
series holds a value and no series is a state. Each publisher therefore
pushes on a ticker. A push on a change is one more sample, not a
replacement for the ticker.

The stored time is monotonic milliseconds since boot. The exposition
adds the NTP epoch offset at scrape time and renders an absolute
Unix-epoch timestamp. If NTP has not synced, the exposition omits the
timestamp and Prometheus stamps the sample at scrape time.

Every response, including 404 and 405, carries
`Access-Control-Allow-Origin: *` so the dashboard on port 80 can read
the endpoint cross-origin for the state snapshot download and sees the
real HTTP status of an error instead of an opaque CORS failure. The
wildcard is safe because the listener is unauthenticated, and it
requires the browser to fetch without credentials.

## CPU busy ratio

Code: `ubos/ubos-metrics/src/cpu_load.rs`.

Each board brackets its executor's idle wait with `Instant::now()` and
adds the elapsed ticks to the per-core slot through `record_idle`. The publisher
task periodically diffs each slot against wall-clock ticks and pushes
`1.0 - idle / elapsed` as `board_cpu_busy_ratio`, labelled `cpu="0"`,
`cpu="1"`, ..., up to the per-board active-core count. The ratio falls
back to `0.0` when no time has elapsed between samples.

Two seams exist depending on the runtime:

- On Cortex-M boards, `run_cortex_m_executor` drives
  `raw::Executor::poll()` under `#[cortex_m_rt::entry]` and brackets
  `cortex_m::asm::wfe()` with `Instant::now()` reads.
- On the ESP32-S3, `esp_rtos::embassy::Executor::run_with_callbacks`
  exposes `before_poll` and `on_idle` hooks; `on_idle` records the entry
  instant and `before_poll` accumulates the delta on the following
  iteration.

### Caveats

Interrupt handlers that wake the wait instruction run before the second
`Instant::now()` and are accounted as idle. The metric is a coarse
executor-level load indicator, not a true busy-cycle count.

On multicore boards the cortex-m thread-mode pender issues `sev`, which
sets the global event register on every core. A wake on one core
unblocks the other core's `wfe` even when that core has no work, so a
quiet core's idle counter is undercounted whenever a busy core makes
progress.

The result is clamped to `0.0..=1.0` to defend against accounting skew
and `f32` rounding in the division. The metric is a coarse ratio rather
than a precise count.

### Origin

The technique of driving `raw::Executor::poll()` manually and bracketing
`wfe` with `Instant::now()` reads is from Giacomo Caironi's "Measuring
CPU usage with Rust Embassy"
(<https://www.giacomocaironi.dev/posts/measuring-cpu-usage-with-rust-embassy/>,
reference impl at <https://gitlab.com/giacomocaironi/embassy-cpu-usage>).

## Voltage

Code: `ubos/ubos-psu/src/lib.rs`,
`ubos/ubos-mining/src/publishers.rs`,
`ubos/ubos-thermal/src/chain.rs`.

`board_psu_output_voltage_set_mv` is the commanded hashboard rail.
`board_psu_output_voltage_measured_mv` is a physical reading of that rail;
a zero value on a board without a voltage sensor means unavailable.
`hashboard_chain_voltage_microvolts` is different:
it sums the on-die core voltages reported by the chips. It remains a
separate metric because it is not a PSU rail measurement.
There is no unlabelled miner voltage series: the confirmed setpoint is
`board_psu_output_voltage_set_mv` and it has no duplicate.

## Miner lifecycle

`board_miner_running` becomes 1 after present chains start receiving work.
It is 0 while the miner waits for a pool, during bring-up and after shutdown.
`board_miner_state{state="..."}` carries one series per `MiningState`,
spelled as `MiningState::as_api_str` does; the current state reads 1
and the others 0, and every series reads 0 before the first publish.
OTA waits for the miner's `Stopped` state, independent of this metric.

## Board temperature

Code: `ubos/ubos-thermal/src/metrics.rs`,
`ubos/ubos-thermal/src/array.rs`, `ubos/ubos-thermal/src/chain.rs`,
`ubos/ubos-metrics/src/mcu_temperature.rs`.

Four gauges in this section carry a temperature. A fifth is the
per-chain chip temperature, under "Per-chain PVT series" below.

- `board_chip_temperature_millicelsius` is the hashboard chip
  temperature. `ubos_thermal::run_thermal` writes it on every tick that
  measures a chip temperature, and the same value gates the thermal
  shutdown. A tick that measures nothing leaves the last written value
  in place.
- `board_ambient_temperature_millicelsius` is the ambient temperature around
  the board. `ubos_thermal::run_thermal` reads the ambient sensor on
  every tick, on a board that measures its air, including one that
  measured no chip temperature and one that published no reading at
  all. The gauge is written on every tick whose read returns a value;
  a tick whose sensor answers nothing leaves the last written value in
  place. It carries no label, because a board reports one ambient
  reading or none.
- `board_temperature_millicelsius` is every reading that a sensor takes.
  `ubos_thermal::array::SensorArray` writes the readings of its sensors
  on every tick, and `ubos_thermal::ChainTemperature` writes the reading
  of each chip together with the per-chain PVT series. See "Reading
  series" below.
- `board_mcu_temperature_millicelsius` is the temperature of the MCU die
  on the control board. `ubos_metrics::mcu_temperature` writes it once
  per second, on the boards whose MCU carries an internal thermometer:
  esp32s3, stm32f407 and stm32f439.

The four gauges measure different hardware. Under load a control board
is tens of degrees cooler than a hashboard, and the air is cooler again.

What the ambient reading is and is not for is stated once, on
`ubos_thermal::AmbientSensor`. With the hashboard rail down it is
still measured, where the chip gauge is only held; see "Two ways out of
an overheat halt" in `docs/MINING.md`.

No gauge marks a held value. A gauge keeps what was last written to it,
so a consumer that must tell a held value from a fresh one reads
`chip_reading_stale` from `/api/info` instead. The ambient gauge can
also run ahead of the watch: a tick that publishes no reading still
writes the gauge, so the two carry different ambient values until the
next tick that publishes.

### The `source` label

`board_chip_temperature_millicelsius` always carries a `source` label. A
board that reports a chip reading and a hashboard reading publishes one
series for each:

- `source="chip"` is the hottest chip reading. This series selects the
  `dangerous_chip_temp` threshold.
- `source="hashboard"` is the hottest hashboard reading. This series
  selects the `dangerous_hashboard_temp` threshold.

### Reading series

Each reading that a sensor takes has its own series under
`board_temperature_millicelsius`. The `source` label tells what the
reading measures, and the other labels tell where it comes from. A
series carries only the labels that apply to it:

| Reading | Series |
|---------|--------|
| Hashboard | `board_temperature_millicelsius{source="hashboard",chain="0",addr="0x48"}` |
| Chip | `board_temperature_millicelsius{source="chip",chain="0",chip="5"}` |
| Ambient | `board_temperature_millicelsius{source="ambient",addr="0x49"}` |
| Control board | `board_temperature_millicelsius{source="control_board",addr="0x48"}` |
| PSU | `board_temperature_millicelsius{source="psu",addr="0x48"}` |

The value is the reading that the sensor measured, with no offset.
`chain` is the chain index that the `hashboard_chain_*` series also use.
Only hashboard and chip readings have a chain.
`addr` is the I2C address of the sensor that took the reading. Every
series except a chip series carries it.
`chip` is the index of the chip on its chain. A chip has the same series
whether a TMP461 diode read it or the chain did.

No board has a sensor table entry for the air, the control board or the
PSU yet. The ambient and MCU gauges above still carry those readings.

### Units

Every temperature gauge is an integer in millicelsius. The rp2040 is a
Cortex-M0+ with no FPU, so a float-valued celsius gauge adds software
floating-point to the exposition path. Millicelsius keeps sub-degree
resolution without soft-float.

## Per-pool series

Code: `ubos/stratum-client/src/publishers.rs`,
`ubos/ubos-mining/src/publishers.rs`.

Boards run one stratum session per configured pool slot, and the
share counters are per-slot. The publishers emit one series per
board pool slot, labelled `pool="0"`, `pool="1"`, ...:

- `board_shares_accepted_total{pool=...}` — pool-accepted shares.
- `board_shares_rejected_total{pool=...}` — pool-rejected shares.
- `board_stratum_reconnects_total{pool=...}` — reconnect attempts.

The miner metrics task pushes one more series, and no stratum session
writes it:

- `board_pool_work_shares_total{chain=...,pool=...}` — chip shares
  for the slot that supplied the work, in the unit below. The ratio
  between these series is the work split that the dispatch scheduler
  delivered. The `chain` label names the chain that found the shares,
  because a board can own more than one. See `ubos_metrics::chain`.

It pushes this series for every slot the board compiles in, so a slot
with no pool in the config still has it.

A share is 2^32 expected hashes. A chip solution found against a
difficulty `d` filter adds `d` to the counter, so the counter times
2^32 is the expected hash count on every chip family, and
`rate(...) * 2^32` is a hashrate in H/s whatever difficulty the chip
filters at.

There are no unlabelled duplicates of these series. A board total is
`sum(...)` over the label.

## Per-chain series

Code: `ubos/ubos-mining/src/publishers.rs`.

Six series are labelled `chain="i"`. Each is pushed once for each chain
the machine wired up, which `ChainStatsHandle::chain_count` reports. The
unused slots of the `MAX_CHAINS`-sized table are never published, and no
per-chain series is pushed before the miner metrics task starts:

- `board_chain_chips` — the chain's chip count from its most recent
  bring-up, or zero for a chain that is absent, faulted, or not yet
  enumerated. A faulted chain keeps the count it found in
  `ChainStats::presence()`. No gauge and no endpoint reports that count.
- `board_chain_present` — 1 when `ChainStats::presence()` is `Present`,
  0 otherwise (absent, faulted, or not yet enumerated).
- `board_chain_frequency_khz` — the last frequency the chain's serial
  actor confirmed. This is the only frequency series; there is no
  unlabelled machine-wide one, because each chain carries its own clock.
  Read it with `board_chain_present`: a chain held in reset keeps its
  last frequency until the actor resets it during bring-up.
- `board_uart_desync_bytes_total` — the chain's lifetime count of UART
  bytes that the driver discarded, on a failed CRC or to find the start
  of a frame.
- `board_hashrate_hps` — the smoothed hashrate of the chain's own
  estimator.
- `board_shares_total` — the chain's lifetime chip shares, in the
  same unit as `board_pool_work_shares_total`.

No series here has an unlabelled duplicate. A board total is `sum(...)`
over the `chain` label, as for the per-pool series above.
`board_hashrate_hps` and `board_shares_total` describe chip output, not
pool credit.

## Per-chain PVT series

Code: `ubos/ubos-thermal/src/chain.rs`.

On a board whose chips are its only thermometer, `ChainTemperature`
publishes two more gauges every ten seconds, from inside the thermal
sample. Both are labelled `chain="i"`, and only a present chain reports:

- `hashboard_chain_voltage_microvolts` — the sum of the core
  voltages the chain's chips report about themselves. The chips sit in
  series across one rail, so this is the string voltage.
- `hashboard_chain_chip_temperature_millicelsius{stat=...}` — the spread
  of die temperatures across the chain, as three series: `lowest`, `mean`
  and `highest`.

The spread is what says whether one die is running away from the rest.
The hottest die itself is not here: the thermal loop already publishes it
as `board_chip_temperature_millicelsius`, the sample it took.

## Hashrate

Code: `ubos/ubos-metrics/src/hashrate.rs`, `ubos/ubos-metrics/src/chain.rs`.

`HashrateEstimator::record_share` feeds a solution's difficulty into an
EWMA-smoothed bucketed estimator over 1-second buckets with α = 1/8
(~8 s smoothing window), and adds the same difficulty, in shares of
2^32 hashes, to the share counter. `hashrate` reads the smoothed value
and applies read-side decay so the displayed rate drops toward zero
when solutions stop arriving. Each board hashboard chain owns one
estimator, inside that chain's `ChainStats`; the ubos-mining serial
actor calls `record_share` on its own chain's `ChainStats` for every
chip solution it consumes.
`ChainStatsHandle::machine_hashrate` and `machine_share_count`, in the
same module,
sum every chain's estimator for the board-wide figures the REST API and
the OLED display show.
