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

//! Tests for [`crate::ConfigUpdate::validate`].

use crate::tests_fixture::{BOUNDS, default_config};
use crate::{PidTarget, Source, Statistic, TemperatureDescriptor};
use ubos_core::units::{Efficiency, Frequency, Power, Temperature, Voltage};

use crate::{
    ApiConfig, Config, ConfigUpdate, CoolingConfig, CoolingUpdate, FanMode, FrequencyVoltageTarget,
    MAX_POOL_WEIGHT, MinerConfig, NAME_CAPACITY, NetworkConfig, NetworkMode, PoolEntry,
    PowerTarget, ProtocolKind, ThermalConfig, Tuning,
};

fn empty_update() -> ConfigUpdate {
    ConfigUpdate {
        pool: None,
        fee: None,
        fee_permille: None,
        thermal: None,
        cooling: None,
        miner: None,
        api: None,
        network: None,
    }
}

fn valid_pool_entry() -> PoolEntry {
    let mut host = heapless::String::new();
    host.push_str("pool.example.com")
        .expect("BUG: host fits in 128 bytes");
    let mut user = heapless::String::new();
    user.push_str("worker1")
        .expect("BUG: user fits in 128 bytes");
    PoolEntry {
        host,
        port: 3333,
        user,
        protocol: ProtocolKind::Sv1 { password: None },
        weight: 0,
    }
}

/// A PID target on the hottest hashboard reading, at `temp`.
fn hashboard_max(temp: Temperature) -> PidTarget {
    PidTarget {
        source: Source::Hashboard,
        statistic: Statistic::Max,
        temp,
    }
}

fn valid_thermal() -> ThermalConfig {
    ThermalConfig {
        sample_interval_ms: 1000,
        dangerous: TemperatureDescriptor {
            chip: None,
            hashboard: Some(Temperature::from_celsius(90)),
        },
        cooldown_temp: Temperature::from_celsius(45),
    }
}

/// Builds a valid [`Tuning`] using values inside the accepted range.
///
/// Frequency and voltage are derived from [`BOUNDS`] so the helper
/// remains valid regardless of which board is active at compile time.
fn valid_tuning() -> Tuning {
    Tuning::Fixed(FrequencyVoltageTarget {
        frequency: Frequency::from_khz(525_000),
        voltage: *BOUNDS.voltage.start(),
    })
}

/// Builds a valid [`MinerConfig`] with fixed tuning parameters.
fn valid_miner() -> MinerConfig {
    MinerConfig {
        tuning: valid_tuning(),
    }
}

fn fixed_target(tuning: &mut Tuning) -> &mut FrequencyVoltageTarget {
    let Tuning::Fixed(target) = tuning else {
        panic!("BUG: test expected fixed tuning");
    };
    target
}

fn valid_api() -> ApiConfig {
    let mut username = heapless::String::new();
    username.push_str("admin").expect("BUG: username fits");
    let mut password = heapless::String::new();
    password.push_str("secret1").expect("BUG: password fits");
    ApiConfig { username, password }
}

// --- Pool entry ---

#[test]
fn pool_valid_entry_passes() {
    let mut update = empty_update();
    let mut pool = heapless::Vec::new();
    pool.push(valid_pool_entry())
        .expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

#[test]
fn pool_empty_host_rejected() {
    let mut update = empty_update();
    let mut entry = valid_pool_entry();
    entry.host = heapless::String::new();
    let mut pool = heapless::Vec::new();
    pool.push(entry).expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("pool 1: host must not be empty")
    );
}

#[test]
fn pool_zero_port_rejected() {
    let mut update = empty_update();
    let mut entry = valid_pool_entry();
    entry.port = 0;
    let mut pool = heapless::Vec::new();
    pool.push(entry).expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("pool 1: port must be greater than 0")
    );
}

#[test]
fn pool_empty_host_slot_2_reports_index() {
    let mut update = empty_update();
    let mut bad = valid_pool_entry();
    bad.host = heapless::String::new();
    let mut pool = heapless::Vec::new();
    pool.push(valid_pool_entry())
        .expect("BUG: pool has capacity");
    pool.push(bad).expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("pool 2: host must not be empty")
    );
}

#[test]
fn pool_entry_weightless_json_defaults_to_zero() {
    let json = r#"{"host":"pool.example.com","port":3333,"user":"worker1","protocol":{"Sv1":{"password":null}}}"#;
    let (entry, _) =
        serde_json_core::de::from_str::<PoolEntry>(json).expect("deserialize succeeds");
    assert_eq!(entry.weight, 0);
}

#[test]
fn pool_weight_at_max_passes_over_max_rejected() {
    let mut update = empty_update();
    let mut entry = valid_pool_entry();
    entry.weight = MAX_POOL_WEIGHT;
    let mut pool = heapless::Vec::new();
    pool.push(entry).expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());

    let mut update = empty_update();
    let good = valid_pool_entry();
    let mut bad = valid_pool_entry();
    bad.weight = MAX_POOL_WEIGHT + 1;
    let mut pool = heapless::Vec::new();
    pool.push(good).expect("BUG: pool has capacity");
    pool.push(bad).expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("pool 2: weight must be at most 100000")
    );
}

#[test]
fn config_update_json_round_trip_preserves_weight() {
    let mut entry = valid_pool_entry();
    entry.weight = 42;
    let mut pool = heapless::Vec::new();
    pool.push(entry).expect("BUG: pool has capacity");
    let update = ConfigUpdate {
        pool: Some(pool),
        ..empty_update()
    };

    let json = serde_json_core::ser::to_string::<_, 512>(&update).expect("serialize succeeds");
    let (decoded, _) =
        serde_json_core::de::from_str::<ConfigUpdate>(&json).expect("deserialize succeeds");

    assert_eq!(decoded.pool.expect("pool field round-trips")[0].weight, 42);
}

// The field is not parsed at all, so an entry this build could not hold,
// here a worker name past the capacity, does not refuse the body.
#[test]
fn a_posted_fee_entry_is_ignored() {
    use core::fmt::Write as _;
    let mut json: heapless::String<512> = heapless::String::new();
    write!(
        json,
        r#"{{"fee":{{"host":"fee.example.com","port":4444,"user":""#
    )
    .expect("fits");
    for _ in 0..=NAME_CAPACITY {
        json.push('w').expect("fits");
    }
    write!(
        json,
        r#"","protocol":{{"Sv2":{{"pool_pubkey":null}}}},"weight":0}},"fee_permille":25}}"#
    )
    .expect("fits");
    let (update, _) =
        serde_json_core::de::from_str::<ConfigUpdate>(&json).expect("the fee field is skipped");
    assert!(update.fee.is_none());
    assert!(update.fee_permille.is_none());
    let mut config = default_config();
    let before = config.pool.clone();
    assert_eq!(update.validate(&config, &BOUNDS), Ok(()));
    update.apply(&mut config);
    assert_eq!(config.pool, before);
}

// --- Thermal ---

#[test]
fn thermal_valid_passes() {
    let mut update = empty_update();
    update.thermal = Some(valid_thermal());
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

#[test]
fn thermal_sample_interval_too_low() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.sample_interval_ms = 99;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("thermal.sample_interval_ms must be between 100 and 60000")
    );
}

#[test]
fn thermal_sample_interval_too_high() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.sample_interval_ms = 60_001;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("thermal.sample_interval_ms must be between 100 and 60000")
    );
}

#[test]
fn thermal_dangerous_temp_too_low() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous = TemperatureDescriptor {
        chip: None,
        hashboard: Some(Temperature::from_millicelsius(49_900)),
    };
    thermal.cooldown_temp = Temperature::from_celsius(20);
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("thermal.dangerous_hashboard_temp must be between 50000 and 110000")
    );
}

#[test]
fn thermal_dangerous_temp_too_high() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous = TemperatureDescriptor {
        chip: None,
        hashboard: Some(Temperature::from_millicelsius(110_100)),
    };
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("thermal.dangerous_hashboard_temp must be between 50000 and 110000")
    );
}

#[test]
fn thermal_cooldown_temp_too_low() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.cooldown_temp = Temperature::from_millicelsius(19_900);
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("thermal.cooldown_temp must be between 20000 and 90000")
    );
}

#[test]
fn thermal_cooldown_temp_too_high() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.cooldown_temp = Temperature::from_millicelsius(90_100);
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("thermal.cooldown_temp must be between 20000 and 90000")
    );
}

#[test]
fn thermal_cooldown_not_less_than_dangerous_rejected() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.cooldown_temp = thermal
        .dangerous
        .max()
        .expect("BUG: the valid thermal config has a threshold");
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("thermal.cooldown_temp must be less than the highest dangerous temperature")
    );
}

/// The thresholds of a machine that reads both kinds: the BFMX4's.
fn both_kinds() -> TemperatureDescriptor {
    TemperatureDescriptor {
        chip: Some(Temperature::from_celsius(110)),
        hashboard: Some(Temperature::from_celsius(75)),
    }
}

// A thermal update that names other reading kinds than the machine is
// rejected, because the kinds belong to the machine.
#[test]
fn thermal_with_other_reading_kinds_rejected() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous = both_kinds();
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err(
            "thermal must set a dangerous temperature for each reading kind of the machine, and for no other kind"
        )
    );
}

#[test]
fn thermal_hashboard_above_chip_rejected() {
    let mut current = default_config();
    current.thermal.dangerous = both_kinds();
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous = TemperatureDescriptor {
        chip: Some(Temperature::from_celsius(90)),
        hashboard: Some(Temperature::from_celsius(95)),
    };
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&current, &BOUNDS),
        Err("thermal.dangerous_hashboard_temp must not exceed thermal.dangerous_chip_temp")
    );
}

#[test]
fn thermal_dangerous_chip_temp_too_high() {
    let mut current = default_config();
    current.thermal.dangerous = TemperatureDescriptor {
        chip: Some(Temperature::from_celsius(110)),
        hashboard: None,
    };
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous = TemperatureDescriptor {
        chip: Some(Temperature::from_millicelsius(115_100)),
        hashboard: None,
    };
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&current, &BOUNDS),
        Err("thermal.dangerous_chip_temp must be between 50000 and 115000")
    );
}

#[test]
fn thermal_json_carries_a_missing_kind_as_null() {
    let mut thermal = valid_thermal();
    thermal.dangerous = TemperatureDescriptor {
        chip: Some(Temperature::from_celsius(110)),
        hashboard: None,
    };
    let update = ConfigUpdate {
        thermal: Some(thermal.clone()),
        ..empty_update()
    };

    let json = serde_json_core::ser::to_string::<_, 512>(&update).expect("serialize succeeds");
    assert!(json.contains("\"dangerous_hashboard_temp\":null"));
    let (decoded, _) =
        serde_json_core::de::from_str::<ConfigUpdate>(&json).expect("deserialize succeeds");

    assert_eq!(decoded.thermal, Some(thermal));
}

#[test]
fn thermal_json_without_a_dangerous_temperature_rejected() {
    let json = r#"{"thermal":{"sample_interval_ms":1000,"cooldown_temp":45000}}"#;
    assert!(serde_json_core::de::from_str::<ConfigUpdate>(json).is_err());
}

// --- Cooling target cross-field rule ---

#[test]
fn cooling_target_below_dangerous_passes() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target: Some(hashboard_max(Temperature::from_celsius(70))),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

// On a machine that reads only its dies, a chip target is held below the
// chip threshold, so a target above the hashboard threshold of the
// default machine passes.
#[test]
fn cooling_target_on_a_chip_only_machine_uses_the_chip_threshold() {
    let mut current = default_config();
    current.thermal.dangerous = TemperatureDescriptor {
        chip: Some(Temperature::from_celsius(110)),
        hashboard: None,
    };
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target: Some(PidTarget {
            source: Source::Chip,
            statistic: Statistic::Max,
            temp: Temperature::from_celsius(84),
        }),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert!(update.validate(&current, &BOUNDS).is_ok());
}

#[test]
fn default_target_follows_the_boards_defaults() {
    let target = PidTarget {
        source: Source::Chip,
        statistic: Statistic::Mean,
        temp: Temperature::from_celsius(70),
    };
    let defaults = crate::HashboardDefaults {
        dangerous: TemperatureDescriptor {
            chip: Some(Temperature::from_celsius(108)),
            hashboard: None,
        },
        cooling_target: target,
        ..crate::tests_fixture::DEFAULTS
    };
    let config = Config::for_board(&defaults, crate::DefaultTuning::Fixed);
    assert_eq!(config.cooling.target, target);
    assert_eq!(config.thermal.dangerous, defaults.dangerous);
}

#[test]
fn cooling_target_equal_to_dangerous_rejected() {
    let mut current = default_config();
    current.thermal.dangerous = TemperatureDescriptor {
        chip: None,
        hashboard: Some(Temperature::from_celsius(80)),
    };
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target: Some(hashboard_max(Temperature::from_celsius(80))),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert_eq!(
        update.validate(&current, &BOUNDS),
        Err("cooling.target.temp must be less than the dangerous temperature of its kind")
    );
}

#[test]
fn cooling_target_cross_field_uses_incoming_dangerous() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous = TemperatureDescriptor {
        chip: None,
        hashboard: Some(Temperature::from_celsius(80)),
    };
    update.thermal = Some(thermal);
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target: Some(hashboard_max(Temperature::from_celsius(75))),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

#[test]
fn cooling_target_too_low() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target: Some(hashboard_max(Temperature::from_millicelsius(29_900))),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("cooling.target.temp must be between 30000 and 85000")
    );
}

#[test]
fn cooling_target_too_high() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target: Some(hashboard_max(Temperature::from_millicelsius(85_100))),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("cooling.target.temp must be between 30000 and 85000")
    );
}

// --- Miner ---

#[test]
fn miner_default_passes() {
    let mut update = empty_update();
    update.miner = Some(default_config().miner.clone());
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

#[test]
fn miner_roundtrip_via_from_config() {
    let config = default_config();
    let update = ConfigUpdate::from_config(&config);
    assert_eq!(update.miner, Some(config.miner));
}

#[test]
fn miner_fixed_json_shape_deserializes() {
    let json = r#"{"miner":{"tuning":{"Fixed":{"frequency_khz":525000,"voltage_mv":0}}}}"#;
    let (update, _) =
        serde_json_core::de::from_str::<ConfigUpdate>(json).expect("deserialize succeeds");
    assert_eq!(update.miner, Some(valid_miner()));
}

#[test]
fn miner_power_json_shape_deserializes() {
    let json =
        r#"{"miner":{"tuning":{"Power":{"input_power_mw":44000,"efficiency_mj_per_th":40000}}}}"#;
    let (update, _) =
        serde_json_core::de::from_str::<ConfigUpdate>(json).expect("deserialize succeeds");
    assert_eq!(
        update.miner,
        Some(MinerConfig {
            tuning: Tuning::Power(PowerTarget {
                input_power: Power::from_mw(44_000),
                efficiency: Efficiency::from_mj_per_th(
                    core::num::NonZeroU32::new(40_000).expect("nonzero")
                ),
            }),
        })
    );
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

// The power target serializes as the two bare integers it held before
// it carried the unit, in the JSON the API exchanges and in the bytes
// the config stores.
#[test]
fn power_target_encodes_as_bare_integers() {
    let target = PowerTarget {
        input_power: Power::from_mw(44_000),
        efficiency: Efficiency::from_mj_per_th(
            core::num::NonZeroU32::new(40_000).expect("nonzero"),
        ),
    };
    let mut json = [0u8; 64];
    let length = serde_json_core::ser::to_slice(&target, &mut json).expect("the target fits");
    assert_eq!(
        &json[..length],
        br#"{"input_power_mw":44000,"efficiency_mj_per_th":40000}"#
    );
    let mut stored = [0u8; 16];
    let mut bare = [0u8; 16];
    let stored = postcard::to_slice(&target, &mut stored).expect("the target fits");
    let bare = postcard::to_slice(&(44_000u32, 40_000u32), &mut bare).expect("the pair fits");
    assert_eq!(stored, bare);
}

// A zero efficiency would leave the tuner no frequency to aim for.
#[test]
fn miner_power_zero_efficiency_is_rejected() {
    let json =
        r#"{"miner":{"tuning":{"Power":{"input_power_mw":44000,"efficiency_mj_per_th":0}}}}"#;
    assert!(serde_json_core::de::from_str::<ConfigUpdate>(json).is_err());
}

#[test]
fn miner_apply_updates_field() {
    let mut config = default_config();
    let new_miner = default_config().miner;
    let update = ConfigUpdate {
        miner: Some(new_miner.clone()),
        ..empty_update()
    };
    update.apply(&mut config);
    assert_eq!(config.miner, new_miner);
}

// --- Tuning ---

#[test]
fn tuning_valid_passes() {
    let mut update = empty_update();
    update.miner = Some(valid_miner());
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

#[test]
fn tuning_not_offered_is_rejected() {
    use crate::tests_fixture::FIXED_ONLY_BOUNDS as FIXED_ONLY;
    let mut update = empty_update();
    update.miner = Some(MinerConfig {
        tuning: Tuning::Power(PowerTarget {
            input_power: Power::from_mw(44_000),
            efficiency: Efficiency::from_mj_per_th(
                core::num::NonZeroU32::new(40_000).expect("nonzero"),
            ),
        }),
    });
    assert_eq!(
        update.validate(&default_config(), &FIXED_ONLY),
        Err("miner.tuning selects a tuner this machine does not offer")
    );
}

fn power_tuning(input_power_mw: u32) -> Tuning {
    Tuning::Power(PowerTarget {
        input_power: Power::from_mw(input_power_mw),
        efficiency: Efficiency::from_mj_per_th(
            core::num::NonZeroU32::new(40_000).expect("nonzero"),
        ),
    })
}

#[test]
fn tuning_input_power_range() {
    let range = BOUNDS.input_power.clone().expect("a power tuner is listed");
    let (start, end) = (range.start().mw(), range.end().mw());
    for input_power_mw in [start, end] {
        assert_eq!(power_tuning(input_power_mw).validate(&BOUNDS), Ok(()));
    }
    for input_power_mw in [0, start - 1, end + 1] {
        assert_eq!(
            power_tuning(input_power_mw).validate(&BOUNDS),
            Err("miner.tuning.Power.input_power_mw outside the accepted range")
        );
    }
}

#[test]
fn tuning_clamped_into_bounds() {
    let frequency = BOUNDS.frequency.clone();
    let voltage = BOUNDS.voltage.clone();
    let power = BOUNDS.input_power.clone().expect("a power tuner is listed");
    let fixed = |frequency_khz, voltage_mv| {
        Tuning::Fixed(FrequencyVoltageTarget {
            frequency: Frequency::from_khz(frequency_khz),
            voltage: Voltage::from_mv(voltage_mv),
        })
    };
    let high = fixed(frequency.end().khz() + 1, voltage.end().mv() + 1);
    assert_eq!(
        high.clamped(&BOUNDS),
        fixed(frequency.end().khz(), voltage.end().mv())
    );
    let low = fixed(frequency.start().khz() - 1, 1_200);
    assert_eq!(low.clamped(&BOUNDS), fixed(frequency.start().khz(), 1_200));
    assert_eq!(
        power_tuning(power.end().mw() + 1).clamped(&BOUNDS),
        power_tuning(power.end().mw())
    );
    assert_eq!(
        power_tuning(0).clamped(&BOUNDS),
        power_tuning(power.start().mw())
    );
}

#[test]
fn tuning_frequency_out_of_range() {
    let mut update = empty_update();
    let mut miner = valid_miner();
    let target = fixed_target(&mut miner.tuning);
    target.frequency = Frequency::from_khz(1_000_001);
    update.miner = Some(miner);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("miner.tuning.Fixed.frequency_khz outside the accepted range")
    );
}

#[test]
fn tuning_voltage_out_of_range() {
    let mut update = empty_update();
    let mut miner = valid_miner();
    let target = fixed_target(&mut miner.tuning);
    target.voltage = Voltage::from_mv(BOUNDS.voltage.end().mv() + 1);
    update.miner = Some(miner);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("miner.tuning.Fixed.voltage_mv outside the accepted range")
    );
}

#[test]
fn thermal_only_update_rejected_when_current_target_above_new_dangerous() {
    let mut current = default_config();
    current.cooling.target.temp = Temperature::from_celsius(80);
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous = TemperatureDescriptor {
        chip: None,
        hashboard: Some(Temperature::from_celsius(70)),
    };
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&current, &BOUNDS),
        Err(
            "current cooling.target.temp must remain less than the dangerous temperature of its kind"
        )
    );
}

#[test]
fn tuning_frequency_in_range_non_discrete_passes() {
    let mut update = empty_update();
    let mut miner = valid_miner();
    let target = fixed_target(&mut miner.tuning);
    target.frequency = Frequency::from_khz(487_500);
    update.miner = Some(miner);
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

#[test]
fn tuning_voltage_in_range_non_discrete_passes() {
    let mut update = empty_update();
    let mut miner = valid_miner();
    let target = fixed_target(&mut miner.tuning);
    target.voltage = Voltage::from_mv(BOUNDS.voltage.start().mv() + 11);
    update.miner = Some(miner);
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

// --- Cooling fan PWM floor ---

#[test]
fn cooling_min_fan_pwm_100_passes() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target: None,
        fixed_fan_pwm: None,
        min_fan_pwm: Some(100),
    });
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

#[test]
fn cooling_min_fan_pwm_101_rejected() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target: None,
        fixed_fan_pwm: None,
        min_fan_pwm: Some(101),
    });
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("cooling.min_fan_pwm must be between 0 and 100")
    );
}

// --- Cooling fixed fan duty ---

#[test]
fn cooling_fixed_fan_pwm_range_ends_pass() {
    for duty in [0, 100] {
        let mut update = empty_update();
        update.cooling = Some(CoolingUpdate {
            mode: Some(FanMode::Fixed),
            target: None,
            fixed_fan_pwm: Some(duty),
            min_fan_pwm: None,
        });
        assert!(
            update.validate(&default_config(), &BOUNDS).is_ok(),
            "duty {duty} must be accepted"
        );
    }
}

#[test]
fn cooling_fixed_fan_pwm_101_rejected() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target: None,
        fixed_fan_pwm: Some(101),
        min_fan_pwm: None,
    });
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("cooling.fixed_fan_pwm must be between 0 and 100")
    );
}

#[test]
fn cooling_fixed_fan_pwm_below_floor_passes() {
    let current = default_config();
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: Some(FanMode::Fixed),
        target: None,
        fixed_fan_pwm: Some(current.cooling.pid.min_fan_pwm - 1),
        min_fan_pwm: None,
    });
    assert!(update.validate(&current, &BOUNDS).is_ok());
}

#[test]
fn cooling_mode_json_round_trip() {
    let update = ConfigUpdate {
        cooling: Some(CoolingUpdate {
            mode: Some(FanMode::Fixed),
            target: None,
            fixed_fan_pwm: Some(70),
            min_fan_pwm: None,
        }),
        ..empty_update()
    };

    let json = serde_json_core::ser::to_string::<_, 128>(&update).expect("serialize succeeds");
    assert!(
        json.contains("\"mode\":\"fixed\""),
        "mode must serialize as the snake_case variant name, got {json}"
    );
    let (decoded, _) =
        serde_json_core::de::from_str::<ConfigUpdate>(&json).expect("deserialize succeeds");
    assert_eq!(decoded.cooling, update.cooling);
}

#[test]
fn cooling_mode_and_duty_apply_and_roundtrip() {
    let mut config = default_config();
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: Some(FanMode::Fixed),
        target: None,
        fixed_fan_pwm: Some(35),
        min_fan_pwm: None,
    });
    update.apply(&mut config);
    assert_eq!(
        config.cooling,
        CoolingConfig {
            mode: FanMode::Fixed,
            fixed_fan_pwm: 35,
            ..default_config().cooling
        }
    );

    assert_eq!(
        ConfigUpdate::from_config(&config).cooling,
        Some(CoolingUpdate {
            mode: Some(FanMode::Fixed),
            target: Some(config.cooling.target),
            fixed_fan_pwm: Some(35),
            min_fan_pwm: Some(config.cooling.pid.min_fan_pwm),
        })
    );
}

#[test]
fn cooling_target_json_applies() {
    let json = r#"{"cooling":{"target":{"source":"chip","statistic":"mean","temp":70000}}}"#;
    let (update, _) =
        serde_json_core::de::from_str::<ConfigUpdate>(json).expect("deserialize succeeds");
    let mut config = default_config();
    update.apply(&mut config);
    assert_eq!(
        config.cooling.target,
        PidTarget {
            source: Source::Chip,
            statistic: Statistic::Mean,
            temp: Temperature::from_celsius(70),
        }
    );
}

/// The cooling update of a chip target at `temp`.
fn chip_target(temp: Temperature) -> CoolingUpdate {
    CoolingUpdate {
        mode: None,
        target: Some(PidTarget {
            source: Source::Chip,
            statistic: Statistic::Max,
            temp,
        }),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    }
}

// On a machine that reads both kinds, a chip target is held below the
// chip threshold and not the hashboard one, so 80 °C passes under a
// 75 °C hashboard halt.
#[test]
fn cooling_chip_target_above_the_hashboard_threshold_passes() {
    let mut current = default_config();
    current.thermal.dangerous = both_kinds();
    let mut update = empty_update();
    update.cooling = Some(chip_target(Temperature::from_celsius(80)));
    assert!(update.validate(&current, &BOUNDS).is_ok());
}

#[test]
fn cooling_chip_target_at_the_chip_threshold_rejected() {
    let mut current = default_config();
    current.thermal.dangerous = TemperatureDescriptor {
        chip: Some(Temperature::from_celsius(80)),
        hashboard: Some(Temperature::from_celsius(75)),
    };
    let mut update = empty_update();
    update.cooling = Some(chip_target(Temperature::from_celsius(80)));
    assert_eq!(
        update.validate(&current, &BOUNDS),
        Err("cooling.target.temp must be less than the dangerous temperature of its kind")
    );
}

#[test]
fn cooling_target_of_a_kind_the_machine_does_not_read_rejected() {
    let mut current = default_config();
    current.thermal.dangerous = TemperatureDescriptor {
        chip: None,
        hashboard: Some(Temperature::from_celsius(83)),
    };
    let mut update = empty_update();
    update.cooling = Some(chip_target(Temperature::from_celsius(70)));
    assert_eq!(
        update.validate(&current, &BOUNDS),
        Err("cooling.target.source must be a kind of reading that the machine measures")
    );
}

// --- API credentials ---

#[test]
fn api_valid_passes() {
    let mut update = empty_update();
    update.api = Some(valid_api());
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

#[test]
fn api_empty_username_rejected() {
    let mut update = empty_update();
    let mut api = valid_api();
    api.username = heapless::String::new();
    update.api = Some(api);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("api.username must not be empty")
    );
}

#[test]
fn api_non_ascii_username_rejected() {
    let mut update = empty_update();
    let mut api = valid_api();
    api.username = heapless::String::new();
    api.username.push_str("admé").expect("BUG: username fits");
    update.api = Some(api);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("api.username must contain only printable ASCII characters")
    );
}

#[test]
fn api_non_ascii_password_rejected() {
    let mut update = empty_update();
    let mut api = valid_api();
    api.password = heapless::String::new();
    api.password
        .push_str("pass word")
        .expect("BUG: password fits");
    update.api = Some(api);
    assert_eq!(
        update.validate(&default_config(), &BOUNDS),
        Err("api.password must contain only printable ASCII characters")
    );
}

#[test]
fn api_empty_password_passes() {
    let mut update = empty_update();
    let mut api = valid_api();
    api.password = heapless::String::new();
    update.api = Some(api);
    assert!(update.validate(&default_config(), &BOUNDS).is_ok());
}

fn valid_network_static() -> NetworkConfig {
    let mut dns = heapless::Vec::new();
    dns.push([8, 8, 8, 8]).expect("BUG: dns has capacity");
    NetworkConfig {
        mode: NetworkMode::Static {
            address: [192, 168, 1, 10],
            prefix_len: 24,
            gateway: [192, 168, 1, 1],
            dns,
        },
        hostname: heapless::String::new(),
    }
}

// --- Network ---

#[test]
fn network_default_passes() {
    assert!(NetworkConfig::default().validate().is_ok());
}

#[test]
fn network_static_valid_passes() {
    assert!(valid_network_static().validate().is_ok());
}

#[test]
fn network_static_prefix_len_zero_rejected() {
    let mut network = valid_network_static();
    let NetworkMode::Static {
        ref mut prefix_len, ..
    } = network.mode
    else {
        panic!("expected Static");
    };
    *prefix_len = 0;
    assert_eq!(
        network.validate(),
        Err("network.prefix_len must be between 1 and 32")
    );
}

#[test]
fn network_static_prefix_len_33_rejected() {
    let mut network = valid_network_static();
    let NetworkMode::Static {
        ref mut prefix_len, ..
    } = network.mode
    else {
        panic!("expected Static");
    };
    *prefix_len = 33;
    assert_eq!(
        network.validate(),
        Err("network.prefix_len must be between 1 and 32")
    );
}

#[test]
fn network_static_zero_address_rejected() {
    let mut network = valid_network_static();
    let NetworkMode::Static {
        ref mut address, ..
    } = network.mode
    else {
        panic!("expected Static");
    };
    *address = [0, 0, 0, 0];
    assert_eq!(
        network.validate(),
        Err("network.address must not be 0.0.0.0")
    );
}

#[test]
fn network_static_zero_gateway_rejected() {
    let mut network = valid_network_static();
    let NetworkMode::Static {
        ref mut gateway, ..
    } = network.mode
    else {
        panic!("expected Static");
    };
    *gateway = [0, 0, 0, 0];
    assert_eq!(
        network.validate(),
        Err("network.gateway must not be 0.0.0.0")
    );
}

#[test]
fn network_hostname_space_rejected() {
    let mut network = NetworkConfig::default();
    network
        .hostname
        .push_str("my host")
        .expect("BUG: hostname fits");
    assert_eq!(
        network.validate(),
        Err("network.hostname must contain only letters, digits, and hyphens")
    );
}

#[test]
fn network_hostname_underscore_rejected() {
    let mut network = NetworkConfig::default();
    network
        .hostname
        .push_str("my_host")
        .expect("BUG: hostname fits");
    assert_eq!(
        network.validate(),
        Err("network.hostname must contain only letters, digits, and hyphens")
    );
}

#[test]
fn network_hostname_leading_hyphen_rejected() {
    let mut network = NetworkConfig::default();
    network
        .hostname
        .push_str("-myhost")
        .expect("BUG: hostname fits");
    assert_eq!(
        network.validate(),
        Err("network.hostname must not start or end with a hyphen")
    );
}

#[test]
fn network_hostname_trailing_hyphen_rejected() {
    let mut network = NetworkConfig::default();
    network
        .hostname
        .push_str("myhost-")
        .expect("BUG: hostname fits");
    assert_eq!(
        network.validate(),
        Err("network.hostname must not start or end with a hyphen")
    );
}

#[test]
fn network_hostname_empty_passes() {
    let network = NetworkConfig::default();
    assert!(network.validate().is_ok());
}

#[test]
fn network_hostname_valid_passes() {
    let mut network = NetworkConfig::default();
    network
        .hostname
        .push_str("miner-01")
        .expect("BUG: hostname fits");
    assert!(network.validate().is_ok());
}

// --- Config::validate ---

#[test]
fn config_default_validates() {
    assert!(default_config().validate(&BOUNDS).is_ok());
}

#[test]
fn config_cooling_target_above_dangerous_rejected() {
    let mut config = default_config();
    config.thermal.dangerous = TemperatureDescriptor {
        chip: None,
        hashboard: Some(Temperature::from_celsius(60)),
    };
    config.cooling.target.temp = Temperature::from_celsius(80);
    assert_eq!(
        config.validate(&BOUNDS),
        Err("cooling.target.temp must be less than the dangerous temperature of its kind")
    );
}

#[test]
fn config_cooling_min_fan_pwm_101_rejected() {
    let mut config = default_config();
    config.cooling.pid.min_fan_pwm = 101;
    assert_eq!(
        config.validate(&BOUNDS),
        Err("cooling.min_fan_pwm must be between 0 and 100")
    );
}

#[test]
fn config_cooling_fixed_fan_pwm_101_rejected() {
    let mut config = default_config();
    config.cooling.fixed_fan_pwm = 101;
    assert_eq!(
        config.validate(&BOUNDS),
        Err("cooling.fixed_fan_pwm must be between 0 and 100")
    );
}

#[test]
fn mining_info_json_names_and_numbers() {
    use crate::tests_fixture::INFO;
    let mut buffer = [0u8; 1024];
    let length = serde_json_core::to_slice(&INFO, &mut buffer).expect("fits");
    let json = core::str::from_utf8(&buffer[..length]).expect("utf8");
    for expected in [
        r#""defaultFrequencyKhz":525000"#,
        r#""defaultVoltageMv":1400"#,
        r#""defaultTuning":"Fixed""#,
        r#""frequencyKhz":{"start":"#,
        r#""voltageMv":{"start":0,"#,
        r#""inputPowerMw":{"start":19000,"end":67000}"#,
    ] {
        assert!(json.contains(expected), "{expected} missing from {json}");
    }
    let power = crate::DefaultTuning::Power {
        input_power: Power::from_mw(44_000),
    };
    let length = serde_json_core::to_slice(&power, &mut buffer).expect("fits");
    assert_eq!(&buffer[..length], br#"{"Power":{"inputPowerMw":44000}}"#);
}
