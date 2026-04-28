//! Async PSU driver for the BMM100 miniminer hashboard.
//!
//! Generic over `embedded_hal_async::i2c::I2c`. Runs unchanged on
//! Linux (via `embedded-i2c-unix`) and on rp2040/rp2350 (via
//! `embassy-rp::i2c`).

#![no_std]
// Embassy executors do not require `Send`; adding the bound to
// every trait method's future would force every I2C impl and all
// internal state to be `Send`, which is not free on no_std.
#![allow(async_fn_in_trait)]

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
pub trait Psu {
    type BusError: embedded_hal_async::i2c::Error;

    async fn enable(&self) -> Result<(), Error<Self::BusError>>;
    async fn disable(&self) -> Result<(), Error<Self::BusError>>;
    async fn set_voltage(
        &self,
        millivolts: u32,
    ) -> Result<(), Error<Self::BusError>>;
    async fn read_input_power_mw(
        &self,
    ) -> Result<u32, Error<Self::BusError>>;
    async fn read_output_power_mw(
        &self,
    ) -> Result<u32, Error<Self::BusError>>;
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
            Error::InitFailed(kind) => assert!(matches!(
                kind,
                embedded_hal_async::i2c::ErrorKind::Bus
            )),
            _ => panic!("BUG: variant mismatch"),
        }
    }
}
