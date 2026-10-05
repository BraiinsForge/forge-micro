// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Pool endpoint configuration and validation.
//!
//! Defines [`PoolEntry`], [`ProtocolKind`], the factory-default pool
//! constants, and per-entry validation.

/// Factory-default pool host for [`crate::Config::default`]. Overridable at
/// compile time by setting the `POOL_HOST` env var; a stored config
/// in flash supersedes both.
pub const DEFAULT_POOL_HOST: &str = "stratum.braiins.com";

/// Factory-default pool port. Overridable via the `POOL_PORT` env var.
pub const DEFAULT_POOL_PORT: u16 = 3333;

/// Factory-default worker name. Empty by default as a sentinel meaning
/// "operator has not configured a worker yet" — most pools accept an
/// empty worker and authorise on a default account, so this fails open;
/// the dashboard should prompt the operator to set a real worker before
/// shipping. Overridable via the `POOL_USER` env var.
pub const DEFAULT_POOL_USER: &str = "";

/// Factory-default SV2 authority public key for `stratum.braiins.com`,
/// pre-decoded from base58check `9awtMD5KQgvRUh2yFbjVeT7b6hjipWcAsQHd6wEhgtDT9soosna`
/// to its 32-byte xonly form. An unconfigured firmware reaches the
/// Braiins prod endpoint with `ValidateSignature` rather than
/// `ValidateNothing`.
pub const DEFAULT_POOL_SV2_PUBKEY: [u8; 32] = [
    0x29, 0x95, 0x4c, 0x97, 0x45, 0x6b, 0x59, 0x9c, 0x83, 0x6f, 0x70, 0xf5, 0x08, 0xd6, 0x19, 0xd3,
    0x28, 0x52, 0x7b, 0xb8, 0x72, 0x29, 0xd7, 0xd7, 0xd2, 0xe6, 0x7e, 0x6d, 0xcd, 0x18, 0xeb, 0xcf,
];

/// Factory-default protocol for a `POOL_PROTOCOL` env value.
///
/// `1`, `sv1` and `stratum` select SV1 with no password. `2`, `sv2`,
/// `stratum2`, an unset variable, and any other value select SV2 with
/// [`DEFAULT_POOL_SV2_PUBKEY`], the same fallback `POOL_PORT` takes.
#[must_use]
pub(crate) fn default_protocol(raw: Option<&str>) -> ProtocolKind {
    match raw {
        Some("1" | "sv1" | "stratum") => ProtocolKind::Sv1 { password: None },
        _ => ProtocolKind::Sv2 {
            pool_pubkey: Some(DEFAULT_POOL_SV2_PUBKEY),
        },
    }
}

/// Format-aware serde for the SV2 authority pubkey.
///
/// JSON (REST, `is_human_readable() == true`) carries the SV2
/// base58check string; postcard (flash, `false`) carries the raw
/// `Option<[u8; 32]>`, byte-identical to a plain field. This is why a
/// stored config survives this change without a schema reset.
mod sv2_pubkey_serde {
    use crate::pubkey;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    // `#[serde(with = "sv2_pubkey_serde")]` on an `Option<[u8; 32]>` field
    // makes serde's derive call this with `&self.pool_pubkey`, i.e.
    // `&Option<[u8; 32]>`; the signature is fixed by that calling
    // convention, not a choice this function can make.
    #[allow(clippy::ref_option)]
    pub fn serialize<S>(value: &Option<[u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if serializer.is_human_readable() {
            match value {
                Some(key) => serializer.serialize_some(pubkey::encode(key).as_str()),
                None => serializer.serialize_none(),
            }
        } else {
            value.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<[u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let encoded = Option::<heapless::String<64>>::deserialize(deserializer)?;
            match encoded {
                Some(text) => pubkey::decode(&text)
                    .map(Some)
                    .map_err(serde::de::Error::custom),
                None => Ok(None),
            }
        } else {
            Option::<[u8; 32]>::deserialize(deserializer)
        }
    }
}

/// Selects the pool mining protocol and carries its protocol-specific
/// parameters.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum ProtocolKind {
    /// Stratum V1 with an optional worker password.
    Sv1 {
        /// `None` means no password sent; most pools authorise by
        /// worker name alone.
        password: Option<heapless::String<128>>,
    },
    /// Stratum V2 with an optional authority public key.
    Sv2 {
        /// `None` means `ValidateNothing`; `Some` means
        /// `ValidateSignature` against the supplied 32-byte key. Over
        /// JSON this is the SV2 base58check string; in flash it is the
        /// raw 32 bytes. See [`crate::pubkey`] and
        /// [`sv2_pubkey_serde`].
        #[serde(with = "sv2_pubkey_serde")]
        pool_pubkey: Option<[u8; 32]>,
    },
}

/// A single pool endpoint entry.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PoolEntry {
    /// DNS name or IPv4 literal of the pool host.
    pub host: heapless::String<128>,
    /// TCP port.
    pub port: u16,
    /// Worker name sent in `mining.authorize`.
    pub user: heapless::String<128>,
    /// Protocol selection and its parameters.
    pub protocol: ProtocolKind,
    /// Scheduler dispatch weight for this slot. Zero marks a
    /// backup-only slot: mined only while no weighted slot has
    /// work. An absent JSON key deserializes as zero, so
    /// weightless configs keep the primary-plus-backups
    /// behavior. See the config-driven weights section of
    /// `docs/MULTIPOOL_DESIGN.md`.
    #[serde(default)]
    pub weight: u32,
}

/// Maximum number of pool slots the firmware persists and validates.
///
/// Drives both the `heapless::Vec<PoolEntry, MAX_POOLS>` capacity in
/// [`crate::Config`] / [`crate::ConfigUpdate`] and the length of the
/// per-slot error-message tables below, so growing the slot count is
/// a single-const change that fails to compile until every table is
/// extended in step.
pub const MAX_POOLS: usize = 3;

/// Upper bound on [`PoolEntry::weight`]. Keeps
/// `weight_sum * (CLAMP_SHARES + 1)` inside `i32` for any slot count up
/// to 8, so the `ubos-mining` scheduler arithmetic cannot overflow on
/// validated input.
pub const MAX_POOL_WEIGHT: u32 = 100_000;

const POOL_HOST_EMPTY: [&str; MAX_POOLS] = [
    "pool 1: host must not be empty",
    "pool 2: host must not be empty",
    "pool 3: host must not be empty",
];

const POOL_PORT_ZERO: [&str; MAX_POOLS] = [
    "pool 1: port must be greater than 0",
    "pool 2: port must be greater than 0",
    "pool 3: port must be greater than 0",
];

const POOL_WEIGHT_RANGE: [&str; MAX_POOLS] = [
    "pool 1: weight must be at most 100000",
    "pool 2: weight must be at most 100000",
    "pool 3: weight must be at most 100000",
];

impl PoolEntry {
    /// Validate this entry against basic connectivity requirements.
    ///
    /// `slot` is the zero-based index into the pool list; the error
    /// message includes the human-readable slot number so the operator
    /// knows which of the UI rows failed. The method covers the full
    /// [`MAX_POOLS`] capacity; out-of-range slot indices fall back to a
    /// generic message rather than panicking, so the public API stays
    /// total even if a future caller iterates an unbounded source.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the host is empty, the port is zero, or the
    /// weight exceeds [`MAX_POOL_WEIGHT`].
    pub fn validate(&self, slot: usize) -> Result<(), &'static str> {
        if self.host.is_empty() {
            return Err(POOL_HOST_EMPTY
                .get(slot)
                .copied()
                .unwrap_or("pool: host must not be empty"));
        }
        if self.port == 0 {
            return Err(POOL_PORT_ZERO
                .get(slot)
                .copied()
                .unwrap_or("pool: port must be greater than 0"));
        }
        if self.weight > MAX_POOL_WEIGHT {
            return Err(POOL_WEIGHT_RANGE
                .get(slot)
                .copied()
                .unwrap_or("pool: weight must be at most 100000"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod pubkey_serde_tests {
    use super::*;

    const KEY_B58: &str = "9awtMD5KQgvRUh2yFbjVeT7b6hjipWcAsQHd6wEhgtDT9soosna";

    fn sv2_entry(pool_pubkey: Option<[u8; 32]>) -> PoolEntry {
        PoolEntry {
            host: heapless::String::try_from("stratum.braiins.com")
                .expect("BUG: host literal fits String<128>"),
            port: 3333,
            user: heapless::String::try_from("worker").expect("BUG: user literal fits String<128>"),
            protocol: ProtocolKind::Sv2 { pool_pubkey },
            weight: 1,
        }
    }

    #[test]
    fn pool_protocol_selects_sv1_by_its_names() {
        for raw in ["1", "sv1", "stratum"] {
            assert_eq!(
                default_protocol(Some(raw)),
                ProtocolKind::Sv1 { password: None },
                "{raw}"
            );
        }
    }

    #[test]
    fn pool_protocol_falls_back_to_sv2_with_the_default_key() {
        let sv2 = ProtocolKind::Sv2 {
            pool_pubkey: Some(DEFAULT_POOL_SV2_PUBKEY),
        };
        for raw in [None, Some("2"), Some("sv2"), Some("stratum2"), Some("x")] {
            assert_eq!(default_protocol(raw), sv2, "{raw:?}");
        }
    }

    #[test]
    fn json_emits_base58_string() {
        let entry = sv2_entry(Some(DEFAULT_POOL_SV2_PUBKEY));
        let mut buf = [0u8; 256];
        let len = serde_json_core::to_slice(&entry, &mut buf).expect("encode must succeed");
        let json = core::str::from_utf8(&buf[..len]).expect("BUG: json output is ASCII");
        assert!(
            json.contains(alloc_free_quote(KEY_B58).as_str()),
            "expected base58 string in {json}"
        );
        // The raw byte-array form must be gone.
        assert!(!json.contains("[41,149"), "byte array leaked: {json}");
    }

    #[test]
    fn json_round_trips_some() {
        let entry = sv2_entry(Some(DEFAULT_POOL_SV2_PUBKEY));
        let mut buf = [0u8; 256];
        let len = serde_json_core::to_slice(&entry, &mut buf).expect("encode must succeed");
        let (decoded, _) =
            serde_json_core::from_slice::<PoolEntry>(&buf[..len]).expect("decode must succeed");
        assert_eq!(decoded, entry);
    }

    #[test]
    fn json_round_trips_none_as_null() {
        let entry = sv2_entry(None);
        let mut buf = [0u8; 256];
        let len = serde_json_core::to_slice(&entry, &mut buf).expect("encode must succeed");
        let json = core::str::from_utf8(&buf[..len]).expect("BUG: json output is ASCII");
        assert!(json.contains("null"), "expected null pubkey in {json}");
        let (decoded, _) =
            serde_json_core::from_slice::<PoolEntry>(&buf[..len]).expect("decode must succeed");
        assert_eq!(decoded, entry);
    }

    #[test]
    fn json_rejects_bad_pubkey() {
        let body = br#"{"host":"h","port":3333,"user":"u","weight":1,"protocol":{"Sv2":{"pool_pubkey":"notvalid!!"}}}"#;
        assert!(serde_json_core::from_slice::<PoolEntry>(body).is_err());
    }

    #[test]
    fn postcard_keeps_raw_bytes() {
        // The postcard branch must be byte-identical to serializing the
        // field as a plain Option<[u8; 32]> — 0x01 tag then 32 bytes.
        let entry = sv2_entry(Some(DEFAULT_POOL_SV2_PUBKEY));
        let mut buf = [0u8; 256];
        let bytes = postcard::to_slice(&entry, &mut buf).expect("encode must succeed");
        // The 32 key bytes appear verbatim (no base58 ASCII).
        assert!(
            bytes.windows(32).any(|w| w == DEFAULT_POOL_SV2_PUBKEY),
            "raw key bytes missing from postcard output"
        );
    }

    /// Wrap a string in JSON double quotes without allocation.
    fn alloc_free_quote(s: &str) -> heapless::String<64> {
        let mut out = heapless::String::new();
        out.push('"').expect("BUG: quote fits String<64>");
        out.push_str(s)
            .expect("BUG: 52-char base58 fits String<64>");
        out.push('"').expect("BUG: quote fits String<64>");
        out
    }
}
