//! V1-specific session config; the firmware passes a reference into
//! `StratumV1Client::new`. Strings are owned so the config can be
//! built fresh from a runtime [`ubos_config::PoolEntry`] each
//! reconnect, instead of borrowing from a compile-time static.

use heapless::String;

/// Maximum byte length of the worker name field. Matches
/// [`ubos_config::PoolEntry::user`] so a `PoolEntry` always fits.
pub const MAX_USER_LEN: usize = 128;

/// Maximum byte length of the password field. Matches the V1
/// `ProtocolKind::Sv1` password capacity so a `PoolEntry` always
/// fits.
pub const MAX_PASS_LEN: usize = 128;

/// `Sv1Config::new` rejects a user or password that does not fit in
/// the fixed-size buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldTooLong {
    User,
    Password,
}

/// Pool credentials for a Stratum V1 session. Owned strings so the
/// config carries no borrow on its source; callers can construct it
/// from a [`ubos_config::PoolEntry`] each reconnect attempt.
pub struct Sv1Config {
    pub user: String<MAX_USER_LEN>,
    pub password: String<MAX_PASS_LEN>,
}

impl Sv1Config {
    /// Construct an `Sv1Config` from `&str` slices.
    ///
    /// # Errors
    ///
    /// Returns `Err(FieldTooLong::User)` if `user` exceeds
    /// [`MAX_USER_LEN`] bytes, `Err(FieldTooLong::Password)` if
    /// `password` exceeds [`MAX_PASS_LEN`] bytes.
    pub fn new(user: &str, password: &str) -> Result<Self, FieldTooLong> {
        let mut user_owned = String::new();
        user_owned.push_str(user).map_err(|_| FieldTooLong::User)?;
        let mut password_owned = String::new();
        password_owned
            .push_str(password)
            .map_err(|_| FieldTooLong::Password)?;
        Ok(Self {
            user: user_owned,
            password: password_owned,
        })
    }
}
