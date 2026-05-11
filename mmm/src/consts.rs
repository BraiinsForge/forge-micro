//! Tunables for the BMM100 BM1362 mining backend.

use embassy_time::Duration;
use ubos_monitor::MonitorConfig;

/// Number of BM1362 chips on a BMM100 hashboard.
pub const NUM_CHIPS: u8 = 4;

/// Thermal-monitor tuning matching the BOS hashboard profile for the
/// BMM101 (4×BM1362) board. Gains and thresholds were carried forward
/// from the upstream `bosminer` configuration.
pub const MONITOR_CONFIG_BMM101: MonitorConfig = MonitorConfig {
    target_temp_c: 67.0,
    hot_temp_c: 80.0,
    dangerous_temp_c: 90.0,
    cooldown_temp_c: 45.0,
    min_fan_pwm: 20,
    p_gain: -5.0,
    i_gain: -0.15,
    d_gain: -100.0,
    smoother_half_decay_secs: 10.0,
    sample_interval: Duration::from_secs(1),
};

/// Maximum chips the registry can address. Sized for the worst-case
/// chain length the protocol allows.
pub const MAX_CHIPS: usize = 16;

/// Hardware shares submitted at diff 1; the chip rolls below this and
/// hands every find back to the host.
pub const DIFFICULTY: u32 = 1;

/// Midstates per work frame. The chip iterates this many extranonce-rolled
/// variants of each header internally; the host picks `work_id` in
/// strides of `NUM_MIDSTATES` so the registry indexing stays aligned.
pub const NUM_MIDSTATES: usize = 8;

/// Slots in the in-memory registry that holds per-`work_id` context for
/// matching incoming `HwSolution` frames back to the originating job.
pub const REGISTRY_SIZE: usize = 256 / NUM_MIDSTATES;

/// Boot baud rate the chip listens on out of reset.
pub const INITIAL_BAUDRATE: u32 = 115_200;

/// Nominal baud the host requests after PLL programming. The chip's
/// PLL cannot hit this exactly; the actual line rate is
/// [`FAST_BAUDRATE_ACTUAL`].
pub const FAST_BAUDRATE: u32 = 3_125_000;

/// Real line rate the chip's divider produces when [`FAST_BAUDRATE`] is
/// requested. Matches what the host UART must be programmed to.
pub const FAST_BAUDRATE_ACTUAL: u32 = 3_000_000;
