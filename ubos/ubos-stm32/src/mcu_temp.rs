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

//! The MCU internal temperature sensor, read through ADC1. TIM3 runs free
//! as a TRGO source for that ADC, and the timer peripheral is moved into
//! the sensor to keep its RCC clock alive for as long as the sensor lives.
//!
//! Both STM32 control boards wire ADC1 to the die thermometer through
//! TIM3 and a DMA ring buffer, so this module holds the wiring and the
//! reading for both.

use embassy_stm32::adc::{Adc, AdcChannel, Exten, RingBufferedAdc, RxDma, SampleTime};
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::peripherals::{ADC1, TIM3};
use embassy_stm32::time::Hertz;
use embassy_stm32::timer::low_level::{MasterMode, RoundTo, Timer as LowLevelTimer};
use embassy_stm32::triggers::TIM3_TRGO;
use embassy_stm32::{Peri, dma};
use ubos_metrics::mcu_temperature::McuTemperatureSensor;

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

/// TIM3 TRGO period: one temperature sample per second.
const TRIGGER_HZ: Hertz = Hertz(1);

/// The MCU internal thermometer. The `_timer` field is never read; it
/// exists to keep TIM3's RCC clock alive for as long as the sensor lives.
pub struct McuTempSensor {
    ring: RingBufferedAdc<'static, ADC1>,
    _timer: LowLevelTimer<'static, TIM3>,
}

impl McuTemperatureSensor for McuTempSensor {
    async fn read(&mut self) -> Option<ubos_core::units::Temperature> {
        let mut samples = [0u16; 1];
        self.ring.read(&mut samples).await.ok()?;
        let vsense_uv = i64::from(samples[0]) * VREF_UV / ADC_FULL_SCALE;
        let millicelsius = CALIBRATION_MC + (vsense_uv - V25_UV) * 1000 / SLOPE_UV_PER_C;
        i32::try_from(millicelsius)
            .ok()
            .map(ubos_core::units::Temperature::from_millicelsius)
    }
}

impl McuTempSensor {
    /// Build the MCU temperature sensor: start TIM3 as a one-shot-per-second
    /// TRGO source, then set ADC1 up as a DMA ring buffer that samples the
    /// die thermometer on each rising edge. `dma_buf` backs the ring and
    /// must outlive the sensor.
    pub fn new<D: RxDma<ADC1>>(
        adc: Peri<'static, ADC1>,
        adc_dma: Peri<'static, D>,
        temp_trigger_timer: Peri<'static, TIM3>,
        dma_buf: &'static mut [u16],
        irqs: impl Binding<D::Interrupt, dma::InterruptHandler<D>> + 'static,
    ) -> Self {
        // TIM3 emits TRGO once per second via master-mode UPDATE; the ADC runs
        // single-shot conversions on each rising edge, so the DMA ring buffer
        // receives exactly one temperature sample per trigger.
        let temp_timer = LowLevelTimer::new(temp_trigger_timer);
        temp_timer.set_frequency(TRIGGER_HZ, RoundTo::Slower);
        temp_timer.set_master_mode(MasterMode::UPDATE);
        temp_timer.start();

        let adc = Adc::new(adc);
        let temp_channel = adc.enable_temperature();
        let temp_ring = adc.into_ring_buffered(
            adc_dma,
            dma_buf,
            irqs,
            [(temp_channel.degrade_adc(), SampleTime::CYCLES480)].into_iter(),
            TIM3_TRGO,
            Exten::RISING_EDGE,
        );

        McuTempSensor {
            ring: temp_ring,
            _timer: temp_timer,
        }
    }
}
