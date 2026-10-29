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

//! The versions of the config schema, and the steps between them.
//!
//! A record is the 8-byte schema hash of the [`Config`] that wrote it,
//! then the postcard payload. A firmware update that changes the schema
//! would lose the operator's config, so each change to the schema adds a
//! version to [`VERSIONS`], and a build converts the record of an older
//! version up to the live config one step at a time. Before an OTA
//! installs an image of an older version, the running firmware converts
//! the live config down to the record of that version, so the older
//! firmware reads it.
//!
//! # Layout
//!
//! Version K lives in a folder `vK`, with a file for each domain of the
//! config that version K changed. The file defines the types and
//! capacities of its domain that changed, with the conversions from and
//! to version K-1, and re-exports the rest of its domain from version
//! K-1. `vK/mod.rs` re-exports the domains that did not change. Thus
//! `vK` names every part of its schema, and what it defines is what K
//! changed. Version 1 defines all of them. `vK/mod.rs` also holds the
//! `Config` of version K, the [`Step`] from version K-1, and the
//! [`Versioned`] hash with an assertion that the `Config` has it.
//!
//! [`live`] is the version the firmware runs, and the newest entry in
//! [`VERSIONS`]. The domain modules of the crate re-export their types
//! from it, and hold every impl on them with the bounds and the factory
//! defaults. Only the live config is built from defaults and validated,
//! so a version defines its types alone and a new version moves no impl.
//!
//! # Changing the schema
//!
//! A version that master holds does not change, because a build of it
//! may have stored its record already. Its hash assertion stops the
//! compile of an edit to its schema, and its record fixture fails the
//! tests of an edit to its hash. The first change after version K
//! reached master:
//!
//! 1. Add `vK+1`, with a file for each domain that changes. Define the
//!    changed types there, with `From` conversions from and to their
//!    `vK` forms, and every type that holds one, up to `Config`.
//!    Re-export the rest from `vK`.
//! 2. Implement [`Step`] for `vK::Config` in `vK+1/mod.rs`, with
//!    `vK+1::Config` as its `Next`. Write complete struct literals in the
//!    step and the conversions, so a field added later stops their
//!    compile.
//! 3. Add the hash of `vK+1::Config` and the hash assertion to
//!    `vK+1/mod.rs`. Write the hash as a byte literal, so the assertion
//!    compares it with the schema rather than with itself.
//! 4. Add `Version::of::<vK+1::Config>(K+1)` to the end of
//!    [`VERSIONS`], point [`live`] at `vK+1`, and bring the impls in the
//!    domain modules to the changed types.
//! 5. Add a record fixture of version K+1 to the tests, with a test that
//!    it loads as a stated [`Config`].
//! 6. Change the test of the fixture of version K so that it upgrades
//!    the fixture to a stated [`Config`].
//! 7. Add a test that converts a [`Config`] down to version K and up
//!    again and gets the original, with the fields that version K lacks
//!    at their defaults.
//!
//! A later change on the same branch, before `vK+1` reaches master,
//! changes `vK+1`, its step, its hash and its fixture.
//!
//! # Pruning
//!
//! The versions older than the oldest one that a supported firmware
//! stores can go, after the definitions that a later version re-exports
//! from them move into the oldest version left. The schema hash symbol in
//! the image of that firmware holds the hash of its version. An OTA to a
//! deleted version then boots on the defaults and the seed, and an OTA to
//! an unrecognized image writes the oldest version left.
//!
//! The `dead_code` expectation below holds while an item here has no
//! caller. Delete it once the first version is listed.

// No version is listed yet, so the steps and the conversions have no
// caller outside the tests until the first one is.
#![cfg_attr(not(test), expect(dead_code))]

use serde::{Serialize, de::DeserializeOwned};

use crate::Config;
use crate::storage::encode_record;

pub(crate) mod v1;

/// The version whose schema the firmware runs. The domain modules
/// re-export their types from it, so a new version points this at itself
/// and nothing else moves.
pub(crate) use self::v1 as live;

/// Every version whose record this build reads, oldest first. The
/// newest entry is the live version.
pub(crate) const VERSIONS: &[Version] = &[];

/// The schema of a version.
pub(crate) trait Versioned: Serialize + DeserializeOwned {
    /// The schema hash in front of a record of this version.
    const HASH: [u8; 8];
}

/// One step from the schema of a version to the schema of the next.
pub(crate) trait Step: Versioned {
    /// The schema of the next version.
    type Next: Live;

    /// This config in the schema after it.
    fn up(self) -> Self::Next;

    /// `next` in this schema, without the fields this schema lacks.
    fn down(next: &Self::Next) -> Self;
}

/// A schema that converts to and from the live config.
///
/// The live [`Config`] converts as itself, and every [`Step`] through
/// the steps after it, so no code spells a chain of steps.
pub(crate) trait Live: Sized {
    /// This config as the live config.
    fn into_live(self) -> Config;

    /// `live` in this schema.
    fn from_live(live: &Config) -> Self;
}

impl Live for Config {
    fn into_live(self) -> Config {
        self
    }

    fn from_live(live: &Config) -> Self {
        live.clone()
    }
}

impl<T: Step> Live for T {
    fn into_live(self) -> Config {
        self.up().into_live()
    }

    fn from_live(live: &Config) -> Self {
        T::down(&T::Next::from_live(live))
    }
}

/// A version and the conversions between its record and the live
/// config.
pub(crate) struct Version {
    /// The number of the version, for the log and the events.
    pub(crate) number: u16,
    /// The schema hash in front of a record of the version.
    pub(crate) hash: [u8; 8],
    /// The live config in a payload, a record of this version without
    /// its hash, or `None` when the payload does not decode.
    pub(crate) upgrade: fn(&[u8]) -> Option<Config>,
    /// Encode the live config as a record of this version into the
    /// buffer, and return the record length, or `None` when it does not
    /// fit.
    pub(crate) downgrade: fn(&Config, &mut [u8]) -> Option<usize>,
}

impl Version {
    /// Version `number`, whose schema is `T`.
    pub(crate) const fn of<T: Versioned + Live>(number: u16) -> Self {
        Self {
            number,
            hash: T::HASH,
            upgrade: upgrade::<T>,
            downgrade: downgrade::<T>,
        }
    }
}

/// The payload of a `T` record converted to the live config.
fn upgrade<T: Versioned + Live>(payload: &[u8]) -> Option<Config> {
    postcard::from_bytes::<T>(payload).ok().map(Live::into_live)
}

/// The live config encoded as a `T` record.
fn downgrade<T: Versioned + Live>(config: &Config, buffer: &mut [u8]) -> Option<usize> {
    encode_record(T::HASH, &T::from_live(config), buffer).ok()
}

/// The version older than the newest of `versions`, oldest first, whose
/// schema is `hash`, or `None` when there is none.
///
/// The newest entry is the build's own version, which needs no step.
pub(crate) fn older_version(hash: [u8; 8], versions: &[Version]) -> Option<&Version> {
    let (_, older) = versions.split_last()?;
    older.iter().find(|version| version.hash == hash)
}

/// The config that the image an OTA staged reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum StagedConfig {
    /// A ubos image that declares this schema hash.
    Schema([u8; 8]),
    /// An image that the board does not recognize as ubos, which leaves
    /// the ubos record in place.
    Unrecognized,
}

/// The version whose record a build of the newest of `versions` writes
/// before it reboots into an image that reads `staged`, or `None` when
/// it writes nothing.
///
/// An unrecognized image takes the oldest version, so a ubos image
/// installed after it upgrades from that record. A build writes nothing
/// for its own schema or for one it does not know: a newer image upgrades
/// from the stored record, and any other image boots on the defaults and
/// the seed.
pub(crate) fn downgrade_version(staged: StagedConfig, versions: &[Version]) -> Option<&Version> {
    let target = match staged {
        StagedConfig::Schema(hash) => hash,
        StagedConfig::Unrecognized => versions.first()?.hash,
    };
    older_version(target, versions)
}

/// The schema hash of this build, under a symbol name that a packer
/// reads from the ELF.
///
/// A packer copies the hash into the update header, so the firmware
/// that installs the update knows which record the new image reads.
/// `pkgs/config-schema-hash` reads the symbol. The linker refuses a
/// second definition of the name, so an image holds one hash. `used`
/// keeps the static through the link, because nothing in the firmware
/// reads it.
#[used]
#[unsafe(export_name = "UBOS_CONFIG_SCHEMA_HASH")]
static SCHEMA_HASH_SYMBOL: [u8; 8] = Config::SCHEMA_HASH;

/// The schema hash of `T` as a record of its version holds it.
///
/// The hash covers the type and field names as well as the layout, and
/// every version hashes its `Config` under the one path, so a frozen
/// copy of a type hashes as the original did.
pub(crate) const fn schema_hash<T: postcard_schema::Schema + ?Sized>() -> [u8; 8] {
    postcard_schema::key::hash::fnv1a64::hash_ty_path::<T>("ubos_config::Config")
}
