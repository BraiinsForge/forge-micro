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
//! trigger sources, and CH3 of this same timer is generating the PWM, so the
//! counter is already the PWM time base. The edges are therefore counted in
//! the NVIC handler for TIM4, which reads CCR4 to clear `CCxIF` (RM0090
//! §18.4.11) and increments [`TACH_EDGES`]. `read_rpms` converts the edges
//! since the previous read, so the caller's cadence sets the measurement
//! window, the same shape every other board's tach reader has.
//!
//! Both channels are configured in `BoardFan::new` via the low-level `Timer`
//! API on the single `Peri<'static, TIM4>`. The handler reaches the same
//! registers through the PAC constant `embassy_stm32::pac::TIM4`, so no
//! peripheral singleton needs to be shared.

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
// Two statements, not one `{self, InterruptExt}` group. The `#[interrupt]`
// attribute macro and the `interrupt` module share this path in different
// namespaces, and a plain `use` imports the name in every namespace it
// occupies. The `self` form of a nested group resolves its prefix as a
// module and imports that alone, so it leaves the attribute macro behind
// and rustc rejects `#[interrupt]` below as a module rather than an
// attribute.
use embassy_stm32::interrupt;
use embassy_stm32::interrupt::InterruptExt;
use embassy_time::Instant;
use portable_atomic::{AtomicU32, AtomicU64, Ordering};
use ubos_cooling::{FanOutput, TachReader, pulses_to_rpm};
use ubos_core::units::{FanPwm, FanRpm};

/// Tach rising edges since the last [`BoardTach::read_rpms`]. Incremented in
/// interrupt context by the TIM4 handler and reset to zero on each read.
static TACH_EDGES: AtomicU32 = AtomicU32::new(0);

/// Microsecond timestamp of the last counted tach edge, for the
/// interrupt-side debounce. Written and read only by the TIM4 handler.
static LAST_EDGE_US: AtomicU64 = AtomicU64::new(0);

/// Target PWM frequency per Intel 4-wire fan specification.
const FAN_PWM_FREQ: Hertz = Hertz(25_000);

/// Rising edges emitted by a 4-wire fan per revolution.
const TACH_PULSES_PER_REV: u64 = 2;

/// Shortest interval between two real tach pulses. The open-collector line
/// rings around the input threshold, so edges closer together than this are
/// chatter and are dropped in the handler. 500 us allows up to ~60 000 RPM at
/// two pulses per revolution, far above any real fan.
const TACH_MIN_PULSE_US: u64 = 500;

/// Alternate function index for TIM4 on PB8 and PB9 (AF2 per RM0090 table 9).
const TIM4_AF: u8 = 2;

/// Fan PWM output backed by TIM4 CH3 (PB8).
///
/// The low-level `Timer` is held so the ARR / CCR3 shadow registers
/// remain valid for the lifetime of the firmware. CH4 input capture for
/// tach is also configured here, and its capture interrupt is armed, so the
/// TIM4 handler counts edges for as long as the firmware runs.
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

        // Clear any edge latched during bring-up, then arm the capture
        // interrupt so the handler counts from here on. `BoardTach::new`
        // drops whatever accumulates before the first measurement window.
        timer.clear_input_interrupt(Channel::Ch4);
        timer.enable_input_interrupt(Channel::Ch4, true);
        // Safety: the handler only reads CCR4, which is the documented way to
        // clear the flag, and updates two atomics. It touches no register this
        // constructor or `FanOutput::set_pwm` writes.
        unsafe { interrupt::TIM4.enable() };

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

/// Count one tach edge per CH4 capture, then return.
///
/// Reading CCR4 clears `CCxIF` per RM0090 §18.4.11, so the flag needs no
/// separate write and the capture enable stays set. The handler is the entire
/// per-pulse cost: there is no task wake and no re-arm.
#[interrupt]
fn TIM4() {
    // Safety: the TIM4 PAC constant is a stable static register block. The
    // read below both takes the captured value and clears this channel's own
    // flag, so it aliases nothing `BoardFan` writes.
    let _ = pac::TIM4.ccr(Channel::Ch4.index()).read();

    // Debounce: the open-collector line rings, so one real pulse can raise
    // several edges. Count one only once TACH_MIN_PULSE_US has passed since
    // the last counted edge.
    let now_us = Instant::now().as_micros();
    if now_us.wrapping_sub(LAST_EDGE_US.load(Ordering::Relaxed)) >= TACH_MIN_PULSE_US {
        LAST_EDGE_US.store(now_us, Ordering::Relaxed);
        TACH_EDGES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Tach reader passed to [`ubos_cooling::run_cooling`].
///
/// Reads and resets [`TACH_EDGES`] and converts the edges since the previous
/// read into RPM over the measured interval. Resetting the counter on read
/// means the reader keeps no edge total of its own, so the reading depends
/// only on the caller's cadence, which the cooling loop fixes at one second.
pub struct BoardTach {
    last: Instant,
}

impl BoardTach {
    /// Open the first measurement window.
    ///
    /// Requires `board_fan` to already exist, which proves CH4 is configured
    /// and its capture interrupt is armed. The argument is only that proof; no
    /// borrow escapes this constructor.
    ///
    /// Edges counted between arming the interrupt and this baseline are
    /// dropped, so the first read divides its edge count by a matching window
    /// rather than by a shorter one.
    #[must_use]
    pub fn new(_board_fan: &BoardFan) -> Self {
        info!("fan tach: {} pulses/rev", TACH_PULSES_PER_REV);
        TACH_EDGES.store(0, Ordering::Relaxed);
        Self {
            last: Instant::now(),
        }
    }
}

impl TachReader<1> for BoardTach {
    const LABELS: [Option<&'static str>; 1] = [None];

    async fn read_rpms(&mut self) -> [FanRpm; 1] {
        let now = Instant::now();
        let edges = u64::from(TACH_EDGES.swap(0, Ordering::Relaxed));
        let elapsed_us = (now - self.last).as_micros();
        self.last = now;
        [pulses_to_rpm(edges, TACH_PULSES_PER_REV, elapsed_us)]
    }
}
