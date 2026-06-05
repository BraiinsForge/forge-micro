//! HTTP API credentials configuration.
//!
//! Defines [`ApiConfig`] for the HTTP API username and password with
//! its custom [`Default`] impl.

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
