//! Tests for `allow_list::is_allowed`.

extern crate std;

use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::allow_list::{is_allowed, Cidr, DEFAULT_ALLOW_LIST};

fn ipv4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(a, b, c, d))
}

// --- addresses inside default allow-list ---

#[test]
fn rfc1918_10_block_is_allowed() {
    assert!(is_allowed(ipv4(10, 0, 0, 1), DEFAULT_ALLOW_LIST));
    assert!(is_allowed(ipv4(10, 255, 255, 254), DEFAULT_ALLOW_LIST));
}

#[test]
fn rfc1918_172_block_is_allowed() {
    assert!(is_allowed(ipv4(172, 16, 0, 1), DEFAULT_ALLOW_LIST));
    assert!(is_allowed(ipv4(172, 31, 255, 254), DEFAULT_ALLOW_LIST));
}

#[test]
fn rfc1918_192_168_block_is_allowed() {
    assert!(is_allowed(ipv4(192, 168, 0, 1), DEFAULT_ALLOW_LIST));
    assert!(is_allowed(ipv4(192, 168, 255, 254), DEFAULT_ALLOW_LIST));
}

#[test]
fn link_local_is_allowed() {
    assert!(is_allowed(ipv4(169, 254, 0, 1), DEFAULT_ALLOW_LIST));
    assert!(is_allowed(ipv4(169, 254, 255, 254), DEFAULT_ALLOW_LIST));
}

// --- addresses outside default allow-list ---

#[test]
fn public_address_is_denied() {
    assert!(!is_allowed(ipv4(8, 8, 8, 8), DEFAULT_ALLOW_LIST));
    assert!(!is_allowed(ipv4(1, 1, 1, 1), DEFAULT_ALLOW_LIST));
}

#[test]
fn just_outside_172_range_is_denied() {
    // 172.15.255.255 is just below the 172.16.0.0/12 block.
    assert!(!is_allowed(ipv4(172, 15, 255, 255), DEFAULT_ALLOW_LIST));
    // 172.32.0.0 is just above the top of the block.
    assert!(!is_allowed(ipv4(172, 32, 0, 0), DEFAULT_ALLOW_LIST));
}

// --- IPv6 handling ---

#[test]
fn native_ipv6_is_denied() {
    let ipv6 = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
    assert!(!is_allowed(ipv6, DEFAULT_ALLOW_LIST));
}

#[test]
fn ipv4_mapped_ipv6_is_screened_as_ipv4() {
    // ::ffff:192.168.1.1 unwraps to 192.168.1.1 and is allowed.
    let mapped_private = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0xc0a8, 0x0101));
    assert!(is_allowed(mapped_private, DEFAULT_ALLOW_LIST));

    // ::ffff:8.8.8.8 unwraps to 8.8.8.8 and is denied.
    let mapped_public = IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0808, 0x0808));
    assert!(!is_allowed(mapped_public, DEFAULT_ALLOW_LIST));
}

// --- Cidr::contains edge prefixes ---

#[test]
fn prefix_zero_block_matches_every_ipv4() {
    let block_zero = &[Cidr {
        base: Ipv4Addr::UNSPECIFIED,
        prefix: 0,
    }];
    assert!(is_allowed(ipv4(8, 8, 8, 8), block_zero));
    assert!(is_allowed(ipv4(1, 2, 3, 4), block_zero));
    assert!(is_allowed(ipv4(255, 255, 255, 255), block_zero));
}

#[test]
fn prefix_thirty_two_block_matches_single_host() {
    let host_only = &[Cidr {
        base: Ipv4Addr::new(10, 0, 0, 42),
        prefix: 32,
    }];
    assert!(is_allowed(ipv4(10, 0, 0, 42), host_only));
    // Adjacent host is not in the /32.
    assert!(!is_allowed(ipv4(10, 0, 0, 43), host_only));
    assert!(!is_allowed(ipv4(10, 0, 0, 41), host_only));
}
