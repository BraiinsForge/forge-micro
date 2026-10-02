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

//! What every board derives from its stored configuration before it can
//! bring a network stack up.

use static_cell::StaticCell;

/// Hostname buffer capacity. The config layer caps an operator-supplied
/// hostname at 32 bytes, so at this capacity no valid configured value
/// silently falls back to the composed default.
const HOSTNAME_CAP: usize = 32;

/// Wait for the first published config, and derive the stack config and
/// the mDNS hostname from it.
///
/// The wait is on the board's `config_task`, with no timeout: that task
/// publishes the persisted config, the board's seed or the compiled
/// defaults unconditionally, so it always resolves. Only the two small results
/// leave this function, so the caller's future does not hold the whole
/// `Config`, nor the decode buffer behind it, for the rest of boot.
///
/// `BASE_LEN` must equal `hostname_base.len()`; it carries
/// [`ubos_mdns::compose_hostname`]'s capacity assertion, which turns a
/// future enlargement of the base into a build failure rather than a
/// boot-time panic.
pub async fn bring_up<const BASE_LEN: usize>(
    config: ubos_config::Handle,
    hostname_base: &'static str,
    mac_suffix: u8,
) -> (embassy_net::Config, &'static str) {
    static MDNS_HOSTNAME: StaticCell<heapless::String<HOSTNAME_CAP>> = StaticCell::new();

    let initial = config.changed().await;
    let net_config = match &initial.network.mode {
        ubos_config::NetworkMode::Static {
            address,
            prefix_len,
            gateway,
            dns,
        } => ubos_net::config::static_ipv4(*address, *prefix_len, *gateway, dns),
        ubos_config::NetworkMode::Dhcp => {
            embassy_net::Config::dhcpv4(embassy_net::DhcpConfig::default())
        }
    };
    let hostname = ubos_mdns::compose_hostname_configured::<HOSTNAME_CAP, BASE_LEN>(
        initial.network.hostname.as_str(),
        hostname_base,
        mac_suffix,
        &MDNS_HOSTNAME,
    );
    (net_config, hostname)
}
