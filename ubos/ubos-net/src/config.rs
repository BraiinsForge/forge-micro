//! Build an `embassy-net` stack configuration from stored network
//! parameters.
//!
//! The config layer stores addresses as raw octets to stay free of the
//! link layer; this module converts them into the `embassy-net`/smoltcp
//! types the stack constructor expects. It takes primitive octet inputs
//! so board `main` functions can build the boot stack config before the
//! stack exists. `ubos-net` does now depend on `ubos-config` — the
//! `net::run_network_control` task subscribes to the config network watch
//! — which is acyclic as long as `ubos-config` stays the bottom store and
//! never depends on the manager, mining, or stratum-client chain. Board
//! `main` functions match on the stored mode and call [`static_ipv4`] for
//! a static profile, using `embassy_net::Config::dhcpv4` for the DHCP case.

use embassy_net::{Config, Ipv4Address, Ipv4Cidr, StaticConfigV4};

/// Build a static IPv4 [`embassy_net::Config`] from octet inputs.
///
/// `address` and `prefix_len` become the interface [`Ipv4Cidr`],
/// `gateway` is set as the default route, and up to three `dns` entries
/// populate the resolver list; any beyond the third are dropped. The
/// caller is responsible for having validated the values, which
/// `ubos_config` does before persisting.
#[must_use]
pub fn static_ipv4(address: [u8; 4], prefix_len: u8, gateway: [u8; 4], dns: &[[u8; 4]]) -> Config {
    let mut dns_servers: heapless::Vec<Ipv4Address, 3> = heapless::Vec::new();
    for server in dns {
        if dns_servers.push(Ipv4Address::from(*server)).is_err() {
            break;
        }
    }
    Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(Ipv4Address::from(address), prefix_len),
        gateway: Some(Ipv4Address::from(gateway)),
        dns_servers,
    })
}
