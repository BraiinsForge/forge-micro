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

//! Tests for the seed path in [`crate::load_initial`].
//!
//! These drive [`crate::load_initial`] directly, the same function
//! [`crate::run`] opens with, against a `FileStorage` in a fresh
//! directory. Most tests boot the store more than once with the seed a
//! board would build on each boot, as the operator changes the config
//! between boots. `FileStorage`'s async methods have no suspend points,
//! so `embassy_futures::block_on` resolves each future on the first
//! poll. Pattern mirrors the factory-reset tests.

extern crate std;

use std::fs;
use std::path::PathBuf;

use embassy_futures::block_on;

use crate::{
    ApiConfig, CONFIG_KEY, Config, DefaultTuning, LOAD_BUFFER_SIZE, MiningInfo, NetworkMode, Seed,
    SeedField, decode_blob, encode_blob, file_storage::FileStorage, load_initial, storage::Storage,
};

const BOARD: &ubos_asic::HashboardDescriptor = &ubos_asic::BMM101_HASHBOARD;

static INFO: MiningInfo = crate::mining_info(BOARD, &crate::MINING_BOUNDS, DefaultTuning::Fixed);

/// The fingerprint of a field's source on the first boot.
const SOURCE: u32 = 0x1111_1111;

/// A fresh temporary directory for one test's store.
fn fresh_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ubos_config_seed_{name}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos()),
    ))
}

/// An API login for `username` that differs from the board default.
fn login(username: &str) -> ApiConfig {
    ApiConfig {
        username: username.try_into().expect("BUG: test username too long"),
        password: "recovered".try_into().expect("BUG: test password too long"),
    }
}

/// A static network mode on `address`.
fn static_mode(address: [u8; 4]) -> NetworkMode {
    NetworkMode::Static {
        address,
        prefix_len: 24,
        gateway: [192, 168, 1, 1],
        dns: heapless::Vec::new(),
    }
}

/// A seed field of `value` read from the source with `fingerprint`.
fn field<T>(value: T, fingerprint: u32) -> SeedField<T> {
    SeedField { value, fingerprint }
}

/// A seed that fills the API login and the network mode, both from
/// their first-boot source.
fn seed() -> Seed {
    Seed {
        api: Some(field(login("operator"), SOURCE)),
        network_mode: Some(field(static_mode([192, 168, 1, 10]), SOURCE)),
        ..Seed::default()
    }
}

/// The board defaults with the fields of [`seed`] written over them.
fn seeded() -> Config {
    let mut config = Config::for_board(BOARD, INFO.default_tuning);
    config.api = login("operator");
    config.network.mode = static_mode([192, 168, 1, 10]);
    config
}

/// What the store holds under the config key, decoded.
fn stored(storage: &mut FileStorage) -> Option<Config> {
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let blob = block_on(storage.load(CONFIG_KEY, &mut buffer))
        .expect("BUG: FileStorage::load returned Err in test")?;
    Some(
        decode_blob::<std::io::Error>(blob).expect("BUG: decode of the stored blob failed in test"),
    )
}

/// Store `config` as an operator's save over the API would.
fn save(storage: &mut FileStorage, config: &Config) {
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let length = encode_blob(config, &mut buffer).expect("BUG: encode_blob failed in test");
    block_on(storage.store(CONFIG_KEY, &buffer[..length]))
        .expect("BUG: FileStorage::store failed in test");
}

/// An empty store takes the defaults with the seed written over them,
/// and the result is persisted so the next load finds it in the store.
#[test]
fn empty_store_takes_and_persists_the_seed() {
    let dir = fresh_dir("empty");
    let mut storage = FileStorage::new(&dir);

    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(
        loaded,
        seeded(),
        "an empty store must start from the defaults with the seed over them"
    );
    assert_eq!(
        stored(&mut storage),
        Some(seeded()),
        "the seeded config must be persisted on the boot that takes it"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A stored config with no seed record, from a firmware without one,
/// takes every seed field once and keeps its other settings.
#[test]
fn stored_config_without_a_record_takes_the_seed() {
    let dir = fresh_dir("upgrade");
    let mut storage = FileStorage::new(&dir);

    let mut existing = Config::for_board(BOARD, INFO.default_tuning);
    existing.thermal.dangerous_estimated_temp_c = 80.0;
    save(&mut storage, &existing);

    let mut expected = seeded();
    expected.thermal.dangerous_estimated_temp_c = 80.0;
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(loaded, expected);
    assert_eq!(stored(&mut storage), Some(expected));

    let _ = fs::remove_dir_all(&dir);
}

/// An empty store with no seed boots on the board defaults and writes
/// nothing, as before.
#[test]
fn empty_store_without_a_seed_stays_empty() {
    let dir = fresh_dir("none");
    let mut storage = FileStorage::new(&dir);

    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, None));
    assert_eq!(loaded, Config::for_board(BOARD, INFO.default_tuning));
    assert_eq!(stored(&mut storage), None, "defaults are not persisted");

    let _ = fs::remove_dir_all(&dir);
}

/// A seed whose fields equal the defaults leaves the store empty, so
/// the defaults stay unstored as on a board with no seed.
#[test]
fn seed_that_changes_nothing_stays_unstored() {
    let dir = fresh_dir("unchanged");
    let mut storage = FileStorage::new(&dir);

    let unchanged = Seed {
        api: Some(field(Config::for_board(BOARD, INFO.default_tuning).api, SOURCE)),
        ..Seed::default()
    };
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(unchanged)));
    assert_eq!(loaded, Config::for_board(BOARD, INFO.default_tuning));
    assert_eq!(stored(&mut storage), None, "defaults are not persisted");

    let _ = fs::remove_dir_all(&dir);
}

/// A seed whose result fails validation is dropped for the config the
/// boot started from, and nothing is persisted.
#[test]
fn invalid_seed_keeps_the_config() {
    let dir = fresh_dir("invalid");
    let mut storage = FileStorage::new(&dir);

    let bad = Seed {
        api: Some(field(login(""), SOURCE)),
        ..Seed::default()
    };
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(bad.clone())));
    assert_eq!(loaded, Config::for_board(BOARD, INFO.default_tuning));
    assert_eq!(
        stored(&mut storage),
        None,
        "a rejected seed is not persisted"
    );

    let mut existing = Config::for_board(BOARD, INFO.default_tuning);
    existing.thermal.dangerous_estimated_temp_c = 80.0;
    save(&mut storage, &existing);
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(bad)));
    assert_eq!(loaded, existing);
    assert_eq!(stored(&mut storage), Some(existing));

    let _ = fs::remove_dir_all(&dir);
}
