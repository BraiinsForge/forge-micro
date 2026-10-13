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

//! The fee pool a build ships: a product's values, the build-time
//! overrides, the worker name and the entry the fee slot runs.
//!
//! A product names a [`FeePool`] with its server, key, device code,
//! build id and share, applies the `DEVFEE_*` variables of the build
//! with [`FeePool::with_overrides`], places it at its last session
//! slot with [`FeeSlot::last_of`] and hands that to [`crate::run`].
//! [`crate::Handle::pool_slot`] then builds the entry for that slot on
//! every read.

#[cfg(test)]
#[path = "tests/fee.rs"]
mod tests;

use crate::pool::{DEVFEE, NAME_CAPACITY, PoolEntry, ProtocolKind};
use crate::pubkey;

/// Denominator of [`FeePool::permille`]: a share is counted in
/// thousandths.
pub const FEE_DEN: i64 = 1000;

/// The fewest session slots a board needs to run the fee: one operator
/// slot beside it, because the fee is a share of the operator's work.
pub const MIN_FEE_SLOTS: usize = 2;

/// Operator shares between two fee shares while the fee scheduler
/// repays a debt. The pace of a repayment: 0 hands the fee every pick
/// until the debt is paid, 1 alternates, and a larger value spreads
/// the repayment over more of the operator's work. Below the steady
/// spacing of `(FEE_DEN - permille) / permille`, or the debt never
/// shrinks, which is what bounds [`MAX_FEE_PERMILLE`].
pub const REPAY_SPACING: i64 = 1;

/// The largest share a pool can state: one a repayment at
/// [`REPAY_SPACING`] still outpaces, so the scheduler accepts every
/// share the build accepts.
pub const MAX_FEE_PERMILLE: i64 = (FEE_DEN - 1) / (REPAY_SPACING + 1);
// The bound is the scheduler's own test, so the two cannot drift apart.
const _: () = assert!(MAX_FEE_PERMILLE * (REPAY_SPACING + 1) < FEE_DEN);
const _: () = assert!((MAX_FEE_PERMILLE + 1) * (REPAY_SPACING + 1) >= FEE_DEN);

/// Build error for a board that places the fee with no operator slot.
const SLOTS_BAD: &str = "a fee slot needs an operator slot beside it";

/// Build error for a `DEVFEE_POOL_PORT` outside the TCP port range.
const PORT_BAD: &str = "DEVFEE_POOL_PORT must be a number from 1 to 65535";

/// Build error for a `DEVFEE_VARIANT` past the account's variant field.
const VARIANT_BAD: &str = "DEVFEE_VARIANT must be a number from 0 to 65535";

/// Build error for a `DEVFEE_PERMILLE` past [`MAX_FEE_PERMILLE`].
const PERMILLE_BAD: &str = "DEVFEE_PERMILLE must be a number from 0 to 499";

/// Build error for a `DEVFEE_POOL_HOST` the entry cannot hold.
const HOST_BAD: &str = "DEVFEE_POOL_HOST must be at most 128 bytes";

/// Build error for a `DEVFEE_POOL_USER` the entry cannot hold.
const USER_BAD: &str = "DEVFEE_POOL_USER must be at most 128 bytes";

/// Build error for a key override without the host it belongs to.
const KEY_BAD: &str = "DEVFEE_POOL_PUBKEY needs DEVFEE_POOL_HOST";

/// Build error for a key override without the shape of an SV2 key.
const KEY_SHAPE_BAD: &str = "DEVFEE_POOL_PUBKEY must be 51 base58 characters";

/// Characters in the base58check form of an SV2 authority key: the
/// 38 payload bytes behind a leading byte of 1 always encode to this
/// many.
const KEY_LEN: usize = 51;

/// The pool account every fee worker belongs to.
const FEE_ACCOUNT: &str = "bdf_000";

/// The first bytes of a worker name, which read `ubos` in base64url.
const WORKER_MARK: [u8; 3] = [0xB9, 0xBA, 0x2C];

/// Bytes of the build's revision a worker name carries: the hex digits
/// the build id starts with.
const REVISION_LEN: usize = 6;

/// The byte before the device code, which puts a `_` where the pool
/// reads the code.
const CODE_MARK: u8 = 0xFF;

/// The fee pool a product mines for: the server, its authority key,
/// the device variant the pool reads, and the share of the work.
///
/// A product states one as a const and applies the build's overrides
/// with [`Self::with_overrides`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeePool {
    /// The fee server.
    pub host: &'static str,
    /// Its SV2 port.
    pub port: u16,
    /// Its authority key as base58check. `None` trusts the server, as
    /// an operator entry with no key does.
    pub pubkey: Option<&'static str>,
    /// The worker name. `None` builds it with [`Self::worker`].
    pub user: Option<&'static str>,
    /// The build id of the firmware, `env!("BUILD_ID")` in the board
    /// that states the pool. The hex digits it starts with name the
    /// revision the worker name carries.
    pub build: &'static str,
    /// The fee account's variant, which the pool reads as the device
    /// kind. 0 reads as an unknown device until the pool allocates a
    /// value for the product.
    pub variant: u16,
    /// The fee slot's share of the delivered work, in thousandths.
    /// Below [`FEE_DEN`]: a whole share would leave the operator
    /// nothing.
    pub permille: i64,
}

/// The Braiins authority key as base58check, which decodes to
/// [`crate::DEFAULT_POOL_SV2_PUBKEY`].
pub const BRAIINS_KEY: &str = "9awtMD5KQgvRUh2yFbjVeT7b6hjipWcAsQHd6wEhgtDT9soosna";

/// The Braiins fee pool the boards of this tree mine for, with no
/// device code and none of the build's overrides. A board states its
/// code and build id over it and applies the overrides: `FeePool {
/// variant: OPEN_CODES + 1, build: env!("BUILD_ID"), ..BRAIINS_POOL
/// }.with_overrides()`. The values are consts of this crate because
/// every board of this tree shares them; a product outside the tree
/// states its own.
pub const BRAIINS_POOL: FeePool = FeePool {
    host: "a830bcc3.bos.braiins.com",
    port: 3336,
    pubkey: Some(BRAIINS_KEY),
    user: None,
    build: "",
    variant: 0,
    permille: 25,
};

/// The bit of a device code that marks a board of this firmware. The
/// codes without it belong to the devices the pool knew before.
pub const UBOS_CODE: u16 = 0x0100;

/// The first device code of the boards of this tree: [`UBOS_CODE`]
/// with the bit that marks the tree. A board adds its index in the
/// alphabetical order of the firmware directories. The codes between
/// [`UBOS_CODE`] and this one belong to the products outside the tree.
pub const OPEN_CODES: u16 = UBOS_CODE | 0x0080;

impl FeePool {
    /// `self` with every `DEVFEE_*` variable of the build applied:
    /// `DEVFEE_POOL_HOST`, `DEVFEE_POOL_PORT`, `DEVFEE_POOL_PUBKEY`,
    /// `DEVFEE_POOL_USER`, `DEVFEE_VARIANT` and `DEVFEE_PERMILLE`. A
    /// key is for one server, so a host override drops the product's
    /// key unless the build names one too, and a key override without
    /// a host stops the build. A host override keeps the port, so a
    /// server on another port takes `DEVFEE_POOL_PORT` as well. A
    /// number outside its range, or a name the entry cannot hold,
    /// stops the build.
    ///
    /// # Panics
    ///
    /// Panics on a port outside 1 to 65535, a variant past 65535, a
    /// share past [`MAX_FEE_PERMILLE`], a host or user past 128 bytes,
    /// or a key without the length and the alphabet of a base58check
    /// key. In a const initializer that is a build error.
    #[must_use]
    pub const fn with_overrides(self) -> Self {
        let host = match option_env!("DEVFEE_POOL_HOST") {
            Some(host) => {
                assert!(host.len() <= NAME_CAPACITY, "{}", HOST_BAD);
                host
            }
            None => self.host,
        };
        let pubkey = match (
            option_env!("DEVFEE_POOL_PUBKEY"),
            option_env!("DEVFEE_POOL_HOST"),
        ) {
            (Some(key), Some(_)) => {
                assert!(is_key_shaped(key), "{}", KEY_SHAPE_BAD);
                Some(key)
            }
            (Some(_), None) => panic!("{}", KEY_BAD),
            (None, Some(_)) => None,
            (None, None) => self.pubkey,
        };
        let user = match option_env!("DEVFEE_POOL_USER") {
            Some(user) => {
                assert!(user.len() <= NAME_CAPACITY, "{}", USER_BAD);
                Some(user)
            }
            None => self.user,
        };
        let port = match option_env!("DEVFEE_POOL_PORT") {
            Some(port) => {
                let port = parse_decimal(port, u16::MAX as i64, PORT_BAD);
                assert!(port > 0, "{}", PORT_BAD);
                // Bounded by the parse above.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    port as u16
                }
            }
            None => self.port,
        };
        let variant = match option_env!("DEVFEE_VARIANT") {
            // Bounded by the parse.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Some(variant) => parse_decimal(variant, u16::MAX as i64, VARIANT_BAD) as u16,
            None => self.variant,
        };
        let permille = match option_env!("DEVFEE_PERMILLE") {
            Some(permille) => parse_decimal(permille, MAX_FEE_PERMILLE, PERMILLE_BAD),
            None => self.permille,
        };
        Self {
            host,
            port,
            pubkey,
            user,
            build: self.build,
            variant,
            permille,
        }
    }

    /// The entry the fee slot runs: this pool, with the worker from
    /// [`Self::worker`] unless [`Self::user`] names one.
    ///
    /// # Panics
    ///
    /// Panics on a value that does not fit the entry: a host or a user
    /// past the entry's capacity, or a key that is not base58check.
    /// Each is a build configuration error, caught on the first boot
    /// of that build.
    #[must_use]
    pub fn entry(&self) -> PoolEntry {
        let host = heapless::String::try_from(self.host)
            .expect("BUG: the fee host exceeds PoolEntry::host capacity");
        let user = match self.user {
            Some(user) => heapless::String::try_from(user)
                .expect("BUG: the fee user exceeds PoolEntry::user capacity"),
            None => self.worker(),
        };
        let pool_pubkey = self
            .pubkey
            .map(|key| pubkey::decode(key).expect("BUG: the fee key is not base58check"));
        PoolEntry {
            host,
            port: self.port,
            user,
            protocol: ProtocolKind::Sv2 { pool_pubkey },
            weight: 0,
        }
    }

    /// The worker name the fee slot authorises with.
    ///
    /// `bdf_000.` followed by base64url of twelve bytes: the mark that
    /// reads `ubos`, the revision from [`Self::build`], the code mark
    /// and [`Self::variant`] little-endian. The encoding is fixed
    /// width, so character 13 of it is always `_`, the top six bits
    /// of the code mark, and characters 14 to 16 are the device code.
    /// The pool reads the code at that position, as it does for every
    /// `bdf_000` worker, and stores the rest as an opaque identifier.
    // The only panics are `.expect()` on the worker's fixed 24 bytes
    // fitting a 128-byte string (BUG-class, not caller-facing), so a
    // `# Panics` doc section would mislead.
    #[allow(clippy::missing_panics_doc)]
    #[must_use]
    pub fn worker(&self) -> heapless::String<NAME_CAPACITY> {
        const REVISION: usize = WORKER_MARK.len();
        const MARK: usize = REVISION + REVISION_LEN;
        let mut raw = [0u8; MARK + 1 + 2];
        raw[..REVISION].copy_from_slice(&WORKER_MARK);
        revision(self.build, &mut raw[REVISION..MARK]);
        raw[MARK] = CODE_MARK;
        raw[MARK + 1..].copy_from_slice(&self.variant.to_le_bytes());

        let mut worker = heapless::String::new();
        worker
            .push_str(FEE_ACCOUNT)
            .expect("BUG: the account fits the worker name");
        worker.push('.').expect("BUG: the dot fits the worker name");
        base64url(&raw, &mut worker);
        worker
    }
}

/// The fee pool a board runs and the session slot it runs it at: the
/// board's last slot, after its operator slots.
///
/// A board builds it with [`Self::last_of`] from its pool and its slot
/// count and hands it to [`crate::run`], which keeps it for the
/// handle. The slot is the board's own count minus one, so a board
/// with three sessions runs two operator slots and the fee, and one
/// with four runs the operator's three and the fee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeSlot {
    /// The pool the slot mines for.
    pub pool: FeePool,
    /// The session slot that runs it.
    pub slot: usize,
}

impl FeeSlot {
    /// `pool` at the last of `slots` session slots, or `None` on a
    /// build with [`DEVFEE`] off, so a board states this in both
    /// states and the switch decides.
    ///
    /// # Panics
    ///
    /// Panics on fewer than [`MIN_FEE_SLOTS`] slots. In a const
    /// initializer that is a build error.
    #[must_use]
    pub const fn last_of(pool: FeePool, slots: usize) -> Option<Self> {
        if !DEVFEE {
            return None;
        }
        assert!(slots >= MIN_FEE_SLOTS, "{}", SLOTS_BAD);
        Some(Self {
            pool,
            slot: slots - 1,
        })
    }

    /// The slot of `fee`, for a reader that labels slots and holds
    /// the board's `Option` as a const.
    #[must_use]
    pub const fn index(fee: &Option<Self>) -> Option<usize> {
        match fee {
            Some(fee) => Some(fee.slot),
            None => None,
        }
    }
}

/// Writes the revision `build` starts with into `out`, two hex digits
/// per byte. A build id that starts otherwise, `nogit` or one too
/// short, leaves `out` zero.
fn revision(build: &str, out: &mut [u8]) {
    let digits = build.as_bytes();
    if digits.len() < 2 * out.len() {
        return;
    }
    for (i, byte) in out.iter_mut().enumerate() {
        let (Some(hi), Some(lo)) = (hex(digits[2 * i]), hex(digits[2 * i + 1])) else {
            out.fill(0);
            return;
        };
        *byte = hi << 4 | lo;
    }
}

/// The value of one hex digit, or `None` for another character.
fn hex(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// Appends `bytes` to `out` as base64url. The input is a whole number
/// of three-byte groups, so there is no padding to write.
fn base64url(bytes: &[u8; 12], out: &mut heapless::String<NAME_CAPACITY>) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    for group in bytes.as_chunks::<3>().0 {
        let bits = usize::from(group[0]) << 16 | usize::from(group[1]) << 8 | usize::from(group[2]);
        for shift in [18, 12, 6, 0] {
            out.push(char::from(ALPHABET[(bits >> shift) & 0x3f]))
                .expect("BUG: the suffix fits the worker name");
        }
    }
}

/// Parses a build-time value from its decimal text, at most `max`. A
/// `const fn` so the value is a compile-time fact.
///
/// # Panics
///
/// Panics with `bad` on text that is not a number from 0 to `max`. In a
/// const initializer that is a build error.
const fn parse_decimal(text: &str, max: i64, bad: &str) -> i64 {
    let digits = text.as_bytes();
    assert!(!digits.is_empty(), "{}", bad);
    let mut value: i64 = 0;
    let mut i = 0;
    while i < digits.len() {
        assert!(digits[i].is_ascii_digit(), "{}", bad);
        value = value * 10 + (digits[i] - b'0') as i64;
        assert!(value <= max, "{}", bad);
        i += 1;
    }
    value
}

/// Whether `key` has the length and the alphabet of an SV2 authority
/// key in base58check, which a const initializer can check. The
/// checksum needs [`crate::pubkey::decode`], which runs at boot.
const fn is_key_shaped(key: &str) -> bool {
    let bytes = key.as_bytes();
    if bytes.len() != KEY_LEN {
        return false;
    }
    let mut i = 0;
    while i < bytes.len() {
        // The base58 alphabet: digits and letters without 0, O, I and l.
        let base58 = matches!(
            bytes[i],
            b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z'
        );
        if !base58 {
            return false;
        }
        i += 1;
    }
    true
}
