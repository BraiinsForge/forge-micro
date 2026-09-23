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

//! Behaviour tests for `PowerTuner` over synthetic readings and PSU samples.

use embassy_time::Instant;
use ubos_asic::BMM101_HASHBOARD;
use ubos_core::units::{Frequency, Power, Voltage};
use ubos_psu::PsuReading;
use ubos_tuner::{ChainReadings, Plan, Readings, Tuner};

use super::PowerTuner;

const TARGET: Power = Power::from_mw(40_000);

/// One present chain at safe boot, before the first PSU sample.
fn safe_boot() -> Readings<1> {
    Readings {
        now: Instant::from_millis(0),
        voltage: BMM101_HASHBOARD.max_rail_voltage(),
        psu: None,
        chains: [Some(ChainReadings {
            chips: u16::from(BMM101_HASHBOARD.num_chips),
            frequency: BMM101_HASHBOARD.chip.frequency_range.min(),
            hottest: None,
            hashrate: 0,
        })],
    }
}

fn frequency(readings: &Readings<1>) -> Frequency {
    readings.chains[0].expect("present chain").frequency
}

/// Advance the clock by the plan delay, apply the plan, and publish a
/// healthy sample of `power` taken after it.
fn apply(readings: &mut Readings<1>, plan: &Plan<1>, power: u32) {
    readings.now += plan.delay;
    if let Some(rail) = plan.rail {
        readings.voltage = rail;
    }
    if let (Some(chain), Some(next)) = (readings.chains[0].as_mut(), plan.frequencies[0]) {
        chain.frequency = next;
    }
    readings.psu = Some(PsuReading {
        input_power: Power::from_mw(power),
        output_power: Power::from_mw(power),
        output_voltage_set: readings.voltage,
        output_voltage_measured: readings.voltage,
        timestamp_ms: readings.now.as_millis(),
        sensor_fault: false,
    });
}

fn is_empty(plan: &Plan<1>) -> bool {
    plan.rail.is_none() && plan.frequencies[0].is_none()
}

#[test]
fn zero_target_stays_at_safe_boot() {
    let mut tuner = PowerTuner::new(&BMM101_HASHBOARD);
    let mut readings = safe_boot();
    let zero = Power::from_mw(0);
    for _ in 0..4 {
        let plan = tuner.step(zero, &readings, &());
        assert!(is_empty(&plan));
        apply(&mut readings, &plan, 1_000);
    }
}

#[test]
fn ramp_waits_for_a_sample_taken_after_the_last_command() {
    let mut tuner = PowerTuner::new(&BMM101_HASHBOARD);
    let mut readings = safe_boot();
    let step = BMM101_HASHBOARD.chip.frequency_step;
    let start = frequency(&readings);
    // The first step enters the ramp, and the second one acts on a sample.
    let plan = tuner.step(TARGET, &readings, &());
    apply(&mut readings, &plan, 10_000);
    let plan = tuner.step(TARGET, &readings, &());
    assert_eq!(
        plan.frequencies,
        [Some(start.step_toward(Frequency::from_mhz(1000), step))]
    );
    assert_eq!(plan.rail, None);
    // The miner takes a while to apply the plan, then waits out its delay.
    // A sample taken while the plan was still being applied does not advance
    // the ramp.
    let commanded = readings.now;
    let applying = embassy_time::Duration::from_millis(100);
    readings.now += applying + plan.delay;
    readings.psu.as_mut().expect("sample").timestamp_ms = (commanded + applying / 2).as_millis();
    readings.chains[0]
        .as_mut()
        .expect("present chain")
        .frequency = plan.frequencies[0].expect("raised clock");
    assert!(is_empty(&tuner.step(TARGET, &readings, &())));
}

#[test]
fn ramp_lowers_the_rail_after_reaching_the_target_and_stops_at_the_limit() {
    let mut tuner = PowerTuner::new(&BMM101_HASHBOARD);
    let mut readings = safe_boot();
    let mut power = 10_000;
    let limit = BMM101_HASHBOARD.chip.frequency_range.max();
    let mut plan = tuner.step(TARGET, &readings, &());
    for _ in 0..1000 {
        apply(&mut readings, &plan, power);
        // Power follows the clock until the target is reached, then holds.
        power = (power + 1_000).min(TARGET.mw());
        plan = tuner.step(TARGET, &readings, &());
        if is_empty(&plan) && frequency(&readings) == limit {
            break;
        }
    }
    assert_eq!(frequency(&readings), limit);
    assert!(readings.voltage < BMM101_HASHBOARD.max_rail_voltage());
    apply(&mut readings, &plan, power);
    assert!(is_empty(&tuner.step(TARGET, &readings, &())));
}

#[test]
fn excess_power_returns_to_safe_boot_and_ends_the_ramp() {
    let mut tuner = PowerTuner::new(&BMM101_HASHBOARD);
    let mut readings = safe_boot();
    let plan = tuner.step(TARGET, &readings, &());
    apply(&mut readings, &plan, 10_000);
    let plan = tuner.step(TARGET, &readings, &());
    apply(&mut readings, &plan, 10_000);
    let plan = tuner.step(TARGET, &readings, &());
    assert!(!is_empty(&plan));
    apply(&mut readings, &plan, 50_000);
    let mut plan = tuner.step(TARGET, &readings, &());
    while !is_empty(&plan) {
        apply(&mut readings, &plan, 50_000);
        plan = tuner.step(TARGET, &readings, &());
    }
    assert_eq!(readings.chains, safe_boot().chains);
    assert_eq!(readings.voltage, BMM101_HASHBOARD.max_rail_voltage());
    apply(&mut readings, &plan, 10_000);
    assert!(is_empty(&tuner.step(TARGET, &readings, &())));
}

#[test]
fn power_drop_after_the_target_restores_the_previous_point() {
    let mut tuner = PowerTuner::new(&BMM101_HASHBOARD);
    let mut readings = safe_boot();
    let plan = tuner.step(TARGET, &readings, &());
    apply(&mut readings, &plan, TARGET.mw());
    let plan = tuner.step(TARGET, &readings, &());
    let previous = (frequency(&readings), readings.voltage);
    apply(&mut readings, &plan, 30_000);
    let plan = tuner.step(TARGET, &readings, &());
    apply(&mut readings, &plan, 30_000);
    assert_eq!((frequency(&readings), readings.voltage), previous);
    assert!(is_empty(&tuner.step(TARGET, &readings, &())));
}

#[test]
fn stale_telemetry_returns_to_safe_boot() {
    let mut tuner = PowerTuner::new(&BMM101_HASHBOARD);
    let mut readings = safe_boot();
    let plan = tuner.step(TARGET, &readings, &());
    apply(&mut readings, &plan, 10_000);
    let plan = tuner.step(TARGET, &readings, &());
    apply(&mut readings, &plan, 10_000);
    readings.psu = None;
    readings.now += embassy_time::Duration::from_secs(61);
    let mut plan = tuner.step(TARGET, &readings, &());
    while !is_empty(&plan) {
        readings.now += plan.delay;
        if let Some(next) = plan.frequencies[0] {
            readings.chains[0]
                .as_mut()
                .expect("present chain")
                .frequency = next;
        }
        plan = tuner.step(TARGET, &readings, &());
    }
    assert_eq!(readings.chains, safe_boot().chains);
}

#[test]
fn a_new_target_restarts_from_safe_boot() {
    let mut tuner = PowerTuner::new(&BMM101_HASHBOARD);
    let mut readings = safe_boot();
    let plan = tuner.step(TARGET, &readings, &());
    apply(&mut readings, &plan, 10_000);
    let plan = tuner.step(TARGET, &readings, &());
    apply(&mut readings, &plan, 10_000);
    assert_ne!(readings.chains, safe_boot().chains);
    let lower = Power::from_mw(20_000);
    let plan = tuner.step(lower, &readings, &());
    assert_eq!(
        plan.frequencies,
        safe_boot().chains.map(|chain| chain.map(|c| c.frequency))
    );
    assert_eq!(plan.rail, None::<Voltage>);
}
