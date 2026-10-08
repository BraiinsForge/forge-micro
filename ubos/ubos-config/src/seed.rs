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

//! The settings a board recovered from the firmware it replaced, which
//! the config task writes over the config it boots on.
//!
//! A board flashed over another firmware can read the operator's pools,
//! login and network setup from where that firmware kept them. It hands
//! them to [`crate::run`] as a [`Seed`], one field per setting, so a
//! setting it could not recover leaves the config's own value in place.
//!
//! The board reads them again on every boot, and the operator can go
//! back to the other firmware and change them there. Each field carries
//! a fingerprint of the source it came from, and the store keeps a
//! record of the fingerprints it last took. A field is written
//! again only when its fingerprint changes, so a setting changed under
//! the other firmware reaches the config once, and one the operator
//! changed here since stays as they left it.

use serde::{Deserialize, Serialize};
use ubos_logging::info;

use crate::{ApiConfig, Config, NetworkMode, PoolEntry, pool};

/// One setting a board recovered.
#[derive(Clone, Debug, PartialEq)]
pub struct SeedField<T> {
    /// The setting, converted to its ubos form.
    pub value: T,
    /// A digest of the source the board read the setting from. It must
    /// stay the same while the source does, because a new fingerprint
    /// writes the setting over the config.
    pub fingerprint: u32,
}

/// The settings a board recovered from the firmware it replaced.
///
/// Each field the board fills replaces the same setting in the config
/// when its fingerprint is new, and each field it leaves `None` keeps
/// the config's own value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seed {
    /// The pool list, in failover order.
    pub pool: Option<SeedField<heapless::Vec<PoolEntry, { pool::MAX_POOLS }>>>,
    /// The HTTP API login.
    pub api: Option<SeedField<ApiConfig>>,
    /// The IP assignment mode.
    pub network_mode: Option<SeedField<NetworkMode>>,
}

/// The fingerprint of each [`Seed`] field the store last took, or
/// `None` for a field it never took.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Taken {
    pool: Option<u32>,
    api: Option<u32>,
    network_mode: Option<u32>,
}

impl Seed {
    /// Write every field of this seed whose fingerprint differs from
    /// the one in `taken` into `config`, and record its fingerprint in
    /// `taken`.
    pub(crate) fn apply(&self, config: &mut Config, taken: &mut Taken) {
        take(
            "pool",
            self.pool.as_ref(),
            &mut config.pool,
            &mut taken.pool,
        );
        take(
            "api login",
            self.api.as_ref(),
            &mut config.api,
            &mut taken.api,
        );
        take(
            "network mode",
            self.network_mode.as_ref(),
            &mut config.network.mode,
            &mut taken.network_mode,
        );
    }
}

/// Replace `setting` with the seed's `field` when the board filled it
/// and its fingerprint is not the `taken` one, and record the new
/// fingerprint.
fn take<T: Clone>(
    name: &str,
    field: Option<&SeedField<T>>,
    setting: &mut T,
    taken: &mut Option<u32>,
) {
    let Some(field) = field else { return };
    if *taken == Some(field.fingerprint) {
        return;
    }
    *setting = field.value.clone();
    *taken = Some(field.fingerprint);
    info!("config: {} set from the seed", name);
}
