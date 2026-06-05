//! Tests for [`crate::ConfigUpdate::validate`].

use crate::{
    ApiConfig, Config, ConfigUpdate, PoolEntry, ProtocolKind, ThermalConfig, TuningConfig,
};

/// Builds a minimal valid [`ConfigUpdate`] with no fields set.
fn empty_update() -> ConfigUpdate {
    ConfigUpdate {
        pool: None,
        thermal: None,
        cooling_target_temp_c: None,
        tuning: None,
        api: None,
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
    }
}

/// Builds a valid [`ThermalConfig`].
fn valid_thermal() -> ThermalConfig {
    ThermalConfig {
        sample_interval_ms: 1000,
        dangerous_temp_c: 90.0,
        cooldown_temp_c: 45.0,
    }
}

/// Builds a valid [`TuningConfig`] using values inside the accepted range.
fn valid_tuning() -> TuningConfig {
    TuningConfig {
        frequency_mhz: 525,
        voltage_mv: 1400,
    }
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
    assert!(update.validate(&Config::default()).is_ok());
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
        Err("pool 2: host must not be empty")
    );
}

// --- Thermal ---

/// Valid thermal config passes.
#[test]
fn thermal_valid_passes() {
    let mut update = empty_update();
    update.thermal = Some(valid_thermal());
    assert!(update.validate(&Config::default()).is_ok());
}

/// `sample_interval_ms` below 100 is rejected.
#[test]
fn thermal_sample_interval_too_low() {
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.sample_interval_ms = 99;
    update.thermal = Some(thermal);
    assert_eq!(
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
        Err("thermal.cooldown_temp_c must be less than thermal.dangerous_temp_c")
    );
}

// --- Cooling target cross-field rule ---

/// `cooling_target_temp_c` below the dangerous limit passes.
///
/// Default current config has `dangerous_temp_c = 90.0`; 70.0 is below
/// that and within the 30..=85 range.
#[test]
fn cooling_target_below_dangerous_passes() {
    let mut update = empty_update();
    update.cooling_target_temp_c = Some(70.0);
    assert!(update.validate(&Config::default()).is_ok());
}

/// `cooling_target_temp_c` equal to `dangerous_temp_c` is rejected.
///
/// Build a current config with dangerous = 80.0 and set target = 80.0
/// so the cross-field rule fires.
#[test]
fn cooling_target_equal_to_dangerous_rejected() {
    let mut current = Config::default();
    current.thermal.dangerous_temp_c = 80.0;
    let mut update = empty_update();
    // 80.0 is within 30..=85 but equals the dangerous limit.
    update.cooling_target_temp_c = Some(80.0);
    assert_eq!(
        update.validate(&current),
        Err("cooling_target_temp_c must be less than dangerous_temp_c")
    );
}

/// When both thermal and `cooling_target_temp_c` are set, the cross-field
/// rule uses the incoming `dangerous_temp_c`, not the current config.
#[test]
fn cooling_target_cross_field_uses_incoming_dangerous() {
    // Current config has dangerous = 90.0; target of 75 with incoming
    // dangerous of 80 should pass because 75 < 80.
    let mut update = empty_update();
    let mut thermal = valid_thermal();
    thermal.dangerous_temp_c = 80.0;
    update.thermal = Some(thermal);
    update.cooling_target_temp_c = Some(75.0);
    assert!(update.validate(&Config::default()).is_ok());
}

/// `cooling_target_temp_c` NaN is rejected.
#[test]
fn cooling_target_nan_rejected() {
    let mut update = empty_update();
    update.cooling_target_temp_c = Some(f32::NAN);
    assert_eq!(
        update.validate(&Config::default()),
        Err("cooling_target_temp_c must be a finite number")
    );
}

/// `cooling_target_temp_c` below 30 is rejected.
#[test]
fn cooling_target_too_low() {
    let mut update = empty_update();
    update.cooling_target_temp_c = Some(29.9);
    assert_eq!(
        update.validate(&Config::default()),
        Err("cooling_target_temp_c must be between 30 and 85")
    );
}

/// `cooling_target_temp_c` above 85 is rejected.
#[test]
fn cooling_target_too_high() {
    let mut update = empty_update();
    update.cooling_target_temp_c = Some(85.1);
    assert_eq!(
        update.validate(&Config::default()),
        Err("cooling_target_temp_c must be between 30 and 85")
    );
}

// --- Tuning ---

/// Valid frequency and voltage pass.
#[test]
fn tuning_valid_passes() {
    let mut update = empty_update();
    update.tuning = Some(valid_tuning());
    assert!(update.validate(&Config::default()).is_ok());
}

/// Frequency outside the accepted range is rejected.
///
/// 501 MHz is between two old discrete values (500, 525) but still
/// within the new 400..=600 range, so it now passes.
/// Use 601 to test the out-of-range path.
#[test]
fn tuning_frequency_out_of_range() {
    let mut update = empty_update();
    let mut tuning = valid_tuning();
    tuning.frequency_mhz = 601;
    update.tuning = Some(tuning);
    assert_eq!(
        update.validate(&Config::default()),
        Err("tuning.frequency_mhz must be between 400 and 600")
    );
}

/// Voltage outside the accepted range is rejected.
///
/// 1099 is below 1100, the new lower bound.
#[test]
fn tuning_voltage_out_of_range() {
    let mut update = empty_update();
    let mut tuning = valid_tuning();
    tuning.voltage_mv = 1099;
    update.tuning = Some(tuning);
    assert_eq!(
        update.validate(&Config::default()),
        Err("tuning.voltage_mv must be between 1100 and 1500")
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
        update.validate(&current),
        Err("current cooling.target_temp_c must remain less than thermal.dangerous_temp_c")
    );
}

/// A value inside the range that was not in the old discrete set passes.
///
/// 501 MHz was previously rejected as "not in the list"; with range
/// semantics it is accepted.
#[test]
fn tuning_frequency_in_range_non_discrete_passes() {
    let mut update = empty_update();
    let mut tuning = valid_tuning();
    tuning.frequency_mhz = 501;
    update.tuning = Some(tuning);
    assert!(update.validate(&Config::default()).is_ok());
}

/// A voltage inside the range that was not in the old discrete set passes.
///
/// 1111 mV was previously rejected as "not in the list"; with range
/// semantics it is accepted.
#[test]
fn tuning_voltage_in_range_non_discrete_passes() {
    let mut update = empty_update();
    let mut tuning = valid_tuning();
    tuning.voltage_mv = 1111;
    update.tuning = Some(tuning);
    assert!(update.validate(&Config::default()).is_ok());
}

// --- API credentials ---

/// Valid API credentials pass.
#[test]
fn api_valid_passes() {
    let mut update = empty_update();
    update.api = Some(valid_api());
    assert!(update.validate(&Config::default()).is_ok());
}

/// Empty username is rejected.
#[test]
fn api_empty_username_rejected() {
    let mut update = empty_update();
    let mut api = valid_api();
    api.username = heapless::String::new();
    update.api = Some(api);
    assert_eq!(
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
        update.validate(&Config::default()),
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
    assert!(update.validate(&Config::default()).is_ok());
}

// --- Config::validate ---

/// `Config::default()` is internally consistent.
#[test]
fn config_default_validates() {
    assert!(Config::default().validate().is_ok());
}

/// A Config decoded from storage with NaN `cooling.target_temp_c` is rejected
/// so `load_initial` falls back to defaults instead of feeding consumers a
/// value that compares false to every bound.
#[test]
fn config_cooling_target_nan_rejected() {
    let mut config = Config::default();
    config.cooling.target_temp_c = f32::NAN;
    assert_eq!(
        config.validate(),
        Err("cooling_target_temp_c must be a finite number")
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
        config.validate(),
        Err("cooling_target_temp_c must be less than dangerous_temp_c")
    );
}
