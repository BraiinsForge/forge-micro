//! Shared configuration constants grouped by subsystem namespace.

use embassy_time::Duration;

use crate::units::{Frequency, Voltage};

/// TCP/UDP socket timeouts shared across NTP and HTTP.
pub struct Net;
impl Net {
    pub const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
}

/// Hardware watchdog feed timing.
pub struct Watchdog;
impl Watchdog {
    pub const FEED_PERIOD: Duration = Duration::from_millis(500);
}

/// BM1362 mining defaults shared by `ubos-config` and `ubos-mining`.
/// Hoisted here so the boot-default frequency, voltage, and chip
/// count have one source of truth; `Config::default()` reads them
/// through `.mhz()`/`.mv()`, and `ubos-mining` re-exports the typed
/// constants directly.
pub struct Mining;
impl Mining {
    /// Default mining frequency programmed at chip init.
    pub const DEFAULT_FREQUENCY: Frequency = Frequency::from_mhz(525);
    /// Default ASIC core voltage programmed at chip init.
    pub const DEFAULT_VOLTAGE: Voltage = Voltage::from_mv(1400);
    /// Number of BM1362 ASIC chips on the BMM100 hashboard. Shared
    /// by `ubos-config` (`MiningInfo`) and `ubos-mining` (consts).
    pub const NUM_CHIPS: u8 = 4;
}
