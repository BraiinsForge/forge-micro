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

//! Generic, protocol-agnostic job identifier.
//!
//! Adapters assign a fresh `JobId` for each new job they surface to
//! the visitor and keep an internal map from `JobId` to the wire-form
//! id (V1 string, V2 u32). Consumers downstream of the trait
//! boundary (`stratum-client`, solvers) only ever see `JobId` and
//! never touch either wire form.

/// 32-bit opaque token correlating a stratum job, its outstanding
/// work assignments, and the eventual share submission.
///
/// `Copy` is intentional: this is a 4-byte handle, not an owning
/// reference.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct JobId(pub u32);
