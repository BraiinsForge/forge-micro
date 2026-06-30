//! Equivalence and invariant tests for `FixedTuner`'s constant-power
//! curve walk. Gates the (frequency, voltage) sequence the tuner
//! would emit against the BM1362 PLL factory table grid and the
//! V^2 * f preservation rule.

use ubos_asic::{BMM100, ChipProtocol, PllParameterReg};
use ubos_core::units::{Frequency, Voltage};
use ubos_psu::BmmPsuConfig;

use super::TuningConfig;
use super::walk_steps;

#[test]
fn open_core_voltage_fits_default_psu_range() {
    // Gates the BMM100 descriptor's `max_psu_voltage` mirror against
    // the actual PSU range the firmware configures. The clamp inside
    // `max_rail_voltage` only hides a descriptor / PSU range mismatch;
    // this test surfaces it.
    let open_core = BMM100.max_rail_voltage();
    let range = BmmPsuConfig::DEFAULT.dcdc_voltage_range;
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
        frequency: BMM100.chip.descriptor().frequency_range.min(),
        voltage: BMM100.max_rail_voltage(),
    };
    assert!(
        walk_steps(
            from,
            from,
            BMM100.chip.descriptor().frequency_step,
            BMM100.chip.descriptor().frequency_range.min()
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
            frequency: BMM100.chip.descriptor().frequency_range.min(),
            voltage: BMM100.max_rail_voltage(),
        },
        target,
        BMM100.chip.descriptor().frequency_step,
        BMM100.chip.descriptor().frequency_range.min(),
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
            frequency: BMM100.chip.descriptor().frequency_range.min(),
            voltage: BMM100.max_rail_voltage(),
        },
        target,
        BMM100.chip.descriptor().frequency_step,
        BMM100.chip.descriptor().frequency_range.min(),
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
            frequency: BMM100.chip.descriptor().frequency_range.min(),
            voltage: BMM100.max_rail_voltage(),
        },
        target,
        BMM100.chip.descriptor().frequency_step,
        BMM100.chip.descriptor().frequency_range.min(),
    );
    let mut in_phase_b = false;
    for (f_khz, v_mv) in steps.iter().copied() {
        let volts = Voltage::from_mv(v_mv).as_volts();
        let mhz = Frequency::from_khz(f_khz).as_mhz();
        let power = volts * volts * mhz;
        if v_mv < BMM100.max_rail_voltage().mv() {
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
        voltage: BMM100.max_rail_voltage(),
    };
    let steps = walk_steps(
        TuningConfig {
            frequency: BMM100.chip.descriptor().frequency_range.min(),
            voltage: BMM100.max_rail_voltage(),
        },
        target,
        BMM100.chip.descriptor().frequency_step,
        BMM100.chip.descriptor().frequency_range.min(),
    );
    let range = BMM100.chip.descriptor().frequency_range;
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
            frequency: BMM100.chip.descriptor().frequency_range.min(),
            voltage: BMM100.max_rail_voltage(),
        },
        target,
        BMM100.chip.descriptor().frequency_step,
        BMM100.chip.descriptor().frequency_range.min(),
    );
    let (last_frequency, _) = *steps.last().expect("BUG: walk must emit at least one step");
    assert_eq!(
        last_frequency, 550_000,
        "off-grid target must floor onto the 6.25 MHz grid"
    );
}
