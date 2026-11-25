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

use std::env;

/// `ubos_build::cortex_m_board` does not fit a bootloader: this one links its
/// arguments with `rustc-link-arg-bins`, takes `--nmagic`, and reaches defmt
/// only behind a cargo feature.
fn main() {
    ubos_build::copy_memory_x();
    println!("cargo:rerun-if-changed=build.rs");

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    // link-rp.x places the `.boot2` section at ORIGIN(BOOT2), where the RP2040
    // bootrom looks for the second-stage loader whose CRC32 it verifies before
    // handing off control.
    println!("cargo:rustc-link-arg-bins=-Tlink-rp.x");
    if env::var("CARGO_FEATURE_DEFMT").is_ok() {
        println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
    }
}
