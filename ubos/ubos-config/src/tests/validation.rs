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

use crate::{
    ApiConfig, Config, ConfigUpdate, CoolingConfig, CoolingUpdate, FanMode, FrequencyVoltageTarget,
    MAX_POOL_WEIGHT, MINING_BOUNDS, MinerConfig, NetworkConfig, NetworkMode, PoolEntry,
    PowerTarget, ProtocolKind, ThermalConfig, Tuning,
};

/// Builds a minimal valid [`ConfigUpdate`] with no fields set.
fn empty_update() -> ConfigUpdate {
    ConfigUpdate {
        pool: None,
        thermal: None,
        cooling: None,
        miner: None,
        api: None,
        network: None,
    }
}

/// Builds a pool entry with valid host and port.
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

/// Builds a valid [`ThermalConfig`].
fn valid_thermal() -> ThermalConfig {
    ThermalConfig {
        sample_interval_ms: 1000,
        dangerous_temp_c: 90.0,
        dangerous_chip_temp_c: 110.0,
        cooldown_temp_c: 45.0,
    }
}

/// Builds a valid [`Tuning`] using values inside the accepted range.
///
/// Frequency and voltage are derived from [`MINING_BOUNDS`] so the helper
/// remains valid regardless of which board is active at compile time.
fn valid_tuning() -> Tuning {
    Tuning::Fixed(FrequencyVoltageTarget {
        frequency_mhz: 525,
        voltage_mv: *MINING_BOUNDS.voltage_mv.start(),
    })
}

/// Builds a valid [`MinerConfig`] with fixed tuning parameters.
fn valid_miner() -> MinerConfig {
    MinerConfig {
        tuning: valid_tuning(),
    }
}

/// Access the fixed target so validation tests can change one field.
fn fixed_target(tuning: &mut Tuning) -> &mut FrequencyVoltageTarget {
    let Tuning::Fixed(target) = tuning else {
        panic!("BUG: test expected fixed tuning");
    };
    target
}

/// Builds a valid [`ApiConfig`].
fn valid_api() -> ApiConfig {
    let mut username = heapless::String::new();
    username.push_str("admin").expect("BUG: username fits");
    let mut password = heapless::String::new();
    password.push_str("secret1").expect("BUG: password fits");
    ApiConfig { username, password }
}

// --- Pool entry ---

/// A valid pool entry passes validation.
#[test]
fn pool_valid_entry_passes() {
    let mut update = empty_update();
    let mut pool = heapless::Vec::new();
    pool.push(valid_pool_entry())
        .expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// A pool entry with an empty host is rejected.
#[test]
fn pool_empty_host_rejected() {
    let mut update = empty_update();
    let mut entry = valid_pool_entry();
    entry.host = heapless::String::new();
    let mut pool = heapless::Vec::new();
    pool.push(entry).expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("pool 1: host must not be empty")
    );
}

/// A pool entry with port 0 is rejected.
#[test]
fn pool_zero_port_rejected() {
    let mut update = empty_update();
    let mut entry = valid_pool_entry();
    entry.port = 0;
    let mut pool = heapless::Vec::new();
    pool.push(entry).expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("pool 1: port must be greater than 0")
    );
}

/// A pool entry in slot 2 reports its slot index in the error message.
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
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("pool 2: host must not be empty")
    );
}

/// A JSON pool entry with no `weight` key deserializes with `weight == 0`,
/// so a weightless config keeps the primary-plus-backups behavior.
#[test]
fn pool_entry_weightless_json_defaults_to_zero() {
    let json = r#"{"host":"pool.example.com","port":3333,"user":"worker1","protocol":{"Sv1":{"password":null}}}"#;
    let (entry, _) =
        serde_json_core::de::from_str::<PoolEntry>(json).expect("deserialize succeeds");
    assert_eq!(entry.weight, 0);
}

/// `weight == MAX_POOL_WEIGHT` is accepted; `MAX_POOL_WEIGHT + 1` is
/// rejected with the slot-1 message, proving the table indexing.
#[test]
fn pool_weight_at_max_passes_over_max_rejected() {
    let mut update = empty_update();
    let mut entry = valid_pool_entry();
    entry.weight = MAX_POOL_WEIGHT;
    let mut pool = heapless::Vec::new();
    pool.push(entry).expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );

    let mut update = empty_update();
    let good = valid_pool_entry();
    let mut bad = valid_pool_entry();
    bad.weight = MAX_POOL_WEIGHT + 1;
    let mut pool = heapless::Vec::new();
    pool.push(good).expect("BUG: pool has capacity");
    pool.push(bad).expect("BUG: pool has capacity");
    update.pool = Some(pool);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("pool 2: weight must be at most 100000")
    );
}

/// A `ConfigUpdate` round-trip through the exact wire codec `ubos-api`
/// uses for `POST /api/config` preserves a nonzero pool weight.
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

// --- Thermal ---

/// Valid thermal config passes.
#[test]
fn thermal_valid_passes() {
    let mut update = empty_update();
    update.thermal = Some(valid_thermal());
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// `sample_interval_ms` below 100 is rejected.
#[test]
fn thermal_sample_interval_too_low() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.sample_interval_ms = 99;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("thermal.sample_interval_ms must be between 100 and 60000")
    );
}

/// `sample_interval_ms` above 60000 is rejected.
#[test]
fn thermal_sample_interval_too_high() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.sample_interval_ms = 60_001;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("thermal.sample_interval_ms must be between 100 and 60000")
    );
}

/// `dangerous_temp_c` below 50 is rejected.
#[test]
fn thermal_dangerous_temp_too_low() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous_temp_c = 49.9;
    thermal.cooldown_temp_c = 20.0;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("thermal.dangerous_temp_c must be between 50 and 110")
    );
}

/// `dangerous_temp_c` above 110 is rejected.
#[test]
fn thermal_dangerous_temp_too_high() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous_temp_c = 110.1;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("thermal.dangerous_temp_c must be between 50 and 110")
    );
}

/// NaN for `dangerous_temp_c` is rejected.
#[test]
fn thermal_dangerous_temp_nan_rejected() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous_temp_c = f32::NAN;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("thermal.dangerous_temp_c must be a finite number")
    );
}

/// `cooldown_temp_c` below 20 is rejected.
#[test]
fn thermal_cooldown_temp_too_low() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.cooldown_temp_c = 19.9;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("thermal.cooldown_temp_c must be between 20 and 90")
    );
}

/// `cooldown_temp_c` above 90 is rejected.
#[test]
fn thermal_cooldown_temp_too_high() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.cooldown_temp_c = 90.1;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("thermal.cooldown_temp_c must be between 20 and 90")
    );
}

/// NaN for `cooldown_temp_c` is rejected.
#[test]
fn thermal_cooldown_temp_nan_rejected() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.cooldown_temp_c = f32::NAN;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("thermal.cooldown_temp_c must be a finite number")
    );
}

/// `cooldown_temp_c` equal to `dangerous_temp_c` is rejected.
#[test]
fn thermal_cooldown_not_less_than_dangerous_rejected() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.cooldown_temp_c = thermal.dangerous_temp_c;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("thermal.cooldown_temp_c must be less than thermal.dangerous_temp_c")
    );
}

// --- Cooling target cross-field rule ---

/// `cooling.target_temp_c` below the dangerous limit passes.
///
/// Default current config has `dangerous_temp_c = 90.0`; 70.0 is below
/// that and within the 30..=85 range.
#[test]
fn cooling_target_below_dangerous_passes() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target_temp_c: Some(70.0),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// `cooling.target_temp_c` equal to `dangerous_temp_c` is rejected.
///
/// Build a current config with dangerous = 80.0 and set target = 80.0
/// so the cross-field rule fires.
#[test]
fn cooling_target_equal_to_dangerous_rejected() {
    let mut current = Config::default();
    current.thermal.dangerous_temp_c = 80.0;
    let mut update = empty_update();
    // 80.0 is within 30..=85 but equals the dangerous limit.
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target_temp_c: Some(80.0),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert_eq!(
        update.validate(&current, &crate::mining::BOUNDS),
        Err("cooling.target_temp_c must be less than dangerous_temp_c")
    );
}

/// When both thermal and `cooling.target_temp_c` are set, the cross-field
/// rule uses the incoming `dangerous_temp_c`, not the current config.
#[test]
fn cooling_target_cross_field_uses_incoming_dangerous() {
    // Current config has dangerous = 90.0; target of 75 with incoming
    // dangerous of 80 should pass because 75 < 80.
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous_temp_c = 80.0;
    update.thermal = Some(thermal);
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target_temp_c: Some(75.0),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// `cooling.target_temp_c` NaN is rejected.
#[test]
fn cooling_target_nan_rejected() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target_temp_c: Some(f32::NAN),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("cooling.target_temp_c must be a finite number")
    );
}

/// `cooling.target_temp_c` below 30 is rejected.
#[test]
fn cooling_target_too_low() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target_temp_c: Some(29.9),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("cooling.target_temp_c must be between 30 and 85")
    );
}

/// `cooling.target_temp_c` above 85 is rejected.
#[test]
fn cooling_target_too_high() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target_temp_c: Some(85.1),
        fixed_fan_pwm: None,
        min_fan_pwm: None,
    });
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("cooling.target_temp_c must be between 30 and 85")
    );
}

// --- Miner ---

/// The active board's default miner config passes validation. The type
/// default is `Fixed`, which a replay-only board rejects, so validate the
/// board-derived default instead.
#[test]
fn miner_default_passes() {
    let mut update = empty_update();
    update.miner = Some(Config::default().miner.clone());
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// `ConfigUpdate::from_config` includes the miner field.
#[test]
fn miner_roundtrip_via_from_config() {
    let config = Config::default();
    let update = ConfigUpdate::from_config(&config);
    assert_eq!(update.miner, Some(config.miner));
}

/// The mining form's JSON shape selects the fixed tuner and its target.
#[test]
fn miner_fixed_json_shape_deserializes() {
    let json = r#"{"miner":{"tuning":{"Fixed":{"frequency_mhz":525,"voltage_mv":0}}}}"#;
    let (update, _) =
        serde_json_core::de::from_str::<ConfigUpdate>(json).expect("deserialize succeeds");
    assert_eq!(update.miner, Some(valid_miner()));
}

/// The mining form's JSON shape selects the power tuner and its target.
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
                input_power_mw: 44_000,
                efficiency_mj_per_th: core::num::NonZeroU32::new(40_000).expect("nonzero"),
            }),
        })
    );
    assert!(update.validate(&Config::default(), &MINING_BOUNDS).is_ok());
}

/// A zero efficiency would leave the tuner no frequency to aim for, so
/// the update does not parse.
#[test]
fn miner_power_zero_efficiency_is_rejected() {
    let json =
        r#"{"miner":{"tuning":{"Power":{"input_power_mw":44000,"efficiency_mj_per_th":0}}}}"#;
    assert!(serde_json_core::de::from_str::<ConfigUpdate>(json).is_err());
}

/// Applying a `miner` update overwrites `Config::miner`.
#[test]
fn miner_apply_updates_field() {
    let mut config = Config::default();
    let new_miner = MinerConfig::default();
    let update = ConfigUpdate {
        miner: Some(new_miner.clone()),
        ..empty_update()
    };
    update.apply(&mut config);
    assert_eq!(config.miner, new_miner);
}

// --- Tuning ---

/// Valid frequency and voltage pass.
#[test]
fn tuning_valid_passes() {
    let mut update = empty_update();
    update.miner = Some(valid_miner());
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// Frequency outside the accepted range is rejected.
///
/// 1001 MHz is above the chip's validated ceiling of 1000 MHz.
#[test]
fn tuning_frequency_out_of_range() {
    let mut update = empty_update();
    let mut miner = valid_miner();
    let target = fixed_target(&mut miner.tuning);
    target.frequency_mhz = 1001;
    update.miner = Some(miner);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("miner.tuning.Fixed.frequency_mhz outside the accepted range")
    );
}

/// Voltage outside the accepted range is rejected.
///
/// There is no lower limit, so only the upper bound rejects: one above the
/// board's rail ceiling exercises the path without asserting a
/// board-specific numeric value.
#[test]
fn tuning_voltage_out_of_range() {
    let mut update = empty_update();
    let mut miner = valid_miner();
    let target = fixed_target(&mut miner.tuning);
    target.voltage_mv = MINING_BOUNDS.voltage_mv.end() + 1;
    update.miner = Some(miner);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("miner.tuning.Fixed.voltage_mv outside the accepted range")
    );
}

/// Thermal-only update rejected when current cooling target >= new dangerous temp.
///
/// Build a current config with `cooling.target_temp_c = 80.0` then send only
/// `thermal.dangerous_temp_c = 70.0`. The current target is above the new
/// dangerous limit, so validation must reject the update.
#[test]
fn thermal_only_update_rejected_when_current_target_above_new_dangerous() {
    let mut current = Config::default();
    current.cooling.target_temp_c = 80.0;
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous_temp_c = 70.0;
    // cooldown must be below dangerous; 45.0 < 70.0 is fine.
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&current, &crate::mining::BOUNDS),
        Err("current cooling.target_temp_c must remain less than thermal.dangerous_temp_c")
    );
}

/// A value inside the expanded range passes.
///
/// 100 MHz is within the new `50..=700` window and exercises the
/// low-frequency end of the chip's validated range.
#[test]
fn tuning_frequency_in_range_non_discrete_passes() {
    let mut update = empty_update();
    let mut miner = valid_miner();
    let target = fixed_target(&mut miner.tuning);
    target.frequency_mhz = 100;
    update.miner = Some(miner);
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// A non-round voltage inside the accepted range passes.
///
/// Eleven above the board's lower bound is non-round and within the range,
/// confirming that range semantics replaced the old discrete-set check.
#[test]
fn tuning_voltage_in_range_non_discrete_passes() {
    let mut update = empty_update();
    let mut miner = valid_miner();
    let target = fixed_target(&mut miner.tuning);
    target.voltage_mv = MINING_BOUNDS.voltage_mv.start() + 11;
    update.miner = Some(miner);
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

// --- Cooling fan PWM floor ---

/// Fan PWM floor of 100 (the full-speed cap) passes validation.
#[test]
fn cooling_min_fan_pwm_100_passes() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target_temp_c: None,
        fixed_fan_pwm: None,
        min_fan_pwm: Some(100),
    });
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// Fan PWM floor above 100 is rejected because the PID clamp range would be
/// inverted (floor > ceiling).
#[test]
fn cooling_min_fan_pwm_101_rejected() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target_temp_c: None,
        fixed_fan_pwm: None,
        min_fan_pwm: Some(101),
    });
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("cooling.min_fan_pwm must be between 0 and 100")
    );
}

// --- Cooling fixed fan duty ---

/// Both ends of the fixed-duty range pass. Zero is accepted deliberately:
/// a stopped fan is a legitimate request, and the board stays covered by
/// the mining halt at `thermal.dangerous_temp_c`.
#[test]
fn cooling_fixed_fan_pwm_range_ends_pass() {
    for duty in [0, 100] {
        let mut update = empty_update();
        update.cooling = Some(CoolingUpdate {
            mode: Some(FanMode::Fixed),
            target_temp_c: None,
            fixed_fan_pwm: Some(duty),
            min_fan_pwm: None,
        });
        assert!(
            update
                .validate(&Config::default(), &crate::mining::BOUNDS)
                .is_ok(),
            "duty {duty} must be accepted"
        );
    }
}

/// A fixed duty above 100 is rejected; a duty cycle has no value above full.
#[test]
fn cooling_fixed_fan_pwm_101_rejected() {
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: None,
        target_temp_c: None,
        fixed_fan_pwm: Some(101),
        min_fan_pwm: None,
    });
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("cooling.fixed_fan_pwm must be between 0 and 100")
    );
}

/// The fixed duty is independent of the PID floor: a duty below
/// `min_fan_pwm` passes, because the floor bounds PID output only.
#[test]
fn cooling_fixed_fan_pwm_below_floor_passes() {
    let current = Config::default();
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: Some(FanMode::Fixed),
        target_temp_c: None,
        fixed_fan_pwm: Some(current.cooling.pid.min_fan_pwm - 1),
        min_fan_pwm: None,
    });
    assert!(update.validate(&current, &crate::mining::BOUNDS).is_ok());
}

/// The fan mode survives the exact wire codec the API uses, so the webui
/// sees the string variant name it sends back. The enum is the first one in
/// `CoolingUpdate`, and `serde-json-core` handles unit variants specially.
#[test]
fn cooling_mode_json_round_trip() {
    let update = ConfigUpdate {
        cooling: Some(CoolingUpdate {
            mode: Some(FanMode::Fixed),
            target_temp_c: None,
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

/// Applying a cooling update overwrites the mode and the fixed duty, leaves
/// the fields it omits at their previous values, and `from_config` round-trips
/// the result.
#[test]
fn cooling_mode_and_duty_apply_and_roundtrip() {
    let mut config = Config::default();
    let mut update = empty_update();
    update.cooling = Some(CoolingUpdate {
        mode: Some(FanMode::Fixed),
        target_temp_c: None,
        fixed_fan_pwm: Some(35),
        min_fan_pwm: None,
    });
    update.apply(&mut config);
    assert_eq!(
        config.cooling,
        CoolingConfig {
            mode: FanMode::Fixed,
            fixed_fan_pwm: 35,
            ..Config::default().cooling
        }
    );

    assert_eq!(
        ConfigUpdate::from_config(&config).cooling,
        Some(CoolingUpdate {
            mode: Some(FanMode::Fixed),
            target_temp_c: Some(config.cooling.target_temp_c),
            fixed_fan_pwm: Some(35),
            min_fan_pwm: Some(config.cooling.pid.min_fan_pwm),
        })
    );
}

// --- API credentials ---

/// Valid API credentials pass.
#[test]
fn api_valid_passes() {
    let mut update = empty_update();
    update.api = Some(valid_api());
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// Empty username is rejected.
#[test]
fn api_empty_username_rejected() {
    let mut update = empty_update();
    let mut api = valid_api();
    api.username = heapless::String::new();
    update.api = Some(api);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("api.username must not be empty")
    );
}

/// Non-ASCII username is rejected.
#[test]
fn api_non_ascii_username_rejected() {
    let mut update = empty_update();
    let mut api = valid_api();
    api.username = heapless::String::new();
    // "é" encodes as two UTF-8 bytes (0xC3 0xA9); both fail is_ascii_graphic.
    api.username.push_str("admé").expect("BUG: username fits");
    update.api = Some(api);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("api.username must contain only printable ASCII characters")
    );
}

/// Non-ASCII password is rejected when non-empty.
#[test]
fn api_non_ascii_password_rejected() {
    let mut update = empty_update();
    let mut api = valid_api();
    api.password = heapless::String::new();
    // Space (0x20) is not ascii_graphic.
    api.password
        .push_str("pass word")
        .expect("BUG: password fits");
    update.api = Some(api);
    assert_eq!(
        update.validate(&Config::default(), &crate::mining::BOUNDS),
        Err("api.password must contain only printable ASCII characters")
    );
}

/// Empty password passes — it means "keep existing".
#[test]
fn api_empty_password_passes() {
    let mut update = empty_update();
    let mut api = valid_api();
    api.password = heapless::String::new();
    update.api = Some(api);
    assert!(
        update
            .validate(&Config::default(), &crate::mining::BOUNDS)
            .is_ok()
    );
}

/// Builds a valid static [`NetworkConfig`].
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

/// Default network config (DHCP, empty hostname) passes validation.
#[test]
fn network_default_passes() {
    assert!(NetworkConfig::default().validate().is_ok());
}

/// A valid static config passes validation.
#[test]
fn network_static_valid_passes() {
    assert!(valid_network_static().validate().is_ok());
}

/// `prefix_len` of 0 is rejected for static mode.
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

/// `prefix_len` of 33 is rejected for static mode.
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

/// Zero address `0.0.0.0` is rejected for static mode.
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

/// Zero gateway `0.0.0.0` is rejected for static mode.
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

/// A hostname with a space is rejected.
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

/// A hostname with an underscore is rejected.
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

/// A hostname starting with a hyphen is rejected.
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

/// A hostname ending with a hyphen is rejected.
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

/// An empty hostname is accepted as the "use build-time default" sentinel.
#[test]
fn network_hostname_empty_passes() {
    let network = NetworkConfig::default();
    assert!(network.validate().is_ok());
}

/// A well-formed hostname passes validation.
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

/// `Config::default()` is internally consistent.
#[test]
fn config_default_validates() {
    assert!(Config::default().validate(&crate::mining::BOUNDS).is_ok());
}

/// A Config decoded from storage with NaN `cooling.target_temp_c` is rejected
/// so `load_initial` falls back to defaults instead of feeding consumers a
/// value that compares false to every bound.
#[test]
fn config_cooling_target_nan_rejected() {
    let mut config = Config::default();
    config.cooling.target_temp_c = f32::NAN;
    assert_eq!(
        config.validate(&crate::mining::BOUNDS),
        Err("cooling.target_temp_c must be a finite number")
    );
}

/// A Config with cooling target above `dangerous_temp_c` is rejected.
///
/// Set `dangerous = 60.0` and `target = 80.0` so target stays inside the
/// 30..=85 range but the cross-field ordering fails.
#[test]
fn config_cooling_target_above_dangerous_rejected() {
    let mut config = Config::default();
    config.thermal.dangerous_temp_c = 60.0;
    config.cooling.target_temp_c = 80.0;
    assert_eq!(
        config.validate(&crate::mining::BOUNDS),
        Err("cooling.target_temp_c must be less than dangerous_temp_c")
    );
}

/// A Config decoded from storage with an out-of-range `min_fan_pwm` is
/// rejected. The floor lives under `cooling.pid` but is operator-tunable, so
/// it reaches flash the same way the fixed duty does and gets the same
/// boot-time bound rather than being silently clamped by `TempControl::new`.
#[test]
fn config_cooling_min_fan_pwm_101_rejected() {
    let mut config = Config::default();
    config.cooling.pid.min_fan_pwm = 101;
    assert_eq!(
        config.validate(&crate::mining::BOUNDS),
        Err("cooling.min_fan_pwm must be between 0 and 100")
    );
}

/// A Config decoded from storage with an out-of-range `fixed_fan_pwm` is
/// rejected, covering the sibling duty on the same path.
#[test]
fn config_cooling_fixed_fan_pwm_101_rejected() {
    let mut config = Config::default();
    config.cooling.fixed_fan_pwm = 101;
    assert_eq!(
        config.validate(&crate::mining::BOUNDS),
        Err("cooling.fixed_fan_pwm must be between 0 and 100")
    );
}
