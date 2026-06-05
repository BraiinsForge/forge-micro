//! HTTP API credentials configuration and validation.
//!
//! Defines [`ApiConfig`] for the HTTP API username and password, with
//! its custom [`Default`] impl and a [`validate`][ApiConfig::validate]
//! method.

/// HTTP API credentials.
///
/// `heapless::String<N>` has a `const fn new()` but no const
/// constructor that accepts content, so `ApiConfig::default` is a
/// regular `Default` impl rather than a `const`. The shadow step
/// publishes the value at runtime; no call site needs it at
/// compile time.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ApiConfig {
    /// Login username; maximum 32 bytes.
    pub username: heapless::String<32>,
    /// Login password; maximum 32 bytes.
    pub password: heapless::String<32>,
}

impl Default for ApiConfig {
    fn default() -> Self {
        let mut username = heapless::String::new();
        let mut password = heapless::String::new();
        // "root" is 4 bytes; both fields have 32-byte capacity, so
        // push_str cannot return an error.
        username
            .push_str("root")
            .expect("BUG: \"root\" exceeds ApiConfig::username capacity");
        // Same invariant: 4-byte literal into a 32-byte heapless::String.
        password
            .push_str("root")
            .expect("BUG: \"root\" exceeds ApiConfig::password capacity");
        Self { username, password }
    }
}

impl ApiConfig {
    /// Validate the credentials.
    ///
    /// Username must be non-empty and consist entirely of printable
    /// ASCII characters. Password may be empty as the "keep existing"
    /// sentinel; if non-empty it must also be all printable ASCII.
    ///
    /// The empty-password exception is load-bearing for the wire
    /// round-trip: the `POST /api/config` handler in `ubos-api`
    /// substitutes the stored password when the incoming field is
    /// empty before calling this validator, so the SPA can edit the
    /// username without forcing the operator to re-type the password.
    /// Tightening this rule to reject empty passwords would break
    /// that round-trip; any tighter policy must land together with a
    /// matching update to the `rest.rs` substitution path.
    ///
    /// # Errors
    ///
    /// Returns the first validation error as a static
    /// operator-readable string.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.username.is_empty() {
            return Err("api.username must not be empty");
        }
        if !self.username.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("api.username must contain only printable ASCII characters");
        }
        if !self.password.is_empty() && !self.password.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err("api.password must contain only printable ASCII characters");
        }
        Ok(())
    }
}
