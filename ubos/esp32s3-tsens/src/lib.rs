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
//! esp-hal 1.0.0 does not expose TSENS for the ESP32-S3, so this crate
//! drives the sensor through the SENS PAC peripheral directly. The
//! register sequence comes from esp-idf v4.4
//! `components/driver/esp32s3/rtc_tempsensor.c`, and the conversion
//! formula and range table from esp-idf v5.4
//! `components/hal/esp32s3/include/hal/temperature_sensor_ll.h`.
//!
//! The SAR I2C analog block, regi2c bus block 0x69, is shared with the
//! Wi-Fi and Bluetooth radios, and a call to [`Tsens::new`] while a radio
//! is using regi2c can corrupt the DAC calibration register. Initialize
//! the temperature sensor before any radio comes up.

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
/// A wider range trades accuracy for coverage, so the variant that covers
/// the expected die temperature reads most precisely.
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
/// There is no [`Drop`]: dropping the handle returns the `SENS` token but
/// leaves the sensor powered, so a caller that reuses the peripheral powers
/// it down itself.
pub struct Tsens<'d> {
    // Owning the SENS token prevents another driver from being constructed
    // from the same peripheral. It does not block direct SENS::regs() access
    // elsewhere, since that is a free-standing const fn.
    _sens: SENS<'d>,
    range: Range,
}

/// Polling interval while waiting for `sar_tsens_ready`. The conversion
/// takes a few microseconds at a clock divider of 6, and yielding between
/// polls lets the executor run instead of busy-waiting through it.
const TSENS_READY_POLL_INTERVAL: Duration = Duration::from_micros(5);

/// Poll iterations before a conversion is declared timed out.
///
/// At 5 µs per poll, 20 iterations gives a 95 µs budget, well beyond the
/// worst-case conversion time at `clk_div=6`. The budget assumes an
/// embassy-time tick rate of at least 1 MHz, since coarser ticks round
/// each `Timer::after` up and stretch it proportionally.
const TSENS_READY_MAX_ITER: u32 = 20;

/// SAR I2C block address for the SAR ADC analog calibration registers.
const REGI2C_SAR_I2C_BLOCK: u8 = 0x69;

/// Host ID the ESP32-S3 uses to reach this block on the SAR I2C master.
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
    /// The esp-idf v4.4 sequence also writes `ANA_CONFIG_REG` and
    /// `ANA_CONFIG2_REG` to enable the SAR I2C bus. Those two writes are
    /// skipped because the reset values already match what they apply.
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

        // The XPD analog override keeps the sensor powered up. Its field is
        // 2 bits wide in the PAC, and esp-idf assigns a C bool to it, which
        // stores 0b01.
        // SAFETY: `bits()` bypasses PAC range checking; 0b01 fits in
        // the 2-bit field.
        SENS::regs()
            .sar_tsens_ctrl2()
            .modify(|_, w| unsafe { w.sar_tsens_xpd_force().bits(0b01) });

        // The analog circuitry needs 300 µs to stabilize after power-up.
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
    /// `None` if `sar_tsens_ready` does not assert within the poll budget.
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

    /// Reads the die temperature, converting the raw code with the range
    /// configured in [`Tsens::new`].
    ///
    /// The conversion uses the nominal formula alone, because the per-chip
    /// eFuse delta is zero on most ESP32-S3 parts and reading it would add
    /// boot-time work for no correction.
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
