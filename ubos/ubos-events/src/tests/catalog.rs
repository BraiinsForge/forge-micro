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

//! Tests for catalog code uniqueness across a representative set of event
//! declarations, verifying that `const_fnv1a` produces no collisions.

// The generated emit functions are only used via CATALOG in the uniqueness
// test below, so suppress the dead-code lint for the inner module.
#[allow(dead_code)]
mod inner {
    crate::events! {
        boot_complete()            info  = "Boot complete",
        mining_halted(reason: u8)  warn  = "Mining halted: {}",
        sensor_fault()             error = "Temperature sensor fault",
        temp_warning(temp_c: u16)  warn  = "Temperature warning at {} °C",
    }
}

#[test]
fn catalog_codes_are_unique() {
    let codes: Vec<u32> = inner::events::CATALOG.iter().map(|def| def.code).collect();
    let mut deduped = codes.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(
        codes.len(),
        deduped.len(),
        "duplicate event codes in CATALOG"
    );
}

/// Firmware-style aggregation of per-crate catalogs into one registry,
/// expressed in a `const` to prove `CATALOG` composes in a const context.
const REGISTRY: &[&[crate::EventDef]] = &[inner::events::CATALOG];

#[test]
fn registry_codes_are_globally_unique() {
    // Mirrors the compile-time assertion firmware applies to the assembled
    // registry: codes must not collide across catalog boundaries.
    assert!(crate::codes_unique(REGISTRY));

    // A catalog repeated across the registry collides with itself.
    let dup: &[&[crate::EventDef]] = &[inner::events::CATALOG, inner::events::CATALOG];
    assert!(!crate::codes_unique(dup));
}

#[test]
fn set_and_get_catalogs() {
    crate::set_catalogs(REGISTRY);
    let catalogs = crate::catalogs();
    assert_eq!(catalogs.len(), 1);
    assert_eq!(catalogs[0].len(), inner::events::CATALOG.len());
}
