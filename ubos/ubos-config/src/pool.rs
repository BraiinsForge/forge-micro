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
//! Re-exports [`PoolEntry`], [`ProtocolKind`] and their capacities from
//! the live version of the config schema. Defines the factory-default
//! pool constants, the fee switch and the fee slot's place, and
//! per-entry validation. The fee pool itself is [`crate::fee`].

pub use crate::versions::live::{MAX_POOLS, NAME_CAPACITY, PoolEntry, ProtocolKind};

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
pub(crate) mod sv2_pubkey_serde {
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

/// Build error for a `DEVFEE` that is none of unset, `0` and `1`.
const DEVFEE_BAD: &str = "DEVFEE must be 0, 1 or unset";

/// Whether this build ships the fee slot. `DEVFEE=1` at build time
/// turns it on, and `DEVFEE=0` or unset leaves it off. Any other
/// value stops the build, so a misspelt value cannot pass for off. A
/// build-time const rather than a cargo feature, because a feature
/// unifies across every crate of one cargo invocation and this value
/// is one board's. A crate that ships the fee states `1` in its own
/// cargo config, and `0` on the command is what turns one build of it
/// off, because a value on the command beats the config and nothing
/// can unset what the config adds.
pub const DEVFEE: bool = match option_env!("DEVFEE") {
    None => false,
    Some(value) => {
        // Bytes, because a const cannot compare a `str`.
        match value.as_bytes() {
            b"0" => false,
            b"1" => true,
            _ => panic!("{}", DEVFEE_BAD),
        }
    }
};

/// Session slots the fee adds: one when [`DEVFEE`] is on.
pub const FEE_SLOTS: usize = if DEVFEE { 1 } else { 0 };

/// Session slots a board can run: the operator's list plus the fee
/// slot the build ships. Sizes the receiver budget every session slot
/// draws on.
pub const POOL_SLOTS: usize = MAX_POOLS + FEE_SLOTS;

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

#[cfg(test)]
mod fee_slot_tests {
    use super::*;
    use crate::fee::{FeePool, FeeSlot};
    use crate::{
        ConfigCommandChannel, ConfigWatch, FeeSlotCell, GenerationWatch, Handle, NetworkWatch,
    };
    use embassy_sync::channel::Channel;
    use embassy_sync::once_lock::OnceLock;
    use embassy_sync::watch::Watch;

    /// The pool a fee board states: here one with a fixed worker, so
    /// the entry reads nothing from the build.
    const FEE: FeePool = FeePool {
        host: "fee.example.com",
        port: 4444,
        pubkey: None,
        user: Some("fee.worker"),
        build: "",
        variant: 0,
        permille: 20,
    };

    /// A board with the operator's three slots and the fee.
    const FOUR_SESSIONS: usize = MAX_POOLS + 1;

    /// A handle over channels nothing publishes on, with `fee` as its
    /// fee cell, so a test reads the default config and its own slot.
    fn handle_over(fee: &'static FeeSlotCell) -> Handle {
        static STATE: ConfigWatch = Watch::new();
        static NETWORK: NetworkWatch = Watch::new();
        static COMMANDS: ConfigCommandChannel = Channel::new();
        static GENERATION: GenerationWatch = Watch::new();
        Handle::new(&STATE, &NETWORK, &COMMANDS, &GENERATION, fee)
    }

    #[test]
    fn an_off_build_places_no_fee_and_runs_the_operator_slots_alone() {
        static CELL: FeeSlotCell = OnceLock::new();
        if DEVFEE {
            return;
        }
        assert_eq!(POOL_SLOTS, MAX_POOLS);
        assert_eq!(FeeSlot::last_of(FEE, FOUR_SESSIONS), None);
        let handle = handle_over(&CELL);
        assert_eq!(handle.pool_slot(MAX_POOLS), None);
        assert_eq!(handle.view().fee, None);
        assert_eq!(handle.view().fee_permille, None);
    }

    #[test]
    fn the_fee_is_the_last_slot_of_the_board() {
        if !DEVFEE {
            return;
        }
        let four = FeeSlot::last_of(FEE, FOUR_SESSIONS).expect("the switch is on");
        assert_eq!(four.slot, FOUR_SESSIONS - 1);
        let three = FeeSlot::last_of(FEE, MAX_POOLS).expect("the switch is on");
        assert_eq!(three.slot, MAX_POOLS - 1);
        assert_eq!(FeeSlot::index(&Some(three)), Some(MAX_POOLS - 1));
        assert_eq!(FeeSlot::index(&None), None);
    }

    #[test]
    fn the_handle_reads_the_operator_list_below_the_fee_and_the_fee_at_its_slot() {
        static CELL: FeeSlotCell = OnceLock::new();
        if !DEVFEE {
            return;
        }
        let fee = FeeSlot::last_of(FEE, FOUR_SESSIONS).expect("the switch is on");
        CELL.init(fee).expect("BUG: the cell is this test's own");
        let handle = handle_over(&CELL);
        let config = handle.current();
        assert_eq!(handle.pool_slot(0), config.pool.first().cloned());
        assert_eq!(handle.pool_slot(MAX_POOLS - 1), None);
        assert_eq!(handle.pool_slot(FOUR_SESSIONS - 1), Some(FEE.entry()));
        assert_eq!(handle.pool_slot(FOUR_SESSIONS), None);
        // The GET view serves the fee entry as its own field, beside
        // the operator list, and a POST that echoes it back validates.
        let view = handle.view();
        assert_eq!(view.pool.as_ref(), Some(&config.pool));
        assert_eq!(view.fee, Some(FEE.entry()));
        assert_eq!(view.fee_permille, Some(FEE.permille));
        assert_eq!(view.validate(&config, &crate::mining::BOUNDS), Ok(()));
    }

    #[test]
    fn a_three_session_board_runs_the_fee_at_its_third_slot() {
        static CELL: FeeSlotCell = OnceLock::new();
        if !DEVFEE {
            return;
        }
        let fee = FeeSlot::last_of(FEE, MAX_POOLS).expect("the switch is on");
        CELL.init(fee).expect("BUG: the cell is this test's own");
        let handle = handle_over(&CELL);
        assert_eq!(handle.pool_slot(MAX_POOLS - 1), Some(FEE.entry()));
        assert_eq!(handle.pool_slot(MAX_POOLS), None);
    }
}
