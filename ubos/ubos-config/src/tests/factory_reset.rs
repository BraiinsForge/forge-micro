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

//! Tests for the `FactoryReset` message path in [`crate::run`].
//!
//! These drive [`crate::factory_reset`] directly — the same helper the
//! `Message::FactoryReset` arm calls — so a regression in the reset
//! (a broken `Config::default()` selection or a dropped `persist`) is
//! caught by real code rather than a reimplementation of the steps. The
//! helper also publishes on `STATE_WATCH`, but that watch is a shared
//! global static and every reset publishes the same default, so the
//! tests do not assert on it: a check could pass spuriously from a
//! sibling test under parallel execution.
//!
//! `FileStorage`'s async methods have no suspend points, so
//! `embassy_futures::block_on` resolves each future on the first poll.
//! Pattern mirrors the ubos-auth test suite.

extern crate std;

use std::fs;

use embassy_futures::block_on;

use crate::TemperatureDescriptor;
use ubos_core::units::{Efficiency, Frequency, Power, Temperature, Voltage};

use crate::{
    Config, DefaultTuning, FrequencyVoltageTarget, LOAD_BUFFER_SIZE, MinerConfig, MiningBounds,
    MiningInfo, PowerTarget, TunerInfo, Tuning, decode_blob, factory_reset,
    file_storage::FileStorage, load_initial, storage::Storage,
};

/// Drive `factory_reset` against empty storage, then reload and decode.
/// Asserts both the returned config and the reloaded blob equal
/// `Config::default()`, confirming the reset writes the correct defaults.
#[test]
fn factory_reset_persists_and_reloads_default() {
    let dir = std::env::temp_dir().join(format!(
        "ubos_config_factory_reset_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos()),
    ));

    let mut storage = FileStorage::new(&dir);

    let reset = block_on(factory_reset(
        &mut storage,
        &ubos_asic::BMM101_HASHBOARD,
        DefaultTuning::Fixed,
    ));
    assert_eq!(
        reset,
        Config::default(),
        "factory_reset must return Config::default()"
    );

    // Reload the persisted blob and verify it decodes back to the defaults.
    let mut load_buffer = [0u8; LOAD_BUFFER_SIZE];
    let blob = block_on(storage.load(crate::storage::CONFIG_KEY, &mut load_buffer))
        .expect("BUG: FileStorage::load returned Err in test")
        .expect("BUG: no blob found after factory-reset store in test");

    let reloaded: Config = decode_blob::<std::io::Error>(blob)
        .expect("BUG: decode after factory-reset store failed in test");

    assert_eq!(
        reloaded,
        Config::default(),
        "config reloaded after factory reset must equal Config::default()"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Verify that `factory_reset` overwrites a previously stored non-default
/// config. Stores a modified config first, then runs the reset, then
/// reloads and asserts the result is the factory default — not the
/// earlier value.
#[test]
fn factory_reset_overwrites_existing_config() {
    let dir = std::env::temp_dir().join(format!(
        "ubos_config_factory_reset_overwrite_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos()),
    ));

    let mut storage = FileStorage::new(&dir);

    // Store a modified config (non-default dangerous temperature) to simulate
    // pre-existing state. 85.0 differs from the default of 83.0.
    let mut modified = Config::default();
    modified.thermal.dangerous = TemperatureDescriptor {
        chip: None,
        hashboard: Some(Temperature::from_celsius(85)),
    };
    let mut modified_buf = [0u8; LOAD_BUFFER_SIZE];
    let modified_len =
        crate::encode_blob(&modified, &mut modified_buf).expect("BUG: encode_blob failed in test");
    block_on(storage.store(crate::storage::CONFIG_KEY, &modified_buf[..modified_len]))
        .expect("BUG: FileStorage::store failed in test");

    // Apply factory reset over the stored non-default config.
    let reset = block_on(factory_reset(
        &mut storage,
        &ubos_asic::BMM101_HASHBOARD,
        DefaultTuning::Fixed,
    ));
    assert_eq!(
        reset,
        Config::default(),
        "factory_reset must return Config::default()"
    );

    // Reload and assert the stored value is now the default.
    let mut load_buffer = [0u8; LOAD_BUFFER_SIZE];
    let blob = block_on(storage.load(crate::storage::CONFIG_KEY, &mut load_buffer))
        .expect("BUG: FileStorage::load returned Err in test")
        .expect("BUG: no blob found after factory-reset overwrite in test");

    let reloaded: Config = decode_blob::<std::io::Error>(blob)
        .expect("BUG: decode after factory-reset overwrite failed in test");

    assert_eq!(
        reloaded,
        Config::default(),
        "config after factory reset must equal Config::default() even when prior config existed"
    );
    assert_ne!(
        reloaded.thermal, modified.thermal,
        "factory reset must clear the previously stored thermal config"
    );

    let _ = fs::remove_dir_all(&dir);
}

static BMM101_INFO: MiningInfo = crate::mining_info(
    &ubos_asic::BMM101_HASHBOARD,
    &crate::MINING_BOUNDS,
    DefaultTuning::Fixed,
);

static FIXED_ONLY_BOUNDS: MiningBounds = crate::bounds(
    &ubos_asic::BMM101_HASHBOARD,
    &[TunerInfo::Fixed { name: "Fixed" }],
    None,
);

static FIXED_ONLY_INFO: MiningInfo = crate::mining_info(
    &ubos_asic::BMM101_HASHBOARD,
    &FIXED_ONLY_BOUNDS,
    DefaultTuning::Fixed,
);

/// Store `tuning` in a config whose thermal limit differs from the
/// default, boot it against `info`, and return what was stored and what
/// boot loaded.
fn boot_with_stored(name: &str, tuning: Tuning, info: &'static MiningInfo) -> (Config, Config) {
    let dir = std::env::temp_dir().join(format!(
        "ubos_config_{name}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos()),
    ));
    let mut storage = FileStorage::new(&dir);
    let mut stored = Config::default();
    stored.thermal.dangerous = TemperatureDescriptor {
        chip: None,
        hashboard: Some(Temperature::from_celsius(85)),
    };
    stored.miner.tuning = tuning;
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let len = crate::encode_blob(&stored, &mut buffer).expect("BUG: encode_blob failed in test");
    block_on(storage.store(crate::storage::CONFIG_KEY, &buffer[..len]))
        .expect("BUG: FileStorage::store failed in test");
    let loaded = block_on(load_initial(
        &mut storage,
        &ubos_asic::BMM101_HASHBOARD,
        info,
        None,
    ));
    let _ = fs::remove_dir_all(&dir);
    (stored, loaded)
}

/// A stored tuning outside a range is clamped into it at boot, and the
/// rest of the stored config survives.
#[test]
fn load_clamps_stored_tuning_and_keeps_the_rest() {
    let frequency = crate::MINING_BOUNDS.frequency.clone();
    let (stored, loaded) = boot_with_stored(
        "clamp",
        Tuning::Fixed(FrequencyVoltageTarget {
            frequency: Frequency::from_khz(frequency.end().khz() + 1),
            voltage: Voltage::from_mv(1_200),
        }),
        &BMM101_INFO,
    );
    let clamped = Tuning::Fixed(FrequencyVoltageTarget {
        frequency: *frequency.end(),
        voltage: Voltage::from_mv(1_200),
    });
    assert_eq!(
        loaded,
        Config {
            miner: MinerConfig { tuning: clamped },
            ..stored
        }
    );
}

/// A stored tuning no listed tuner reads is replaced by the default
/// tuning at boot, and the rest of the stored config survives.
#[test]
fn load_replaces_unoffered_tuning_and_keeps_the_rest() {
    let (stored, loaded) = boot_with_stored(
        "unoffered",
        Tuning::Power(PowerTarget {
            input_power: Power::from_mw(44_000),
            efficiency: Efficiency::from_mj_per_th(
                core::num::NonZeroU32::new(40_000).expect("nonzero"),
            ),
        }),
        &FIXED_ONLY_INFO,
    );
    let default = DefaultTuning::Fixed.tuning(&ubos_asic::BMM101_HASHBOARD);
    assert_eq!(
        loaded,
        Config {
            miner: MinerConfig { tuning: default },
            ..stored
        }
    );
}
