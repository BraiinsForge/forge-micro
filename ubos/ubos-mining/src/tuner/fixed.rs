//! `FixedTuner` — chip bring-up plus a constant-power-curve ramp.
//!
//! Drives the per-chip `ubos_asic::ChipProtocol` bring-up over a
//! [`SerialBus`] (reset, enumerate, domains, cores, nonce split, then
//! the baud-rate switch), then walks a coupled voltage / frequency ramp
//! from the chip's open-core voltage at the safe-boot frequency to the configured
//! `(voltage, frequency)` target. The ramp preserves `V^2 * f` along the
//! walk so the chain never exceeds the target power at any intermediate
//! step. Host-side policy — PSU voltage, the host UART baud switch, and
//! work dispatch — stays here; the chip register sequence lives in the
//! `ChipProtocol` impl.

use embassy_time::{Duration, Timer};
use libm::sqrtf;
use ubos_asic::{BoardDescriptor, ChipAddress, ChipBus, ChipProtocol, TicketMaskReg};
use ubos_core::units::{Frequency, Voltage};
use ubos_logging::{debug, error, warn};

use crate::bus::SerialBus;
use crate::consts::DIFFICULTY;
use crate::control::publish_tuner_step;
use crate::miner::MinerControl;

use super::{Tuner, TunerError, TuningConfig};

/// Upper bound on the number of ramp steps any single transition
/// produces. The chain's safe operating range spans 50..=1000 MHz; at
/// the 6.25 MHz step that is ~152 steps full-span, plus headroom for
/// the constant-power curve's smaller late-stage voltage increments.
/// Used only by the cfg(test) helpers; the production `while` loops
/// carry no buffer.
#[cfg(test)]
const RAMP_STEP_CAP: usize = 256;

/// Tuner that walks a `V^2 * f = P_target` curve from the chip's
/// open-core voltage at the safe-boot frequency to the configured
/// operating point. Holds `&'static BoardDescriptor` so the open-core voltage
/// and ramp constraints come from one chip-aware source instead of
/// duplicated module constants.
pub struct FixedTuner {
    board: &'static BoardDescriptor,
    target: TuningConfig,
}

impl FixedTuner {
    /// Build a tuner for `board`, anchored at the chip's minimum operating
    /// clock and the board's max rail voltage; the bring-up loop ramps from
    /// there to the configured operating point.
    #[must_use]
    pub fn new(board: &'static BoardDescriptor) -> Self {
        Self {
            board,
            target: TuningConfig {
                frequency: board.chip.descriptor().frequency_range.min(),
                voltage: board.max_rail_voltage(),
            },
        }
    }

    /// Settling delay held after each frequency programming write.
    const RAMP_SETTLE: Duration = Duration::from_secs(1);

    /// Period between consecutive work-frame sends.
    const WORK_PERIOD: Duration = Duration::from_secs(1);

    /// Reset + enumerate attempts before a chip-count mismatch is treated
    /// as terminal.
    const ENUMERATE_ATTEMPTS: u8 = 3;

    /// Post-reset anchor frequency: the chip's minimum operating clock,
    /// taken from its descriptor. `init` anchors the chain here before
    /// ramping up and the ramp never floors below it; `deinit` ramps
    /// back down to it.
    fn safe_boot(&self) -> Frequency {
        self.board.chip.descriptor().frequency_range.min()
    }

    /// Walk the chain from `(self.target.voltage, self.target.frequency)`
    /// to `target` along the constant-power curve `V^2 * f = P_target`.
    ///
    /// `target.frequency` is first floored onto the chip's PLL step grid
    /// via [`snap_down`], so every programmed hop lands on an exactly
    /// representable hash clock and telemetry reports the realized
    /// frequency. Phase A holds the current voltage and steps frequency in
    /// `frequency_step` hops toward the snapped target until `V_now^2 * f`
    /// reaches `P_target = V_target^2 * f_target`. Phase B then lowers
    /// voltage to `sqrt(P_target / f_next)` before each frequency hop so
    /// the chain never exceeds the target power at an intermediate step.
    async fn walk_to_target<M: MinerControl>(
        &mut self,
        miner: &M,
        bus: &mut SerialBus,
        target: TuningConfig,
    ) -> Result<(), TunerError> {
        let step = self.board.chip.descriptor().frequency_step;
        // Floor the configured frequency onto the chip's PLL grid so
        // every programmed step is exactly representable and telemetry
        // reports the realized hash clock rather than the request.
        let target = TuningConfig {
            frequency: snap_down(target.frequency, step),
            voltage: target.voltage,
        };
        let target_volts = target.voltage.as_volts();
        // P_target lives in V^2 * MHz. With V <= 1.5 V and f <= 1000 MHz
        // the value is well inside f32 mantissa precision.
        let power_target = target_volts * target_volts * target.frequency.as_mhz();

        let mut voltage_now = self.target.voltage;
        let mut frequency_now = self.target.frequency;

        crate::events::frequency_ramp_started(target.frequency.khz());

        // Phase A: hold V_now, step f toward f_target until V_now^2 * f
        // crosses P_target. If V_now <= V_target, V_now^2 * f_target is
        // already <= P_target, so this loop walks straight to
        // f_target and Phase B never runs.
        let voltage_now_squared = voltage_now.as_volts() * voltage_now.as_volts();
        let phase_a_cap = phase_a_target(
            power_target,
            voltage_now_squared,
            target.frequency,
            step,
            self.safe_boot(),
        );
        while frequency_now != phase_a_cap {
            let next = step_frequency(frequency_now, phase_a_cap, step);
            debug!(
                "fixed-tuner phase A: frequency {} kHz -> {} kHz at {} mV",
                frequency_now.khz(),
                next.khz(),
                voltage_now.mv()
            );
            let chip = self.board.chip;
            chip.set_hash_freq(bus, next).await?;
            publish_tuner_step(next, voltage_now);
            frequency_now = next;
            Timer::after(Self::RAMP_SETTLE).await;
        }

        // Phase B: voltage steps down to `sqrt(power_target / next_frequency)`
        // before each frequency hop, then frequency steps up. Loop
        // continues until V matches V_target and f matches f_target.
        while voltage_now != target.voltage || frequency_now != target.frequency {
            let next_frequency = step_frequency(frequency_now, target.frequency, step);
            // The curve math lands within a millivolt of the target,
            // but the f32 round-trip can leave a 1 mV residual that
            // integer equality never closes, spinning this loop
            // forever. Snap the final hop to the exact target so
            // convergence rides on the integer frequency comparison,
            // which `step_frequency` always drives home.
            let next_voltage = if next_frequency == target.frequency {
                target.voltage
            } else {
                curve_voltage(power_target, next_frequency, target.voltage)
            };
            debug!(
                "fixed-tuner phase B: voltage {} mV -> {} mV, frequency {} kHz -> {} kHz",
                voltage_now.mv(),
                next_voltage.mv(),
                frequency_now.khz(),
                next_frequency.khz()
            );
            miner.set_voltage(next_voltage).await?;
            let chip = self.board.chip;
            chip.set_hash_freq(bus, next_frequency).await?;
            publish_tuner_step(next_frequency, next_voltage);
            voltage_now = next_voltage;
            frequency_now = next_frequency;
            Timer::after(Self::RAMP_SETTLE).await;
        }

        self.target = target;
        crate::events::frequency_ramp_complete(target.frequency.khz());
        Ok(())
    }
}

impl<M: MinerControl> Tuner<M> for FixedTuner {
    async fn init(&mut self, miner: &M, target: TuningConfig) -> Result<(), TunerError> {
        let mut bus = SerialBus;

        // Open-core voltage anchors the curve at the chip's safe
        // headroom; cores get clocked at the safe-boot baseline and the
        // walk then trades voltage for frequency along V^2 * f.
        let safe_boot = self.safe_boot();
        let open_core = self.board.max_rail_voltage();
        debug!(
            "fixed-tuner init: anchoring at open-core {} mV / safe-boot {} kHz",
            open_core.mv(),
            safe_boot.khz()
        );
        miner.set_voltage(open_core).await?;
        publish_tuner_step(safe_boot, open_core);
        self.target = TuningConfig {
            frequency: safe_boot,
            voltage: open_core,
        };

        // Reset and enumerate, retrying a transient chain glitch a few
        // times. A persistent count mismatch is terminal: the miner must
        // not run a board whose chain it cannot account for.
        let chip = self.board.chip;
        for attempt in 1..=Self::ENUMERATE_ATTEMPTS {
            chip.reset_chain(&mut bus).await;
            let found = chip.enumerate_chips(&mut bus).await;
            if found == self.board.num_chips {
                break;
            }
            // The last attempt's failure is terminal; earlier ones warn
            // and retry.
            if attempt == Self::ENUMERATE_ATTEMPTS {
                error!(
                    "fixed-tuner: chain count {} != expected {}; stopping",
                    found, self.board.num_chips
                );
                return Err(TunerError::ChipCountMismatch {
                    found,
                    expected: self.board.num_chips,
                });
            }
            warn!(
                "fixed-tuner: enumerated {} chips, expected {} (attempt {}/{})",
                found,
                self.board.num_chips,
                attempt,
                Self::ENUMERATE_ATTEMPTS
            );
        }

        chip.initialize_domains(&mut bus, self.board).await;
        chip.initialize_cores(&mut bus, self.board).await;
        chip.split_nonce_space(&mut bus, self.board).await;
        // Anchor the PLL at the safe-boot baseline on the boot UART. The
        // counting number follows on the fast UART after the baud switch,
        // matching the capture, which never sends it at the boot baud.
        chip.set_pll(&mut bus, safe_boot).await?;
        // Re-arm the cores against the now-running hash clock; without it the
        // cores stay configured against the boot clock and the hashrate halves.
        chip.post_pll_rearm(&mut bus, self.board).await;
        // Initial share filter accepts every share (difficulty 1).
        let ticket = TicketMaskReg::from_difficulty(DIFFICULTY)
            .expect("BUG: DIFFICULTY must be a power of two");
        bus.write_register(ChipAddress::All, ticket).await;
        // Baud switch last: program the chip side, then the host UART.
        // The rate comes from the chip so a BM1370 uses its SI-safe
        // 520 833 Hz instead of the generic 3.125 Mbaud.
        chip.enable_fast_uart(&mut bus).await;
        miner.set_baud_rate(chip.fast_uart_host_rate()).await?;
        // Counting number on the fast UART, after the baud switch.
        chip.set_hcn(&mut bus, safe_boot).await?;
        miner
            .start_sending_work(Self::WORK_PERIOD)
            .await
            .map_err(TunerError::Miner)?;

        self.walk_to_target(miner, &mut bus, target).await
    }

    async fn deinit(&mut self, miner: &M) -> Result<(), TunerError> {
        let mut bus = SerialBus;
        let safe = TuningConfig {
            frequency: self.safe_boot(),
            voltage: self.board.max_rail_voltage(),
        };
        self.walk_to_target(miner, &mut bus, safe).await
    }
}

/// Round `frequency` down to the chip's PLL step grid. The walk programs
/// only multiples of `step`; the BM1362 PLL calculator resolves each to
/// the nearest reachable clock, so the realized hash clock tracks the
/// published frequency without drifting between hops.
fn snap_down(frequency: Frequency, step: Frequency) -> Frequency {
    let step_khz = step.khz();
    Frequency::from_khz(frequency.khz() / step_khz * step_khz)
}

/// Step `current` toward `target` by one `step` hop, clamping to
/// land exactly on `target` if the remaining gap is smaller than the
/// step. Comparison runs in integer kHz space so the loop converges
/// even when `target` came from float rounding inside the V^2 * f
/// math; in-band float comparison would tail-loop on the last fraction
/// of a step.
fn step_frequency(current: Frequency, target: Frequency, step: Frequency) -> Frequency {
    use core::cmp::Ordering;
    let step_khz = step.khz();
    let next_khz = match current.cmp(&target) {
        Ordering::Less => (current.khz() + step_khz).min(target.khz()),
        Ordering::Greater => current.khz().saturating_sub(step_khz).max(target.khz()),
        Ordering::Equal => return current,
    };
    Frequency::from_khz(next_khz)
}

/// Phase A's frequency cap: the `Frequency` at which `voltage_now_squared * f`
/// just reaches `power_target`. Clamped to `target` so phase A doesn't
/// overshoot when `V_now <= V_target` (in that case the cap is at or
/// past target frequency and the walk reduces to a plain frequency
/// ramp at constant voltage).
fn phase_a_target(
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
    let curve_snapped = snap_down(Frequency::from_khz(curve_kilohertz), step);
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

/// Pure mirror of `walk_to_target` for offline equivalence testing.
/// Returns the `(frequency_khz, voltage_mv)` pairs the tuner would
/// issue when walking from `(from_voltage, from_frequency)` to
/// `(target_voltage, target_frequency)`.
#[cfg(test)]
pub(super) fn walk_steps(
    from: TuningConfig,
    target: TuningConfig,
    step: Frequency,
    floor: Frequency,
) -> heapless::Vec<(u32, u32), RAMP_STEP_CAP> {
    let mut steps = heapless::Vec::new();
    let target = TuningConfig {
        frequency: snap_down(target.frequency, step),
        voltage: target.voltage,
    };
    let target_volts = target.voltage.as_volts();
    let power_target = target_volts * target_volts * target.frequency.as_mhz();

    let mut voltage_now = from.voltage;
    let mut frequency_now = from.frequency;

    let voltage_now_squared = voltage_now.as_volts() * voltage_now.as_volts();
    let phase_a_cap = phase_a_target(
        power_target,
        voltage_now_squared,
        target.frequency,
        step,
        floor,
    );
    while frequency_now != phase_a_cap {
        let next = step_frequency(frequency_now, phase_a_cap, step);
        steps
            .push((next.khz(), voltage_now.mv()))
            .expect("BUG: phase A exceeds RAMP_STEP_CAP");
        frequency_now = next;
    }

    while voltage_now != target.voltage || frequency_now != target.frequency {
        let next_frequency = step_frequency(frequency_now, target.frequency, step);
        let next_voltage = if next_frequency == target.frequency {
            target.voltage
        } else {
            curve_voltage(power_target, next_frequency, target.voltage)
        };
        steps
            .push((next_frequency.khz(), next_voltage.mv()))
            .expect("BUG: phase B exceeds RAMP_STEP_CAP");
        voltage_now = next_voltage;
        frequency_now = next_frequency;
    }

    steps
}

#[cfg(test)]
#[path = "../tests/tuner.rs"]
mod tests;
