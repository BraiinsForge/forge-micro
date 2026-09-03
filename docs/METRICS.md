# Metrics

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

Each firmware exposes a Prometheus text endpoint on
`http://<board-ip>:<Metrics::PORT>/`. A Prometheus server scrapes it on
an interval.

## Endpoint model

Code: `ubos/ubos-metrics/src/metrics.rs`.

Any async context pushes a `Metric` value into a static `Channel` via
`push`. The `run` task owns a local `FnvIndexMap` and drains the channel,
upserting by name + label set. On each TCP connection it drains any
pending pushes first, then renders the full map plus a fixed set of
derived metrics (uptime, request count, NTP sync state) in
Prometheus exposition format 0.0.4.

Every response, including 404 and 405, carries
`Access-Control-Allow-Origin: *` so the dashboard on port 80 can read
the endpoint cross-origin for the state snapshot download and sees the
real HTTP status of an error instead of an opaque CORS failure. The
wildcard is safe because the listener is unauthenticated, and it
requires the browser to fetch without credentials.

## CPU busy ratio

Code: `ubos/ubos-metrics/src/cpu_load.rs`.

Each board brackets its executor's idle wait with `Instant::now()` and
adds the elapsed ticks to the per-core slot in `IDLE_TICKS`. The publisher
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

## Board temperature

Code: `ubos/ubos-thermal/src/metrics.rs`,
`ubos/ubos-thermal/src/array.rs`,
`ubos/ubos-metrics/src/mcu_temperature.rs`.

Two gauges carry a temperature, and each one has a single writer:

- `board_chip_temperature_millicelsius` is the hashboard chip
  temperature. `ubos_thermal::run_thermal` writes it once per configured
  sample interval, and the same value gates the thermal shutdown.
- `board_mcu_temperature_millicelsius` is the temperature of the MCU die
  on the control board. `ubos_metrics::mcu_temperature` writes it once
  per second, on the boards whose MCU carries an internal thermometer:
  esp32s3, stm32f407 and stm32f439.

The two gauges measure different hardware. Under load a control board is
tens of degrees cooler than a hashboard.

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

There are no unlabelled duplicates of these series; board totals are
`sum(...)` over the label. `board_shares_total` and the hashrate
estimator stay global because they describe chip output, not pool
credit.

## Hashrate

Code: `ubos/ubos-metrics/src/hashrate.rs`.

`record_share` bumps `SHARE_COUNT` and feeds the share's difficulty into
an EWMA-smoothed bucketed estimator over 1-second buckets with α = 1/8
(~8 s smoothing window). `hashrate` reads the smoothed value and applies
read-side decay so the displayed rate drops toward zero when shares stop
arriving. The ubos-mining backend calls `record_share` for every share it
consumes (one call per diff-1 chip share); see `ubos_mining::serial`.
