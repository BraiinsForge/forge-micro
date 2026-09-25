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

//! Tests for the first-boot seed path in [`crate::load_initial`].
//!
//! These drive [`crate::load_initial`] directly, the same function
//! [`crate::run`] opens with, against a `FileStorage` in a fresh
//! directory. `FileStorage`'s async methods have no suspend points, so
//! `embassy_futures::block_on` resolves each future on the first poll.
//! Pattern mirrors the factory-reset tests.

extern crate std;

use std::fs;
use std::path::PathBuf;

use embassy_futures::block_on;

use crate::{
    Config, DefaultTuning, LOAD_BUFFER_SIZE, MiningInfo, decode_blob, file_storage::FileStorage,
    load_initial, storage::Storage,
};

const BOARD: &ubos_asic::HashboardDescriptor = &ubos_asic::BMM101_HASHBOARD;

static INFO: MiningInfo = crate::mining_info(BOARD, &crate::MINING_BOUNDS, DefaultTuning::Fixed);

/// A fresh temporary directory for one test's store.
fn fresh_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ubos_config_seed_{name}_{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos()),
    ))
}

/// A config that differs from the board defaults in one field.
fn seed() -> Config {
    let mut seed = Config::for_board(BOARD, INFO.default_tuning);
    seed.thermal.dangerous_estimated_temp_c = 85.0;
    seed
}

/// What the store holds under the config key, decoded.
fn stored(storage: &mut FileStorage) -> Option<Config> {
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let blob = block_on(storage.load(crate::storage::CONFIG_KEY, &mut buffer))
        .expect("BUG: FileStorage::load returned Err in test")?;
    Some(
        decode_blob::<std::io::Error>(blob).expect("BUG: decode of the stored blob failed in test"),
    )
}

/// An empty store takes the seed, and the seed is persisted so the next
/// load finds it in the store.
#[test]
fn empty_store_takes_and_persists_the_seed() {
    let dir = fresh_dir("empty");
    let mut storage = FileStorage::new(&dir);

    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(loaded, seed(), "an empty store must start from the seed");
    assert_eq!(
        stored(&mut storage),
        Some(seed()),
        "the seed must be persisted on the boot that takes it"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A stored config wins over a seed, so a seed offered on every boot
/// never overrides what the operator has since changed.
#[test]
fn stored_config_wins_over_the_seed() {
    let dir = fresh_dir("stored");
    let mut storage = FileStorage::new(&dir);

    let mut existing = Config::for_board(BOARD, INFO.default_tuning);
    existing.thermal.dangerous_estimated_temp_c = 80.0;
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let length =
        crate::encode_blob(&existing, &mut buffer).expect("BUG: encode_blob failed in test");
    block_on(storage.store(crate::storage::CONFIG_KEY, &buffer[..length]))
        .expect("BUG: FileStorage::store failed in test");

    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(seed())));
    assert_eq!(loaded, existing, "a stored config must win over the seed");
    assert_eq!(
        stored(&mut storage),
        Some(existing),
        "the seed must not touch a store that already holds a config"
    );

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

/// A seed that fails validation is dropped for the defaults and nothing
/// is persisted.
#[test]
fn invalid_seed_falls_back_to_defaults() {
    let dir = fresh_dir("invalid");
    let mut storage = FileStorage::new(&dir);

    let mut bad = seed();
    bad.cooling.target_temp_c = bad.thermal.dangerous_estimated_temp_c;
    let loaded = block_on(load_initial(&mut storage, BOARD, &INFO, Some(bad)));
    assert_eq!(loaded, Config::for_board(BOARD, INFO.default_tuning));
    assert_eq!(
        stored(&mut storage),
        None,
        "a rejected seed is not persisted"
    );

    let _ = fs::remove_dir_all(&dir);
}
