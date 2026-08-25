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

#[cfg(target_arch = "arm")]
mod imp {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    use embassy_net::{Config, DhcpConfig};
    use embassy_net_afpacket::AfPacketDevice;
    use log::{info, warn};

    pub type HostDevice = AfPacketDevice;

    /// Last-resort MAC reported to the embassy stack on the `AF_PACKET`
    /// path when neither the override env var nor the sysfs read succeed.
    /// Locally-administered with bit 1 of byte 0 set and unicast with bit 0
    /// clear; the rest spells "HR" + 0x000001 so the address is recognisable
    /// in tcpdump and DHCP-server logs.
    const AFPACKET_FALLBACK_MAC: [u8; 6] = [0x02, 0x48, 0x52, 0x00, 0x00, 0x01];

    /// Env var that overrides MAC selection on the `AF_PACKET` path. Value
    /// must parse as `aa:bb:cc:dd:ee:ff`; malformed values are ignored.
    const AFPACKET_MAC_ENV: &str = "AFPACKET_MAC";

    const AFPACKET_DEFAULT_IF: &str = "eth0";

    /// Parse `aa:bb:cc:dd:ee:ff` into raw bytes. Returns `None` on any
    /// malformed input so callers can log and fall back.
    fn parse_mac(text: &str) -> Option<[u8; 6]> {
        let mut out = [0u8; 6];
        let mut parts = text.split(':');
        for slot in &mut out {
            *slot = u8::from_str_radix(parts.next()?, 16).ok()?;
        }
        if parts.next().is_some() {
            return None;
        }
        Some(out)
    }

    /// Read the kernel's view of the interface MAC from sysfs. The file is
    /// the canonical Linux source and exists for every device the kernel
    /// knows about, so this avoids a second `SIOCGIFHWADDR` site in the
    /// runner.
    fn read_host_mac(ifname: &str) -> Option<[u8; 6]> {
        let path = format!("/sys/class/net/{ifname}/address");
        let raw = std::fs::read_to_string(&path).ok()?;
        parse_mac(raw.trim())
    }

    /// Mix a 6-byte seed into a stable, locally-administered MAC. Same
    /// seed produces the same output, so DHCP leases keyed on the result
    /// persist across runs of the runner on the same host while still
    /// distinguishing different hosts on the same LAN.
    fn derive_synthetic_mac(seed: [u8; 6]) -> [u8; 6] {
        let mut hasher = DefaultHasher::new();
        seed.hash(&mut hasher);
        let mixed = hasher.finish().to_le_bytes();
        let mut mac = [mixed[0], mixed[1], mixed[2], mixed[3], mixed[4], mixed[5]];
        // Force locally-administered with bit 1 set and unicast with bit 0
        // clear so the result cannot collide with a real OUI on the wire.
        mac[0] = (mac[0] & 0xfc) | 0x02;
        mac
    }

    /// Pick the synthetic MAC for the `AF_PACKET` path. Override env var
    /// wins, then a derivation seeded with the host NIC's real MAC, then a
    /// fixed fallback if sysfs is unreadable.
    fn afpacket_mac(ifname: &str) -> [u8; 6] {
        if let Ok(text) = std::env::var(AFPACKET_MAC_ENV) {
            if let Some(mac) = parse_mac(text.trim()) {
                info!("host-runner: {AFPACKET_MAC_ENV}=`{text}` accepted as override");
                return mac;
            }
            warn!("host-runner: {AFPACKET_MAC_ENV}=`{text}` is malformed, ignoring");
        }
        let Some(host_mac) = read_host_mac(ifname) else {
            warn!("host-runner: failed to read MAC for {ifname} from sysfs, using fallback");
            return AFPACKET_FALLBACK_MAC;
        };
        derive_synthetic_mac(host_mac)
    }

    /// Open the link driver and return the matching `embassy_net` config.
    ///
    /// The TAP path keeps its setup-network-aligned static IP. The
    /// `AF_PACKET` path runs DHCP because the real LAN it attaches to has
    /// its own server and the runner cannot know the subnet ahead of time.
    pub fn open_host_device() -> (HostDevice, Config) {
        let env_ifname = std::env::var("AFPACKET_IF");
        let ifname = env_ifname.as_deref().unwrap_or(AFPACKET_DEFAULT_IF);
        let mac = afpacket_mac(ifname);
        let [m0, m1, m2, m3, m4, m5] = mac;
        info!(
            "host-runner: binding raw socket to {ifname} \
                 with synthetic MAC {m0:02x}:{m1:02x}:{m2:02x}:{m3:02x}:{m4:02x}:{m5:02x}"
        );
        // Failure here is unrecoverable: without a working link driver
        // there is nothing for the rest of the runner to do.
        let device =
            AfPacketDevice::with_synthetic_mac(ifname, mac).expect("BUG: open AF_PACKET socket");
        (device, Config::dhcpv4(DhcpConfig::default()))
    }
}

#[cfg(not(target_arch = "arm"))]
mod imp {
    use embassy_net::{Config, Ipv4Address, Ipv4Cidr, StaticConfigV4};
    use embassy_net_tuntap::TunTapDevice;
    use log::info;

    pub type HostDevice = TunTapDevice;

    const TAP_NAME: &str = "tap0";
    const HOST_IP: Ipv4Address = Ipv4Address::new(192, 168, 69, 2);
    const PREFIX_LEN: u8 = 24;
    const GATEWAY: Ipv4Address = Ipv4Address::new(192, 168, 69, 1);
    const DNS_SERVER: Ipv4Address = Ipv4Address::new(1, 1, 1, 1);

    /// Open the TUN/TAP device and return it with a static IPv4 config.
    ///
    /// The non-ARM half of this module, and the one a developer meets:
    /// running `host-runner` on a workstation takes this path. The ARM
    /// half above opens a raw `AF_PACKET` socket on a real interface
    /// instead, which is what a board in the rack wants.
    ///
    /// The addressing is fixed rather than discovered, because the peer
    /// is a `tap0` the developer has already configured.
    pub fn open_host_device() -> (HostDevice, Config) {
        info!("host-runner: starting on tap device {TAP_NAME}");
        let device = TunTapDevice::new(TAP_NAME).expect("BUG: open TAP device");
        let mut dns_servers: heapless::Vec<Ipv4Address, 3> = heapless::Vec::new();
        let _ = dns_servers.push(DNS_SERVER);
        let config = Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(HOST_IP, PREFIX_LEN),
            gateway: Some(GATEWAY),
            dns_servers,
        });
        (device, config)
    }
}

pub use imp::HostDevice;
pub use imp::open_host_device;
