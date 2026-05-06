//! Stratum V2 elementary mining messages — borrowed-first, `no_std`.
//!
//! See `docs/superpowers/specs/2026-05-06-messages-design.md`.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

use ii_sv2_framing::ExtType;

/// SV2 extension byte for the basic mining protocol.
pub const MINING_EXTENSION_TYPE: ExtType = 0x00;

use ii_sv2_types::{Decode, Encode, Error as TypeError, Str0_255};

/// Telemetry the client provides to the server during connection setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceInfo<'a> {
    /// E.g. "Bitmain"
    pub vendor: Str0_255<'a>,
    /// E.g. "S9i 13.5"
    pub hw_rev: Str0_255<'a>,
    /// E.g. "braiins-os-2018-09-22-1-hash"
    pub fw_ver: Str0_255<'a>,
    /// Vendor-defined unique device id; clients may set to "" for telemetry-off.
    pub dev_id: Str0_255<'a>,
}

impl<'a> Encode for DeviceInfo<'a> {
    fn encoded_len(&self) -> usize {
        self.vendor.encoded_len()
            + self.hw_rev.encoded_len()
            + self.fw_ver.encoded_len()
            + self.dev_id.encoded_len()
    }

    fn encode_to(&self, out: &mut [u8]) -> Result<usize, TypeError> {
        let mut n = 0;
        n += self.vendor.encode_to(&mut out[n..])?;
        n += self.hw_rev.encode_to(&mut out[n..])?;
        n += self.fw_ver.encode_to(&mut out[n..])?;
        n += self.dev_id.encode_to(&mut out[n..])?;
        Ok(n)
    }
}

impl<'de> Decode<'de> for DeviceInfo<'de> {
    fn decode(input: &mut &'de [u8]) -> Result<Self, TypeError> {
        Ok(Self {
            vendor: Str0_255::decode(input)?,
            hw_rev: Str0_255::decode(input)?,
            fw_ver: Str0_255::decode(input)?,
            dev_id: Str0_255::decode(input)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ii_sv2_types::{Decode, Encode, Str0_255};

    #[test]
    fn device_info_roundtrip() {
        let original = DeviceInfo {
            vendor: Str0_255::try_from("Bitmain").unwrap(),
            hw_rev: Str0_255::try_from("S9i 13.5").unwrap(),
            fw_ver: Str0_255::try_from("braiins-os-2018").unwrap(),
            dev_id: Str0_255::try_from("dev-001").unwrap(),
        };
        let mut buf = [0u8; 64];
        let n = original.encode_to(&mut buf).unwrap();
        let mut cursor: &[u8] = &buf[..n];
        let decoded = DeviceInfo::decode(&mut cursor).unwrap();
        assert_eq!(decoded, original);
        assert!(cursor.is_empty());
    }
}
