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

//! The versions of the config schema, each in a folder of its own.
//!
//! Version K lives in a folder `vK`, with a file for each domain of the
//! config that version K changed. The file defines the types and
//! capacities of its domain that changed, and re-exports the rest of its
//! domain from version K-1. `vK/mod.rs` re-exports the domains that did
//! not change. Thus `vK` names every part of its schema, and what it
//! defines is what K changed. Version 1 defines all of them. `vK/mod.rs`
//! also holds the `Config` of version K.
//!
//! [`live`] is the version the firmware runs. The domain modules of the
//! crate re-export their types from it, and hold every impl on them with
//! the bounds and the factory defaults. Only the live config is built
//! from defaults and validated, so a version defines its types alone and
//! a new version moves no impl.

pub(crate) mod v1;

/// The version whose schema the firmware runs. The domain modules
/// re-export their types from it, so a new version points this at itself
/// and nothing else moves.
pub(crate) use self::v1 as live;
