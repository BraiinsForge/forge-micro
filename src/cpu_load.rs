//! CPU busy ratio published as a Prometheus gauge.
//!
//! # Model
//!
//! Boards bracket their executor's idle wait with
//! [`embassy_time::Instant::now`] and add the elapsed ticks to the
//! per-core slot in [`IDLE_TICKS`]. The publisher task periodically
//! diffs each slot against wall-clock ticks and pushes
//! `1.0 - idle / elapsed` as `board_cpu_busy_ratio` into the shared
//! metrics channel, labelled `cpu="0"`, `cpu="1"`, ... up to the
//! per-board active-core count. The ratio falls back to `0.0` when
//! no time has elapsed between samples.
//!
//! Two seams exist depending on the runtime:
//!
//! - On the Cortex-M boards, [`run_cortex_m_executor`] drives
//!   `raw::Executor::poll()` under `#[cortex_m_rt::entry]` and
//!   brackets `cortex_m::asm::wfe()` with `Instant::now()` reads.
//! - On ESP32-S3, `esp_rtos::embassy::Executor::run_with_callbacks`
//!   exposes `before_poll` and `on_idle` hooks; `on_idle` records the
//!   entry instant and `before_poll` accumulates the delta on the
//!   following iteration.
//!
//! # Caveats
//!
//! Interrupt handlers that wake the wait instruction run before the
//! second `Instant::now()` and are accounted as idle. The metric is a
//! coarse executor-level load indicator, not a true busy-cycle count.
//!
//! The final `clamp(0.0, 1.0)` defends against the accounting skew
//! above and any residual `f32` rounding in the division; the metric
//! is reported as a coarse ratio rather than a precise count.
//!
//! # Origin
//!
//! The technique of driving `raw::Executor::poll()` manually and
//! bracketing `wfe` with `Instant::now()` reads is from Giacomo
//! Caironi's "Measuring CPU usage with Rust Embassy"
//! (<https://www.giacomocaironi.dev/posts/measuring-cpu-usage-with-rust-embassy/>,
//! reference impl at <https://gitlab.com/giacomocaironi/embassy-cpu-usage>).

use core::num::NonZeroU64;

use embassy_time::{Duration, Instant, Ticker};
use portable_atomic::{AtomicU64, Ordering};

use crate::metrics::{push, Label, LabelValue, Metric, Value};

/// Maximum number of cores tracked by [`IDLE_TICKS`]. Each board
/// declares how many of these slots it populates by passing `N_CPUS`
/// to [`run_cpu_load`]. Bump together with the [`IDLE_TICKS`] and
/// [`CPU_LABELS`] array literals when a board starts driving a second
/// executor.
pub const MAX_CPUS: usize = 1;

/// Total `embassy_time` ticks spent halted in each core's idle wait.
/// Boards add to slot `N` from the idle hook running on core `N`.
pub static IDLE_TICKS: [AtomicU64; MAX_CPUS] = [AtomicU64::new(0)];

/// Static `cpu` label values, indexed by core ID. Avoids per-window
/// integer-to-string formatting on the publish path.
const CPU_LABELS: [&str; MAX_CPUS] = ["0"];

/// Window length over which the published ratio is averaged, paired
/// with its `window` label string. Co-located so changing the cadence
/// forces the dashboard label to follow.
const WINDOW: (Duration, &str) = (Duration::from_secs(5), "5s");

/// Publish `board_cpu_busy_ratio` once per [`WINDOW`] for each of the
/// `N_CPUS` active cores.
///
/// Call this from a firmware `#[embassy_executor::task]`. `N_CPUS`
/// must be at most [`MAX_CPUS`].
pub async fn run_cpu_load<const N_CPUS: usize>() -> ! {
    const { assert!(N_CPUS <= MAX_CPUS, "N_CPUS exceeds MAX_CPUS") };

    let mut ticker = Ticker::every(WINDOW.0);
    let mut prev_idle: [u64; N_CPUS] =
        core::array::from_fn(|index| IDLE_TICKS[index].load(Ordering::Relaxed));
    let mut prev_instant = Instant::now();

    loop {
        ticker.next().await;
        // Sample every core's idle counter up front so all per-cpu
        // ratios share the same elapsed window. Publishing awaits
        // between cores, so a per-iteration sample would skew later
        // cores against an already-stale `now_instant`.
        let now_idle: [u64; N_CPUS] =
            core::array::from_fn(|index| IDLE_TICKS[index].load(Ordering::Relaxed));
        let now_instant = Instant::now();
        let elapsed = (now_instant - prev_instant).as_ticks();

        for cpu_index in 0..N_CPUS {
            let idle = now_idle[cpu_index].wrapping_sub(prev_idle[cpu_index]);
            let busy = busy_ratio(idle, elapsed);

            let mut labels = heapless::Vec::new();
            // Window and cpu labels are fixed and known to fit; pushes cannot fail.
            let _ = labels.push(Label {
                name: "window",
                value: LabelValue::Static(WINDOW.1),
            });
            let _ = labels.push(Label {
                name: "cpu",
                value: LabelValue::Static(CPU_LABELS[cpu_index]),
            });
            push(Metric {
                name: "board_cpu_busy_ratio",
                labels,
                value: Value::Float(busy),
                timestamp_ms: 0,
            })
            .await;

            prev_idle[cpu_index] = now_idle[cpu_index];
        }

        prev_instant = now_instant;
    }
}

/// Convert idle and elapsed tick counts into a clamped 0..=1 busy
/// ratio. Returns 0.0 when no time has elapsed since the last sample.
fn busy_ratio(idle: u64, elapsed: u64) -> f32 {
    let Some(elapsed) = NonZeroU64::new(elapsed) else {
        return 0.0;
    };
    #[allow(clippy::cast_precision_loss)]
    let ratio = idle as f32 / elapsed.get() as f32;
    (1.0 - ratio).clamp(0.0, 1.0)
}

#[cfg(target_arch = "arm")]
pub use cortex_m_executor::run_cortex_m_executor;

/// Cortex-M-specific seam between `cortex_m_rt::entry` and the
/// `raw::Executor` poll/wfe loop that feeds [`IDLE_TICKS`]. Gated
/// to ARM so the shared library still compiles for ESP32-S3.
#[cfg(target_arch = "arm")]
mod cortex_m_executor {
    use embassy_executor::Spawner;
    use embassy_time::Instant;
    use portable_atomic::Ordering;
    use static_cell::StaticCell;

    use super::{IDLE_TICKS, MAX_CPUS};

    /// Sentinel value the `embassy-executor` cortex-m pender treats as
    /// thread mode. The pender issues `sev` for this branch, which
    /// breaks the `wfe` below when a task is woken.
    const THREAD_PENDER: usize = usize::MAX;

    /// Drive an embassy `raw::Executor` on Cortex-M, accumulating the
    /// time spent halted in `wfe` into `IDLE_TICKS[CPU_INDEX]`.
    ///
    /// `spawn` is invoked once with the executor's [`Spawner`] so the
    /// firmware can spawn its boot task. The function never returns.
    ///
    /// Call this from inside `#[cortex_m_rt::entry] fn main`.
    pub fn run_cortex_m_executor<const CPU_INDEX: usize, F>(spawn: F) -> !
    where
        F: FnOnce(Spawner),
    {
        // Function-local statics are not duplicated per generic
        // instantiation, so this single slot is shared across all
        // `CPU_INDEX` values. Safe today because `MAX_CPUS = 1` keeps
        // the function callable with `CPU_INDEX = 0` only. Bumping
        // `MAX_CPUS` requires turning this into a `[StaticCell; N]`
        // indexed by `CPU_INDEX`.
        static EXECUTOR: StaticCell<embassy_executor::raw::Executor> = StaticCell::new();
        const { assert!(CPU_INDEX < MAX_CPUS, "CPU_INDEX exceeds MAX_CPUS") };
        let executor = EXECUTOR.init(embassy_executor::raw::Executor::new(
            THREAD_PENDER as *mut (),
        ));
        let spawner = executor.spawner();
        spawn(spawner);
        loop {
            // SAFETY: `raw::Executor::poll` is not reentrant. This loop
            // is the sole caller, and no interrupt handler invokes
            // `poll`.
            unsafe { executor.poll() };
            let before = Instant::now();
            cortex_m::asm::wfe();
            let after = Instant::now();
            IDLE_TICKS[CPU_INDEX].fetch_add((after - before).as_ticks(), Ordering::Relaxed);
        }
    }
}
