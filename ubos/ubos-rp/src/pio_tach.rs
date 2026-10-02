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

//! PIO tachometer shared by every RP board with a fan header.
//!
//! One PIO state machine per tach line counts the ticks between two
//! qualified rising edges and pushes the count; the CPU converts that to
//! RPM. The functions are generic over the PIO block, because a board whose
//! tach lines span both GPIO windows loads the program into both blocks.

use embassy_rp::Peri;
use embassy_rp::gpio::Pull;
use embassy_rp::pio::{
    Common, Config, Direction, FifoJoin, Instance, LoadedProgram, PioPin, StateMachine,
};
use embassy_rp::pio_programs::clock_divider::calculate_pio_clock_divider;
use ubos_cooling::pulses_to_rpm;
use ubos_core::units::FanRpm;

/// PIO state-machine clock. The counting loops are exactly
/// [`TACH_CYCLES_PER_ITER`] PIO cycles per period tick, so at 3 MHz one
/// tick is 1 µs.
pub const TACH_PIO_HZ: u32 = 3_000_000;

/// PIO cycles per period-counter decrement. Every loop that advances the
/// period counter (`s_deb`, `high`, `low` in [`tach_program`]) is padded
/// to this exact length so elapsed ticks map linearly to time regardless
/// of the input duty cycle.
pub const TACH_CYCLES_PER_ITER: u32 = 3;

const MICROS_PER_SECOND: u64 = 1_000_000;

/// Fastest RPM accepted as a real measurement; no fan on this board
/// approaches it.
pub const TACH_MAX_PLAUSIBLE_RPM: u64 = 30_000;

/// Shortest plausible period in ticks for a tach with `pulses_per_rev`
/// pulses per revolution.
///
/// Anything shorter implies more than [`TACH_MAX_PLAUSIBLE_RPM`] and is
/// discarded as residual noise. A zero `pulses_per_rev` yields `u32::MAX`, so
/// every pulse is discarded rather than the divisor reaching zero.
// Fits u32: the divisor is far larger than can push the quotient past 2^32.
#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub const fn min_iters(pulses_per_rev: u64) -> u32 {
    let ticks_per_pulse = TACH_MAX_PLAUSIBLE_RPM * TACH_CYCLES_PER_ITER as u64 * pulses_per_rev;
    match (60 * TACH_PIO_HZ as u64).checked_div(ticks_per_pulse) {
        Some(iters) => iters as u32,
        None => u32::MAX,
    }
}

/// Build the tach period-counter program.
///
/// `X` is a down-counter started at `0xFFFF_FFFF` at the rising edge and
/// snapshotted into the ISR at the next one, so the CPU recovers the elapsed
/// tick count as `0xFFFF_FFFF - value`.
///
/// A rising edge counts only once the line has stayed high for 31
/// consecutive ticks, which rejects the chatter the slow RC rising edge
/// produces around the input threshold. The debounce runs at the same
/// cycles-per-tick as the counting loops, so it does not bias the period.
pub fn tach_program<P: Instance>(common: &mut Common<'static, P>) -> LoadedProgram<'static, P> {
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

/// Bind `sm` to `pin` and load `program` onto it, with the RX FIFO
/// joined for full depth. `pin` is pulled up, because the tach line
/// floats high between pulses. Returns `sm` enabled and pushing periods.
pub fn setup_tach<P: Instance, const SM: usize>(
    common: &mut Common<'static, P>,
    mut sm: StateMachine<'static, P, SM>,
    pin: Peri<'static, impl PioPin>,
    program: &LoadedProgram<'static, P>,
) -> StateMachine<'static, P, SM> {
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
pub fn latest_rpm<P: Instance, const SM: usize>(
    sm: &mut StateMachine<'static, P, SM>,
    pulses_per_rev: u64,
) -> FanRpm {
    let mut iters = None;
    while let Some(raw) = sm.rx().try_pull() {
        let elapsed = u32::MAX - raw;
        if elapsed >= min_iters(pulses_per_rev) {
            iters = Some(elapsed);
        }
    }
    let Some(iters) = iters else {
        return FanRpm::from_rpm(0);
    };
    // One measured period spans one pulse, counted in ticks of
    // TACH_CYCLES_PER_ITER PIO cycles, so its length in microseconds is
    // ticks × cycles / pio_hz × 10^6.
    let elapsed_us = u64::from(iters) * u64::from(TACH_CYCLES_PER_ITER) * MICROS_PER_SECOND
        / u64::from(TACH_PIO_HZ);
    pulses_to_rpm(1, pulses_per_rev, elapsed_us)
}
