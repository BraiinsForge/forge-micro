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
//! V^2 * f preservation rule.

use ubos_asic::BMM101_HASHBOARD;
use ubos_asic::bm13xx::registers::PllParameterReg;
use ubos_core::units::{Frequency, Voltage};
use ubos_psu::BmmPsuDescriptor;

use super::FixedTarget;
use super::FixedTuner;
use embassy_time::Instant;
use ubos_tuner::{ChainReadings, Readings, Tuner};

/// Drive the production policy with the state produced by each plan.
fn walk_steps(from: FixedTarget, target: FixedTarget) -> heapless::Vec<(u32, u32), 256> {
    let mut tuner = FixedTuner::new(&BMM101_HASHBOARD);
    let mut readings = Readings {
        now: Instant::from_millis(0),
        voltage: from.voltage,
        psu: None,
        chains: [Some(ChainReadings {
            chips: u16::from(BMM101_HASHBOARD.num_chips),
            frequency: from.frequency,
            hottest: None,
            hashrate: 0,
        })],
    };
    let mut steps = heapless::Vec::new();
    loop {
        let plan = tuner.step(target, &readings, &());
        if plan.rail.is_none() && plan.frequencies[0].is_none() {
            return steps;
        }
        if let Some(next) = plan.rail {
            readings.voltage = next;
        }
        let chain = readings.chains[0].as_mut().expect("present chain");
        if let Some(next) = plan.frequencies[0] {
            chain.frequency = next;
        }
        steps
            .push((chain.frequency.khz(), readings.voltage.mv()))
            .expect("ramp must converge");
        readings.now += plan.delay;
    }
}

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
    let from = FixedTarget {
        frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
        voltage: BMM101_HASHBOARD.max_rail_voltage(),
    };
    assert!(walk_steps(from, from).is_empty());
}

#[test]
fn walk_emits_target_as_final_step() {
    let target = FixedTarget {
        frequency: Frequency::from_mhz(550),
        voltage: Voltage::from_mv(1100),
    };
    let steps = walk_steps(
        FixedTarget {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
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
    let target = FixedTarget {
        frequency: Frequency::from_mhz(550),
        voltage: Voltage::from_mv(1012),
    };
    let steps = walk_steps(
        FixedTarget {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
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
    let target = FixedTarget {
        frequency: Frequency::from_mhz(550),
        voltage: Voltage::from_mv(1100),
    };
    let v_target = target.voltage.as_volts();
    let p_target = v_target * v_target * target.frequency.as_mhz();
    let steps = walk_steps(
        FixedTarget {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
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
    let target = FixedTarget {
        frequency: Frequency::from_khz(500_000),
        voltage: BMM101_HASHBOARD.max_rail_voltage(),
    };
    let steps = walk_steps(
        FixedTarget {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
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
    let target = FixedTarget {
        frequency: Frequency::from_khz(551_000),
        voltage: Voltage::from_mv(1100),
    };
    let steps = walk_steps(
        FixedTarget {
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            voltage: BMM101_HASHBOARD.max_rail_voltage(),
        },
        target,
    );
    let (last_frequency, _) = *steps.last().expect("BUG: walk must emit at least one step");
    assert_eq!(
        last_frequency, 550_000,
        "off-grid target must floor onto the 6.25 MHz grid"
    );
}

#[test]
fn absent_chain_produces_no_commands() {
    let mut tuner = FixedTuner::new(&BMM101_HASHBOARD);
    let target = FixedTarget {
        frequency: Frequency::from_mhz(550),
        voltage: Voltage::from_mv(1100),
    };
    let readings = Readings {
        now: Instant::from_millis(0),
        voltage: BMM101_HASHBOARD.max_rail_voltage(),
        psu: None,
        chains: [None],
    };
    let plan = tuner.step(target, &readings, &());
    assert_eq!(plan.frequencies, [None]);
    assert_eq!(plan.rail, None);
}

#[test]
fn a_new_target_restarts_a_completed_policy() {
    let board = &BMM101_HASHBOARD;
    let mut tuner = FixedTuner::new(board);
    let voltage = board.max_rail_voltage();
    let safe = board.chip.frequency_range.min();
    let target = FixedTarget {
        frequency: safe,
        voltage,
    };
    let readings = Readings {
        now: Instant::from_millis(0),
        voltage,
        psu: None,
        chains: [Some(ChainReadings {
            chips: u16::from(board.num_chips),
            frequency: safe,
            hottest: None,
            hashrate: 0,
        })],
    };
    let plan = tuner.step(target, &readings, &());
    assert_eq!(plan.frequencies, [None]);
    assert_eq!(plan.rail, None);
    let target = FixedTarget {
        frequency: Frequency::from_khz(safe.khz() + board.chip.frequency_step.khz()),
        voltage,
    };
    let plan = tuner.step(target, &readings, &());
    assert_eq!(plan.frequencies, [Some(target.frequency)]);
    assert_eq!(plan.rail, None);
    assert!(matches!(
        tuner.progress.as_ref().expect("progress after step").phase,
        super::Phase::Curve
    ));
}

#[test]
fn applied_voltage_takes_precedence_over_psu_samples() {
    let board = &BMM101_HASHBOARD;
    let target = FixedTarget {
        frequency: board.chip.frequency_range.min(),
        voltage: board.max_rail_voltage(),
    };
    let readings = Readings {
        now: Instant::from_millis(1000),
        voltage: target.voltage,
        psu: Some(ubos_psu::PsuReading {
            input_power: ubos_core::units::Power::from_mw(0),
            output_power: ubos_core::units::Power::from_mw(0),
            output_voltage_set: Voltage::from_mv(1200),
            output_voltage_measured: Voltage::from_mv(1000),
            timestamp_ms: 0,
            sensor_fault: false,
        }),
        chains: [Some(ChainReadings {
            chips: u16::from(board.num_chips),
            frequency: target.frequency,
            hottest: None,
            hashrate: 0,
        })],
    };
    let mut tuner = FixedTuner::new(board);
    let plan = tuner.step(target, &readings, &());
    assert_eq!(plan.frequencies, [None]);
    assert_eq!(plan.rail, None);
}

#[test]
fn a_new_target_replaces_an_unfinished_transition() {
    let board = &BMM101_HASHBOARD;
    let mut tuner = FixedTuner::new(board);
    let original = FixedTarget {
        frequency: board.chip.frequency_range.min(),
        voltage: board.max_rail_voltage(),
    };
    let mut readings = Readings {
        now: Instant::from_millis(0),
        voltage: original.voltage,
        psu: None,
        chains: [Some(ChainReadings {
            chips: u16::from(board.num_chips),
            frequency: original.frequency,
            hottest: None,
            hashrate: 0,
        })],
    };
    let target = FixedTarget {
        frequency: Frequency::from_mhz(550),
        ..original
    };
    let first = tuner.step(target, &readings, &());
    readings.chains[0]
        .as_mut()
        .expect("present chain")
        .frequency = first.frequencies[0].expect("first frequency step");
    readings.now += first.delay;
    let plan = tuner.step(original, &readings, &());
    assert_eq!(plan.frequencies, [Some(original.frequency)]);
    assert_eq!(plan.rail, None);
}

#[test]
fn unequal_chains_converge_without_exceeding_the_shared_rail_power_ceiling() {
    const STEPS: usize = 256;
    let board = &BMM101_HASHBOARD;
    let target = FixedTarget {
        frequency: Frequency::from_mhz(550),
        voltage: Voltage::from_mv(1100),
    };
    let chain = |frequency| {
        Some(ChainReadings {
            chips: u16::from(board.num_chips),
            frequency,
            hottest: None,
            hashrate: 0,
        })
    };
    let mut readings = Readings {
        now: Instant::from_millis(0),
        voltage: board.max_rail_voltage(),
        psu: None,
        chains: [
            chain(Frequency::from_mhz(50)),
            None,
            chain(Frequency::from_mhz(100)),
        ],
    };
    let mut tuner = FixedTuner::new(board);
    let target_power = target.voltage.as_volts().powi(2) * target.frequency.as_mhz();
    for _ in 0..STEPS {
        let plan = tuner.step(target, &readings, &());
        assert_eq!(plan.frequencies[1], None);
        if let Some(voltage) = plan.rail {
            readings.voltage = voltage;
        }
        for (chain, next) in readings.chains.iter_mut().zip(plan.frequencies) {
            if let (Some(chain), Some(frequency)) = (chain, next) {
                chain.frequency = frequency;
            }
        }
        if readings.voltage < board.max_rail_voltage() {
            for chain in readings.chains.iter().flatten() {
                let power = readings.voltage.as_volts().powi(2) * chain.frequency.as_mhz();
                assert!(power <= target_power * 1.01);
            }
        }
        if plan.rail.is_none() && plan.frequencies.iter().all(Option::is_none) {
            assert_eq!(readings.voltage, target.voltage);
            assert!(
                readings
                    .chains
                    .iter()
                    .flatten()
                    .all(|chain| chain.frequency == target.frequency)
            );
            return;
        }
        readings.now += plan.delay;
    }
    panic!("ramp did not converge");
}
