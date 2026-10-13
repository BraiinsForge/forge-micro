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

//! The MCU internal temperature sensor, read through ADC1.
//!
//! Both STM32 control boards wire ADC1 to the die thermometer and nothing
//! else, so this module holds the wiring and the reading for both.

use embassy_stm32::Peri;
use embassy_stm32::adc::{Adc, SampleTime, Temperature};
use embassy_stm32::peripherals::ADC1;
use ubos_thermal::McuTemperatureSensor;

/// The die thermometer reads about 0.76 V at 25 °C and moves 2.5 mV per
/// degree, sampled by the 12-bit ADC against a 3.3 V reference. The
/// reading in millicelsius is `CALIBRATION_MC` plus the sense voltage
/// above `V25_UV`, in microvolts, scaled by `SLOPE_UV_PER_C`. The math
/// runs in `i64` because the raw sample times `VREF_UV` overflows `i32`.
const VREF_UV: i64 = 3_300_000;
const ADC_FULL_SCALE: i64 = 4095;
const V25_UV: i64 = 760_000;
const SLOPE_UV_PER_C: i64 = 2_500;
const CALIBRATION_MC: i64 = 25_000;

/// Sample time for the die thermometer. The sensor needs about 10 µs to
/// settle on the sample-and-hold capacitor, and 480 ADC cycles is the
/// longest the peripheral offers, so it clears that on every ADC clock
/// this board runs.
const TEMP_SAMPLE_TIME: SampleTime = SampleTime::CYCLES480;

/// The MCU internal thermometer.
pub struct McuTempSensor {
    adc: Adc<'static, ADC1>,
    channel: Temperature,
}

/// The read converts once and returns, so the caller keeps its own
/// period. The conversion takes tens of microseconds and leaves
/// interrupts enabled.
impl McuTemperatureSensor for McuTempSensor {
    async fn read(&mut self) -> Option<ubos_core::units::Temperature> {
        let raw = self.adc.blocking_read(&mut self.channel, TEMP_SAMPLE_TIME);
        let vsense_uv = i64::from(raw) * VREF_UV / ADC_FULL_SCALE;
        let millicelsius = CALIBRATION_MC + (vsense_uv - V25_UV) * 1000 / SLOPE_UV_PER_C;
        i32::try_from(millicelsius)
            .ok()
            .map(ubos_core::units::Temperature::from_millicelsius)
    }
}

impl McuTempSensor {
    /// Build the MCU temperature sensor by taking ADC1 and switching the
    /// die thermometer onto it. The channel stays enabled for as long as
    /// the sensor lives, so each read is one conversion.
    #[must_use]
    pub fn new(adc: Peri<'static, ADC1>) -> Self {
        let adc = Adc::new(adc);
        let channel = adc.enable_temperature();
        McuTempSensor { adc, channel }
    }
}
