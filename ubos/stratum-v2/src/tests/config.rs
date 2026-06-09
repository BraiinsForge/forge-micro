//! `Sv2Config` construction tests.

use crate::config::{Sv2Config, UserTooLong, MAX_USER_LEN};

#[test]
fn new_stores_user_identity() {
    match Sv2Config::new("bc1q.worker", None) {
        Ok(config) => {
            assert_eq!(config.user.as_str(), "bc1q.worker");
            assert_eq!(config.pool_pubkey, None);
        }
        Err(err) => panic!("valid user should fit: {err:?}"),
    }
}

#[test]
fn new_stores_pool_pubkey() {
    let pubkey = [0x42; 32];
    match Sv2Config::new("bc1q.worker", Some(pubkey)) {
        Ok(config) => assert_eq!(config.pool_pubkey, Some(pubkey)),
        Err(err) => panic!("valid user should fit: {err:?}"),
    }
}

#[test]
fn new_accepts_max_length_user() {
    let user = "x".repeat(MAX_USER_LEN);
    match Sv2Config::new(&user, None) {
        Ok(config) => assert_eq!(config.user.len(), MAX_USER_LEN),
        Err(err) => panic!("max length user should fit: {err:?}"),
    }
}

#[test]
fn new_rejects_overlong_user() {
    let user = "x".repeat(MAX_USER_LEN + 1);

    assert_eq!(Sv2Config::new(&user, None).err(), Some(UserTooLong));
}
