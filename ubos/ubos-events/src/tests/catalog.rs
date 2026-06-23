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
