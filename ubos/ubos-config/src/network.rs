//! Network interface configuration and validation.
//!
//! Defines [`NetworkMode`] (DHCP vs. static assignment) and
//! [`NetworkConfig`], which is stored alongside the rest of
//! [`crate::Config`] and loaded at boot before the network stack
//! is created.

/// Maximum length in bytes of a network hostname.
pub const HOSTNAME_MAX: usize = 32;

/// Selects the IP address assignment mode and carries the parameters
/// for each variant.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum NetworkMode {
    /// Address is assigned by a DHCP server on the local network.
    #[default]
    Dhcp,
    /// Address is configured statically by the operator.
    Static {
        /// IPv4 address as four octets.
        address: [u8; 4],
        /// Network prefix length (1–32).
        prefix_len: u8,
        /// Default gateway address.
        gateway: [u8; 4],
        /// DNS resolver addresses; up to three entries.
        dns: heapless::Vec<[u8; 4], 3>,
    },
}

/// Network interface parameters for both IP assignment and hostname.
///
/// Stored as part of [`crate::Config`] and applied at boot before the
/// network stack is initialised so the embassy-net stack sees the
/// correct mode on its first poll.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NetworkConfig {
    /// IP assignment mode; defaults to DHCP.
    pub mode: NetworkMode,
    /// mDNS hostname for the device; empty means use the build-time
    /// default.
    pub hostname: heapless::String<HOSTNAME_MAX>,
}

impl NetworkConfig {
    /// Validate the network configuration.
    ///
    /// An empty hostname is accepted as a sentinel for "use the
    /// build-time mDNS base name". A non-empty hostname must satisfy
    /// RFC-1123 label rules: each character must be an ASCII
    /// alphanumeric or a hyphen, and the first and last characters
    /// must not be hyphens.
    ///
    /// For [`NetworkMode::Static`], the address and gateway must be
    /// non-zero (`0.0.0.0` is not a valid host address), and
    /// `prefix_len` must be in the range 1–32.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a static operator-readable message on the
    /// first field that fails its rule.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.hostname.is_empty() {
            if !self
                .hostname
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err("network.hostname must contain only letters, digits, and hyphens");
            }
            let bytes = self.hostname.as_bytes();
            if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
                return Err("network.hostname must not start or end with a hyphen");
            }
        }

        let NetworkMode::Static {
            address,
            prefix_len,
            gateway,
            ..
        } = &self.mode
        else {
            return Ok(());
        };

        if *address == [0u8; 4] {
            return Err("network.address must not be 0.0.0.0");
        }
        if *gateway == [0u8; 4] {
            return Err("network.gateway must not be 0.0.0.0");
        }
        if !(1..=32).contains(prefix_len) {
            return Err("network.prefix_len must be between 1 and 32");
        }

        Ok(())
    }
}
