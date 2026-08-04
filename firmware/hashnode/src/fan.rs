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
//! `PWM_SLICE5` channels A/B (GPIO10/GPIO11, `RP_PWM_A`/`RP_PWM_B`) and
//! `PWM_SLICE9` channel B (GPIO43, `RP_PWM_C` on the `FAN_CONTROL`
//! header) drive the fan PWM inputs at 25 kHz, the rate specified by
//! the Intel 4-wire fan spec. `BoardFan` exposes the outputs behind the
//! `ubos_cooling::FanOutput` trait at one shared duty so the cooling
//! task can drive them from a PID loop.
//!
//! Five connectors expose a tachometer: GPIO24 (`RP_TACH_A`), GPIO25
//! (`RP_TACH_B`), GPIO23 (`RP_TACH_C`), GPIO38 (`RP_TACH_D`), and
//! GPIO44 (`RP_TACH_E`). None map to a `PWM_SLICE` channel B, so the
//! PWM slice edge counter is unusable; instead each tach drives one PIO
//! state machine that measures the pulse-to-pulse period in hardware
//! and pushes it to its RX FIFO. A PIO block addresses a single 32-pin
//! GPIO window (and embassy-rp selects the low window whenever a state
//! machine's pins all sit below GPIO32), so the tachs split across
//! blocks: `PIO0` SM0-2 take GPIO23/24/25, `PIO1` SM0-1 take
//! GPIO38/44. `BoardTach` drains the latest period per fan each control
//! tick behind `ubos_cooling::TachReader`, which publishes one labeled
//! series per fan. Unpopulated connectors simply read 0 rpm.
//!
//! The board's tach RC network charges the filter cap through a 4.7 kΩ
//! pull-up, so the rising edge ramps slowly through the input threshold
//! and supply noise chatters it into bursts of spurious edges. The PIO
//! program rejects that chatter by only accepting an edge once the line
//! has stayed high for 31 consecutive samples (~31 µs), longer than a
//! chatter pulse yet far shorter than a real tach half-period.

use embassy_rp::Peri;
use embassy_rp::gpio::Pull;
use embassy_rp::pac;
use embassy_rp::peripherals::{
    PIN_10, PIN_11, PIN_23, PIN_24, PIN_25, PIN_38, PIN_43, PIN_44, PIO0, PIO1, PWM_SLICE5,
    PWM_SLICE9,
};
use embassy_rp::pio::{
    Common, Config, Direction, FifoJoin, Instance, LoadedProgram, Pio, PioPin, StateMachine,
};
use embassy_rp::pio_programs::clock_divider::calculate_pio_clock_divider;
use embassy_rp::pwm::{Config as PwmConfig, Pwm};
use ubos_cooling::{FanOutput, TachReader};
use ubos_core::units::{FanPwm, FanRpm};

use crate::config::Fan;

/// Number of fan connectors, each with its own tachometer.
pub const TACH_COUNT: usize = 5;

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

/// PWM outputs driving the fan inputs. Construct once and pass into the
/// cooling task; the cooling loop sets duty via `FanOutput::set_pwm`.
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
    fn set_pwm(&mut self, pwm: FanPwm) {
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
            // Every channel gets the same duty so all fans track.
            config.compare_a = compare as u16;
            config.compare_b = compare as u16;
        }
        self.pwm_ab.set_config(&config);
        self.pwm_c.set_config(&config);
    }
}

/// Reads the latest measured RPM for all five connectors into the
/// cooling loop. Each field owns the PIO state machine driving that
/// connector's tachometer.
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
    /// Load the tach program into both PIO blocks and start one state
    /// machine per connector. Consumes both `Pio`s; the `common`
    /// handles are only needed for setup and are dropped here, which
    /// leaves the running state machines untouched.
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
            latest_rpm(&mut self.sm_a),
            latest_rpm(&mut self.sm_b),
            latest_rpm(&mut self.sm_c),
            latest_rpm(&mut self.sm_d),
            latest_rpm(&mut self.sm_e),
        ]
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
fn tach_program<P: Instance>(common: &mut Common<'static, P>) -> LoadedProgram<'static, P> {
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

fn setup_tach<P: Instance, const SM: usize>(
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
fn latest_rpm<P: Instance, const SM: usize>(sm: &mut StateMachine<'static, P, SM>) -> FanRpm {
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
