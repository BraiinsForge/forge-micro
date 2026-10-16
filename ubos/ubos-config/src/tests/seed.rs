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
use std::path::{Path, PathBuf};

use embassy_futures::block_on;

use ubos_core::units::Temperature;

use crate::{
    ApiConfig, CONFIG_KEY, Config, DefaultTuning, LOAD_BUFFER_SIZE, MiningInfo, NetworkMode, Seed,
    SeedField, decode_blob, encode_blob, file_storage::FileStorage, load_initial, storage::Storage,
};

const BOARD: &ubos_asic::HashboardDescriptor = &ubos_asic::BMM101_HASHBOARD;

static INFO: MiningInfo = crate::mining_info(BOARD, &crate::MINING_BOUNDS, DefaultTuning::Fixed);

/// The fingerprint of a field's source on the first boot.
const SOURCE: u32 = 0x1111_1111;
/// The fingerprint of the same source after the other firmware changed
/// it.
const CHANGED_SOURCE: u32 = 0x2222_2222;

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

/// A `FileStorage` that counts its writes, and refuses every config
/// write while `refuse_config` is set, as a flash fault would.
struct Probe {
    inner: FileStorage,
    writes: usize,
    refuse_config: bool,
}

impl Probe {
    fn new(dir: &Path) -> Self {
        Self {
            inner: FileStorage::new(dir),
            writes: 0,
            refuse_config: false,
        }
    }
}

impl Storage for Probe {
    type Error = std::io::Error;

    async fn load<'b>(
        &mut self,
        key: u8,
        buffer: &'b mut [u8],
    ) -> Result<Option<&'b [u8]>, Self::Error> {
        self.inner.load(key, buffer).await
    }

    async fn store(&mut self, key: u8, data: &[u8]) -> Result<(), Self::Error> {
        if self.refuse_config && key == CONFIG_KEY {
            return Err(std::io::Error::other("config write refused in test"));
        }
        self.writes += 1;
        self.inner.store(key, data).await
    }
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

/// A seed whose source has not changed since the store took it leaves
/// the operator's changes in place, a factory reset among them.
#[test]
fn seed_already_taken_keeps_the_operator_changes() {
    let dir = fresh_dir("taken");
    let mut storage = FileStorage::new(&dir);
    block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));

    let mut edited = seeded();
    edited.api = login("edited");
    save(&mut storage, &edited);
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(loaded, edited, "an unchanged seed must not touch the store");

    save(&mut storage, &Config::for_board(BOARD, INFO.default_tuning));
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(
        loaded,
        Config::for_board(BOARD, INFO.default_tuning),
        "a factory reset must keep an unchanged seed out"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A field whose source changed is written again, and every other
/// setting keeps the value the operator gave it, the other seed field
/// among them.
#[test]
fn changed_field_is_taken_again_alone() {
    let dir = fresh_dir("changed");
    let mut storage = FileStorage::new(&dir);
    block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));

    let mut edited = seeded();
    edited.api = login("edited");
    edited.thermal.dangerous_hashboard_temp = Temperature::from_celsius(80);
    save(&mut storage, &edited);

    let changed = Seed {
        network_mode: Some(field(static_mode([192, 168, 1, 20]), CHANGED_SOURCE)),
        ..seed()
    };
    let mut expected = edited.clone();
    expected.network.mode = static_mode([192, 168, 1, 20]);
    let loaded = block_on(load_initial(
        &mut storage,
        BOARD,
        &INFO,
        Some(changed.clone()),
    ));
    assert_eq!(loaded, expected, "only the changed field must be taken");
    assert_eq!(stored(&mut storage), Some(expected.clone()));

    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(changed)));
    assert_eq!(loaded, expected, "a field is taken once per change");

    let _ = fs::remove_dir_all(&dir);
}

/// A stored config with no seed record, from a firmware without one,
/// takes every seed field once and keeps its other settings.
#[test]
fn stored_config_without_a_record_takes_the_seed() {
    let dir = fresh_dir("upgrade");
    let mut storage = FileStorage::new(&dir);

    let mut existing = Config::for_board(BOARD, INFO.default_tuning);
    existing.thermal.dangerous_hashboard_temp = Temperature::from_celsius(80);
    save(&mut storage, &existing);

    let mut expected = seeded();
    expected.thermal.dangerous_hashboard_temp = Temperature::from_celsius(80);
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(loaded, expected);
    assert_eq!(stored(&mut storage), Some(expected));

    let _ = fs::remove_dir_all(&dir);
}

/// A config write that fails leaves the fingerprints unsaved, so the
/// next boot takes the seed again rather than losing it.
#[test]
fn failed_config_write_takes_the_seed_again() {
    let dir = fresh_dir("failed-write");
    let mut storage = Probe::new(&dir);
    storage.refuse_config = true;
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(loaded, seeded(), "the boot still runs on the seed");
    assert_eq!(storage.writes, 0, "the fingerprints wait for the config");

    storage.refuse_config = false;
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(loaded, seeded());
    assert_eq!(stored(&mut storage.inner), Some(seeded()));

    let _ = fs::remove_dir_all(&dir);
}

/// A seed that matches the defaults saves its fingerprints once, and a
/// later boot with the same seed writes nothing, though the store still
/// holds no config.
#[test]
fn seed_matching_the_defaults_is_recorded_once() {
    let dir = fresh_dir("recorded-once");
    let mut storage = Probe::new(&dir);
    let unchanged = Seed {
        api: Some(field(
            Config::for_board(BOARD, INFO.default_tuning).api,
            SOURCE,
        )),
        ..Seed::default()
    };

    block_on(load_initial(
        &mut storage,
        BOARD,
        &INFO,
        Some(unchanged.clone()),
    ));
    assert_eq!(storage.writes, 1, "only the fingerprints are saved");
    block_on(load_initial(&mut storage, BOARD, &INFO, Some(unchanged)));
    assert_eq!(storage.writes, 1, "an unchanged seed writes nothing");

    let _ = fs::remove_dir_all(&dir);
}

/// A field the board leaves out keeps the stored value and the taken
/// fingerprint, so an unreadable source never makes the next boot take
/// an unchanged field again.
#[test]
fn field_left_out_keeps_its_value_and_fingerprint() {
    let dir = fresh_dir("left-out");
    let mut storage = FileStorage::new(&dir);
    block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));

    let mut edited = seeded();
    edited.api = login("edited");
    save(&mut storage, &edited);

    let without_api = Seed {
        api: None,
        ..seed()
    };
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(without_api)));
    assert_eq!(loaded, edited);
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(loaded, edited, "the api fingerprint must still be taken");

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
        api: Some(field(
            Config::for_board(BOARD, INFO.default_tuning).api,
            SOURCE,
        )),
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
    existing.thermal.dangerous_hashboard_temp = Temperature::from_celsius(80);
    save(&mut storage, &existing);
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(bad)));
    assert_eq!(loaded, existing);
    assert_eq!(stored(&mut storage), Some(existing));

    let _ = fs::remove_dir_all(&dir);
}
