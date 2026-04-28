//! Linux sysfs GPIO driver for the BMM100 reset line.
//!
//! Uses the legacy `/sys/class/gpio` interface (export + direction +
//! value) rather than gpiocdev because the runner only needs one pin,
//! the legacy interface is one syscall to toggle, and it's still
//! shipped on every kernel the runner targets.
//!
//! Permissions: writes to `/sys/class/gpio/export` and the per-pin
//! `direction` / `value` files require either root or membership in
//! the `gpio` group on most distros. That's a deployment concern.

use std::fs::{metadata, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use mmm::reset::ResetPin;

const SYSFS_GPIO_BASE: &str = "/sys/class/gpio";

/// Maximum time to wait for the kernel to populate `gpio{N}/` after
/// writing to the export file. Some kernels return from the write
/// before the directory is visible; spin briefly so the follow-on
/// `direction` open doesn't fail with ENOENT.
const EXPORT_SETTLE_TIMEOUT: Duration = Duration::from_millis(200);

/// Poll interval inside the export wait loop. Cheap `stat()` so the
/// short polling is fine.
const EXPORT_SETTLE_POLL: Duration = Duration::from_millis(10);

/// Upstream `mmm` resolves the reset line by gpiocdev label
/// (`HB0_RESET`); sysfs only takes a flat integer, so the deployment
/// host's value goes here. Compute as `cat /sys/class/gpio/gpiochip{N}/base`
/// plus the offset of `HB0_RESET` in the device tree.
// TODO: fill with the resolved integer for the target host once known.
pub const GPIO_HB0_RESET: u32 = 0;

/// Holds an exported sysfs GPIO line as an output. `value` stays open
/// across calls so each toggle is one `write()` syscall rather than
/// open + write + close.
pub struct SysfsResetPin {
    value: File,
    number: u32,
}

impl SysfsResetPin {
    /// Export the line, set direction to "out", and pre-open the
    /// value file. Idempotent: if `gpio{number}/` already exists the
    /// export step is skipped, so re-runs of the runner don't fail
    /// on stale kernel state.
    ///
    /// # Errors
    ///
    /// Any of the underlying sysfs file operations propagate. Common
    /// causes: missing kernel `CONFIG_GPIO_SYSFS`, insufficient
    /// permissions on the sysfs files, or `number` outside the
    /// kernel's exportable range.
    pub fn export(number: u32) -> io::Result<Self> {
        if metadata(Self::pin_dir(number)).is_err() {
            let mut export_file = OpenOptions::new()
                .write(true)
                .open(format!("{SYSFS_GPIO_BASE}/export"))?;
            write!(export_file, "{number}")?;
            Self::wait_for_pin_dir(number)?;
        }

        let mut direction = OpenOptions::new()
            .write(true)
            .open(format!("{}/direction", Self::pin_dir(number).display()))?;
        direction.write_all(b"out")?;

        let value = OpenOptions::new()
            .write(true)
            .open(format!("{}/value", Self::pin_dir(number).display()))?;

        log::info!("sysfs gpio{number}: exported as output");
        Ok(Self { value, number })
    }

    fn pin_dir(number: u32) -> PathBuf {
        PathBuf::from(format!("{SYSFS_GPIO_BASE}/gpio{number}"))
    }

    /// Spin until `gpio{number}/` appears or the timeout elapses. The
    /// export write returns before the kernel finishes populating
    /// the per-pin attributes on some platforms, so a follow-on open
    /// of `direction` would race.
    fn wait_for_pin_dir(number: u32) -> io::Result<()> {
        let deadline = std::time::Instant::now() + EXPORT_SETTLE_TIMEOUT;
        loop {
            if metadata(Self::pin_dir(number)).is_ok() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("gpio{number}/ did not appear after export"),
                ));
            }
            thread::sleep(EXPORT_SETTLE_POLL);
        }
    }
}

impl ResetPin for SysfsResetPin {
    fn enter_reset(&mut self) {
        if let Err(e) = self.value.write_all(b"0") {
            log::error!("gpio{}: enter_reset failed: {e}", self.number);
        }
    }

    fn exit_reset(&mut self) {
        if let Err(e) = self.value.write_all(b"1") {
            log::error!("gpio{}: exit_reset failed: {e}", self.number);
        }
    }
}
