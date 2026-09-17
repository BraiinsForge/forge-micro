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

//! Equivalence and invariant tests for `FixedTuner`'s constant-power
//! curve walk. Gates the (frequency, voltage) sequence the tuner
//! would emit against the BM1362 PLL factory table grid and the
//! V^2 * f preservation rule, plus per-chain dispatch tests against a
//! mock `MinerControl`/`ChainControl` pair covering multi-chain
//! lockstep ramping and absent-chain exclusion.

use core::cell::RefCell;

use embassy_time::Duration;
use ubos_asic::BMM101_HASHBOARD;
use ubos_asic::bm13xx::registers::PllParameterReg;
use ubos_core::units::{Frequency, Voltage};
use ubos_psu::BmmPsuDescriptor;

use crate::chain::control::ChainControl;
use crate::miner::{MinerControl, MinerError};

use super::super::Tuner;
use super::FixedTuner;
use super::TuningConfig;
use super::walk_steps;

#[test]
fn open_core_voltage_fits_default_psu_range() {
    // Gates the BMM101_HASHBOARD descriptor's `max_psu_voltage` mirror against
    // the actual PSU range the firmware configures. The clamp inside
    // `max_rail_voltage` only hides a descriptor / PSU range mismatch;
    // this test surfaces it.
    let open_core = BMM101_HASHBOARD.max_rail_voltage();
    let range = BmmPsuDescriptor::DEFAULT.dcdc_voltage_range;
    assert!(
        range.voltage_to_dac(open_core.mv()).is_some(),
        "open core {} mV is outside PSU range [{}, {}] mV",
        open_core.mv(),
        range.start_mv(),
        range.end_mv(),
    );
}

#[test]
fn walk_to_same_state_is_empty() {
    let from = TuningConfig {
        frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
        voltage: BMM101_HASHBOARD.max_rail_voltage(),
    };
    assert!(
        walk_steps(
            from,
            from,
            BMM101_HASHBOARD.chip.frequency_step,
            BMM101_HASHBOARD.chip.frequency_range.min()
        )
        .is_empty()
    );
}

#[test]
fn walk_emits_target_as_final_step() {
    let target = TuningConfig {
        frequency: Frequency::from_mhz(550),
        voltage: Voltage::from_mv(1100),
    };
    let steps = walk_steps(
        TuningConfig {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
        BMM101_HASHBOARD.chip.frequency_step,
        BMM101_HASHBOARD.chip.frequency_range.min(),
    );
    let (last_f, last_v) = *steps.last().expect("BUG: walk must emit at least one step");
    assert_eq!(last_f, target.frequency.khz());
    assert_eq!(last_v, target.voltage.mv());
}

#[test]
fn walk_converges_for_float_lossy_voltage() {
    // 1012 mV is one of the millivolt targets whose f32 round-trip
    // truncates to 1011, leaving a residual the curve math never
    // closes. Before the final-step snap this spun the phase B loop
    // forever; the walk must land exactly on the target and terminate.
    let target = TuningConfig {
        frequency: Frequency::from_mhz(550),
        voltage: Voltage::from_mv(1012),
    };
    let steps = walk_steps(
        TuningConfig {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
        BMM101_HASHBOARD.chip.frequency_step,
        BMM101_HASHBOARD.chip.frequency_range.min(),
    );
    let (last_f, last_v) = *steps.last().expect("BUG: walk must emit at least one step");
    assert_eq!(last_f, target.frequency.khz());
    assert_eq!(last_v, target.voltage.mv());
}

#[test]
fn walk_preserves_power_ceiling_after_phase_a() {
    // P_target = V_target^2 * f_target. Phase B steps must stay below
    // or equal to P_target (within float rounding). Phase A walks at
    // constant V_open which may briefly exceed P_target on its final
    // hop; the gating invariant kicks in after the curve is entered.
    let target = TuningConfig {
        frequency: Frequency::from_mhz(550),
        voltage: Voltage::from_mv(1100),
    };
    let v_target = target.voltage.as_volts();
    let p_target = v_target * v_target * target.frequency.as_mhz();
    let steps = walk_steps(
        TuningConfig {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
        BMM101_HASHBOARD.chip.frequency_step,
        BMM101_HASHBOARD.chip.frequency_range.min(),
    );
    let mut in_phase_b = false;
    for (f_khz, v_mv) in steps.iter().copied() {
        let volts = Voltage::from_mv(v_mv).as_volts();
        let mhz = Frequency::from_khz(f_khz).as_mhz();
        let power = volts * volts * mhz;
        if v_mv < BMM101_HASHBOARD.max_rail_voltage().mv() {
            in_phase_b = true;
        }
        if in_phase_b {
            assert!(
                power <= p_target * 1.01,
                "phase B step (f={f_khz} kHz, v={v_mv} mV) violates power ceiling: {power} > {p_target}",
            );
        }
    }
}

#[test]
fn walk_visits_every_25mhz_grid_point_with_pll_table_match() {
    let target = TuningConfig {
        frequency: Frequency::from_khz(500_000),
        voltage: BMM101_HASHBOARD.max_rail_voltage(),
    };
    let steps = walk_steps(
        TuningConfig {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
        BMM101_HASHBOARD.chip.frequency_step,
        BMM101_HASHBOARD.chip.frequency_range.min(),
    );
    let range = BMM101_HASHBOARD.chip.frequency_range;
    for (f_khz, _) in steps.iter().copied() {
        assert!(
            PllParameterReg::from_freq(f_khz, range).is_some(),
            "walk emits {f_khz} kHz but calculator rejects it",
        );
    }
}

#[test]
fn walk_floors_off_grid_target_onto_step_grid() {
    // 551 MHz is not a 6.25 MHz multiple. The walk must floor it to
    // 550 MHz so the programmed hash clock is exactly representable.
    let target = TuningConfig {
        frequency: Frequency::from_khz(551_000),
        voltage: Voltage::from_mv(1100),
    };
    let steps = walk_steps(
        TuningConfig {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
        BMM101_HASHBOARD.chip.frequency_step,
        BMM101_HASHBOARD.chip.frequency_range.min(),
    );
    let (last_frequency, _) = *steps.last().expect("BUG: walk must emit at least one step");
    assert_eq!(
        last_frequency, 550_000,
        "off-grid target must floor onto the 6.25 MHz grid"
    );
}

/// A recorded call into a [`MockChain`]: which chain, and which
/// `ChainControl` method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Configure,
    PvtSweep,
    SetFrequency(u32),
    StartSendingWork,
}

/// Call log a [`MockMiner`]'s chains share, recording `(chain, call)`
/// pairs in dispatch order so a test can check both which chains were
/// addressed and the order calls interleaved in.
type CallLog = RefCell<heapless::Vec<(usize, Call), 512>>;

/// A `ChainControl` mock that records every call it receives instead
/// of touching real hardware.
struct MockChain<'a> {
    chain: usize,
    log: &'a CallLog,
}

impl ChainControl for MockChain<'_> {
    fn chain(&self) -> usize {
        self.chain
    }

    async fn reset(&self) -> Result<(), MinerError> {
        Ok(())
    }

    async fn reset_and_enumerate(&self) -> u8 {
        0
    }

    async fn configure(&self) -> Result<(), MinerError> {
        self.log
            .borrow_mut()
            .push((self.chain, Call::Configure))
            .expect("BUG: call log capacity exceeded");
        Ok(())
    }

    async fn pvt_sweep(&self) -> Result<(), MinerError> {
        self.log
            .borrow_mut()
            .push((self.chain, Call::PvtSweep))
            .expect("BUG: call log capacity exceeded");
        Ok(())
    }

    async fn set_frequency(&self, freq: Frequency) -> Result<(), MinerError> {
        self.log
            .borrow_mut()
            .push((self.chain, Call::SetFrequency(freq.khz())))
            .expect("BUG: call log capacity exceeded");
        Ok(())
    }

    async fn start_sending_work(&self, _period: Duration) -> Result<(), MinerError> {
        self.log
            .borrow_mut()
            .push((self.chain, Call::StartSendingWork))
            .expect("BUG: call log capacity exceeded");
        Ok(())
    }

    async fn stop_sending_work(&self) -> Result<(), MinerError> {
        Ok(())
    }
}

/// A `MinerControl` mock exposing only the chains it was given as
/// present; a test constructs it with fewer chains than the board's
/// full chain count to model an absent connector.
struct MockMiner<'a> {
    chains: heapless::Vec<MockChain<'a>, 4>,
}

impl<'a> MinerControl for MockMiner<'a> {
    type Chain = MockChain<'a>;

    async fn set_voltage(&self, _voltage: Voltage) -> Result<(), MinerError> {
        Ok(())
    }

    fn present_chains(&self) -> impl Iterator<Item = &Self::Chain> {
        self.chains.iter()
    }
}

#[test]
fn init_configures_and_starts_work_on_every_present_chain() {
    let log = CallLog::default();
    let miner = MockMiner {
        chains: heapless::Vec::from_array([
            MockChain {
                chain: 0,
                log: &log,
            },
            MockChain {
                chain: 1,
                log: &log,
            },
        ]),
    };
    let mut tuner = FixedTuner::new(&BMM101_HASHBOARD);
    let target = TuningConfig {
        frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
        voltage: BMM101_HASHBOARD.max_rail_voltage(),
    };

    embassy_futures::block_on(tuner.init(&miner, target)).expect("BUG: init against the mock");

    let calls = log.borrow();
    for chain in 0..2 {
        let position = |wanted: Call| {
            calls
                .iter()
                .position(|(c, call)| *c == chain && *call == wanted)
        };
        let configured = position(Call::Configure).expect("chain was never configured");
        let swept = position(Call::PvtSweep).expect("chain was never swept");
        let working = position(Call::StartSendingWork).expect("chain never started work");
        // The sweep gives the thermal loop a reading to hold through the
        // ramp, so it belongs after the chain answers on the fast line
        // and before work starts.
        assert!(
            configured < swept && swept < working,
            "chain {chain} swept out of order"
        );
    }
}

#[test]
fn frequency_steps_interleave_across_present_chains() {
    let log = CallLog::default();
    let miner = MockMiner {
        chains: heapless::Vec::from_array([
            MockChain {
                chain: 0,
                log: &log,
            },
            MockChain {
                chain: 1,
                log: &log,
            },
        ]),
    };
    let mut tuner = FixedTuner::new(&BMM101_HASHBOARD);
    let target = TuningConfig {
        frequency: Frequency::from_mhz(75),
        voltage: BMM101_HASHBOARD.max_rail_voltage(),
    };

    embassy_futures::block_on(tuner.walk_to_target(&miner, target, Duration::from_millis(0)))
        .expect("BUG: walk must not fail against the mock");

    let calls = log.borrow();
    let frequency_calls: heapless::Vec<(usize, u32), 512> = calls
        .iter()
        .filter_map(|(chain, call)| match call {
            Call::SetFrequency(khz) => Some((*chain, *khz)),
            _ => None,
        })
        .collect();
    assert!(
        frequency_calls.len() >= 4,
        "walk from safe-boot to 75 MHz should emit at least two steps per chain"
    );

    // Every step programs chain 0 then chain 1 at the same frequency
    // before the walk moves to the next step, rather than chain 0
    // ramping fully before chain 1 starts.
    let mut index = 0;
    while index + 1 < frequency_calls.len() {
        let (chain_a, freq_a) = frequency_calls[index];
        let (chain_b, freq_b) = frequency_calls[index + 1];
        assert_eq!(chain_a, 0, "step {index} did not start with chain 0");
        assert_eq!(chain_b, 1, "step {index} did not follow with chain 1");
        assert_eq!(
            freq_a, freq_b,
            "chain 0 and chain 1 must see the same frequency at step {index}"
        );
        index += 2;
    }
}

#[test]
fn absent_chain_is_never_addressed() {
    let log = CallLog::default();
    // Only chain 0 is present; a real miner would have found chain 1
    // absent during enumeration and left it out of `present_chains`.
    let miner = MockMiner {
        chains: heapless::Vec::from_array([MockChain {
            chain: 0,
            log: &log,
        }]),
    };
    let mut tuner = FixedTuner::new(&BMM101_HASHBOARD);
    let target = TuningConfig {
        frequency: Frequency::from_mhz(75),
        voltage: BMM101_HASHBOARD.max_rail_voltage(),
    };

    embassy_futures::block_on(tuner.walk_to_target(&miner, target, Duration::from_millis(0)))
        .expect("BUG: walk must not fail against the mock");

    let calls = log.borrow();
    assert!(
        calls.iter().all(|(chain, _)| *chain == 0),
        "chain 1 was never enumerated present and must not be addressed"
    );
}
