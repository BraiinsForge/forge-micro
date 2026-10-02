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

//! Fan PWM drive and tachometer readback for the ESP32-S3 control board.
//!
//! LEDC `LowSpeed` Timer0 Channel0 drives the 25 kHz PWM on GPIO10, and PCNT
//! Unit0 counts rising edges on the tach input.

use defmt::info;
use embassy_time::Instant;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::ledc::channel::ChannelIFace;
use esp_hal::ledc::timer::TimerIFace;
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
use esp_hal::pcnt::Pcnt;
use esp_hal::pcnt::channel::{CtrlMode, EdgeMode};
use esp_hal::peripherals::{GPIO10, GPIO11, LEDC, PCNT};
use esp_hal::time::Rate;
use static_cell::StaticCell;
use ubos_cooling::{FanOutput, TachReader, pulses_to_rpm};
use ubos_core::units::{FanPwm, FanRpm};

/// Target PWM frequency per Intel 4-wire fan specification.
const FAN_PWM_FREQ: Rate = Rate::from_khz(25);

/// Duty resolution: 10 bits give 1024 steps, about 0.1 % of duty at 25 kHz.
const DUTY_BITS: esp_hal::ledc::timer::config::Duty = esp_hal::ledc::timer::config::Duty::Duty10Bit;

/// Fan PWM output backed by LEDC `LowSpeed` Timer0, Channel0.
pub struct BoardFan {
    channel: esp_hal::ledc::channel::Channel<'static, LowSpeed>,
}

impl BoardFan {
    /// The LEDC peripheral is consumed here, so it must come from
    /// `Board::take` and cannot be claimed twice.
    ///
    /// # Panics
    ///
    /// Panics if the LEDC timer or channel rejects its configuration,
    /// which cannot happen for the fixed 25 kHz, 10-bit setup used here.
    #[must_use]
    pub fn new(ledc: LEDC<'static>, pwm_pin: GPIO10<'static>) -> Self {
        static LEDC_CELL: StaticCell<Ledc<'static>> = StaticCell::new();
        static TIMER_CELL: StaticCell<esp_hal::ledc::timer::Timer<'static, LowSpeed>> =
            StaticCell::new();

        // `ledc` is moved into a static cell so Channel0 can hold a 'static
        // reference to the timer.
        let ledc_ref: &'static mut Ledc<'static> = LEDC_CELL.init(Ledc::new(ledc));
        ledc_ref.set_global_slow_clock(LSGlobalClkSource::APBClk);

        let mut lstimer0 = ledc_ref.timer::<LowSpeed>(esp_hal::ledc::timer::Number::Timer0);
        // Valid frequency and duty bit count; timer configuration cannot fail
        // for APB clock with this combination.
        lstimer0
            .configure(esp_hal::ledc::timer::config::Config {
                duty: DUTY_BITS,
                clock_source: esp_hal::ledc::timer::LSClockSource::APBClk,
                frequency: FAN_PWM_FREQ,
            })
            .expect("BUG: LEDC timer configuration failed");
        let lstimer0_ref: &'static _ = TIMER_CELL.init(lstimer0);

        let mut channel0 = ledc_ref.channel(esp_hal::ledc::channel::Number::Channel0, pwm_pin);
        // Valid timer reference and 0 % duty; cannot fail.
        channel0
            .configure(esp_hal::ledc::channel::config::Config {
                timer: lstimer0_ref,
                duty_pct: 0,
                drive_mode: esp_hal::gpio::DriveMode::PushPull,
            })
            .expect("BUG: LEDC channel configuration failed");

        Self { channel: channel0 }
    }
}

impl FanOutput for BoardFan {
    async fn set_pwm(&mut self, pwm: FanPwm) {
        let percent = pwm.percent();
        // `set_duty` takes a percentage and cannot fail with a valid timer
        // attached.
        self.channel
            .set_duty(percent)
            .expect("BUG: LEDC set_duty failed");
    }
}

/// Tachometer reader backed by PCNT Unit0 counting rising tach edges.
pub struct BoardTach {
    /// Owned to keep PCNT Unit0 configured for the lifetime of the reader.
    pcnt: Pcnt<'static>,
    counter: esp_hal::pcnt::unit::Counter<'static, 0>,
    last: Instant,
    pulses_per_rev: u64,
}

impl BoardTach {
    /// The PCNT peripheral is consumed here, so it must come from
    /// `Board::take` and cannot be claimed twice.
    ///
    /// # Panics
    ///
    /// Panics if PCNT rejects the high limit or the filter setting,
    /// which cannot happen for the fixed values used here.
    #[must_use]
    pub fn new(pcnt: PCNT<'static>, tach_pin: GPIO11<'static>, pulses_per_rev: u64) -> Self {
        let pcnt_driver = Pcnt::new(pcnt);
        // Wrap at i16::MAX to defer overflow handling; the high limit must be
        // positive.
        pcnt_driver
            .unit0
            .set_high_limit(Some(i16::MAX))
            .expect("BUG: PCNT high limit invalid");
        pcnt_driver
            .unit0
            .set_filter(None)
            .expect("BUG: PCNT filter");

        // The tach output is open-collector, so the pull-up idles the line
        // high between pulses.
        let tach_input = Input::new(tach_pin, InputConfig::default().with_pull(Pull::Up));
        let tach_signal = tach_input.peripheral_input();

        let ch0 = &pcnt_driver.unit0.channel0;
        ch0.set_edge_signal(tach_signal);
        // No control signal; keep counter regardless of level.
        ch0.set_ctrl_mode(CtrlMode::Keep, CtrlMode::Keep);
        ch0.set_input_mode(EdgeMode::Hold, EdgeMode::Increment);

        pcnt_driver.unit0.clear();
        pcnt_driver.unit0.resume();

        let counter = pcnt_driver.unit0.counter.clone();

        info!("fan tach: {} pulses/rev", pulses_per_rev);
        Self {
            pcnt: pcnt_driver,
            counter,
            last: Instant::now(),
            pulses_per_rev,
        }
    }
}

impl TachReader<1> for BoardTach {
    const LABELS: [Option<&'static str>; 1] = [None];

    async fn read_rpms(&mut self) -> [FanRpm; 1] {
        let now = Instant::now();
        // A negative value means a spurious decrement; treat as zero.
        let counts = u64::try_from(self.counter.get().max(0)).unwrap_or(0);
        self.pcnt.unit0.clear();
        let elapsed_us = (now - self.last).as_micros();
        self.last = now;
        [pulses_to_rpm(counts, self.pulses_per_rev, elapsed_us)]
    }
}
