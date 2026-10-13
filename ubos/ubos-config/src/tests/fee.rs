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

/// A product's pool, with the built worker and the pool's key.
const POOL: FeePool = FeePool {
    host: "fee.example.com",
    port: 4444,
    pubkey: Some(BRAIINS_KEY),
    user: None,
    build: "0123456789ab-251012170000",
    variant: 0,
    permille: 20,
};

/// Whether this build sets no `DEVFEE_*` variable, which is what the
/// override tests pin.
fn plain_build() -> bool {
    [
        option_env!("DEVFEE_POOL_HOST"),
        option_env!("DEVFEE_POOL_PORT"),
        option_env!("DEVFEE_POOL_PUBKEY"),
        option_env!("DEVFEE_POOL_USER"),
        option_env!("DEVFEE_VARIANT"),
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
    let entry = POOL.entry();
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

#[test]
fn a_named_user_replaces_the_built_worker() {
    let pool = FeePool {
        user: Some("bdf_000.bench"),
        ..POOL
    };
    assert_eq!(pool.entry().user.as_str(), "bdf_000.bench");
}

#[test]
fn the_worker_carries_the_mark_the_revision_and_the_code() {
    let pool = FeePool {
        build: "87e21019d62a-251012170000",
        variant: 0x0185,
        ..POOL
    };
    assert_eq!(pool.worker().as_str(), "bdf_000.ubosh-IQGdYq_4UB");
}

#[test]
fn a_build_without_a_revision_carries_zero_bytes() {
    let pool = FeePool {
        build: "nogit-251012170000",
        ..POOL
    };
    assert_eq!(pool.worker().as_str(), "bdf_000.ubosAAAAAAAA_wAA");
    let pool = FeePool { build: "", ..POOL };
    assert_eq!(pool.worker().as_str(), "bdf_000.ubosAAAAAAAA_wAA");
}

#[test]
fn the_worker_has_the_layout_the_pool_parses() {
    let worker = POOL.worker();
    let suffix = worker.strip_prefix("bdf_000.").expect("the account prefix");
    assert_eq!(suffix.len(), 16);
    assert_eq!(&suffix[..4], "ubos", "the mark names the firmware");
    assert_eq!(&suffix[12..13], "_", "the code mark marks the device code");
}
