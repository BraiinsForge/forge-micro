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

//! Build script declaring the compile-time env vars that override the
//! factory-default pool endpoint, so cargo rebuilds this crate when they
//! change.
//!
//! [`crate::Config::for_board`] reads `POOL_USER`, `POOL_HOST`,
//! `POOL_PORT`, and `POOL_PROTOCOL` through `option_env!`. Cargo does not track `option_env!`
//! inputs on its own, so without these `rerun-if-env-changed` lines a
//! `POOL_USER=braiinstest cargo run` could silently reuse a stale build
//! that still has the previous (or empty) worker baked in.
fn main() {
    println!("cargo:rerun-if-env-changed=POOL_USER");
    println!("cargo:rerun-if-env-changed=POOL_HOST");
    println!("cargo:rerun-if-env-changed=POOL_PORT");
    println!("cargo:rerun-if-env-changed=POOL_PROTOCOL");
}
