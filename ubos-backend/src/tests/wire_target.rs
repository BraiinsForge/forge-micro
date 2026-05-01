use super::{wire_target_for, LOCAL_TARGET};

#[test]
fn wire_target_picks_local_when_pool_is_harder() {
    // Pool floor in regtest: 16 leading zero bits.
    let mut pool = [0xFFu8; 32];
    pool[31] = 0x00;
    pool[30] = 0x00;
    assert_eq!(wire_target_for(&pool), LOCAL_TARGET);
}

#[test]
fn wire_target_picks_pool_when_pool_is_easier() {
    // Pool unusually easy: 4 leading zero bits.
    let mut pool = [0xFFu8; 32];
    pool[31] = 0x0F;
    assert_eq!(wire_target_for(&pool), pool);
}

#[test]
fn wire_target_handles_equal_targets() {
    assert_eq!(wire_target_for(&LOCAL_TARGET), LOCAL_TARGET);
}
