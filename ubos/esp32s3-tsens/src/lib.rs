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

//! ESP32-S3 internal temperature sensor (TSENS) driver.
//!
//! esp-hal 1.0.0 does not expose TSENS for the ESP32-S3. This crate
//! drives the sensor directly through the SENS PAC peripheral using
//! the register sequence from esp-idf v4.4
//! `components/driver/esp32s3/rtc_tempsensor.c`
//! (<https://github.com/espressif/esp-idf/blob/release/v4.4/components/driver/esp32s3/rtc_tempsensor.c>).
//! The conversion formula and range table come from esp-idf v5.4
//! `components/hal/esp32s3/include/hal/temperature_sensor_ll.h`
//! (<https://github.com/espressif/esp-idf/blob/release/v5.4/components/hal/esp32s3/include/hal/temperature_sensor_ll.h>).
//!
//! The SAR I2C analog block (regi2c bus, block 0x69) is shared with
//! the Wi-Fi and Bluetooth radio subsystems. Calling [`Tsens::new`]
//! while either radio is actively using regi2c may corrupt the DAC
//! calibration register. Initialize the temperature sensor before
//! bringing up any radio, or coordinate access externally.

#![no_std]

use embassy_time::{Duration, Timer};
use esp_hal::peripherals::SENS;
use ubos_core::units::Temperature;

unsafe extern "C" {
    fn esp_rom_regi2c_read(block: u8, block_hostid: u8, reg_add: u8) -> u8;
    fn rom_i2c_writeReg(block: u8, block_hostid: u8, reg_add: u8, indata: u8);
}

/// Temperature range and calibration offset for the TSENS DAC trim.
///
/// Each variant pairs a DAC trim value (written to the SAR I2C analog
/// block via `reg_value`) with a Celsius offset coefficient (applied in
/// software during conversion via `offset`). A wider operating range
/// trades accuracy for coverage; choose the variant that includes the
/// expected die temperature for best precision.
#[derive(Clone, Copy, Default, defmt::Format)]
pub enum Range {
    /// -40..20 °C, ±3 °C accuracy.
    Cold,
    /// -30..50 °C, ±2 °C accuracy.
    Cool,
    /// -10..80 °C, ±1 °C accuracy.
    #[default]
    Standard,
    /// 20..100 °C, ±2 °C accuracy.
    Warm,
    /// 50..125 °C, ±3 °C accuracy.
    Hot,
}

impl Range {
    /// DAC trim value written to the `I2C_SARADC_TSENS_DAC` field.
    fn reg_value(self) -> u8 {
        match self {
            Self::Cold => 10,
            Self::Cool => 11,
            Self::Standard => 15,
            Self::Warm => 7,
            Self::Hot => 5,
        }
    }

    /// Offset coefficient used in the Celsius conversion formula.
    fn offset(self) -> i8 {
        match self {
            Self::Cold => 2,
            Self::Cool => 1,
            Self::Standard => 0,
            Self::Warm => -1,
            Self::Hot => -2,
        }
    }
}

/// ESP32-S3 on-die temperature sensor driver.
///
/// Owns the `SENS` peripheral token for the lifetime of the driver.
/// Call [`Tsens::new`] to run the hardware initialization sequence,
/// then use [`read_raw`](Tsens::read_raw) or
/// [`read_temperature`](Tsens::read_temperature) to take readings.
///
/// The driver does not implement [`Drop`]: dropping the handle returns
/// the `SENS` token but leaves the sensor powered. Power it down
/// explicitly before reusing the peripheral if needed.
pub struct Tsens<'d> {
    // Owning the SENS token prevents another driver from being constructed
    // from the same peripheral. It does not block direct SENS::regs() access
    // elsewhere, since that is a free-standing const fn.
    _sens: SENS<'d>,
    range: Range,
}

/// Polling interval while waiting for `sar_tsens_ready`.
///
/// The TSENS conversion takes a few microseconds at a clock divider of
/// 6. Yielding between polls lets the executor run other tasks instead
/// of busy-waiting through the conversion.
const TSENS_READY_POLL_INTERVAL: Duration = Duration::from_micros(5);

/// Maximum number of poll iterations before declaring a timeout.
///
/// At 5 µs per poll, 20 iterations gives a 95 µs budget — well
/// beyond the worst-case conversion time at `clk_div=6`. The budget
/// assumes the embassy-time tick rate is at least 1 MHz; coarser ticks
/// would round each `Timer::after` up and stretch the budget
/// proportionally.
const TSENS_READY_MAX_ITER: u32 = 20;

/// SAR I2C block address for the SAR ADC analog calibration registers.
const REGI2C_SAR_I2C_BLOCK: u8 = 0x69;

/// Host ID for the SAR I2C master. The ESP32-S3 uses host 1 for this block.
const REGI2C_SAR_I2C_HOST_ID: u8 = 1;

/// Register address of `I2C_SARADC_TSENS_DAC` within the SAR I2C block.
const REGI2C_TSENS_DAC_REG: u8 = 0x06;

/// Mask for the 4-bit `I2C_SARADC_TSENS_DAC` field at bits 3:0.
const REGI2C_TSENS_DAC_MASK: u8 = 0x0F;

/// Coefficients of the esp-idf v5.4 conversion formula
/// `millicelsius = (RAW_COEFF * raw - OFFSET_COEFF * offset - BIAS) / DIVISOR`
/// derived from the float form `(0.4386 * raw - 27.88 * offset - 20.52) * 1000`
/// by multiplying each coefficient by 10000 and dividing the final result
/// by 10 to land in millicelsius.
const RAW_COEFF: i32 = 4386;
const OFFSET_COEFF: i32 = 278_800;
const BIAS: i32 = 205_200;
const DIVISOR: i32 = 10;

impl<'d> Tsens<'d> {
    /// Initialize the TSENS hardware and return a ready driver.
    ///
    /// Runs the init sequence from esp-idf v4.4: writes the range
    /// calibration value, sets the clock divider, gates the TSENS clock,
    /// forces power-up via software control, enables the XPD force, waits
    /// 300 µs for analog stabilization, then clears `dump_out`. The two
    /// writes that the v4.4 sequence performs against `ANA_CONFIG_REG`
    /// and `ANA_CONFIG2_REG` to enable the SAR I2C bus are skipped: their
    /// reset values already match what those writes would apply.
    pub async fn new(sens: SENS<'d>, range: Range) -> Self {
        // Write the DAC calibration range value to I2C_SARADC_TSENS_DAC.
        // Read-modify-write preserves any reserved upper bits already set.
        // SAFETY: regi2c block 0x69 must not be in concurrent use; see the
        // module-level doc on radio coexistence.
        unsafe {
            let current = esp_rom_regi2c_read(
                REGI2C_SAR_I2C_BLOCK,
                REGI2C_SAR_I2C_HOST_ID,
                REGI2C_TSENS_DAC_REG,
            );
            let new_value =
                (current & !REGI2C_TSENS_DAC_MASK) | (range.reg_value() & REGI2C_TSENS_DAC_MASK);
            rom_i2c_writeReg(
                REGI2C_SAR_I2C_BLOCK,
                REGI2C_SAR_I2C_HOST_ID,
                REGI2C_TSENS_DAC_REG,
                new_value,
            );
        }

        // Clock divider of 6 is the esp-idf v4.4 default. The field is
        // 8 bits wide.
        // SAFETY: `bits()` bypasses PAC range checking; 6 fits in the
        // 8-bit field.
        SENS::regs()
            .sar_tsens_ctrl()
            .modify(|_, w| unsafe { w.sar_tsens_clk_div().bits(6) });

        // Enable the TSENS clock gate in the SAR peripheral clock register.
        SENS::regs()
            .sar_peri_clk_gate_conf()
            .modify(|_, w| w.tsens_clk_en().set_bit());

        // Force power-up via software control rather than FSM. Both bits
        // must be set: force bit enables SW override, power_up bit asserts
        // it.
        SENS::regs().sar_tsens_ctrl().modify(|_, w| {
            w.sar_tsens_power_up_force()
                .set_bit()
                .sar_tsens_power_up()
                .set_bit()
        });

        // Enable the XPD analog override so the sensor stays powered up.
        // The field is 2 bits wide in the PAC; esp-idf assigns a C bool
        // to it, which stores 0b01. Match that exactly rather than
        // guessing wider semantics.
        // SAFETY: `bits()` bypasses PAC range checking; 0b01 fits in
        // the 2-bit field.
        SENS::regs()
            .sar_tsens_ctrl2()
            .modify(|_, w| unsafe { w.sar_tsens_xpd_force().bits(0b01) });

        // Wait 300 µs for the analog circuitry to stabilize after power-up.
        // esp-idf v4.4 explicitly delays at this point.
        Timer::after(Duration::from_micros(300)).await;

        // Clear dump_out after the stabilization delay so the first
        // explicit read call starts from a clean state.
        SENS::regs()
            .sar_tsens_ctrl()
            .modify(|_, w| w.sar_tsens_dump_out().clear_bit());

        defmt::debug!("tsens: initialized with range {}", range);

        Self { _sens: sens, range }
    }

    /// Trigger a single TSENS conversion and return the raw 8-bit output.
    ///
    /// Asserts `dump_out` to start the conversion, polls
    /// `sar_tsens_ready` with a bounded budget while yielding to the
    /// executor between polls, then de-asserts `dump_out`. Returns
    /// `None` if the ready bit does not assert within the budget.
    pub async fn read_raw(&mut self) -> Option<u8> {
        SENS::regs()
            .sar_tsens_ctrl()
            .modify(|_, w| w.sar_tsens_dump_out().set_bit());

        let mut raw = None;
        for _ in 0..TSENS_READY_MAX_ITER {
            let control = SENS::regs().sar_tsens_ctrl().read();
            if control.sar_tsens_ready().bit_is_set() {
                raw = Some(control.sar_tsens_out().bits());
                break;
            }
            Timer::after(TSENS_READY_POLL_INTERVAL).await;
        }

        SENS::regs()
            .sar_tsens_ctrl()
            .modify(|_, w| w.sar_tsens_dump_out().clear_bit());

        if raw.is_none() {
            defmt::warn!(
                "tsens: conversion timed out after {} iterations",
                TSENS_READY_MAX_ITER
            );
        }
        raw
    }

    /// Read the die temperature.
    ///
    /// Applies the esp-idf v5.4 linear formula using the calibration
    /// offset for the configured [`Range`]. The per-chip eFuse delta
    /// `s_deltaT` is not applied; on most ESP32-S3 parts the eFuse field
    /// is zero, so the correction is a no-op and reading it would only add
    /// boot-time work. Add eFuse access here if sub-degree accuracy is
    /// ever required.
    pub async fn read_temperature(&mut self) -> Option<Temperature> {
        let output = self.read_raw().await?;
        Some(
            RawTsens {
                output,
                range: self.range,
            }
            .into(),
        )
    }
}

/// A TSENS conversion output with the [`Range`] it was taken in, because
/// the output alone does not give the temperature.
#[derive(Clone, Copy)]
struct RawTsens {
    output: u8,
    range: Range,
}

/// The esp-idf linear formula, in integer millicelsius.
impl From<RawTsens> for Temperature {
    fn from(raw: RawTsens) -> Self {
        let output = i32::from(raw.output);
        let offset = i32::from(raw.range.offset());
        Self::from_millicelsius((RAW_COEFF * output - OFFSET_COEFF * offset - BIAS) / DIVISOR)
    }
}
