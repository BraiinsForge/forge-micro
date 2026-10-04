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
`ubos/ubos-thermal/src/array.rs`,
`ubos/ubos-metrics/src/mcu_temperature.rs`.

Four gauges in this section carry a temperature, and each one has a
single writer. A fifth is the per-chain chip temperature, under
"Per-chain PVT series" below.

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
- `board_lm75a_temperature_millicelsius` is a per-sensor reading from
  each LM75A on the board, labelled by I2C address.
  `ubos_thermal::array::SensorArray` writes it beside the aggregate the
  chip gauge carries.
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

A board that reports one chip-temperature estimate publishes
`board_chip_temperature_millicelsius` with no label. A board that reports
a real die reading and a PCB estimate publishes one series for each:

- `source="chip"` comes from a remote diode wired to a chip die. This
  series selects the `dangerous_chip_temp_c` threshold.
- `source="approximated"` is a PCB reading plus the fixed offset that the
  sensor table gives for that sensor.

### Per-sensor series

`SensorArray` publishes one series for each PCB sensor whose driver has a
gauge name. Today the LM75A is the only such driver:
`board_lm75a_temperature_millicelsius{addr="0x48"}`. The value carries
the same PCB-to-die offset as the estimate above.

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
- `board_pool_work_shares_total{pool=...}` — diff-1 chip shares per
  originating slot; the ratio between these series is the work
  split the dispatch scheduler actually delivered.

`board_uart_desync_bytes_total` and `board_pool_work_shares_total` also
carry a `chain="i"` label identifying the hashboard chain they came
from, since a board can own more than one; see `ubos_metrics::chain`.

There are no unlabelled duplicates of these series; board totals are
`sum(...)` over the label.

## Per-chain state

Code: `ubos/ubos-mining/src/publishers.rs`.

Five state series are labelled `chain="i"`. Each is pushed once per chain the
machine wired up, which `ChainStatsHandle::chain_count` reports. The
unused slots of the `MAX_CHAINS`-sized table are never published, and
no per-chain series is emitted before the miner has started:

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
- `board_hashrate_hps` — the smoothed hashrate of the chain's own
  estimator.
- `board_shares_total` — the chain's lifetime count of diff-1 chip
  shares.

There are no unlabelled duplicates of the last two. A board total is
`sum(...)` over the `chain` label, as for the per-pool series above.
They describe chip output, not pool credit.

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

`HashrateEstimator::record_share` feeds a share's difficulty into an
EWMA-smoothed bucketed estimator over 1-second buckets with α = 1/8
(~8 s smoothing window). `hashrate` reads the smoothed value and applies
read-side decay so the displayed rate drops toward zero when shares stop
arriving. Each board hashboard chain owns one estimator, inside that
chain's `ChainStats`; the ubos-mining serial actor calls `record_share`
on its own chain's `ChainStats` for every diff-1 chip share it consumes.
`ChainStatsHandle::machine_hashrate` and `machine_share_count`, in the
same module,
sum every chain's estimator for the board-wide figures the REST API and
the OLED display show.
