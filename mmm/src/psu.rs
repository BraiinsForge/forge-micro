//! Mock PSU.
//!
//! Stand-in for the real BMM100 power supply driver while the I2C side
//! is still being ported. Exposes the same async surface a real driver
//! would (`enable`, `disable`, `set_voltage`) so consumer code can be
//! written against the final API today; methods log, mutate the
//! mock's atomic state, and observe the chip's settling delays so
//! downstream timing assumptions hold.
//!
//! Voltage is carried as `u32` millivolts to avoid soft-float on
//! no-FPU targets; the BMM100 chip rail sits around 750 mV, well
//! within u32 range.

use embassy_time::{Duration, Timer};
use embedded_io::ErrorKind;
use portable_atomic::{AtomicBool, AtomicU32, Ordering};
use ubos_logging::info;

/// Time the PSU rail takes to ramp up or down before the rest of the
/// system can rely on the new state. Mirrors the chip-level RC time.
const SETTLE_DELAY: Duration = Duration::from_secs(1);

/// Single PSU handle. The real driver will hold an I2C bus reference;
/// this mock keeps only the observable rail state. Methods take `&self`
/// so the type lives as a workspace `static` and any task can call in.
pub struct Psu {
    enabled: AtomicBool,
    voltage_mv: AtomicU32,
}

impl Psu {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            voltage_mv: AtomicU32::new(0),
        }
    }

    /// Set the rail voltage. Takes effect immediately on the mock; on a
    /// real driver this would push the new setpoint to the regulator
    /// over I2C and may return an error if the bus rejects the write.
    ///
    /// # Errors
    ///
    /// The mock never errors. Documented for interface parity with the
    /// real driver, which will surface I2C failures as `ErrorKind`.
    // The mock has nothing to await; the async marker is part of the
    // API contract the real driver will inherit.
    #[allow(clippy::unused_async)]
    pub async fn set_voltage(&self, millivolts: u32) -> Result<(), ErrorKind> {
        info!("PSU: setting voltage to {} mV", millivolts);
        self.voltage_mv.store(millivolts, Ordering::Relaxed);
        Ok(())
    }

    /// Enable the rail. Includes the chip-level settling delay so a
    /// caller awaiting this method can assume the rail is up by the
    /// time the future resolves.
    ///
    /// # Errors
    ///
    /// The mock never errors. The real driver will surface enable
    /// command failures (I2C NACK, regulator fault) as `ErrorKind`.
    pub async fn enable(&self) -> Result<(), ErrorKind> {
        info!("PSU: enabling output");
        self.enabled.store(true, Ordering::Relaxed);
        Timer::after(SETTLE_DELAY).await;
        info!("PSU: enabled");
        Ok(())
    }

    /// Disable the rail. Includes the same settling delay as `enable`.
    ///
    /// # Errors
    ///
    /// Mirrors `enable` — the mock never errors; the real driver will
    /// surface I2C / regulator failures.
    pub async fn disable(&self) -> Result<(), ErrorKind> {
        info!("PSU: disabling output");
        self.enabled.store(false, Ordering::Relaxed);
        Timer::after(SETTLE_DELAY).await;
        info!("PSU: disabled");
        Ok(())
    }

    /// Snapshot of the rail state. Useful from tests and from a
    /// supervisor task that needs to gate work dispatch on PSU state.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Last voltage setpoint in millivolts.
    #[must_use]
    pub fn voltage_mv(&self) -> u32 {
        self.voltage_mv.load(Ordering::Relaxed)
    }
}

impl Default for Psu {
    fn default() -> Self {
        Self::new()
    }
}

/// Single workspace PSU handle. Callers reach in directly, e.g.
/// `mmm::psu::PSU.enable().await`. When the real driver lands the
/// `static` stays put; only `Psu` gains an I2C bus parameter.
pub static PSU: Psu = Psu::new();
