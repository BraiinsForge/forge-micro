//! Fan PWM drive and tachometer readback.
//!
//! The BMM Adapter has `FAN_PWM` on J13 pin 3 and `FAN_RPM` on
//! J13 pin 4, but the Intel 4-wire fan spec has TACH
//! on pin 3 and PWM on pin 4. Here we work with it
//! according to the spec, so the cable between the connector and
//! the fan must swap pins 3 and 4 relative to the schematic net
//! names.
//!
//! Slice 4 channel A on `PIN_8` drives the fan's PWM input at 25 kHz,
//! the rate specified by the Intel 4-wire fan spec. `BoardFan` exposes
//! the PWM output behind the `ubos_cooling::FanOutput` trait so the
//! cooling task can drive duty cycle from a PID loop.
//!
//! Slice 7 channel B on `PIN_15` runs in `RisingEdge` divmode and
//! counts tach pulses. Standard 4-wire fans emit two pulses per
//! revolution, so RPM is `pulses × 60 / 2` over the elapsed interval.
//! `BoardTach` exposes the count behind `ubos_cooling::TachReader`; the
//! cooling task samples it once per control tick. The tach output is
//! open-collector, so the pad's internal pull-up idles the line high
//! between pulses.

use defmt::info;
use embassy_rp::Peri;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{PIN_8, PIN_15, PWM_SLICE4, PWM_SLICE7};
use embassy_rp::pwm::{Config as PwmConfig, InputMode, Pwm};
use embassy_time::Instant;
use ubos_cooling::{FanOutput, TachReader};
use ubos_core::units::{FanPwm, FanRpm};

use crate::config::Fan;

/// PWM output driving the fan input. Construct once and pass into the
/// cooling task; the cooling loop sets duty via `FanOutput::set_pwm`.
pub struct BoardFan {
    pwm: Pwm<'static>,
    top: u16,
}

impl BoardFan {
    #[must_use]
    pub fn new(slice: Peri<'static, PWM_SLICE4>, pin: Peri<'static, PIN_8>) -> Self {
        let mut pwm_config = PwmConfig::default();
        pwm_config.top = Fan::PWM_TOP;
        let pwm = Pwm::new_output_a(slice, pin, pwm_config);
        Self {
            pwm,
            top: Fan::PWM_TOP,
        }
    }
}

impl FanOutput for BoardFan {
    fn set_pwm(&mut self, pwm: FanPwm) {
        let percent = pwm.percent();
        let compare = u32::from(self.top) * u32::from(percent) / 100;
        let mut config = PwmConfig::default();
        config.top = self.top;
        // u16 cast safe: compare <= self.top, which fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        {
            config.compare_a = compare as u16;
        }
        self.pwm.set_config(&config);
    }
}

/// Tachometer reader. Owns the PWM slice running as a rising-edge
/// counter on the tach pin; `read_rpm` snapshots and resets the count,
/// then converts it to RPM over the interval since the previous call.
pub struct BoardTach {
    tach: Pwm<'static>,
    last: Instant,
}

impl BoardTach {
    #[must_use]
    pub fn new(slice: Peri<'static, PWM_SLICE7>, pin: Peri<'static, PIN_15>) -> Self {
        let tach = Pwm::new_input(
            slice,
            pin,
            Pull::Up,
            InputMode::RisingEdge,
            PwmConfig::default(),
        );
        info!("fan tach: {} pulses/rev", Fan::TACH_PULSES_PER_REV);
        Self {
            tach,
            last: Instant::now(),
        }
    }
}

impl TachReader for BoardTach {
    fn read_rpm(&mut self) -> FanRpm {
        let now = Instant::now();
        let counts = u64::from(self.tach.counter());
        self.tach.set_counter(0);
        let elapsed_us = (now - self.last).as_micros();
        self.last = now;
        // `checked_div` yields 0 on the first call, when the interval can
        // round to zero microseconds. The 16-bit counter cannot overflow
        // at the control cadence: a 16 000 RPM fan emits ~533 edges/s.
        let rpm = (counts * 60 * 1_000_000 / Fan::TACH_PULSES_PER_REV)
            .checked_div(elapsed_us)
            .unwrap_or(0);
        FanRpm::from_rpm(u32::try_from(rpm).unwrap_or(u32::MAX))
    }
}
