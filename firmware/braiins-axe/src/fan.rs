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
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{PIN_8, PIN_9, PIN_27, PIN_28, PIO0, PWM_SLICE4};
use embassy_rp::pio::{Common, Config, Direction, FifoJoin, LoadedProgram, PioPin, StateMachine};
use embassy_rp::pio_programs::clock_divider::calculate_pio_clock_divider;
use embassy_rp::pwm::{Config as PwmConfig, Pwm};
use ubos_cooling::{FanOutput, TachReader};
use ubos_core::units::{FanPwm, FanRpm};

use crate::config::Fan;

/// Number of fan connectors, each with its own tachometer.
pub const TACH_COUNT: usize = 2;

/// PIO state-machine clock. The counting loops are exactly
/// [`TACH_CYCLES_PER_ITER`] PIO cycles per period tick, so at 3 MHz one
/// tick is 1 µs.
const TACH_PIO_HZ: u32 = 3_000_000;

/// PIO cycles per period-counter decrement. Every loop that advances the
/// period counter (`s_deb`, `high`, `low` in [`tach_program`]) is padded
/// to this exact length so elapsed ticks map linearly to time regardless
/// of the input duty cycle.
const TACH_CYCLES_PER_ITER: u32 = 3;

/// Fastest RPM accepted as a real measurement; no fan on this board
/// approaches it.
const TACH_MAX_PLAUSIBLE_RPM: u64 = 30_000;

/// Shortest plausible period in ticks; anything shorter implies more
/// than [`TACH_MAX_PLAUSIBLE_RPM`] and is discarded as residual noise
/// rather than a real pulse.
// Fits u32: the divisor is far larger than can push the quotient past 2^32.
#[allow(clippy::cast_possible_truncation)]
const TACH_MIN_ITERS: u32 = (60 * TACH_PIO_HZ as u64
    / (TACH_MAX_PLAUSIBLE_RPM * TACH_CYCLES_PER_ITER as u64 * Fan::TACH_PULSES_PER_REV))
    as u32;

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
        [latest_rpm(&mut self.sm_a), latest_rpm(&mut self.sm_b)]
    }
}

/// Build the tach period-counter program.
///
/// The program measures the number of `TACH_CYCLES_PER_ITER`-cycle ticks
/// between two consecutive qualified rising edges and pushes it. `X` is a
/// down-counter started at `0xFFFF_FFFF` at the rising edge, snapshotted
/// into the ISR at the next rising edge, so the CPU recovers the elapsed
/// tick count as `0xFFFF_FFFF - value`.
///
/// A rising edge counts only once the line has stayed high for 31
/// consecutive ticks, which rejects the chatter the slow RC rising edge
/// produces around the input threshold. The debounce runs inside the
/// counting loop (`s_deb`) at the same cycles-per-tick as the
/// `high`/`low` loops, so it does not bias the measured period.
fn tach_program(common: &mut Common<'static, PIO0>) -> LoadedProgram<'static, PIO0> {
    // `set y, N` takes a 5-bit immediate, so the debounce window maxes at
    // 31 ticks (31 µs), comfortably longer than a single chatter pulse and
    // far shorter than a real tach half-period.
    let program = pio::pio_asm!(
        "seek_low:",
        "    jmp pin seek_low", // wait for a low baseline
        "seek_rise:",
        "    jmp pin got_start", // low: watch for the rising edge
        "    jmp seek_rise",
        "got_start:",
        "    mov x, ~null", // start timing at the candidate edge
        "    set y, 30",    // 31-tick debounce window
        "s_deb:",
        "    jmp x-- s_deb1", // keep timing through the debounce
        "    jmp seek_low",   // X underflow: fan stalled, restart
        "s_deb1:",
        "    jmp pin s_deb2", // still high?
        "    jmp seek_rise",  // dropped early: chatter, discard
        "s_deb2:",
        "    jmp y-- s_deb", // require the high to persist
        "high:",
        "    jmp x-- high1",
        "    jmp seek_low",
        "high1:",
        "    jmp pin high2", // still high: keep counting
        "    jmp low",       // fell: enter low phase
        "high2:",
        "    jmp high",
        "low:",
        "    jmp x-- low1",
        "    jmp seek_low",
        "low1:",
        "    jmp pin got_end", // rose: candidate period end
        "    jmp low",
        "got_end:",
        "    mov isr, x", // snapshot the period at the candidate edge
        "    set y, 30",
        "e_deb:",
        "    jmp x-- e_deb1", // keep counting so a reject resumes seamlessly
        "    jmp seek_low",
        "e_deb1:",
        "    jmp pin e_deb2", // still high?
        "    jmp low",        // dropped: chatter, resume low counting
        "e_deb2:",
        "    jmp y-- e_deb",
        "    push noblock", // qualified edge: emit the snapshot
        "    jmp seek_low",
    );
    common.load_program(&program.program)
}

fn setup_tach<const SM: usize>(
    common: &mut Common<'static, PIO0>,
    mut sm: StateMachine<'static, PIO0, SM>,
    pin: Peri<'static, impl PioPin>,
    program: &LoadedProgram<'static, PIO0>,
) -> StateMachine<'static, PIO0, SM> {
    let mut pin = common.make_pio_pin(pin);
    pin.set_pull(Pull::Up);
    sm.set_pin_dirs(Direction::In, &[&pin]);

    let mut config = Config::default();
    config.use_program(program, &[]);
    config.set_jmp_pin(&pin);
    config.set_in_pins(&[&pin]);
    config.clock_divider = calculate_pio_clock_divider(TACH_PIO_HZ);
    config.fifo_join = FifoJoin::RxOnly;
    sm.set_config(&config);
    sm.set_enable(true);
    sm
}

/// Drain a state machine's FIFO and convert the most recent plausible
/// period into RPM. Returns zero when no fresh period is available, which
/// is what a stopped fan produces.
fn latest_rpm<const SM: usize>(sm: &mut StateMachine<'static, PIO0, SM>) -> FanRpm {
    let mut iters = None;
    while let Some(raw) = sm.rx().try_pull() {
        let elapsed = u32::MAX - raw;
        if elapsed >= TACH_MIN_ITERS {
            iters = Some(elapsed);
        }
    }
    let Some(iters) = iters else {
        return FanRpm::from_rpm(0);
    };
    // rpm = 60 * pio_hz / (ticks * cycles_per_tick * pulses_per_rev). One
    // measured period spans one pulse, and there are TACH_PULSES_PER_REV
    // pulses per revolution.
    let numerator = 60u64 * u64::from(TACH_PIO_HZ);
    let denominator = u64::from(iters) * u64::from(TACH_CYCLES_PER_ITER) * Fan::TACH_PULSES_PER_REV;
    #[allow(clippy::cast_possible_truncation)]
    FanRpm::from_rpm((numerator / denominator) as u32)
}
