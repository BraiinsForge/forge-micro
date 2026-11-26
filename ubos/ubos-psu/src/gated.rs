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

//! Generic enable-pin sequencing around a [`Psu`].
//!
//! A board whose power stage needs one or more GPIO enable lines raised
//! in order, an optional power-good input to sample, and a settle delay
//! between the two wraps its `PMBus` driver in [`GatedPsu`] instead of
//! writing that sequencing by hand.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_time::{Duration, Timer};
use embedded_hal::digital::{InputPin, OutputPin};
use ubos_core::units::{Power, Voltage};

use crate::{Error, ModelString, Psu, StatusFlags, VoltageSource};

/// One GPIO line reached through `&self`. `Psu` methods take `&self`, but
/// the pin traits take `&mut self`, so the pin sits in a `RefCell`. The
/// `NoopRawMutex` wrapper excludes nothing; it is what lets the cell live
/// in a `Sync` static, and the claim it rests on, every caller on one
/// executor, is the board's to document where it builds the driver.
type PinCell<P> = BlockingMutex<NoopRawMutex, RefCell<P>>;

/// Whether the inner device's own on and off commands are part of the
/// sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InnerGate {
    /// The inner [`Psu::enable`] and [`Psu::disable`] run inside the
    /// sequence, after the lines rise and before they drop.
    DriveOperation,
    /// Only the enable lines gate the output, for a device that rejects
    /// the software on and off command.
    PinsOnly,
}

/// Sequences up to `N` enable outputs, an optional power-good input, and
/// a settle delay around an inner [`Psu`].
///
/// `enable` sends the inner heartbeat, raises the enable lines in array
/// order, waits `settle` if set, then samples `power_good` if present
/// and logs a warning when it reads low. `disable` and `terminate` drop
/// the lines in reverse order. `inner_gate` says whether the inner
/// device's own [`Psu::enable`] and [`Psu::disable`] are part of the
/// sequence.
pub struct GatedPsu<D, EN, PG, const N: usize> {
    inner: D,
    enables: [PinCell<EN>; N],
    power_good: Option<PinCell<PG>>,
    settle: Option<Duration>,
    inner_gate: InnerGate,
}

impl<D, EN, PG, const N: usize> GatedPsu<D, EN, PG, N>
where
    D: Psu,
    EN: OutputPin,
    PG: InputPin,
{
    /// Construct a `GatedPsu` from an inner driver and already-configured
    /// pins. `enables` are raised on `enable` in array order and dropped
    /// on `disable`/`terminate` in reverse order.
    pub fn new(
        inner: D,
        enables: [EN; N],
        power_good: Option<PG>,
        settle: Option<Duration>,
        inner_gate: InnerGate,
    ) -> Self {
        Self {
            inner,
            enables: enables.map(|pin| BlockingMutex::new(RefCell::new(pin))),
            power_good: power_good.map(|pin| BlockingMutex::new(RefCell::new(pin))),
            settle,
            inner_gate,
        }
    }
}

impl<D, EN, PG, const N: usize> Psu for GatedPsu<D, EN, PG, N>
where
    D: Psu,
    EN: OutputPin,
    PG: InputPin,
{
    type BusError = D::BusError;

    async fn enable(&self) -> Result<(), Error<D::BusError>> {
        // A no-mutation read that also drives the inner device's lazy
        // init, so protection limits are programmed before any enable
        // line goes high.
        self.inner.read_status().await?;
        // The supervisor of a supply with a heartbeat sees a pulse before
        // any enable line goes high.
        self.inner.heartbeat().await?;
        for line in &self.enables {
            line.lock(|pin| {
                let _ = pin.borrow_mut().set_high();
            });
        }
        if self.inner_gate == InnerGate::DriveOperation {
            self.inner.enable().await?;
        }
        if let Some(settle) = self.settle {
            Timer::after(settle).await;
        }
        if let Some(power_good) = &self.power_good {
            // A read that fails is reported as low: the warning is the
            // only thing this sample feeds, and a rail whose readiness
            // cannot be read deserves it.
            let is_low = power_good.lock(|pin| pin.borrow_mut().is_low().unwrap_or(true));
            if is_low {
                ubos_logging::warn!("psu power-good still low after enable");
            }
        }
        Ok(())
    }

    async fn disable(&self) -> Result<(), Error<D::BusError>> {
        let result = if self.inner_gate == InnerGate::DriveOperation {
            self.inner.disable().await
        } else {
            Ok(())
        };
        for line in self.enables.iter().rev() {
            line.lock(|pin| {
                let _ = pin.borrow_mut().set_low();
            });
        }
        result
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<Voltage, Error<D::BusError>> {
        self.inner.set_voltage(voltage).await
    }

    async fn read_input_power(&self) -> Result<Power, Error<D::BusError>> {
        self.inner.read_input_power().await
    }

    async fn read_output_power(&self) -> Result<Power, Error<D::BusError>> {
        self.inner.read_output_power().await
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<D::BusError>> {
        self.inner.read_output_voltage().await
    }

    async fn read_secondary_voltage(&self) -> Option<Result<Voltage, Error<D::BusError>>> {
        self.inner.read_secondary_voltage().await
    }

    fn voltage_source(&self) -> VoltageSource {
        self.inner.voltage_source()
    }

    async fn read_status(&self) -> Result<StatusFlags, Error<D::BusError>> {
        self.inner.read_status().await
    }

    async fn model(&self) -> Option<ModelString> {
        self.inner.model().await
    }

    async fn heartbeat_interval(&self) -> Option<Duration> {
        self.inner.heartbeat_interval().await
    }

    async fn heartbeat(&self) -> Result<(), Error<D::BusError>> {
        self.inner.heartbeat().await
    }

    async fn terminate(&self) {
        if self.inner_gate == InnerGate::DriveOperation {
            let _ = self.inner.disable().await;
        }
        for line in self.enables.iter().rev() {
            line.lock(|pin| {
                let _ = pin.borrow_mut().set_low();
            });
        }
        self.inner.terminate().await;
    }
}
