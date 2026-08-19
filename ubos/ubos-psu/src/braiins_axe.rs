// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! `BraiinsAxePsu` handle and `Psu` trait impl for the Braiins Axe power
//! stage: one TPS53647 multiphase `PMBus` VRM. All telemetry comes
//! from the controller itself — there is no separate output monitor.

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embedded_hal_async::i2c::I2c;
use portable_atomic::{AtomicBool, Ordering};
use ubos_core::units::{Power, Voltage};

use crate::state::{InitStatus, State};
use crate::tps53647::{self, Tps53647Config};
use crate::{Error, Psu};

/// `RawMutex` choice exposed for callers that need to declare the
/// `'static` mutex storage cell.
pub type RawMutex = CriticalSectionRawMutex;

/// Runtime configuration for `BraiinsAxePsu`.
#[derive(Clone, Copy, Debug)]
pub struct BraiinsAxePsuDescriptor {
    /// TPS53647 VRM configuration.
    pub tps: Tps53647Config,
}

impl BraiinsAxePsuDescriptor {
    /// Default configuration for the Braiins Axe board.
    pub const DEFAULT: Self = Self {
        tps: Tps53647Config::DEFAULT,
    };
}

/// State protected by the `BraiinsAxePsu` mutex.
pub type BraiinsAxeState<I> = State<I, BraiinsAxePsuDescriptor>;

/// Handle to the Braiins Axe PSU. Cheap to clone-by-reference; the underlying
/// state lives behind a `'static` mutex supplied by the caller.
pub struct BraiinsAxePsu<I: I2c + 'static> {
    state: &'static Mutex<RawMutex, BraiinsAxeState<I>>,
    terminated: &'static AtomicBool,
}

impl<I: I2c + 'static> BraiinsAxePsu<I> {
    /// Construct a handle from `'static` state and terminated flag.
    pub fn new(
        state: &'static Mutex<RawMutex, BraiinsAxeState<I>>,
        terminated: &'static AtomicBool,
    ) -> Self {
        Self { state, terminated }
    }

    /// Replay a cached init outcome or run the TPS53647 init once and
    /// cache it.
    pub(crate) async fn ensure_initialized(
        state: &mut BraiinsAxeState<I>,
    ) -> Result<(), Error<I::Error>> {
        if let Some(cached) = state.cached_init() {
            return cached;
        }
        let result = tps53647::init(&mut state.i2c, &state.config.tps).await;
        state.record_init(&result);
        result
    }
}

impl<I: I2c + 'static> Psu for BraiinsAxePsu<I> {
    type BusError = I::Error;

    async fn enable(&self) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps53647::enable(i2c, &config.tps).await
    }

    async fn disable(&self) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps53647::disable(i2c, &config.tps).await
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<(), Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        let result = tps53647::set_voltage(i2c, &config.tps, voltage.mv()).await;
        // Record the commanded value so telemetry consumers can compare
        // it with the READ_VOUT readback. Only on success: a failed
        // write leaves the rail at the previous commanded value.
        if result.is_ok() {
            crate::record_set_voltage(voltage);
        }
        result
    }

    async fn read_input_power(&self) -> Result<Power, Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps53647::read_pin_mw(i2c, &config.tps)
            .await
            .map(Power::from_mw)
    }

    async fn read_output_power(&self) -> Result<Power, Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps53647::read_pout_mw(i2c, &config.tps)
            .await
            .map(Power::from_mw)
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps53647::read_vout_mv(i2c, &config.tps)
            .await
            .map(Voltage::from_mv)
    }

    fn voltage_source(&self) -> Option<&'static str> {
        Some("tps53647")
    }

    async fn read_status(&self) -> Result<crate::StatusFlags, Error<I::Error>> {
        if self.terminated.load(Ordering::Acquire) {
            return Err(Error::Terminated);
        }
        let mut guard = self.state.lock().await;
        Self::ensure_initialized(&mut guard).await?;
        let State { i2c, config, .. } = &mut *guard;
        tps53647::read_status(i2c, &config.tps).await
    }

    async fn terminate(&self) {
        if self.terminated.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut guard = self.state.lock().await;
        if guard.init == InitStatus::Initialized {
            let State { i2c, config, .. } = &mut *guard;
            let _ = tps53647::disable(i2c, &config.tps).await;
        }
    }
}

#[cfg(test)]
#[path = "tests/braiins_axe.rs"]
mod tests;
