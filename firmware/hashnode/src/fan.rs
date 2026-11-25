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

//! Fan PWM drive and tachometer readback; `config::Board` names the pads.
//!
//! No tach maps to a `PWM_SLICE` channel B, so the slice edge counter is
//! unusable and each tach drives one PIO state machine that measures the
//! pulse-to-pulse period in hardware. A PIO block addresses a single 32-pin
//! GPIO window, so the five split across blocks, and an unpopulated
//! connector reads 0 rpm.
//!
//! The board's tach RC network charges the filter cap through a 4.7 kΩ
//! pull-up, so the rising edge ramps slowly through the input threshold
//! and supply noise chatters it into bursts of spurious edges. The PIO
//! program rejects that chatter by only accepting an edge once the line
//! has stayed high for 31 consecutive samples (~31 µs), longer than a
//! chatter pulse yet far shorter than a real tach half-period.

use embassy_rp::Peri;
use embassy_rp::pac;
use embassy_rp::peripherals::{
    PIN_10, PIN_11, PIN_23, PIN_24, PIN_25, PIN_38, PIN_43, PIN_44, PIO0, PIO1, PWM_SLICE5,
    PWM_SLICE9,
};
use embassy_rp::pio::{Pio, StateMachine};
use embassy_rp::pwm::{Config as PwmConfig, Pwm};
use ubos_cooling::{FanOutput, TachReader};
use ubos_core::units::{FanPwm, FanRpm};
use ubos_rp::pio_tach::{latest_rpm, setup_tach, tach_program};

use crate::config::Fan;

/// Number of fan connectors, each with its own tachometer.
pub const TACH_COUNT: usize = 5;

/// Every channel runs at one shared duty, so all the fans track together.
pub struct BoardFan {
    pwm_ab: Pwm<'static>,
    pwm_c: Pwm<'static>,
    top: u16,
}

impl BoardFan {
    #[must_use]
    pub fn new(
        slice_ab: Peri<'static, PWM_SLICE5>,
        pin_a: Peri<'static, PIN_10>,
        pin_b: Peri<'static, PIN_11>,
        slice_c: Peri<'static, PWM_SLICE9>,
        pin_c: Peri<'static, PIN_43>,
    ) -> Self {
        let mut pwm_config = PwmConfig::default();
        pwm_config.top = Fan::PWM_TOP;
        let pwm_ab = Pwm::new_output_ab(slice_ab, pin_a, pin_b, pwm_config.clone());
        // GPIO43 is PWM_SLICE9's B channel; the slice's A pin is not
        // routed to a fan, so only channel B is claimed.
        let pwm_c = Pwm::new_output_b(slice_c, pin_c, pwm_config);
        Self {
            pwm_ab,
            pwm_c,
            top: Fan::PWM_TOP,
        }
    }
}

impl FanOutput for BoardFan {
    async fn set_pwm(&mut self, pwm: FanPwm) {
        let percent = pwm.percent();
        // An inverting buffer drives the fans, so a higher duty on the GPIO
        // runs the fan slower. Invert the duty here so the commanded percent
        // matches the actual fan speed; confirmed on hardware by a duty/RPM
        // sweep.
        let duty = 100 - percent;
        let compare = u32::from(self.top) * u32::from(duty) / 100;
        let mut config = PwmConfig::default();
        config.top = self.top;
        // u16 cast safe: compare <= self.top, which fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        {
            config.compare_a = compare as u16;
            config.compare_b = compare as u16;
        }
        self.pwm_ab.set_config(&config);
        self.pwm_c.set_config(&config);
    }
}

/// Each field owns the PIO state machine driving that connector's
/// tachometer.
// One field per connector; the state machines are distinct const-generic
// types, so they cannot share an array and every clear name shares a prefix.
#[allow(clippy::struct_field_names)]
pub struct BoardTach {
    sm_a: StateMachine<'static, PIO0, 0>,
    sm_b: StateMachine<'static, PIO0, 1>,
    sm_c: StateMachine<'static, PIO0, 2>,
    sm_d: StateMachine<'static, PIO1, 0>,
    sm_e: StateMachine<'static, PIO1, 1>,
}

impl BoardTach {
    /// Consumes both `Pio`s: the `common` handles are needed for setup
    /// only, and dropping them leaves the running state machines alone.
    #[must_use]
    pub fn new(
        mut pio0: Pio<'static, PIO0>,
        mut pio1: Pio<'static, PIO1>,
        pin_a: Peri<'static, PIN_24>,
        pin_b: Peri<'static, PIN_25>,
        pin_c: Peri<'static, PIN_23>,
        pin_d: Peri<'static, PIN_38>,
        pin_e: Peri<'static, PIN_44>,
    ) -> Self {
        let program0 = tach_program(&mut pio0.common);
        let sm_a = setup_tach(&mut pio0.common, pio0.sm0, pin_a, &program0);
        let sm_b = setup_tach(&mut pio0.common, pio0.sm1, pin_b, &program0);
        let sm_c = setup_tach(&mut pio0.common, pio0.sm2, pin_c, &program0);
        let program1 = tach_program(&mut pio1.common);
        let sm_d = setup_tach(&mut pio1.common, pio1.sm0, pin_d, &program1);
        let sm_e = setup_tach(&mut pio1.common, pio1.sm1, pin_e, &program1);
        // embassy-rp 0.10.0 subtracts the PIO GPIOBASE window shift from the
        // IN/SET/OUT/SIDESET bases but not from EXECCTRL.JMP_PIN, which the
        // RP2350 also interprets relative to GPIOBASE. GPIO38/GPIO44 sit in
        // PIO1's base-16 window, so the driver leaves `jmp pin` watching the
        // wrong GPIO and the tach program never sees an edge (0 rpm). Re-point
        // it at the window-relative index. Base-0 blocks (PIO0, GPIO<32) are
        // correct as written, so only PIO1 needs the fix-up.
        pac::PIO1
            .sm(0)
            .execctrl()
            .modify(|w| w.set_jmp_pin(Fan::TACH_D_GPIO - Fan::PIO_HIGH_WINDOW_BASE));
        pac::PIO1
            .sm(1)
            .execctrl()
            .modify(|w| w.set_jmp_pin(Fan::TACH_E_GPIO - Fan::PIO_HIGH_WINDOW_BASE));
        Self {
            sm_a,
            sm_b,
            sm_c,
            sm_d,
            sm_e,
        }
    }
}

impl TachReader<TACH_COUNT> for BoardTach {
    const LABELS: [Option<&'static str>; TACH_COUNT] =
        [Some("a"), Some("b"), Some("c"), Some("d"), Some("e")];

    async fn read_rpms(&mut self) -> [FanRpm; TACH_COUNT] {
        [
            latest_rpm(&mut self.sm_a, Fan::TACH_PULSES_PER_REV),
            latest_rpm(&mut self.sm_b, Fan::TACH_PULSES_PER_REV),
            latest_rpm(&mut self.sm_c, Fan::TACH_PULSES_PER_REV),
            latest_rpm(&mut self.sm_d, Fan::TACH_PULSES_PER_REV),
            latest_rpm(&mut self.sm_e, Fan::TACH_PULSES_PER_REV),
        ]
    }
}
