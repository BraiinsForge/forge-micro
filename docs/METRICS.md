# Metrics

Each firmware exposes a Prometheus text endpoint on
`http://<board-ip>:<Metrics::PORT>/`. The bench NUC scrapes it from
`nuc/`.

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

## Hashrate

Code: `ubos/ubos-metrics/src/hashrate.rs`.

`record_share` bumps `SHARE_COUNT` and feeds the share's difficulty into
an EWMA-smoothed bucketed estimator over 1-second buckets with α = 1/8
(~8 s smoothing window). `hashrate` reads the smoothed value and applies
read-side decay so the displayed rate drops toward zero when shares stop
arriving. The ubos-mining backend calls `record_share` for every share it
consumes (one call per diff-1 chip share); see `ubos_mining::serial`.
