//! `Sv2Config` construction tests.

use crate::config::{Sv2Config, UserTooLong, MAX_USER_LEN};

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
