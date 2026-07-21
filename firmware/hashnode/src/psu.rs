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

//! Hashnode PSU integration over the shared I2C1 bus.
//!
//! Wraps the shared `SIC45x` `PMBus` driver in [`BfmPsu`], which owns the
//! `SIC_EN` (GPIO30) hardware-enable pin. On this board the `SIC45x` is gated
//! by that pin, not by the `PMBus` `OPERATION` register, so `enable`/`disable`
//! drive the GPIO and delegate the register reads and `set_voltage` to the
//! inner driver. The bus mutex is owned by `main` so other I2C consumers can
//! take additional `I2cDevice` handles from the same mutex.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_embedded_hal::shared_bus::I2cDeviceError;
use embassy_rp::gpio::Output;
use embassy_rp::i2c::{Async, Error as I2cError, I2c};
use embassy_rp::peripherals::I2C1;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::mutex::Mutex;
use portable_atomic::AtomicBool;
use static_cell::StaticCell;
use ubos_core::units::{Power, Voltage};
use ubos_machine::HASHNODE as MACHINE;
use ubos_psu::{BfmPsu, BfmState, Error, Psu, StatusFlags};

/// Bus mutex shared by every consumer on I2C1. `NoopRawMutex` is sufficient
/// because every I2C consumer runs on the Core 0 executor. The chip-serial
/// actor (`mining::serial_task`) runs on Core 1 but does not touch I2C, so
/// there is no cross-core contention. If a future I2C consumer moves to
/// Core 1, bump this to `CriticalSectionRawMutex`.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, I2C1, Async>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C1, Async>>;

/// PSU state guarded by the raw mutex the SIC driver requires.
type PsuStateInner = Mutex<CriticalSectionRawMutex, BfmState<Device>>;

/// I2C bus error surfaced by the shared-bus device.
type BusError = I2cDeviceError<I2cError>;

/// The shared `SIC45x` `PMBus` driver instantiated for this board's bus.
type InnerPsu = BfmPsu<Device>;

static STATE: StaticCell<PsuStateInner> = StaticCell::new();
static TERMINATED: StaticCell<AtomicBool> = StaticCell::new();
static PSU: StaticCell<BoardPsu> = StaticCell::new();

/// Hashnode power stage: the `SIC45x` `PMBus` driver plus the `SIC_EN` (GPIO30)
/// hardware-enable pin. Public so the `mining` module can spell it in
/// `miner_task`'s signature.
///
/// `enable`/`disable` gate the rail through the GPIO, which is active-high and
/// held off by a 10 kΩ pull-down at power-on; the SIC itself rejects `PMBus`
/// `OPERATION` writes on this board. Everything else delegates to the inner
/// driver.
pub struct BoardPsu {
    inner: InnerPsu,
    sic_enable: Mutex<NoopRawMutex, Output<'static>>,
}

impl Psu for BoardPsu {
    type BusError = BusError;

    async fn enable(&self) -> Result<(), Error<BusError>> {
        // Program the protection limits and confirm the SIC answers before
        // energising the rail. A read drives the inner lazy init, which
        // writes the limit registers.
        self.inner.read_status().await?;
        self.sic_enable.lock().await.set_high();
        Ok(())
    }

    async fn disable(&self) -> Result<(), Error<BusError>> {
        self.sic_enable.lock().await.set_low();
        Ok(())
    }

    async fn set_voltage(&self, voltage: Voltage) -> Result<(), Error<BusError>> {
        self.inner.set_voltage(voltage).await
    }

    async fn read_input_power(&self) -> Result<Power, Error<BusError>> {
        self.inner.read_input_power().await
    }

    async fn read_output_power(&self) -> Result<Power, Error<BusError>> {
        self.inner.read_output_power().await
    }

    async fn read_output_voltage(&self) -> Result<Voltage, Error<BusError>> {
        self.inner.read_output_voltage().await
    }

    async fn read_secondary_voltage(&self) -> Option<Result<Voltage, Error<BusError>>> {
        self.inner.read_secondary_voltage().await
    }

    fn voltage_source(&self) -> Option<&'static str> {
        self.inner.voltage_source()
    }

    fn secondary_voltage_source(&self) -> Option<&'static str> {
        self.inner.secondary_voltage_source()
    }

    async fn read_status(&self) -> Result<StatusFlags, Error<BusError>> {
        self.inner.read_status().await
    }

    async fn terminate(&self) {
        // Drop the rail first, then mark the inner driver terminated so the
        // power-stats loop parks.
        self.sic_enable.lock().await.set_low();
        self.inner.terminate().await;
    }
}

/// Construct the `BoardPsu` over an `I2cDevice` carved from `bus`, taking
/// ownership of the `SIC_EN` pin (passed in already driven low), and return
/// the `'static` references the miner and power-stats tasks need.
pub fn init(
    bus: &'static Bus,
    sic_enable: Output<'static>,
) -> (&'static BoardPsu, &'static AtomicBool) {
    let device = I2cDevice::new(bus);
    let state: &'static _ = STATE.init(Mutex::new(BfmState::new(device, MACHINE.psu)));
    let terminated: &'static _ = TERMINATED.init(AtomicBool::new(false));
    let inner = BfmPsu::new(state, terminated);
    let psu: &'static _ = PSU.init(BoardPsu {
        inner,
        sic_enable: Mutex::new(sic_enable),
    });
    (psu, terminated)
}

#[embassy_executor::task]
pub async fn power_stats_task(psu: &'static BoardPsu) -> ! {
    ubos_psu::run_power_stats(psu).await
}
