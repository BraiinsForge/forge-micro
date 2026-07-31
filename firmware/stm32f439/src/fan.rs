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

//! Fan PWM drive and tachometer readback via TIM4.
//!
//! `TIM4_CH3` (PB8, AF2) drives the fan's 25 kHz PWM input.
//! `TIM4_CH4` (PB9, AF2) captures rising edges from the fan tach signal.
//! Standard 4-wire fans emit two pulses per revolution, so RPM is
//! `pulses × 60 / 2` over the sample window.
//!
//! The STM32 GP timer cannot count external edges on TI4FP4 in external
//! clock mode 1: RM0090 §18.3.3 restricts that mode to TI1FP1 and TI2FP2
//! trigger sources. Instead, the tach task polls the CH4 capture interrupt
//! flag (`CCxIF`) at `TACH_POLL_INTERVAL` intervals and counts rising-edge
//! captures over `TACH_WINDOW`. Reading CCR4 clears `CCxIF` atomically
//! (RM0090 §18.4.11), so no NVIC binding is required.
//!
//! Both channels are configured in `BoardFan::new` via the low-level `Timer`
//! API on the single `Peri<'static, TIM4>`. `TachHardware` accesses the
//! same timer registers via the PAC constant `embassy_stm32::pac::TIM4`
//! so no peripheral singleton needs to be shared.

use defmt::info;
use embassy_stm32::Peri;
use embassy_stm32::gpio::{AfType, Flex, OutputType, Pull, Speed};
use embassy_stm32::pac;
use embassy_stm32::peripherals::{PB8, PB9, TIM4};
use embassy_stm32::time::Hertz;
use embassy_stm32::timer::Channel;
use embassy_stm32::timer::low_level::{
    CountingMode, FilterValue, InputCaptureMode, InputTISelection, OutputCompareMode, RoundTo,
    Timer,
};
use embassy_time::{Duration, Instant, Timer as EmbassyTimer};
use portable_atomic::{AtomicU32, Ordering};
use ubos_cooling::{FanOutput, TachReader};
use ubos_core::units::{FanPwm, FanRpm};

/// Board-local RPM sample written by [`run_tach`] and read by [`Tach`].
static FAN_RPM_SAMPLE: AtomicU32 = AtomicU32::new(0);

/// Target PWM frequency per Intel 4-wire fan specification.
const FAN_PWM_FREQ: Hertz = Hertz(25_000);

/// Tach sample window: accumulate edges for one second then report RPM.
const TACH_WINDOW: Duration = Duration::from_secs(1);

/// Poll interval for the tach edge-count loop. 1 ms gives sub-percent
/// accuracy up to ~1000 Hz pulse rate (30 000 RPM at 2 pulses/rev).
const TACH_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Rising edges emitted by a 4-wire fan per revolution.
const TACH_PULSES_PER_REV: u64 = 2;

/// Alternate function index for TIM4 on PB8 and PB9 (AF2 per RM0090 table 9).
const TIM4_AF: u8 = 2;

/// Fan PWM output backed by TIM4 CH3 (PB8).
///
/// The low-level `Timer` is held so the ARR / CCR3 shadow registers
/// remain valid for the lifetime of the firmware. CH4 input capture for
/// tach is also configured here; `TachHardware` accesses those registers
/// via the PAC constant without needing the peripheral singleton.
pub struct BoardFan {
    timer: Timer<'static, TIM4>,
    /// Cached ARR + 1; used to convert percent to absolute compare value.
    arr: u32,
    /// Pin bindings kept alive so AF routing stays active.
    _pwm_pin: Flex<'static>,
    _tach_pin: Flex<'static>,
}

impl BoardFan {
    /// Construct a `BoardFan` from TIM4 (PB8 PWM, PB9 tach).
    ///
    /// Configures TIM4 for 25 kHz PWM on CH3 and input capture on CH4
    /// in a single timer instance. Both pins are set to AF2.
    #[must_use]
    pub fn new(
        tim4: Peri<'static, TIM4>,
        pb8: Peri<'static, PB8>,
        pb9: Peri<'static, PB9>,
    ) -> Self {
        let mut pwm_pin = Flex::new(pb8);
        critical_section::with(|_| {
            pwm_pin.set_as_af_unchecked(
                TIM4_AF,
                AfType::output(OutputType::PushPull, Speed::VeryHigh),
            );
        });

        let mut tach_pin = Flex::new(pb9);
        critical_section::with(|_| {
            // Pull-up: tach output is open-collector, idles high between pulses.
            tach_pin.set_as_af_unchecked(TIM4_AF, AfType::input(Pull::Up));
        });

        let timer = Timer::new(tim4);
        timer.set_counting_mode(CountingMode::EdgeAlignedUp);
        // Use Faster rounding (actual frequency >= 25 kHz) to stay within spec.
        timer.set_frequency(FAN_PWM_FREQ, RoundTo::Faster);
        timer.enable_outputs();
        timer.set_autoreload_preload(true);

        // CH3: PWM mode 1 output (high while CNT < CCR3).
        timer.set_output_compare_mode(Channel::Ch3, OutputCompareMode::PwmMode1);
        timer.set_output_compare_preload(Channel::Ch3, true);
        timer.set_compare_value(Channel::Ch3, 0u16);
        timer.enable_channel(Channel::Ch3, true);

        // CH4: input capture on TI4 (normal mapping), no filter, no prescaler,
        // rising-edge polarity (CCxP = 0, CCxNP = 0).
        timer.set_input_ti_selection(Channel::Ch4, InputTISelection::Normal);
        timer.set_input_capture_filter(Channel::Ch4, FilterValue::NO_FILTER);
        timer.set_input_capture_prescaler(Channel::Ch4, 0);
        timer.set_input_capture_mode(Channel::Ch4, InputCaptureMode::Rising);
        timer.enable_channel(Channel::Ch4, true);

        timer.generate_update_event();
        timer.start();

        // Resolve ARR from the register so the percent conversion is exact.
        // `set_frequency` writes PSC + ARR; ARR holds top − 1, so we add 1.
        let arr = u32::from(timer.regs_gp16().arr().read().arr()) + 1;

        // Clear any stale capture flag before the tach task starts counting.
        timer.clear_input_interrupt(Channel::Ch4);

        Self {
            timer,
            arr,
            _pwm_pin: pwm_pin,
            _tach_pin: tach_pin,
        }
    }
}

impl FanOutput for BoardFan {
    fn set_pwm(&mut self, pwm: FanPwm) {
        let percent = pwm.percent();
        let compare = u32::from(percent) * self.arr / 100;
        // compare ≤ arr ≤ 65 535 (TIM4 is a 16-bit timer); truncation is safe.
        #[allow(clippy::cast_possible_truncation)]
        self.timer.set_compare_value(Channel::Ch3, compare as u16);
    }
}

/// Tach input hardware consumed by [`run_tach`].
///
/// Holds no peripheral singleton: the PAC constant `pac::TIM4` gives
/// register access without requiring a separate `Peri<'static, TIM4>`.
/// All CH4 configuration was already applied in `BoardFan::new`.
pub struct TachHardware;

impl TachHardware {
    /// Construct the tach hardware handle.
    ///
    /// Requires `board_fan` to already exist so CH4 is configured.
    /// The `_board_fan` argument is only a proof-of-configuration; no
    /// borrow escapes this constructor.
    #[must_use]
    pub fn new(_board_fan: &BoardFan) -> Self {
        Self
    }
}

/// Sample tach pulses and publish RPM as a gauge. Runs forever.
///
/// Polls `CCxIF` for TIM4 CH4 every `TACH_POLL_INTERVAL` via the PAC.
/// Reading CCR4 via `get_capture_value` clears the flag per RM0090
/// §18.4.11 so no separate flag-clear call is needed.
pub async fn run_tach(_hardware: TachHardware) -> ! {
    info!(
        "fan tach: {} pulses/rev, window {} s",
        TACH_PULSES_PER_REV,
        TACH_WINDOW.as_secs(),
    );

    loop {
        let deadline = Instant::now() + TACH_WINDOW;
        let mut pulses: u64 = 0;

        while Instant::now() < deadline {
            EmbassyTimer::after(TACH_POLL_INTERVAL).await;
            // Check CH4 capture interrupt flag (bit 4 of TIM4 SR).
            // Safety: TIM4 PAC constant is valid; read-only to SR, then
            // CCR4 read clears the flag atomically.
            let ch4_flag = pac::TIM4.sr().read().ccif(Channel::Ch4.index());
            if ch4_flag {
                // Reading CCR4 atomically clears CCxIF per RM0090 §18.4.11.
                let _ = pac::TIM4.ccr(Channel::Ch4.index()).read();
                pulses += 1;
            }
        }

        let rpm = pulses * 60 / TACH_PULSES_PER_REV / TACH_WINDOW.as_secs();
        FAN_RPM_SAMPLE.store(u32::try_from(rpm).unwrap_or(u32::MAX), Ordering::Relaxed);
    }
}

/// Tach reader passed to [`ubos_cooling::run_cooling`].
///
/// Reads the latest RPM sample written by [`run_tach`] into the cooling loop.
pub struct Tach;

impl TachReader<1> for Tach {
    const LABELS: [Option<&'static str>; 1] = [None];

    fn read_rpms(&mut self) -> [FanRpm; 1] {
        [FanRpm::from_rpm(FAN_RPM_SAMPLE.load(Ordering::Relaxed))]
    }
}
