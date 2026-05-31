//! `FixedTuner` — chip bring-up plus a constant-power-curve ramp via
//! [`crate::miner::MinerControl`].
//!
//! Owns the full bring-up sequence: chain wake-up, address assignment,
//! pre-PLL configuration, baud-rate switch, per-chip post-PLL
//! configuration, then a coupled voltage / frequency ramp from the
//! chip's open-core voltage at `SAFE_BOOT` to the configured
//! `(voltage, frequency)` target. The ramp preserves `V^2 * f` along
//! the walk so the chain never exceeds the target power at any
//! intermediate step.

use embassy_time::{Duration, Timer};
use libm::sqrtf;
use ubos_asic::{
    AnalogMuxControl, BMM100, BaudRateReg, BoardDescriptor, ChipNonceOffsetReg, ControlCommand,
    CoreControlReg, CoreRegister, DomainSelect, HashCountingNumberReg, IoDriverStrengthCtrlReg,
    MidstateCalReg, MiscCtrlReg, Pll0DividerReg, PllParameterReg, RawChipAddress, Register,
    SoftResetControlReg, TicketMaskReg,
};
use ubos_core::units::{Frequency, Voltage};
use ubos_logging::debug;

use crate::control::publish_tuner_step;
use crate::miner::MinerControl;

use super::{Tuner, TunerError, TuningConfig};

/// Post-reset PLL frequency. `init` programs the chain to this
/// baseline before ramping up; `deinit` ramps back down to it.
const SAFE_BOOT: Frequency = Frequency::from_khz(50_000);

/// Upper bound on the number of ramp steps any single transition
/// produces. The chain's safe operating range spans 50..=700 MHz; at
/// the 6.25 MHz step that is ~104 steps full-span, plus headroom for
/// the constant-power curve's smaller late-stage voltage increments.
/// Used only by the cfg(test) helpers; the production `while` loops
/// carry no buffer.
#[cfg(test)]
const RAMP_STEP_CAP: usize = 160;

/// Tuner that walks a `V^2 * f = P_target` curve from the chip's
/// open-core voltage at `SAFE_BOOT` to the configured operating
/// point. Holds `&'static BoardDescriptor` so the open-core voltage
/// and ramp constraints come from one chip-aware source instead of
/// duplicated module constants.
pub struct FixedTuner {
    board: &'static BoardDescriptor,
    target: TuningConfig,
}

impl Default for FixedTuner {
    fn default() -> Self {
        Self {
            board: &BMM100,
            target: TuningConfig {
                frequency: SAFE_BOOT,
                voltage: BMM100.max_rail_voltage(),
            },
        }
    }
}

impl FixedTuner {
    /// Settling delay held after each frequency programming write.
    const RAMP_SETTLE: Duration = Duration::from_secs(1);

    /// Number of BM1362 chips on the hashboard.
    const CHIP_COUNT: u8 = 4;

    /// Address stride between successive chips on the chain. The captured
    /// trace assigns 0x00, 0x01, 0x02, 0x03 — stride 1.
    const ADDRESS_STRIDE: u8 = 1;

    /// Period between consecutive work-frame sends. Matches the captured
    /// `Step::StartSendingWork(Duration::from_secs(1))` interval.
    const WORK_PERIOD: Duration = Duration::from_secs(1);

    /// Number of pre-address `ChainInactive` broadcasts. The captured
    /// trace issues three to tolerate single-frame loss before address
    /// assignment begins.
    const CHAIN_INACTIVE_REPEATS: usize = 3;

    /// Settle delay between `ChainInactive` broadcasts.
    const CHAIN_INACTIVE_GAP: Duration = Duration::from_millis(300);

    /// Settle delay between the broadcast register writes in the pre-
    /// enumerate phase.
    const PRE_ENUMERATE_GAP: Duration = Duration::from_millis(100);

    /// Settle delay between the two post-address `CoreControl` writes.
    const POST_ADDRESS_GAP: Duration = Duration::from_millis(50);

    /// `ClockDelayCtrl` value loaded broadcast during the post-address-assign
    /// phase, before the per-chip post-PLL pass writes 0x08. Captured wire
    /// payload `0x8000_8034` corresponds to
    /// `CoreControlReg::for_core_write(ClockDelayCtrl, 0x34)`.
    const PRE_PLL_CLOCK_DELAY: u8 = 0x34;

    async fn pre_enumerate<M: MinerControl>(&self, miner: &M) -> Result<(), TunerError> {
        write_register(miner, RawChipAddress::All, SoftResetControlReg::quiescent()).await;
        Timer::after(Self::PRE_ENUMERATE_GAP).await;
        write_register(
            miner,
            RawChipAddress::All,
            MiscCtrlReg::default_bringup_config(),
        )
        .await;
        Timer::after(Self::PRE_ENUMERATE_GAP).await;
        write_register(
            miner,
            RawChipAddress::All,
            MidstateCalReg::factory_default(),
        )
        .await;
        Timer::after(Self::PRE_ENUMERATE_GAP).await;
        Ok(())
    }

    async fn deactivate_chain<M: MinerControl>(&self, miner: &M) -> Result<(), TunerError> {
        for _ in 0..Self::CHAIN_INACTIVE_REPEATS {
            miner.send_command(ControlCommand::chain_inactive()).await;
            Timer::after(Self::CHAIN_INACTIVE_GAP).await;
        }
        Ok(())
    }

    async fn assign_addresses<M: MinerControl>(&self, miner: &M) -> Result<(), TunerError> {
        for index in 0..Self::CHIP_COUNT {
            let hw_addr = index * Self::ADDRESS_STRIDE;
            miner
                .send_command(ControlCommand::set_chip_address(hw_addr))
                .await;
        }
        Ok(())
    }

    async fn post_address_assign<M: MinerControl>(&self, miner: &M) -> Result<(), TunerError> {
        write_register(
            miner,
            RawChipAddress::All,
            CoreControlReg::default_hash_clock_config(),
        )
        .await;
        Timer::after(Self::POST_ADDRESS_GAP).await;
        write_register(
            miner,
            RawChipAddress::All,
            CoreControlReg::for_core_write(CoreRegister::ClockDelayCtrl, Self::PRE_PLL_CLOCK_DELAY),
        )
        .await;
        Timer::after(Self::POST_ADDRESS_GAP).await;
        Ok(())
    }

    async fn pre_pll_configure<M: MinerControl>(&self, miner: &M) -> Result<(), TunerError> {
        write_register(miner, RawChipAddress::All, AnalogMuxControl::disabled()).await;
        write_register(
            miner,
            RawChipAddress::All,
            IoDriverStrengthCtrlReg::default_drive_strengths(),
        )
        .await;
        Ok(())
    }

    async fn switch_fast_baud<M: MinerControl>(&self, miner: &M) -> Result<(), TunerError> {
        write_register(miner, RawChipAddress::All, BaudRateReg::fast_3mbaud()).await;
        // The chip is now listening at 3.125 Mbaud; switch the host
        // controller to match. `set_baud_rate` translates the
        // requested rate to the actual chip line rate internally.
        miner
            .set_baud_rate(crate::consts::FAST_BAUDRATE)
            .await
            .map_err(TunerError::Miner)?;
        Ok(())
    }

    async fn post_pll_configure<M: MinerControl>(&self, miner: &M) -> Result<(), TunerError> {
        for index in 0..Self::CHIP_COUNT {
            let chip = RawChipAddress::Chip(index * Self::ADDRESS_STRIDE);
            write_register(
                miner,
                chip,
                SoftResetControlReg::reset_core_domain(DomainSelect::Domain1),
            )
            .await;
            write_register(miner, chip, MiscCtrlReg::default_bringup_config()).await;
            write_register(miner, chip, CoreControlReg::default_hash_clock_config()).await;
            write_register(miner, chip, CoreControlReg::default_clock_delay_config()).await;
            write_register(miner, chip, CoreControlReg::default_core_reg2_config()).await;
        }
        Ok(())
    }

    async fn assign_chip_nonce_offsets<M: MinerControl>(
        &self,
        miner: &M,
    ) -> Result<(), TunerError> {
        // Split the 16-bit nonce space across the chain so each chip
        // searches a distinct quarter. The exact offsets come from the
        // captured trace: 0x0000, 0x3fff, 0x7ffe, 0xbffd — chip-quarter
        // boundaries minus a small per-chip skew.
        const OFFSETS: [u16; FixedTuner::CHIP_COUNT as usize] = [0x0000, 0x3fff, 0x7ffe, 0xbffd];
        for (index, offset) in OFFSETS.iter().copied().enumerate() {
            // Safe to cast: CHIP_COUNT fits in u8 and the index
            // is bounded by the array length.
            #[allow(clippy::cast_possible_truncation)]
            let chip = RawChipAddress::Chip(index as u8 * Self::ADDRESS_STRIDE);
            write_register(miner, chip, ChipNonceOffsetReg::from_offset(offset)).await;
        }
        Ok(())
    }

    async fn write_initial_hcn<M: MinerControl>(&self, miner: &M) -> Result<(), TunerError> {
        let hcn = HashCountingNumberReg::from_freq(SAFE_BOOT.khz()).ok_or(TunerError::Miner(
            crate::miner::MinerError::UnsupportedFrequency(SAFE_BOOT.khz()),
        ))?;
        write_register(miner, RawChipAddress::All, hcn).await;
        Ok(())
    }

    async fn set_initial_ticket_mask<M: MinerControl>(&self, miner: &M) -> Result<(), TunerError> {
        // Difficulty 1 = mask zero, accept every share. Production
        // tuning narrows this via `MinerControl::set_ticket_mask` later
        // once the host wants a coarser filter.
        //
        // `from_difficulty` rejects only zero and non-powers-of-two, so
        // the constant 1 always encodes and the expect cannot fire.
        let mask = TicketMaskReg::from_difficulty(1)
            .expect("BUG: difficulty 1 must encode")
            .ticket_mask();
        miner.set_ticket_mask(mask).await.map_err(TunerError::Miner)
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
        target: TuningConfig,
    ) -> Result<(), TunerError> {
        let step = self.board.chip.frequency_step;
        // Floor the configured frequency onto the chip's PLL grid so
        // every programmed step is exactly representable and telemetry
        // reports the realized hash clock rather than the request.
        let target = TuningConfig {
            frequency: snap_down(target.frequency, step),
            voltage: target.voltage,
        };
        let target_volts = target.voltage.as_volts();
        // P_target lives in V^2 * MHz. With V <= 1.5 V and f <= 700 MHz
        // the value is well inside f32 mantissa precision.
        let power_target = target_volts * target_volts * target.frequency.as_mhz();

        let mut voltage_now = self.target.voltage;
        let mut frequency_now = self.target.frequency;

        // Phase A: hold V_now, step f toward f_target until V_now^2 * f
        // crosses P_target. If V_now <= V_target, V_now^2 * f_target is
        // already <= P_target, so this loop walks straight to
        // f_target and Phase B never runs.
        let voltage_now_squared = voltage_now.as_volts() * voltage_now.as_volts();
        let phase_a_cap = phase_a_target(power_target, voltage_now_squared, target.frequency, step);
        while frequency_now != phase_a_cap {
            let next = step_frequency(frequency_now, phase_a_cap, step);
            debug!(
                "fixed-tuner phase A: frequency {} kHz -> {} kHz at {} mV",
                frequency_now.khz(),
                next.khz(),
                voltage_now.mv()
            );
            miner.set_frequency(RawChipAddress::All, next).await?;
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
            miner
                .set_frequency(RawChipAddress::All, next_frequency)
                .await?;
            publish_tuner_step(next_frequency, next_voltage);
            voltage_now = next_voltage;
            frequency_now = next_frequency;
            Timer::after(Self::RAMP_SETTLE).await;
        }

        self.target = target;
        Ok(())
    }
}

impl<M: MinerControl> Tuner<M> for FixedTuner {
    async fn init(&mut self, miner: &M, target: TuningConfig) -> Result<(), TunerError> {
        // Open-core voltage anchors the curve at the chip's safe
        // headroom; cores get clocked at SAFE_BOOT and the walk then
        // trades voltage for frequency along V^2 * f.
        let open_core = self.board.max_rail_voltage();
        debug!(
            "fixed-tuner init: anchoring at open-core {} mV / safe-boot {} kHz",
            open_core.mv(),
            SAFE_BOOT.khz()
        );
        miner.set_voltage(open_core).await?;
        publish_tuner_step(SAFE_BOOT, open_core);
        self.target = TuningConfig {
            frequency: SAFE_BOOT,
            voltage: open_core,
        };

        self.pre_enumerate(miner).await?;
        self.deactivate_chain(miner).await?;
        self.assign_addresses(miner).await?;
        self.post_address_assign(miner).await?;
        self.pre_pll_configure(miner).await?;
        // Initial PLL at SAFE_BOOT, still 115_200 baud.
        write_pll_only(miner, RawChipAddress::All, SAFE_BOOT.khz()).await?;
        self.switch_fast_baud(miner).await?;
        self.post_pll_configure(miner).await?;
        self.assign_chip_nonce_offsets(miner).await?;
        self.write_initial_hcn(miner).await?;
        self.set_initial_ticket_mask(miner).await?;
        miner
            .start_sending_work(Self::WORK_PERIOD)
            .await
            .map_err(TunerError::Miner)?;

        self.walk_to_target(miner, target).await
    }

    async fn deinit(&mut self, miner: &M) -> Result<(), TunerError> {
        let safe = TuningConfig {
            frequency: SAFE_BOOT,
            voltage: self.board.max_rail_voltage(),
        };
        self.walk_to_target(miner, safe).await
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
) -> Frequency {
    let curve_mhz = power_target / voltage_now_squared;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let curve_kilohertz = (curve_mhz * 1000.0) as u32;
    // Snap the curve crossing onto the PLL grid before clamping. With a
    // grid-aligned target and SAFE_BOOT, the resulting cap stays on the
    // grid so phase A's final step lands on an exact hash clock.
    let curve_snapped = snap_down(Frequency::from_khz(curve_kilohertz), step).khz();
    Frequency::from_khz(curve_snapped.min(target.khz()).max(SAFE_BOOT.khz()))
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

/// Typed register write helper: converts the typed value to its
/// big-endian payload, then dispatches through the byte-level
/// `MinerControl::write_register_raw` escape hatch. This is the only
/// callable the tuner has for register writes; the trait deliberately
/// does not generalize over `Register` to keep
/// `async_fn_in_trait` free of generic parameters.
async fn write_register<M, R>(miner: &M, addr: RawChipAddress, reg: R)
where
    M: MinerControl,
    R: Register,
{
    let value = u32::from_be_bytes(reg.to_be_bytes());
    miner.write_register_raw(addr, R::REG_NUM, value).await;
}

/// Initial PLL write at `target_khz` using the divider-fence +
/// duplicate-write pattern, without touching HCN. The trace performs
/// this once before the baud-rate switch so the chip's PLL is at
/// `SAFE_BOOT` exactly while still running at the post-reset
/// `INITIAL_BAUDRATE`; the HCN write comes later (post-baud).
async fn write_pll_only<M: MinerControl>(
    miner: &M,
    addr: RawChipAddress,
    target_khz: u32,
) -> Result<(), TunerError> {
    let pll = PllParameterReg::from_freq(target_khz).ok_or(TunerError::Miner(
        crate::miner::MinerError::UnsupportedFrequency(target_khz),
    ))?;
    let fence = Pll0DividerReg::zeroed();
    write_register(miner, addr, fence).await;
    write_register(miner, addr, pll).await;
    write_register(miner, addr, fence).await;
    write_register(miner, addr, pll).await;
    Ok(())
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
    let phase_a_cap = phase_a_target(power_target, voltage_now_squared, target.frequency, step);
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
