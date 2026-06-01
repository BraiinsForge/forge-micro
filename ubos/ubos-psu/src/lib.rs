//! Async PSU driver for the BMM100 miniminer hashboard.
//!
//! Generic over `embedded_hal_async::i2c::I2c`. Runs unchanged on
//! Linux (via `embedded-i2c-unix`) and on rp2040/rp2350 (via
//! `embassy-rp::i2c`).

#![cfg_attr(not(test), no_std)]
// Embassy executors do not require `Send`; adding the bound to
// every trait method's future would force every I2C impl and all
// internal state to be `Send`, which is not free on no_std.
#![allow(async_fn_in_trait)]
// Hardware register getters are intentionally side-effect-free
// returns; #[must_use] sprinkled across every accessor is noise.
#![allow(clippy::must_use_candidate)]
// Bounded casts inside hardware drivers (DAC byte, register
// pointer) are safe by construction; the math above each cast
// rejects out-of-range values explicitly.
#![allow(clippy::cast_possible_truncation)]

/// PSU status flags decoded from the `TPS6287x` STATUS register.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
// Mirrors a hardware status byte; collapsing the bits into a state
// machine would lose the 1:1 mapping to the silicon register.
#[allow(clippy::struct_excessive_bools)]
pub struct StatusFlags {
    pub current_limit: bool,
    pub thermal_shutdown: bool,
    pub overvoltage: bool,
    pub undervoltage: bool,
}

/// Errors surfaced by the [`Psu`] trait. `E` is the underlying I2C
/// bus error type.
#[derive(Debug)]
pub enum Error<E> {
    Bus(E),
    /// Init failed; only the standardized kind is bubbled up
    /// because there is no `Clone` bound on `E`. The concrete
    /// error is logged at the call site that first observed it.
    InitFailed(embedded_hal_async::i2c::ErrorKind),
    OutOfRange,
    Fault(StatusFlags),
    Terminated,
}

/// Async PSU control surface. One implementation per hardware
/// platform; consumers stay generic over `Psu`.
pub mod config;
pub mod ina230;
pub mod miniminer;
pub mod state;
pub mod tps6287x;
pub mod watchdog;

pub use config::MiniminerPsuConfig;
pub use miniminer::{run_watchdog, MiniminerPsu, RawMutex};
pub use state::State;

use ubos_core::units::{Power, Voltage};

/// Cadence at which [`run_power_stats`] reads INA230 input/output power.
/// Matches the host-runner cadence so the prometheus series rates are
/// comparable across firmware targets.
pub const POWER_STATS_INTERVAL: embassy_time::Duration = embassy_time::Duration::from_secs(2);

/// Periodic log of input and output power readings. Runs forever.
///
/// Reads both sides at [`POWER_STATS_INTERVAL`] and emits a single info
/// line on success, or one of two error lines depending on which side
/// failed. Board firmware crates wrap this in a `#[embassy_executor::task]`
/// of their own because tasks cannot be generic.
pub async fn run_power_stats<P>(psu: &P) -> !
where
    P: Psu,
{
    use ubos_metrics::metrics::{push, Metric, Value};

    let mut ticker = embassy_time::Ticker::every(POWER_STATS_INTERVAL);
    loop {
        ticker.next().await;
        let input = psu.read_input_power().await;
        let output = psu.read_output_power().await;
        match (input, output) {
            (Ok(input), Ok(output)) => {
                ubos_logging::info!(
                    "psu sensors: input={} mW output={} mW",
                    input.mw(),
                    output.mw()
                );
                push(Metric::with_no_labels(
                    "board_psu_input_power_mw",
                    Value::Uint(u64::from(input.mw())),
                ))
                .await;
                push(Metric::with_no_labels(
                    "board_psu_output_power_mw",
                    Value::Uint(u64::from(output.mw())),
                ))
                .await;
            }
            (Err(error), Ok(_)) | (Ok(_), Err(error)) => {
                ubos_logging::error!(
                    "psu sensor read failed: {:?}",
                    ubos_logging::Debug2Format(&error),
                );
            }
            (Err(input_error), Err(output_error)) => {
                ubos_logging::error!(
                    "psu sensor read failed: input={:?} output={:?}",
                    ubos_logging::Debug2Format(&input_error),
                    ubos_logging::Debug2Format(&output_error),
                );
            }
        }

        match psu.read_status().await {
            Ok(flags) => {
                push(Metric::with_no_labels(
                    "board_psu_status_current_limit",
                    Value::Uint(u64::from(flags.current_limit)),
                ))
                .await;
                push(Metric::with_no_labels(
                    "board_psu_status_thermal_shutdown",
                    Value::Uint(u64::from(flags.thermal_shutdown)),
                ))
                .await;
                push(Metric::with_no_labels(
                    "board_psu_status_overvoltage",
                    Value::Uint(u64::from(flags.overvoltage)),
                ))
                .await;
                push(Metric::with_no_labels(
                    "board_psu_status_undervoltage",
                    Value::Uint(u64::from(flags.undervoltage)),
                ))
                .await;
            }
            // A terminated PSU will not recover; stop polling it so the
            // task does not spam warn logs on every tick indefinitely.
            // Parking here is symmetric with the `(Err(_), Err(_))` power-read
            // path: both represent an unrecoverable device condition and
            // neither can reset the hardware — the miner task owns that.
            Err(Error::Terminated) => {
                ubos_logging::warn!("psu status: device terminated; halting poll loop");
                core::future::pending::<()>().await;
                unreachable!()
            }
            Err(error) => {
                ubos_logging::warn!(
                    "psu status read failed: {:?}",
                    ubos_logging::Debug2Format(&error),
                );
            }
        }
    }
}

pub trait Psu {
    type BusError: embedded_hal_async::i2c::Error;

    async fn enable(&self) -> Result<(), Error<Self::BusError>>;
    async fn disable(&self) -> Result<(), Error<Self::BusError>>;
    async fn set_voltage(&self, voltage: Voltage) -> Result<(), Error<Self::BusError>>;
    async fn read_input_power(&self) -> Result<Power, Error<Self::BusError>>;
    async fn read_output_power(&self) -> Result<Power, Error<Self::BusError>>;
    /// Read the `TPS6287x` STATUS register and decode the four fault flags.
    async fn read_status(&self) -> Result<StatusFlags, Error<Self::BusError>>;
    async fn terminate(&self);
}

#[cfg(test)]
mod surface_tests {
    use super::*;

    #[test]
    fn status_flags_default_all_clear() {
        let flags = StatusFlags::default();
        assert!(!flags.current_limit);
        assert!(!flags.thermal_shutdown);
        assert!(!flags.overvoltage);
        assert!(!flags.undervoltage);
    }

    #[test]
    fn error_init_failed_carries_kind() {
        let err: Error<core::convert::Infallible> =
            Error::InitFailed(embedded_hal_async::i2c::ErrorKind::Bus);
        match err {
            Error::InitFailed(kind) => {
                assert!(matches!(kind, embedded_hal_async::i2c::ErrorKind::Bus));
            }
            _ => panic!("BUG: variant mismatch"),
        }
    }
}
