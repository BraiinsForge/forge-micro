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

//! Braiins Axe PSU integration over the shared I2C0 bus.
//!
//! Wraps the shared TPS53647 `PMBus` driver in [`BraiinsAxePsu`] and owns the
//! three power-sequencing GPIOs: `PWR_EN` (TPS53647 ENABLE), `LDO_EN`
//! (0V8/1V2 aux LDOs), and the `VR_RDY` power-good input. The TPS
//! requires both the ENABLE pin and the `OPERATION` on-bit
//! (`ON_OFF_CONFIG` 17h), so `enable` raises the pins and then
//! delegates to the inner driver's `OPERATION` write. The bus mutex is
//! owned by `main` so other I2C consumers can take additional
//! `I2cDevice` handles from the same mutex.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_embedded_hal::shared_bus::I2cDeviceError;
use embassy_rp::gpio::{Input, Output};
use embassy_rp::i2c::{Async, Error as I2cError, I2c};
use embassy_rp::peripherals::I2C0;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::mutex::Mutex;
use portable_atomic::AtomicBool;
use static_cell::StaticCell;
use ubos_core::units::{Power, Voltage};
use ubos_machine::BRAIINS_AXE as MACHINE;
use ubos_psu::{BraiinsAxePsu, BraiinsAxeState, Error, Psu, StatusFlags};

/// Bus mutex shared by every consumer on I2C0. `NoopRawMutex` is
/// sufficient because every I2C consumer runs on the Core 0 executor.
/// The chip-serial actor (`mining::serial_task`) runs on Core 1 but
/// does not touch I2C, so there is no cross-core contention.
pub type Bus = Mutex<NoopRawMutex, I2c<'static, I2C0, Async>>;

/// `embedded-hal-async` device handle for one consumer of the shared bus.
type Device = I2cDevice<'static, NoopRawMutex, I2c<'static, I2C0, Async>>;

/// PSU state guarded by the raw mutex the TPS driver requires.
type PsuStateInner = Mutex<CriticalSectionRawMutex, BraiinsAxeState<Device>>;

/// I2C bus error surfaced by the shared-bus device.
type BusError = I2cDeviceError<I2cError>;

/// The shared TPS53647 `PMBus` driver instantiated for this board's bus.
type InnerPsu = BraiinsAxePsu<Device>;

static STATE: StaticCell<PsuStateInner> = StaticCell::new();
static TERMINATED: StaticCell<AtomicBool> = StaticCell::new();
static PSU: StaticCell<BoardPsu> = StaticCell::new();

/// How long after `OPERATION` on before sampling `VR_RDY`. Covers the
/// soft-start ramp; the exact timing is a hardware-validation item.
const VR_RDY_SETTLE: embassy_time::Duration = embassy_time::Duration::from_millis(10);

/// Braiins Axe power stage: the TPS53647 `PMBus` driver plus the
/// `PWR_EN` / `LDO_EN` outputs and the `VR_RDY` power-good input.
/// Public so the `mining` module can spell it in `miner_task`'s
/// signature.
pub struct BoardPsu {
    inner: InnerPsu,
    pwr_en: Mutex<NoopRawMutex, Output<'static>>,
    ldo_en: Mutex<NoopRawMutex, Output<'static>>,
    vr_rdy: Input<'static>,
}

impl Psu for BoardPsu {
    type BusError = BusError;

    async fn enable(&self) -> Result<(), Error<BusError>> {
        // Program the limits and confirm the TPS answers before
        // energising any rail. A read drives the inner lazy init,
        // which runs with ENABLE still low and the output off.
        self.inner.read_status().await?;
        // Aux rails (0V8/1V2 LDOs) before the core rail, then the
        // ENABLE pin, then the OPERATION on-bit starts the soft-start
        // ramp (ON_OFF_CONFIG requires pin AND OPERATION).
        self.ldo_en.lock().await.set_high();
        self.pwr_en.lock().await.set_high();
        self.inner.enable().await?;
        embassy_time::Timer::after(VR_RDY_SETTLE).await;
        if self.vr_rdy.is_low() {
            defmt::warn!("psu VR_RDY still low after enable");
        }
        Ok(())
    }

    async fn disable(&self) -> Result<(), Error<BusError>> {
        // OPERATION off first so the controller ramps down, then drop
        // the enable pin and the aux rails.
        let result = self.inner.disable().await;
        self.pwr_en.lock().await.set_low();
        self.ldo_en.lock().await.set_low();
        result
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

    fn voltage_source(&self) -> Option<&'static str> {
        self.inner.voltage_source()
    }

    async fn read_status(&self) -> Result<StatusFlags, Error<BusError>> {
        self.inner.read_status().await
    }

    async fn terminate(&self) {
        // Drop the rails first, then mark the inner driver terminated
        // so the power-stats loop parks.
        let _ = self.inner.disable().await;
        self.pwr_en.lock().await.set_low();
        self.ldo_en.lock().await.set_low();
        self.inner.terminate().await;
    }
}

/// Construct the `BoardPsu` over an `I2cDevice` carved from `bus`,
/// taking ownership of the sequencing pins (both outputs passed in
/// already driven low), and return the `'static` references the miner
/// and power-stats tasks need.
pub fn init(
    bus: &'static Bus,
    pwr_en: Output<'static>,
    ldo_en: Output<'static>,
    vr_rdy: Input<'static>,
) -> (&'static BoardPsu, &'static AtomicBool) {
    let device = I2cDevice::new(bus);
    let state: &'static _ = STATE.init(Mutex::new(BraiinsAxeState::new(device, MACHINE.psu)));
    let terminated: &'static _ = TERMINATED.init(AtomicBool::new(false));
    let inner = BraiinsAxePsu::new(state, terminated);
    let psu: &'static _ = PSU.init(BoardPsu {
        inner,
        pwr_en: Mutex::new(pwr_en),
        ldo_en: Mutex::new(ldo_en),
        vr_rdy,
    });
    (psu, terminated)
}

#[embassy_executor::task]
pub async fn power_stats_task(psu: &'static BoardPsu) -> ! {
    ubos_psu::run_power_stats(psu).await
}
