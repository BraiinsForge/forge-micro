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
//! A product names a [`FeePool`] with its server, key, share and the
//! [`FeeWorker`] that builds its worker name, applies the `DEVFEE_*`
//! variables of the build with [`FeePool::with_overrides`], places it
//! at its last session slot with [`FeeSlot::last_of`] and hands that
//! to [`crate::run`]. [`crate::Handle::pool_slot`] then builds the
//! entry for that slot on every read, and asks the worker for the
//! name each time.

#[cfg(test)]
#[path = "tests/fee.rs"]
mod tests;

use crate::pool::{DEVFEE, NAME_CAPACITY, PoolEntry, ProtocolKind};
use crate::pubkey;
use core::fmt::Debug;
use embassy_sync::once_lock::OnceLock;

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

/// Build error for a `DEVFEE_PERMILLE` past [`MAX_FEE_PERMILLE`].
const PERMILLE_BAD: &str = "DEVFEE_PERMILLE must be a number from 0 to 499";

/// Build error for a `DEVFEE_POOL_HOST` the entry cannot hold.
const HOST_BAD: &str = "DEVFEE_POOL_HOST must be at most 128 bytes";

/// Build error for a `DEVFEE_POOL_USER` the entry cannot hold.
const USER_BAD: &str = "DEVFEE_POOL_USER must be at most 128 bytes";

/// Build error for a fixed worker name the entry cannot hold.
const WORKER_BAD: &str = "a fee worker name must be at most 128 bytes";

/// Build error for a key override without the host it belongs to.
const KEY_BAD: &str = "DEVFEE_POOL_PUBKEY needs DEVFEE_POOL_HOST";

/// Build error for a key override without the shape of an SV2 key.
const KEY_SHAPE_BAD: &str = "DEVFEE_POOL_PUBKEY must be 51 base58 characters";

/// Characters in the base58check form of an SV2 authority key: the
/// 38 payload bytes behind a leading byte of 1 always encode to this
/// many.
const KEY_LEN: usize = 51;

/// The worker name a fee session authorises with.
///
/// The product that states the fee pool builds the name from the
/// operator's pools as currently configured, so a name can follow
/// the operator's account. The config crate asks on every read of
/// the fee entry and keeps nothing of the answer, and a config change
/// restarts the fee session with the name the next read gives.
pub trait FeeWorker: Debug + Sync {
    /// The name, over `operator`, the operator pools the board runs,
    /// in slot order.
    fn worker(&self, operator: &[PoolEntry]) -> heapless::String<NAME_CAPACITY>;
}

/// A worker name that is the same whatever the operator runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixed(&'static str);

impl Fixed {
    /// `name` as the worker.
    ///
    /// # Panics
    ///
    /// Panics on a name past the entry's capacity. In a const
    /// initializer that is a build error.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        assert!(name.len() <= NAME_CAPACITY, "{}", WORKER_BAD);
        Self(name)
    }
}

impl FeeWorker for Fixed {
    fn worker(&self, _operator: &[PoolEntry]) -> heapless::String<NAME_CAPACITY> {
        heapless::String::try_from(self.0).expect("BUG: Fixed::new bounds the name")
    }
}

/// The worker the pool reads as a device it has not registered, whose
/// fee it pays to the fee account as a whole. The boards of this tree
/// send it, and a product states a worker of its own.
pub static UNREGISTERED: Fixed = Fixed::new("bdf_000.K7Qo2x3lAEoA_wAA");

/// The worker the build's `DEVFEE_POOL_USER` names, when it names one.
static USER_OVERRIDE: Option<Fixed> = match option_env!("DEVFEE_POOL_USER") {
    Some(user) => {
        assert!(user.len() <= NAME_CAPACITY, "{}", USER_BAD);
        Some(Fixed(user))
    }
    None => None,
};

/// The fee pool a product mines for: the server, its authority key,
/// the worker it authorises with, and the share of the work.
///
/// A product states one as a const and applies the build's overrides
/// with [`Self::with_overrides`].
#[derive(Debug, Clone, Copy)]
pub struct FeePool {
    /// The fee server.
    pub host: &'static str,
    /// Its SV2 port.
    pub port: u16,
    /// Its authority key as base58check. `None` trusts the server, as
    /// an operator entry with no key does.
    pub pubkey: Option<&'static str>,
    /// What builds the worker name.
    pub worker: &'static dyn FeeWorker,
    /// The fee slot's share of the delivered work, in thousandths.
    /// Below [`FEE_DEN`]: a whole share would leave the operator
    /// nothing.
    pub permille: i64,
}

/// Two pools are equal when they name the same values and the same
/// worker object, which a product names as a static of its own.
impl PartialEq for FeePool {
    fn eq(&self, other: &Self) -> bool {
        self.host == other.host
            && self.port == other.port
            && self.pubkey == other.pubkey
            && core::ptr::addr_eq(self.worker, other.worker)
            && self.permille == other.permille
    }
}

impl Eq for FeePool {}

/// The Braiins authority key as base58check, which decodes to
/// [`crate::DEFAULT_POOL_SV2_PUBKEY`].
pub const BRAIINS_KEY: &str = "9awtMD5KQgvRUh2yFbjVeT7b6hjipWcAsQHd6wEhgtDT9soosna";

/// The Braiins fee pool the boards of this tree mine for, with the
/// unregistered worker and none of the build's overrides. A board
/// applies the overrides: `BRAIINS_POOL.with_overrides()`. The values
/// are consts of this crate because every board of this tree shares
/// them; a product outside the tree states its own.
pub const BRAIINS_POOL: FeePool = FeePool {
    host: "a830bcc3.bos.braiins.com",
    port: 3336,
    pubkey: Some(BRAIINS_KEY),
    worker: &UNREGISTERED,
    permille: 25,
};

impl FeePool {
    /// `self` with every `DEVFEE_*` variable of the build applied:
    /// `DEVFEE_POOL_HOST`, `DEVFEE_POOL_PORT`, `DEVFEE_POOL_PUBKEY`,
    /// `DEVFEE_POOL_USER` and `DEVFEE_PERMILLE`. A
    /// key is for one server, so a host override drops the product's
    /// key unless the build names one too, and a key override without
    /// a host stops the build. A host override keeps the port, so a
    /// server on another port takes `DEVFEE_POOL_PORT` as well. A
    /// number outside its range, or a name the entry cannot hold,
    /// stops the build.
    ///
    /// # Panics
    ///
    /// Panics on a port outside 1 to 65535, a
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
        let worker: &'static dyn FeeWorker = match &USER_OVERRIDE {
            Some(user) => user,
            None => self.worker,
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
        let permille = match option_env!("DEVFEE_PERMILLE") {
            Some(permille) => parse_decimal(permille, MAX_FEE_PERMILLE, PERMILLE_BAD),
            None => self.permille,
        };
        Self {
            host,
            port,
            pubkey,
            worker,
            permille,
        }
    }

    /// The entry the fee slot runs: this pool, with the name
    /// [`Self::worker`] builds over `operator`, the configured operator
    /// pools.
    ///
    /// # Panics
    ///
    /// Panics on a value that does not fit the entry: a host past the
    /// entry's capacity, or a key that is not base58check. Each is a
    /// build configuration error, caught on the first boot of that
    /// build.
    #[must_use]
    pub fn entry(&self, operator: &[PoolEntry]) -> PoolEntry {
        let host = heapless::String::try_from(self.host)
            .expect("BUG: the fee host exceeds PoolEntry::host capacity");
        let user = self.worker.worker(operator);
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

/// The fee slot the board handed to [`crate::run`].
///
/// Read with `try_get`, from core 0 only: the sessions and the API run
/// there, and `run` sets it before the first config publish that each
/// of them waits for.
pub(crate) static FEE_SLOT: OnceLock<FeeSlot> = OnceLock::new();

/// Records the fee slot [`crate::run`] was handed.
///
/// # Panics
///
/// Panics on a second call. One slot is a boot-time fact, and a second
/// `run` is a wiring bug.
pub(crate) fn install(fee: FeeSlot) {
    assert!(FEE_SLOT.init(fee).is_ok(), "BUG: fee slot installed twice");
}
