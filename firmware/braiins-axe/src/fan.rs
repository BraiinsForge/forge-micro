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

//! Fan PWM drive and tachometer readback.
//!
//! `PWM_SLICE4` channels A (GPIO8) and B (GPIO9) drive the two fan
//! connectors' PWM inputs at 25 kHz, the rate specified by the Intel
//! 4-wire fan spec. `BoardFan` exposes the PWM output behind the
//! `ubos_cooling::FanOutput` trait so the cooling task can drive duty
//! cycle from a PID loop.
//!
//! The two fan connectors each expose a tachometer: GPIO27
//! (`RP_TACHO1`) and GPIO28 (`RP_TACHO2`). Neither maps to a
//! `PWM_SLICE` channel B, so the PWM slice edge counter is
//! unusable; instead each tach drives one PIO state machine on `PIO0`
//! that measures the pulse-to-pulse period in hardware and pushes it to
//! its RX FIFO. `BoardTach` drains the latest period per fan each control
//! tick behind `ubos_cooling::TachReader`, which publishes one labeled
//! series per fan. Measuring in PIO removes the per-edge CPU wakeups the
//! old `wait_for_rising_edge` loop incurred.
//!
//! The tach filtering matches the hashnode: the RC network charges the
//! filter cap through a pull-up, so the rising edge ramps slowly through the input threshold
//! and supply noise chatters it into bursts of spurious edges. The PIO
//! program rejects that chatter by only accepting an edge once the line
//! has stayed high for 31 consecutive samples (~31 µs), longer than a
//! chatter pulse yet far shorter than a real tach half-period.

use embassy_rp::Peri;
use embassy_rp::peripherals::{PIN_8, PIN_9, PIN_27, PIN_28, PIO0, PWM_SLICE4};
use embassy_rp::pio::{Common, StateMachine};
use embassy_rp::pwm::{Config as PwmConfig, Pwm};
use ubos_cooling::{FanOutput, TachReader};
use ubos_core::units::{FanPwm, FanRpm};
use ubos_rp::pio_tach::{latest_rpm, setup_tach, tach_program};

use crate::config::Fan;

/// Number of fan connectors, each with its own tachometer.
pub const TACH_COUNT: usize = 2;

/// PWM output driving the fan input. Construct once and pass into the
/// cooling task; the cooling loop sets duty via `FanOutput::set_pwm`.
pub struct BoardFan {
    pwm: Pwm<'static>,
    top: u16,
}

impl BoardFan {
    #[must_use]
    pub fn new(
        slice: Peri<'static, PWM_SLICE4>,
        pin_a: Peri<'static, PIN_8>,
        pin_b: Peri<'static, PIN_9>,
    ) -> Self {
        let mut pwm_config = PwmConfig::default();
        pwm_config.top = Fan::PWM_TOP;
        // GPIO8 (RP_PWM1) and GPIO9 (RP_PWM2) are both channels of PWM_SLICE4;
        // driving both at one duty keeps the two fan connectors in step.
        let pwm = Pwm::new_output_ab(slice, pin_a, pin_b, pwm_config);
        Self {
            pwm,
            top: Fan::PWM_TOP,
        }
    }
}

impl FanOutput for BoardFan {
    fn set_pwm(&mut self, pwm: FanPwm) {
        let percent = pwm.percent();
        // A low-side BSS138 stage drives the fan PWM input, inverting the
        // GPIO duty like the hashnode's buffer does. Invert here so the
        // commanded percent matches the actual fan speed; pending a
        // duty/RPM sweep on hardware.
        let duty = 100 - percent;
        let compare = u32::from(self.top) * u32::from(duty) / 100;
        let mut config = PwmConfig::default();
        config.top = self.top;
        // u16 cast safe: compare <= self.top, which fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        {
            // Both channels (GPIO8/GPIO9) get the same duty.
            config.compare_a = compare as u16;
            config.compare_b = compare as u16;
        }
        self.pwm.set_config(&config);
    }
}

/// Reads the latest measured RPM for both connectors into the
/// cooling loop. Each field owns the PIO state machine driving that
/// connector's tachometer.
// One field per connector; the state machines are distinct const-generic
// types, so they cannot share an array and every clear name shares a prefix.
#[allow(clippy::struct_field_names)]
pub struct BoardTach {
    sm_a: StateMachine<'static, PIO0, 0>,
    sm_b: StateMachine<'static, PIO0, 1>,
}

impl BoardTach {
    /// Load the tach program into `PIO0` and start one state machine per
    /// connector. Borrows `common` rather than consuming the `Pio`: SM2
    /// belongs to the WS2812 status ring, so `main` splits the block.
    #[must_use]
    pub fn new(
        common: &mut Common<'static, PIO0>,
        sm0: StateMachine<'static, PIO0, 0>,
        sm1: StateMachine<'static, PIO0, 1>,
        pin_a: Peri<'static, PIN_27>,
        pin_b: Peri<'static, PIN_28>,
    ) -> Self {
        let program = tach_program(common);
        let sm_a = setup_tach(common, sm0, pin_a, &program);
        let sm_b = setup_tach(common, sm1, pin_b, &program);
        Self { sm_a, sm_b }
    }
}

impl TachReader<TACH_COUNT> for BoardTach {
    const LABELS: [Option<&'static str>; TACH_COUNT] = [Some("a"), Some("b")];

    async fn read_rpms(&mut self) -> [FanRpm; TACH_COUNT] {
        [
            latest_rpm(&mut self.sm_a, Fan::TACH_PULSES_PER_REV),
            latest_rpm(&mut self.sm_b, Fan::TACH_PULSES_PER_REV),
        ]
    }
}
