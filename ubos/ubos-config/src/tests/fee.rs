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

//! Tests of the fee pool: the overrides, the entry and the worker name.

use super::*;
use crate::DEFAULT_POOL_SV2_PUBKEY;

/// A worker that answers with the user of the operator's first pool,
/// so a test sees what the entry hands a product.
#[derive(Debug)]
struct FirstOperator;

impl FeeWorker for FirstOperator {
    fn worker(&self, operator: &[PoolEntry]) -> heapless::String<NAME_CAPACITY> {
        operator
            .first()
            .map_or_else(heapless::String::new, |pool| pool.user.clone())
    }
}

/// A product's pool, with a worker that reads the operator's pools
/// and the pool's key.
const POOL: FeePool = FeePool {
    host: "fee.example.com",
    port: 4444,
    pubkey: Some(BRAIINS_KEY),
    worker: &FirstOperator,
    permille: 20,
};

/// An operator's pool at `user`.
fn operator(user: &str) -> PoolEntry {
    PoolEntry {
        host: heapless::String::try_from("pool.example.com").expect("fits"),
        port: 3333,
        user: heapless::String::try_from(user).expect("fits"),
        protocol: ProtocolKind::Sv2 { pool_pubkey: None },
        weight: 1,
    }
}

/// Whether this build sets no `DEVFEE_*` variable, which is what the
/// override tests pin.
fn plain_build() -> bool {
    [
        option_env!("DEVFEE_POOL_HOST"),
        option_env!("DEVFEE_POOL_PORT"),
        option_env!("DEVFEE_POOL_PUBKEY"),
        option_env!("DEVFEE_POOL_USER"),
        option_env!("DEVFEE_PERMILLE"),
    ]
    .iter()
    .all(Option::is_none)
}

#[test]
fn a_plain_build_overrides_nothing() {
    if !plain_build() {
        return;
    }
    assert_eq!(POOL.with_overrides(), POOL);
}

/// Live only in a build that overrides the host and not the key, which
/// is the one input the rule changes an answer for.
#[test]
fn a_host_override_drops_the_key() {
    let Some(host) = option_env!("DEVFEE_POOL_HOST") else {
        return;
    };
    if option_env!("DEVFEE_POOL_PUBKEY").is_some() {
        return;
    }
    let pool = POOL.with_overrides();
    assert_eq!(pool.host, host);
    assert_eq!(pool.pubkey, None);
}

#[test]
fn permille_parses_its_range() {
    assert_eq!(parse_decimal("0", MAX_FEE_PERMILLE, PERMILLE_BAD), 0);
    assert_eq!(parse_decimal("20", MAX_FEE_PERMILLE, PERMILLE_BAD), 20);
    assert_eq!(parse_decimal("499", MAX_FEE_PERMILLE, PERMILLE_BAD), 499);
}

/// The build refuses every share the scheduler would refuse at boot:
/// one that a repayment at the spacing cannot outpace.
#[test]
#[should_panic(expected = "DEVFEE_PERMILLE must be a number from 0 to 499")]
fn permille_refuses_a_share_the_scheduler_cannot_repay() {
    let _ = parse_decimal("500", MAX_FEE_PERMILLE, PERMILLE_BAD);
}

#[test]
#[should_panic(expected = "DEVFEE_PERMILLE must be a number from 0 to 499")]
fn permille_refuses_text() {
    let _ = parse_decimal("2%", MAX_FEE_PERMILLE, PERMILLE_BAD);
}

/// The shape check passes the key the open boards ship and refuses a
/// wrong length, a character outside the alphabet, and an empty key.
#[test]
fn a_key_override_needs_the_shape_of_a_key() {
    let key = BRAIINS_KEY;
    assert!(is_key_shaped(key));
    assert!(!is_key_shaped(&key[1..]));
    assert!(!is_key_shaped(&key.replace('a', "0")));
    assert!(!is_key_shaped(&key.replace('a', "l")));
    assert!(!is_key_shaped(""));
}

#[test]
fn the_entry_is_weightless_and_keyed() {
    let entry = POOL.entry(&[]);
    assert_eq!(entry.weight, 0);
    assert_eq!(entry.host.as_str(), POOL.host);
    assert_eq!(entry.port, POOL.port);
    assert_eq!(
        entry.protocol,
        ProtocolKind::Sv2 {
            pool_pubkey: Some(DEFAULT_POOL_SV2_PUBKEY)
        }
    );
}

/// Live only in a build that names a user, which is the one input the
/// override changes an answer for.
#[test]
fn a_user_override_replaces_the_worker() {
    let Some(user) = option_env!("DEVFEE_POOL_USER") else {
        return;
    };
    assert_eq!(POOL.with_overrides().entry(&[]).user.as_str(), user);
}

#[test]
fn the_entry_carries_the_name_the_worker_builds() {
    let entry = POOL.entry(&[operator("account.worker"), operator("other.worker")]);
    assert_eq!(entry.user.as_str(), "account.worker");
    assert_eq!(POOL.entry(&[]).user.as_str(), "");
}

#[test]
fn a_fixed_worker_is_the_same_over_any_operator() {
    let fixed = Fixed::new("bdf_000.bench");
    assert_eq!(fixed.worker(&[]).as_str(), "bdf_000.bench");
    assert_eq!(
        fixed.worker(&[operator("account.worker")]).as_str(),
        "bdf_000.bench"
    );
}

#[test]
fn the_unregistered_worker_is_the_name_the_pool_reads_as_one() {
    assert_eq!(
        UNREGISTERED.worker(&[]).as_str(),
        "bdf_000.K7Qo2x3lAEoA_wAA"
    );
}

#[test]
fn the_braiins_pool_carries_the_open_default_key() {
    let entry = BRAIINS_POOL.entry(&[]);
    assert_eq!(
        entry.protocol,
        ProtocolKind::Sv2 {
            pool_pubkey: Some(DEFAULT_POOL_SV2_PUBKEY)
        }
    );
    assert_eq!(entry.weight, 0);
}
