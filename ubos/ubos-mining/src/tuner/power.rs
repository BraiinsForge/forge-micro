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

//! Measured-input-power policy.
//!
//! Every ramp starts at safe boot: the minimum clock at the open-core rail.
//! Phase A raises the clock until measured PSU input power reaches the
//! target. Phase B keeps raising it while it lowers the rail according to
//! `P = alpha * V^2 * f`. Only a healthy PSU sample taken after the last
//! command advances the ramp.
//!
//! The ramp ends at the frequency where the theoretical hashrate of the
//! present chips meets the target efficiency at the target power, or at
//! the chip's maximum frequency if that is lower, even when power stays
//! below the target. Excess power or stale telemetry returns the chains
//! to safe boot and ends the ramp. A power drop after the target was
//! reached restores the previous operating point. A zero target stays
//! at safe boot. A new target restarts the ramp from safe boot. The
//! policy does not regulate power after the ramp ends.

use embassy_time::{Duration, Instant};
use libm::sqrtf;
use ubos_asic::HashboardDescriptor;
use ubos_config::{TunerInfo, Tuning};
use ubos_core::units::{Frequency, Power, Voltage};
use ubos_logging::{debug, warn};
use ubos_tuner::{Plan, Readings, Tuner};

use super::{MachineTuner, PowerTarget};

/// Upper safety margin as a divisor of the target, e.g. `1 / 40 = 2.5%`.
const TARGET_POWER_UPPER_MARGIN_DIVISOR: u32 = 20; // 1 / 20 = 5%
/// Lower restoration margin as a divisor of the target, e.g. `1 / 40 = 2.5%`.
const TARGET_POWER_LOWER_MARGIN_DIVISOR: u32 = 20; // 1 / 20 = 5%
/// Maximum age of the last healthy PSU sample before tuning returns to safe boot.
const PSU_READING_MAX_DELAY: Duration = Duration::from_secs(60);

/// Measured-power policy. Hardware state remains in the serial actors and PSU.
pub struct PowerTuner {
    board: &'static HashboardDescriptor,
    /// None before the first tuning step with a present chain.
    progress: Option<Progress>,
}

/// Target, phase and telemetry bookkeeping retained between steps.
struct Progress {
    target: PowerTarget,
    phase: Phase,
    /// When the last command took effect. A sample taken earlier does not
    /// describe that command.
    commanded_at: Instant,
    /// The delay of the last plan with a command, until the next step.
    /// The miner waits that long after it applies a plan, so the next
    /// step less this delay is when the command took effect.
    prev_delay: Option<Duration>,
    /// Timestamp of the last healthy PSU sample, or the ramp start.
    healthy_at: Instant,
}

#[derive(Clone, Copy)]
enum Phase {
    /// Walk to safe boot, then ramp.
    Restart,
    /// Walk to safe boot, then stop.
    Retreat,
    /// Phase A before the target is reached, phase B after it. `last` is
    /// the operating point before the most recent step.
    Ramp {
        target_reached: bool,
        last: OperatingPoint,
    },
    Complete,
}

/// The shared clock of the present chains and the confirmed rail voltage.
#[derive(Clone, Copy, PartialEq, Eq)]
struct OperatingPoint {
    frequency: Frequency,
    voltage: Voltage,
}

impl OperatingPoint {
    /// The fastest present chain sets the clock; `None` without one.
    fn of<const CHAINS: usize>(readings: &Readings<CHAINS>) -> Option<Self> {
        Some(Self {
            frequency: readings
                .chains
                .iter()
                .flatten()
                .map(|chain| chain.frequency)
                .max()?,
            voltage: readings.voltage,
        })
    }

    /// Predict a higher-frequency operating point at the target power with no voltage increase.
    fn raise_frequency_lower_voltage(
        self,
        input_power: Power,
        target_power: Power,
        frequency_limit: Frequency,
        frequency_step: Frequency,
    ) -> Self {
        let frequency = self.frequency.step_toward(frequency_limit, frequency_step);
        let alpha = power_alpha(input_power, self.voltage, self.frequency);
        // TODO: Floor the rail at the chips' minimum working voltage once the
        // board descriptor carries one. Until then only a failed rail write,
        // or the power collapse that restores the previous point, stops the
        // rail from falling below it.
        let voltage = predicted_voltage(target_power, alpha, frequency).min(self.voltage);
        debug!(
            "power-tuner phase B: voltage {} mV -> {} mV, frequency {} kHz -> {} kHz; alpha={}",
            self.voltage.mv(),
            voltage.mv(),
            self.frequency.khz(),
            frequency.khz(),
            alpha
        );
        Self { frequency, voltage }
    }
}

/// Power thresholds and the frequency ceiling that terminate a tuning ramp.
struct Limits {
    /// Safe power limit which should not be hit.
    max_power: Power,
    /// Lower power bound which should not be crossed once the target was already met.
    min_power_after_target: Power,
    /// Frequency limit at which the tuning should terminate.
    max_frequency: Frequency,
}

/// Specifies every tuning or termination action of the power tuner.
// TODO: Simplify the action set as the tuning policy evolves.
#[derive(Debug, PartialEq, Eq)]
enum TuningAction {
    // TODO: Merge the frequency-limit actions if the below-target diagnostic is no longer needed.
    MaxFrequencyBelowTarget,
    MaxFrequencyAtTarget,
    PowerTooHigh,
    RestoreBetterState,
    // TODO: Consider unifying these two actions, since both increase the frequency.
    RampUpFrequencyToTarget,
    RaiseFrequencyLowerVoltageAtTarget,
}

impl TuningAction {
    /// Choose the next tuner action based on input measurements.
    ///
    /// This function defines which action takes priority in each tuner state.
    fn next(
        limits: &Limits,
        frequency: Frequency,
        input_power: Power,
        target_reached: bool,
    ) -> TuningAction {
        // Check termination conditions.
        if input_power >= limits.max_power {
            return Self::PowerTooHigh;
        }
        if target_reached && input_power <= limits.min_power_after_target {
            return Self::RestoreBetterState;
        }
        if frequency >= limits.max_frequency && target_reached {
            return Self::MaxFrequencyAtTarget;
        }
        if frequency >= limits.max_frequency {
            return Self::MaxFrequencyBelowTarget;
        }

        // Continue the ramp, adjusting voltage only after the target has been reached.
        if target_reached {
            return Self::RaiseFrequencyLowerVoltageAtTarget;
        }
        Self::RampUpFrequencyToTarget
    }
}

impl PowerTuner {
    /// This policy as the mining form lists it. The ramp ends at the
    /// target efficiency, so the policy reads it.
    pub const INFO: TunerInfo = TunerInfo::Power {
        name: "Power",
        reads_efficiency: true,
    };

    /// Build a policy for `board`, whose chip descriptor supplies the
    /// frequency grid and range and whose rail ceiling is the open-core voltage.
    #[must_use]
    pub fn new(board: &'static HashboardDescriptor) -> Self {
        Self {
            board,
            progress: None,
        }
    }

    /// Settling time after each ramp step. The next step waits for a PSU
    /// sample taken after it, so the PSU poll interval also bounds the rate.
    const RAMP_SETTLE: Duration = Duration::from_millis(500);
    /// Settling time between steps down to safe boot. A step down needs no
    /// power feedback, so it runs faster than the ramp.
    const SAFE_BOOT_SETTLE: Duration = Duration::from_millis(200);

    /// A plan that commands nothing and asks for the next step after the
    /// ramp settle time.
    fn unchanged<const CHAINS: usize>() -> Plan<CHAINS> {
        Plan {
            rail: None,
            frequencies: [None; CHAINS],
            delay: Self::RAMP_SETTLE,
        }
    }

    fn safe_boot(&self) -> OperatingPoint {
        OperatingPoint {
            frequency: self.board.chip.frequency_range.min(),
            voltage: self.board.max_rail_voltage(),
        }
    }

    /// Bound the requested power and cap frequency on the chip's step grid,
    /// where the present chips meet the target efficiency.
    fn limits<const CHAINS: usize>(
        &self,
        target: PowerTarget,
        readings: &Readings<CHAINS>,
    ) -> Limits {
        let target_mw = target.input_power.mw();
        let cores: u64 = readings
            .chains
            .iter()
            .flatten()
            .map(|chain| u64::from(chain.chips) * u64::from(self.board.chip.core_count))
            .sum();
        let chip_freq_max = self.board.chip.frequency_range.max();
        Limits {
            max_power: Power::from_mw(
                target_mw.saturating_add(target_mw / TARGET_POWER_UPPER_MARGIN_DIVISOR),
            ),
            min_power_after_target: Power::from_mw(
                target_mw.saturating_sub(target_mw / TARGET_POWER_LOWER_MARGIN_DIVISOR),
            ),
            // Without a clock from the efficiency, the chip's maximum is the only cap.
            max_frequency: Frequency::try_from_power_and_efficiency(
                target.input_power,
                target.efficiency,
                cores,
            )
            .map_or(chip_freq_max, |frequency| frequency.min(chip_freq_max))
            .max(self.safe_boot().frequency)
            .snap_down(self.board.chip.frequency_step),
        }
    }

    /// Enter phase A from safe boot.
    fn start_ramp<const CHAINS: usize>(
        &self,
        target: PowerTarget,
        readings: &Readings<CHAINS>,
    ) -> Phase {
        crate::events::frequency_ramp_started(self.limits(target, readings).max_frequency.khz());
        Phase::Ramp {
            target_reached: false,
            last: self.safe_boot(),
        }
    }

    /// Step every present clock down toward safe boot, then raise the rail
    /// to open-core. `None` once the chains are at safe boot.
    fn walk_to_safe_boot<const CHAINS: usize>(
        &self,
        readings: &Readings<CHAINS>,
    ) -> Option<Plan<CHAINS>> {
        let safe = self.safe_boot();
        let step = self.board.chip.frequency_step;
        let frequencies = readings.chains.map(|chain| {
            let current = chain?.frequency;
            (current != safe.frequency).then(|| current.step_toward(safe.frequency, step))
        });
        if frequencies.iter().any(Option::is_some) {
            return Some(Plan {
                rail: None,
                frequencies,
                delay: Self::SAFE_BOOT_SETTLE,
            });
        }
        (readings.voltage != safe.voltage).then_some(Plan {
            rail: Some(safe.voltage),
            frequencies,
            delay: Self::SAFE_BOOT_SETTLE,
        })
    }

    /// One ramp step from `point`, the present operating point: the next
    /// phase and its plan.
    fn ramp<const CHAINS: usize>(
        &self,
        progress: &mut Progress,
        target_reached: bool,
        last: OperatingPoint,
        point: OperatingPoint,
        readings: &Readings<CHAINS>,
    ) -> (Phase, Plan<CHAINS>) {
        let unchanged = Self::unchanged();
        let retreat = || self.walk_to_safe_boot(readings).unwrap_or(unchanged);
        let waiting = Phase::Ramp {
            target_reached,
            last,
        };
        // TODO: Reject a sample dated after `readings.now`, which a clock fault could produce.
        let healthy = readings.psu.filter(|psu| !psu.sensor_fault);
        if let Some(psu) = healthy {
            progress.healthy_at = Instant::from_millis(psu.timestamp_ms);
        }
        if readings.now.saturating_duration_since(progress.healthy_at) > PSU_READING_MAX_DELAY {
            warn!("power-tuner: PSU reading stale; stepping to safe boot");
            return (Phase::Retreat, retreat());
        }
        let Some(psu) =
            healthy.filter(|psu| Instant::from_millis(psu.timestamp_ms) > progress.commanded_at)
        else {
            return (waiting, unchanged);
        };

        let target = progress.target.input_power;
        let limits = self.limits(progress.target, readings);
        let target_reached = target_reached || psu.input_power >= target;
        let next = match TuningAction::next(
            &limits,
            point.frequency,
            psu.input_power,
            target_reached,
        ) {
            TuningAction::PowerTooHigh => {
                warn!(
                    "power-tuner: measured {} mW above {} mW; stepping to safe boot",
                    psu.input_power.mw(),
                    limits.max_power.mw()
                );
                return (Phase::Retreat, retreat());
            }
            TuningAction::MaxFrequencyBelowTarget => {
                warn!(
                    "power-tuner: stopped at {} kHz below target {} mW (measured {} mW)",
                    limits.max_frequency.khz(),
                    target.mw(),
                    psu.input_power.mw()
                );
                return (Phase::Complete, unchanged);
            }
            TuningAction::MaxFrequencyAtTarget => return (Phase::Complete, unchanged),
            TuningAction::RestoreBetterState => {
                // TODO: Select a stable fallback when tuning deteriorates. The previous state is not necessarily optimal.
                warn!(
                    "power-tuner: measured {} mW below {} mW after reaching target; restoring {} kHz / {} mV",
                    psu.input_power.mw(),
                    limits.min_power_after_target.mw(),
                    last.frequency.khz(),
                    last.voltage.mv()
                );
                return (Phase::Complete, Self::plan_to(last, readings));
            }
            TuningAction::RampUpFrequencyToTarget => {
                let frequency = point
                    .frequency
                    .step_toward(limits.max_frequency, self.board.chip.frequency_step);
                debug!(
                    "power-tuner phase A: frequency {} kHz -> {} kHz at {} mV; power {} / {} mW",
                    point.frequency.khz(),
                    frequency.khz(),
                    point.voltage.mv(),
                    psu.input_power.mw(),
                    target.mw()
                );
                OperatingPoint { frequency, ..point }
            }
            TuningAction::RaiseFrequencyLowerVoltageAtTarget => point
                .raise_frequency_lower_voltage(
                    psu.input_power,
                    target,
                    limits.max_frequency,
                    self.board.chip.frequency_step,
                ),
        };
        (
            Phase::Ramp {
                target_reached,
                last: point,
            },
            Self::plan_to(next, readings),
        )
    }

    /// Commands that move every present chain and the rail to `point`.
    fn plan_to<const CHAINS: usize>(
        point: OperatingPoint,
        readings: &Readings<CHAINS>,
    ) -> Plan<CHAINS> {
        Plan {
            rail: (point.voltage != readings.voltage).then_some(point.voltage),
            frequencies: readings
                .chains
                .map(|chain| (chain?.frequency != point.frequency).then_some(point.frequency)),
            delay: Self::RAMP_SETTLE,
        }
    }
}

impl<const CHAINS: usize> MachineTuner<CHAINS> for PowerTuner {
    type PlatformReadings = ();

    const TUNERS: &'static [TunerInfo] = &[Self::INFO];

    fn select(tuning: &Tuning, board: &'static HashboardDescriptor) -> Option<Self> {
        match tuning {
            Tuning::Power(_) => Some(Self::new(board)),
            Tuning::Fixed(_) => None,
        }
    }

    fn step(&mut self, tuning: &Tuning, readings: &Readings<CHAINS>, (): &()) -> Plan<CHAINS> {
        match tuning {
            Tuning::Power(target) => Tuner::step(self, PowerTarget::from(*target), readings, &()),
            // `select` answers `None` for a fixed target, so the miner
            // never arrives here; the arm answers the public trait.
            Tuning::Fixed(_) => {
                warn!("power-tuner: asked to plan for a target it does not serve");
                Self::unchanged()
            }
        }
    }
}

impl<const CHAINS: usize> Tuner<CHAINS> for PowerTuner {
    type Target = PowerTarget;
    type PlatformReadings = ();

    fn step(&mut self, target: PowerTarget, readings: &Readings<CHAINS>, (): &()) -> Plan<CHAINS> {
        let unchanged = Self::unchanged();
        let Some(point) = OperatingPoint::of(readings) else {
            return unchanged;
        };
        let mut progress = match self.progress.take() {
            Some(progress) if progress.target == target => progress,
            _ => {
                let phase = if target.input_power.mw() == 0 {
                    warn!("power-tuner: zero input-power target; staying at safe boot");
                    Phase::Retreat
                } else {
                    Phase::Restart
                };
                Progress {
                    target,
                    phase,
                    commanded_at: readings.now,
                    prev_delay: None,
                    healthy_at: readings.now,
                }
            }
        };
        // `now` is fresh, and the miner waits the delay after applying a plan, so `now - delay` is when it took effect.
        if let Some(delay) = progress.prev_delay.take()
            && let Some(applied) = readings.now.checked_sub(delay)
        {
            progress.commanded_at = applied;
        }
        let (phase, plan) = match progress.phase {
            Phase::Restart => match self.walk_to_safe_boot(readings) {
                Some(plan) => (Phase::Restart, plan),
                None => (self.start_ramp(target, readings), unchanged),
            },
            Phase::Retreat => match self.walk_to_safe_boot(readings) {
                Some(plan) => (Phase::Retreat, plan),
                None => (Phase::Complete, unchanged),
            },
            Phase::Ramp {
                target_reached,
                last,
            } => self.ramp(&mut progress, target_reached, last, point, readings),
            Phase::Complete => (Phase::Complete, unchanged),
        };
        if matches!(phase, Phase::Complete) && !matches!(progress.phase, Phase::Complete) {
            let restored = plan.frequencies.iter().flatten().max().copied();
            crate::events::frequency_ramp_complete(restored.unwrap_or(point.frequency).khz());
        }
        if plan.rail.is_some() || plan.frequencies.iter().any(Option::is_some) {
            progress.commanded_at = readings.now;
            progress.prev_delay = Some(plan.delay);
        }
        progress.phase = phase;
        self.progress = Some(progress);
        plan
    }
}

/// Estimate the coefficient in `P = alpha * V^2 * f` from measured input power.
#[allow(clippy::cast_precision_loss)]
fn power_alpha(input_power: Power, voltage: Voltage, frequency: Frequency) -> f32 {
    input_power.mw() as f32 / (voltage.as_volts() * voltage.as_volts() * frequency.as_mhz())
}

/// Estimate the rail voltage needed for the target power at a candidate frequency.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn predicted_voltage(target: Power, alpha: f32, frequency: Frequency) -> Voltage {
    let volts = sqrtf(target.mw() as f32 / (alpha * frequency.as_mhz()));
    Voltage::from_mv((volts * 1000.0) as u32)
}

#[cfg(test)]
#[path = "../tests/power.rs"]
mod tests;
