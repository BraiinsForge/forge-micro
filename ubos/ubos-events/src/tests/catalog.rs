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
