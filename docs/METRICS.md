# Metrics

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

Each firmware exposes a Prometheus text endpoint on
`http://<board-ip>:<Metrics::PORT>/`. A Prometheus server scrapes it on
an interval.

## Endpoint model

Code: `ubos/ubos-metrics/src/metrics.rs`, `ubos/ubos-metrics/src/store.rs`,
`ubos/ubos-metrics/src/value.rs`.

Any async context pushes a `Metric` value into a static `Channel` with
`Metric::push`, which waits while the channel is full, or
`Metric::try_push`, which drops the sample instead. The value of a
`Metric` is a typed unit from `ubos_core::units`, a
`ubos_metrics::value::Ratio`, a flag or a count. See "Units" below. A
push site builds the `Metric` with the `metric!` macro. See "Names"
below.

Each board declares
one `MetricsStatics` with `metrics_statics!`, which holds the series
map and the response buffer. `serve_all!` sizes it from the
`MAX_METRICS` and `METRICS_RESPONSE_BYTES` constants in the board's
`config.rs`. The `run_ingest` task is the only writer of the map. It
drains the channel and upserts by name and label set. On each TCP
connection, the exposition service reads the map. It
renders every series that has not expired and a fixed set of derived
metrics (uptime, request count, NTP sync state) in Prometheus
exposition format 0.0.4.

Every push of a measured series is one sample of one value, and it
carries the time of the push. A sample says nothing about the value
between two pushes, so a measured series holds no value and is no
state. Each publisher of a measured series therefore pushes on a
ticker. A push on a change is one more sample, not a replacement for
the ticker. A constant series is the exception, see "Expiry".

The stored time is monotonic milliseconds since boot. The exposition
adds the NTP epoch offset at scrape time and renders an absolute
Unix-epoch timestamp. The exposition omits the timestamp of a constant
series, and of every series while NTP has not synced. Prometheus then
stamps the sample at scrape time.

### Expiry

A series leaves the exposition when its publisher stops pushing it. The
render hides a series when the age of its sample is more than its TTL.
The age is the monotonic time now minus the time of the sample. The map
keeps the entry until a push finds the map full. That push removes every
expired entry first. The expiry does not depend on NTP, so it works
before the first sync.

A measured series has a TTL. A series without its own TTL uses
`Metrics::DEFAULT_TTL`, which is 180 seconds. This is three times the
slowest push interval of 60 seconds, so two lost samples in a row do
not remove a series. A publisher with its own rate calls
`Metric::ttl` with its push interval times `Metrics::TTL_INTERVALS`.
`Metric::ttl` rounds up to whole seconds.

A constant series never expires and has no timestamp. Prometheus stamps
it at scrape time. Call `Metric::constant` to make one, and push it
with `push`, so the channel never drops it. The last call of `ttl` and
`constant` wins.

Prometheus does not mark a sample stale when it has an explicit
timestamp and then disappears. The series stays visible until five
minutes after its last timestamp. A TTL below five minutes does not
shorten this. The TTL matters for a series before the first NTP sync,
for a reader that is not Prometheus, and for the size of the exposition.

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
the busy ticks over the elapsed ticks as `cpu_busy_ratio`, labelled
`cpu="0"`, `cpu="1"`, ..., up to the per-board active-core count. The
value is a `Ratio` in thousandths, and the math uses integers only. The
ratio is 0 when no time elapsed between two samples.

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

The result is clamped to `0.0..=1.0` to defend against accounting skew.
The division uses integers. The metric is a coarse ratio rather than a
precise count.

### Origin

The technique of driving `raw::Executor::poll()` manually and bracketing
`wfe` with `Instant::now()` reads is from Giacomo Caironi's "Measuring
CPU usage with Rust Embassy"
(<https://www.giacomocaironi.dev/posts/measuring-cpu-usage-with-rust-embassy/>,
reference impl at <https://gitlab.com/giacomocaironi/embassy-cpu-usage>).

## Names

Code: `ubos/ubos-metrics/src/name.rs`, `ubos/ubos-metrics/src/catalog.rs`.

A push site builds its metric with the `metric!` macro and adds each
label with `Metric::label`:

```rust
metric!("chip_voltage_sum_volts", voltage)
    .label(HASHBOARD_LABEL, chain)
    .try_push();
```

A count and a flag have no unit type in `ubos_core::units`. A push site
wraps a count in `Count` and a flag in `Flag`. Both types are in
`ubos_metrics::value`:

```rust
metric!("pool_reconnects", Count(reconnects))
metric!("hashboard_present", Flag(present))
```

The macro checks the name when the crate is compiled. `Name::new` is the
check. It refuses a name that breaks one of these rules:

- A name has lowercase letters, digits and single underscores only.
- A name does not start with `board_`. It starts with `hashboard_` only
  if the hashboard is the noun of the series, as in `hashboard_present`.
- A name does not end in a scaled unit such as `_mv` or `_percent`.
- A name of a typed unit ends in the suffix of that unit: `_celsius`,
  `_volts`, `_watts`, `_amperes`, `_hertz` or `_ratio`.
- A name of a count, a flag, a fan duty or a fan speed ends in none of
  these suffixes.

The `nix` check `metric-constructor` refuses a direct call of
`Metric::new` or `Name::new` outside `ubos-metrics`, because such a call
can skip the check.

A name that one site pushes is a literal at that site. `catalog.rs`
holds each name and each label that more than one site uses.

The `hashboard` label is the index of the hashboard, from 0. It is the
chain index of the REST API. The `pool` label is the index of the pool
slot, from 0. A counter has no `_total` suffix.

## Voltage

Code: `ubos/ubos-psu/src/supply.rs`,
`ubos/ubos-mining/src/publishers.rs`,
`ubos/ubos-thermal/src/chain.rs`.

`psu_set_voltage_volts` is the voltage the supply landed the last
commanded hashboard rail on, which is the request rounded to the supply's
step.
`psu_voltage_volts` is a physical reading of that rail. Its `type` label
is `psu` for the supply's own readback and `feedback` for a
separate monitor on the rail, and a board that has both exports one series
for each. A zero value on a board without a voltage sensor means
unavailable.
`chip_voltage_sum_volts` is different:
it sums the on-die core voltages reported by the chips. It remains a
separate metric because it is not a PSU rail measurement.
There is no unlabelled miner voltage series: the confirmed setpoint is
`psu_set_voltage_volts` and it has no duplicate.

## Supply power and status

Code: `ubos/ubos-psu/src/supply.rs`.

`psu_power_watts` is the power of the supply. Its `type` label is
`input` or `output`. A sum over `type` adds the input to the output, so
a query selects one type.

`psu_status{flag="..."}` carries one series for each status flag of the
supply: `current_limit`, `thermal_shutdown`, `overvoltage` and
`undervoltage`. A series reads 1 while the supply raises its flag.

## Miner lifecycle

`miner_state{state="..."}` carries one series per `MiningState`,
spelled as `MiningState::as_api_str` does; the current state reads 1
and the others 0, and every series reads 0 before the first publish.
`miner_state{state="running"}` becomes 1 after present chains start
to receive work.
OTA waits for the miner's `Stopped` state, independent of this metric.

## Board temperature

Code: `ubos/ubos-thermal/src/metrics.rs`,
`ubos/ubos-thermal/src/array.rs`, `ubos/ubos-thermal/src/chain.rs`.

The series in this section carry a temperature. One more is the summary
of the chip temperatures, under "Per-hashboard PVT series" below, and
one more is the temperature the cooling PID follows, under "Cooling
and fans".

- `ambient_temperature_celsius` is the ambient temperature around
  the board. `ubos_thermal::run_thermal` reads the ambient sensor on
  every tick, on a board that measures its air, including one that
  measured no chip temperature and one that published no reading at
  all. The gauge is pushed on every tick whose read returns a value.
  If the channel is full, the push drops that sample. A tick whose
  sensor answers nothing writes nothing, so the gauge leaves the
  exposition when its TTL passes. It carries no label, because a board
  reports one ambient reading or none.
- The reading series hold every reading that a sensor takes.
  `ubos_thermal::array::SensorArray` pushes the readings of its sensors
  on every tick, and `ubos_thermal::ChainTemperature` pushes the reading
  of each chip together with the per-hashboard PVT series. If the
  channel is full, a push drops that sample. See "Reading series"
  below.
- `mcu_temperature_celsius` is the temperature of the MCU die
  on the control board. `ubos_thermal::run_thermal` writes it on every
  tick, from the board's `ubos_thermal::McuTemperatureSensor`, on the
  boards whose MCU carries an internal thermometer: esp32s3.

These gauges measure different hardware. Under load a control board
is tens of degrees cooler than a hashboard, and the air is cooler again.

What the ambient reading is and is not for is stated once, on
`ubos_thermal::AmbientSensor`. If the hashboard rail is down,
`ubos_thermal::run_thermal` still reads it. See "Two ways out of an
overheat halt" in `docs/MINING.md`.

No gauge marks a held value. A gauge keeps what was last written to it
until its TTL passes, so a consumer that must tell a held value from a
fresh one reads `/api/system/temperature` instead, which reports `null`
for a kind the last sample did not read. The ambient gauge can also run
ahead of the watch: a tick that publishes no reading still writes the
gauge, so the two carry different ambient values until the next tick
that publishes.

### Reading series

Each reading that a sensor takes has its own series. The name tells
what the reading measures, and the labels tell where it comes from:

| Reading | Series |
|---------|--------|
| Hashboard | `pcb_temperature_celsius{hashboard="0",id="0x48"}` |
| Chip | `chip_temperature_celsius{hashboard="0",chip_addr="5"}` |
| Ambient | `ambient_temperature_celsius{id="0x49"}` |
| Control board | `control_board_temperature_celsius{id="0x48"}` |
| PSU | `psu_temperature_celsius{id="0x48"}` |

The value is the reading that the sensor measured, with no offset.
Only hashboard and chip readings have a `hashboard` label.
`id` is the I2C address of the sensor that took the reading. Every
series except a chip series carries it.
`chip_addr` is the address of the chip on its hashboard. A chip has the
same series whether a TMP461 diode read it or the chain did.

No board has a sensor table entry for the air, the control board or the
PSU yet. The ambient and MCU gauges above still carry those readings.

### Units

A series that has a unit is in the base unit that its name states. The
base units are degrees Celsius, volts, watts, amps and hertz. A fan duty
cycle is in percent, and a fan speed is in RPM.

The store holds each value as its typed unit, for example a
`Temperature` in millicelsius. `Value` in `value.rs` is the only code
that writes a unit as text. It writes a unit held in thousandths with
three decimals, so 25 500 millicelsius is `25.500` and 21 600 mV is
`21.600`. The zeros at the end stay.

The render uses integer math only. The rp2040 is a Cortex-M0+ with no
FPU, and no board has an FPU for `f64`, so a float render adds software
floating-point to the exposition path.

## Cooling and fans

Code: `ubos/ubos-cooling/src/lib.rs`.

`ubos_cooling::run_cooling` writes these series on every step of the
cooling loop:

- `fan_speed_control` is the fan duty that the loop commands, from 0 to
  100.
- `fan_rpm` is the speed of a fan. A board with more than one
  tachometer adds the `fan` label.
- `pid_temperature_celsius{type="measured"}` is the value
  that the PID follows. The raw readings behind it are under
  "Reading series" above.
- `pid_temperature_celsius{type="target"}` is the setpoint
  for that value.
- `pid_source{source="...",stat="..."}` carries one series for
  each kind and statistic. The series that the PID follows reads 1, and
  the other three read 0.

If the fan mode is fixed, or a failsafe holds the fan at full, the PID
does not run. Then every `pid_source` series reads 0, and the PID
pushes no `pid_temperature_celsius` series, so both leave the
exposition when their TTL passes.

## Per-pool series

Code: `ubos/stratum-client/src/asic_session.rs`,
`ubos/ubos-mining/src/publishers.rs`.

A board runs one stratum session for each configured pool slot. Every
series here carries the slot in a label, `pool="0"`, `pool="1"`, and
more.

The session of a slot pushes three counters. It pushes them at each
reconnect attempt, after each pool message that moved a counter, and
every 2 s while the slot has a pool, through connect and backoff too.
A slot that has never had a pool in the config has no session, so it
has none of these three series. A slot whose pool is removed keeps
pushing its last counts every 2 s.

- `pool_submits{pool=...,type="accepted"}` — submits that the pool
  accepted.
- `pool_submits{pool=...,type="rejected"}` — submits that the pool
  rejected.
- `pool_reconnects{pool=...}` — reconnect attempts.

The miner metrics task pushes one more series, and no stratum session
writes it:

- `pool_work_shares{hashboard=...,pool=...}` — chip shares
  for the slot that supplied the work, in the unit below. The ratio
  between these series is the work split that the dispatch scheduler
  delivered. The `hashboard` label names the hashboard that found the
  shares, because a board can own more than one. See
  `ubos_metrics::chain`.

It pushes this series for every slot the board compiles in, so a slot
with no pool in the config still has it.

A share is 2^32 expected hashes. A chip solution found against a
difficulty `d` filter adds `d` to the counter, so the counter times
2^32 is the expected hash count on every chip family, and
`rate(...) * 2^32` is a hashrate in H/s whatever difficulty the chip
filters at.

There are no unlabelled duplicates of these series. A board total is
`sum(...)` over the label.

## Per-hashboard series

Code: `ubos/ubos-mining/src/publishers.rs`.

Five series are labelled `hashboard="i"`. Each is pushed once for each
chain the machine wired up, which `ChainStatsHandle::chain_count`
reports. The unused slots of the `MAX_CHAINS`-sized table are never
published, and no per-hashboard series is pushed before the miner
metrics task starts:

- `hashboard_present` — 1 when `ChainStats::presence()` is `Present`,
  0 otherwise (absent, faulted, or not yet enumerated).
- `frequency_hertz` — the last frequency the chain's serial
  actor confirmed. This is the only frequency series; there is no
  unlabelled machine-wide one, because each chain carries its own clock.
  Read it with `hashboard_present`: a chain held in reset keeps its
  last frequency until the actor resets it during bring-up.
- `uart_desync_bytes` — the chain's lifetime count of UART
  bytes that the driver discarded, on a failed CRC or to find the start
  of a frame.
- `hashrate_hps` — the smoothed hashrate of the chain's own
  estimator.
- `hashboard_shares{type="valid"}` — the chain's lifetime chip shares,
  in the same unit as `pool_work_shares`. `valid` is the only type,
  because a chain counts only the shares that it found.

No series here has an unlabelled duplicate. A board total is `sum(...)`
over the `hashboard` label, as for the per-pool series above.
`hashrate_hps` and `hashboard_shares` describe chip output, not
pool credit.

## Per-hashboard PVT series

Code: `ubos/ubos-thermal/src/chain.rs`.

On a board whose chips are its only thermometer, `ChainTemperature`
publishes two more gauges every ten seconds, from inside the thermal
sample. Both are labelled `hashboard="i"`, and only a present chain
reports:

- `chip_voltage_sum_volts` — the sum of the core
  voltages the chain's chips report about themselves. The chips sit in
  series across one rail, so this is the string voltage.
- `chip_temperature_summary_celsius{stat=...}` — the spread
  of die temperatures across the chain, as three series: `min`, `mean`
  and `max`.

The spread is what says whether one die is running away from the rest.
The reading of each die is under `chip_temperature_celsius`.

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
