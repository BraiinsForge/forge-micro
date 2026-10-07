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

//! Constant-power-curve policy for a fixed frequency and voltage target.
//!
//! The miner supplies the applied voltage separately from the PSU sample.
//! Each step returns commands only; the miner owns hardware access and delays.

use embassy_time::Duration;
use libm::sqrtf;
use ubos_asic::HashboardDescriptor;
use ubos_config::{TunerInfo, Tuning};
use ubos_core::units::{Frequency, Voltage};
use ubos_logging::{Debug2Format, debug, warn};
use ubos_tuner::{Plan, Readings, Tuner};

use super::{FixedTarget, MachineTuner};

/// Fixed-target policy. Hardware state remains in the serial actors and PSU.
pub struct FixedTuner {
    board: &'static HashboardDescriptor,
    /// None before the first tuning step with a present chain.
    progress: Option<Progress>,
}

/// Target and next phase retained between tuning steps.
struct Progress {
    target: FixedTarget,
    phase: Phase,
}

#[derive(Clone, Copy)]
enum Phase {
    /// Phase A: Move frequency to the stored cap while keeping voltage unchanged.
    Frequency(Frequency),
    /// Phase B: Adjust frequency and voltage along the constant-power curve to the target.
    Curve,
    Complete,
}

impl Phase {
    fn log_plan<const CHAINS: usize>(
        self,
        target: FixedTarget,
        readings: &Readings<CHAINS>,
        plan: &Plan<CHAINS>,
    ) {
        if plan.rail.is_none() && plan.frequencies.iter().all(Option::is_none) {
            return;
        }
        let phase_name = match self {
            Phase::Frequency(..) => "A",
            Phase::Curve => "B",
            // A complete ramp only ever produces an empty plan, which the
            // guard above has already returned on.
            Phase::Complete => return,
        };
        debug!(
            "fixed-tuner phase {} plan: frequencies {:?} kHz, commands {:?} kHz, rail {} -> {} mV; target {} kHz / {} mV",
            phase_name,
            Debug2Format(
                &readings
                    .chains
                    .map(|chain| chain.map(|chain| chain.frequency.khz()))
            ),
            Debug2Format(
                &plan
                    .frequencies
                    .map(|frequency| frequency.map(Frequency::khz))
            ),
            readings.voltage.mv(),
            plan.rail.unwrap_or(readings.voltage).mv(),
            target.frequency.khz(),
            target.voltage.mv(),
        );
    }
}

impl FixedTuner {
    /// This policy as the mining form lists it.
    pub const INFO: TunerInfo = TunerInfo::Fixed { name: "Fixed" };

    /// Build a policy for `board`, whose chip descriptor supplies the
    /// frequency grid and range and whose rail ceiling bounds the curve.
    /// The first step reads the operating point from the readings.
    #[must_use]
    pub fn new(board: &'static HashboardDescriptor) -> Self {
        Self {
            board,
            progress: None,
        }
    }

    /// Settling time between changes to the operating point.
    const RAMP_SETTLE: Duration = Duration::from_secs(1);

    /// A plan that commands nothing and asks for the next step after the
    /// settle time.
    fn unchanged<const CHAINS: usize>() -> Plan<CHAINS> {
        Plan {
            rail: None,
            frequencies: [None; CHAINS],
            delay: Self::RAMP_SETTLE,
        }
    }
}

impl<const CHAINS: usize> MachineTuner<CHAINS> for FixedTuner {
    type PlatformReadings = ();

    const TUNERS: &'static [TunerInfo] = &[Self::INFO];

    fn select(tuning: &Tuning, board: &'static HashboardDescriptor) -> Option<Self> {
        match tuning {
            Tuning::Fixed(_) => Some(Self::new(board)),
            Tuning::Power(_) => None,
        }
    }

    fn step(&mut self, tuning: &Tuning, readings: &Readings<CHAINS>, (): &()) -> Plan<CHAINS> {
        match tuning {
            Tuning::Fixed(target) => Tuner::step(self, (*target).into(), readings, &()),
            // This policy has a target only under `Fixed`, and `select`
            // answers `None` for anything else, so the miner never
            // arrives here. The arm exists because the trait is public
            // and this method must answer for any configuration. It is
            // matched rather than caught by an else, so a new `Tuning`
            // variant is a compile error here too.
            Tuning::Power(_) => {
                warn!("fixed tuner: asked to plan for a target it does not serve");
                Self::unchanged()
            }
        }
    }
}

impl<const CHAINS: usize> Tuner<CHAINS> for FixedTuner {
    type Target = FixedTarget;
    type PlatformReadings = ();

    fn step(&mut self, target: FixedTarget, readings: &Readings<CHAINS>, (): &()) -> Plan<CHAINS> {
        let unchanged = Self::unchanged();
        let present = || readings.chains.iter().flatten();
        if present().next().is_none() {
            return unchanged;
        }
        let all_at = |frequency| present().all(|chain| chain.frequency == frequency);
        let steps_toward = |frequency| {
            readings.chains.map(|chain| {
                let current = chain?.frequency;
                (current != frequency)
                    .then(|| current.step_toward(frequency, self.board.chip.frequency_step))
            })
        };
        let step = self.board.chip.frequency_step;
        let target = FixedTarget {
            frequency: target.frequency.snap_down(step).clamp(
                self.board.chip.frequency_range.min(),
                self.board.chip.frequency_range.max(),
            ),
            voltage: target.voltage.min(self.board.max_rail_voltage()),
        };
        let voltage = readings.voltage;
        let power_target =
            (target.voltage.as_volts() * target.voltage.as_volts()) * target.frequency.as_mhz();
        let phase = match &self.progress {
            Some(progress) if progress.target == target => progress.phase,
            _ => {
                crate::events::frequency_ramp_started(target.frequency.khz());
                let cap = frequency_at_target_power(
                    power_target,
                    voltage.as_volts() * voltage.as_volts(),
                    target.frequency,
                    step,
                    self.board.chip.frequency_range.min(),
                );
                if all_at(cap) {
                    Phase::Curve
                } else {
                    Phase::Frequency(cap)
                }
            }
        };
        let (next_phase, plan) = match phase {
            Phase::Frequency(cap) => {
                let frequencies = steps_toward(cap);
                let next_phase = if frequencies
                    .iter()
                    .flatten()
                    .all(|frequency| *frequency == cap)
                {
                    Phase::Curve
                } else {
                    phase
                };
                (
                    next_phase,
                    Plan {
                        frequencies,
                        ..unchanged
                    },
                )
            }
            Phase::Curve => {
                if all_at(target.frequency) && voltage == target.voltage {
                    crate::events::frequency_ramp_complete(target.frequency.khz());
                    (Phase::Complete, unchanged)
                } else {
                    let frequencies = steps_toward(target.frequency);
                    // The fastest chain limits the voltage on the shared rail.
                    let frequency = present()
                        .map(|chain| chain.frequency.step_toward(target.frequency, step))
                        .max()
                        .expect("BUG: present chain checked above");
                    // Snap the last voltage command exactly, avoiding a residual from float rounding.
                    let next_voltage = if frequency == target.frequency {
                        target.voltage
                    } else {
                        curve_voltage(power_target, frequency, target.voltage)
                    };
                    (
                        Phase::Curve,
                        Plan {
                            frequencies,
                            rail: (next_voltage != voltage).then_some(next_voltage),
                            ..unchanged
                        },
                    )
                }
            }
            Phase::Complete => (Phase::Complete, unchanged),
        };
        phase.log_plan(target, readings, &plan);
        self.progress = Some(Progress {
            target,
            phase: next_phase,
        });
        plan
    }
}

/// Frequency where `voltage_now_squared * f` reaches the target power estimate.
/// Uses the currently applied voltage, then snaps to the PLL grid and clamps
/// between the minimum and target frequencies.
fn frequency_at_target_power(
    power_target: f32,
    voltage_now_squared: f32,
    target: Frequency,
    step: Frequency,
    floor: Frequency,
) -> Frequency {
    let curve_mhz = power_target / voltage_now_squared;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let curve_kilohertz = (curve_mhz * 1000.0) as u32;
    // Snap the curve crossing onto the PLL grid before clamping. With a
    // grid-aligned target and floor, the resulting cap stays on the grid
    // so phase A's final step lands on an exact hash clock.
    let curve_snapped = Frequency::from_khz(curve_kilohertz).snap_down(step);
    curve_snapped.min(target).max(floor)
}

/// Voltage that keeps `V^2 * f = power_target` at `f`. Clamped to
/// `target_voltage` so the curve walk converges instead of overshooting
/// past the target on the last step.
fn curve_voltage(power_target: f32, frequency: Frequency, target_voltage: Voltage) -> Voltage {
    let curve_volts = sqrtf(power_target / frequency.as_mhz());
    let floor_volts = target_voltage.as_volts();
    let clamped_volts = if curve_volts < floor_volts {
        floor_volts
    } else {
        curve_volts
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Voltage::from_mv((clamped_volts * 1000.0) as u32)
}

#[cfg(test)]
#[path = "../tests/tuner.rs"]
mod tests;
