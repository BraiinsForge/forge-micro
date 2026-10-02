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

// A temperature takes the width of its own kind, so the browser knows to
// show it in degrees, and its payload is the millicelsius.
#[test]
fn a_temperature_is_catalogued_as_a_temperature_and_stored_in_millicelsius() {
    struct Temperature(i32);
    impl Temperature {
        const fn millicelsius(&self) -> i32 {
            self.0
        }
    }
    assert_eq!(
        crate::__arg_width!(Temperature),
        crate::ArgWidth::Temperature
    );
    assert_eq!(
        crate::__arg_value!(Temperature, Temperature(95_125)),
        crate::Arg::I32(95_125)
    );
}

#[test]
fn a_unit_is_catalogued_as_its_kind_and_stored_as_its_integer() {
    struct Frequency(u32);
    impl Frequency {
        const fn khz(&self) -> u32 {
            self.0
        }
    }
    struct Voltage(u32);
    impl Voltage {
        const fn mv(&self) -> u32 {
            self.0
        }
    }
    struct Power(u32);
    impl Power {
        const fn mw(&self) -> u32 {
            self.0
        }
    }
    struct Efficiency(u32);
    impl Efficiency {
        const fn mj_per_th(&self) -> u32 {
            self.0
        }
    }
    assert_eq!(crate::__arg_width!(Frequency), crate::ArgWidth::Frequency);
    assert_eq!(crate::__arg_width!(Voltage), crate::ArgWidth::Voltage);
    assert_eq!(crate::__arg_width!(Power), crate::ArgWidth::Power);
    assert_eq!(crate::__arg_width!(Efficiency), crate::ArgWidth::Efficiency);
    assert_eq!(
        crate::__arg_value!(Frequency, Frequency(487_500)),
        crate::Arg::U32(487_500)
    );
    assert_eq!(
        crate::__arg_value!(Voltage, Voltage(1_405)),
        crate::Arg::U32(1_405)
    );
    assert_eq!(
        crate::__arg_value!(Power, Power(44_000)),
        crate::Arg::U32(44_000)
    );
    assert_eq!(
        crate::__arg_value!(Efficiency, Efficiency(40_000)),
        crate::Arg::U32(40_000)
    );
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
    assert!(crate::codes_unique(REGISTRY));

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
