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

//! mDNS responder advertising the board hostname and its DNS-SD services.
//!
//! `edge-mdns` runs the RFC 6762 §8.1 probe phase, the §8.3 announcement
//! phase and query-response handling. This module supplies the A record
//! for `<hostname>.local.` and the PTR, SRV and TXT records for every
//! entry the caller passes to [`run_mdns`].

use core::convert::Infallible;
use core::fmt::Write;
use core::net::{Ipv4Addr, Ipv6Addr};

use edge_mdns::buf::VecBufAccess;
use edge_mdns::domain::base::Ttl;
use edge_mdns::host::{Host, Service, ServiceAnswers};
use edge_mdns::io::{self, IPV4_DEFAULT_SOCKET, MdnsIoError};
use edge_mdns::{HostAnswer, HostAnswers, HostAnswersMdnsHandler, MdnsError};
use edge_nal::UdpSplit;
use edge_nal_embassy::{Udp, UdpBuffers, UdpError};
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use heapless::Vec;
use rand_core_v010::TryRng;
use static_cell::{ConstStaticCell, StaticCell};
use ubos_logging::{Debug2Format, info, warn};

use crate::registry::ServiceEntry;

#[cfg(test)]
#[path = "tests/responder.rs"]
mod tests;

/// A [`ServiceEntry`] declares no protocol, so every advertisement is TCP.
const TCP_PROTOCOL: &str = "_tcp";
/// RFC 6762 §10 recommends 120 s for volatile addresses such as DHCP leases.
const TTL_SECS: u32 = 120;
const RETRY_DELAY_SECS: u64 = 5;
/// RFC 6762 §17 caps mDNS messages at the interface UDP payload max.
const BUFFER_SIZE: usize = 1472;
const METADATA_SLOTS: usize = 4;
/// The responder holds a single socket for the lifetime of each run attempt.
const SOCKET_POOL_SIZE: usize = 1;
/// Bound on the services one run advertises. The per-run TXT storage and
/// the answer-record vector are stack-allocated for it.
const MAX_SERVICES: usize = 8;
/// TXT key-value pairs per service, the runtime `board=` and `mcu=` pairs
/// included, so a [`ServiceEntry`] may declare at most this minus two.
const MAX_TXT_PAIRS: usize = 6;

/// Nothing signals this: an address change is picked up by the
/// socket-error restart loop in [`run_mdns`] instead. The static exists
/// because `edge_mdns::io::Mdns::new` borrows a `&Signal` for the
/// responder's lifetime.
static MDNS_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

static RECV_BUFFER: VecBufAccess<CriticalSectionRawMutex, { BUFFER_SIZE }> = VecBufAccess::new();
static SEND_BUFFER: VecBufAccess<CriticalSectionRawMutex, { BUFFER_SIZE }> = VecBufAccess::new();

static UDP_BUFFERS: ConstStaticCell<
    UdpBuffers<{ SOCKET_POOL_SIZE }, { BUFFER_SIZE }, { BUFFER_SIZE }, { METADATA_SLOTS }>,
> = ConstStaticCell::new(UdpBuffers::new());

/// A counter-based RNG used only for the 0-250 ms probe-jitter delay.
///
/// RFC 6762 §8.1 requires a random delay before sending probes to reduce
/// multicast collisions. Cryptographic quality is not required, so an LCG
/// seeded from boot time plus the per-board hostname avoids pulling an
/// entropy source into the no-std target.
struct CounterRng(u64);

impl CounterRng {
    /// Seed the RNG by mixing the current monotonic tick with `hostname`.
    ///
    /// The hostname seeds the high half of the LCG state through a
    /// FNV-1a fold; the boot tick seeds the low half. Boards with
    /// distinct hostnames start the LCG at different points even when
    /// they boot simultaneously.
    fn new(hostname: &str) -> Self {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash: u64 = FNV_OFFSET;
        for &byte in hostname.as_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        let ticks = Instant::now().as_ticks();
        // rotate_left(32) spreads the lower-bits-dominant tick count into
        // the high half, so boards that boot at the same instant still pick
        // different probe jitter.
        Self(hash ^ ticks.rotate_left(32))
    }
}

impl TryRng for CounterRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // The narrowing cast drops the low half of the LCG state by
        // construction, not by accident.
        Ok((self.0 >> 32) as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        let low = u64::from(self.try_next_u32()?);
        let high = u64::from(self.try_next_u32()?);
        Ok((high << 32) | low)
    }

    fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), Infallible> {
        for chunk in destination.chunks_mut(4) {
            let bytes = self.try_next_u32()?.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        Ok(())
    }
}

/// `HostAnswers` adapter visiting every service in `services` against a
/// shared `host`.
///
/// Each visited [`ServiceAnswers`] re-emits the host's A record, so an A
/// query response carries one copy per service. RFC 6762 §10 has receivers
/// collapse duplicate records into a single cache entry, and stripping them
/// here would mean reimplementing `Service::visit_answers`.
struct ServicesAnswers<'a> {
    host: &'a Host<'a>,
    services: &'a [Service<'a>],
}

impl HostAnswers for ServicesAnswers<'_> {
    fn visit<F, E>(&self, mut f: F) -> Result<(), E>
    where
        F: FnMut(HostAnswer) -> Result<(), E>,
        E: From<MdnsError>,
    {
        for service in self.services {
            ServiceAnswers::new(self.host, service).visit(&mut f)?;
        }
        Ok(())
    }
}

/// Build the TXT key-value pairs for a registry entry, appending the
/// runtime `board=` and `mcu=` pairs.
///
/// Panics if the entry declares more static keys than [`MAX_TXT_PAIRS`]
/// leaves once those two are reserved. `declare_service!` data is static,
/// so the panic fires on the first call rather than on some later input.
fn build_txt_pairs<'a>(
    entry: &'a ServiceEntry,
    board_name: &'a str,
    mcu_name: &'a str,
) -> Vec<(&'a str, &'a str), { MAX_TXT_PAIRS }> {
    let mut pairs: Vec<(&str, &str), { MAX_TXT_PAIRS }> = Vec::new();
    // A length mismatch would let zip() drop the trailing entries of the
    // longer slice, advertising fewer pairs than declare_service! names.
    // The mismatch is static, so a debug assertion catches it in tests
    // without spending release flash on a panic.
    debug_assert_eq!(
        entry.txt_keys.len(),
        entry.txt_values.len(),
        "BUG: declare_service! passed mismatched TXT key/value slice lengths"
    );
    let static_cap = MAX_TXT_PAIRS.saturating_sub(2);
    assert!(
        entry.txt_keys.len() <= static_cap,
        "BUG: declare_service! passed more static TXT keys than MAX_TXT_PAIRS - 2 can hold"
    );
    for (key, value) in entry
        .txt_keys
        .iter()
        .zip(entry.txt_values.iter())
        .take(static_cap)
    {
        pairs
            .push((key, value))
            .expect("BUG: pairs sized for MAX_TXT_PAIRS entries; take(static_cap) bounds iteration to static_cap = MAX_TXT_PAIRS - 2");
    }
    pairs
        .push(("board", board_name))
        .expect("BUG: pairs has capacity for board= entry; static-key assert proves at most MAX_TXT_PAIRS - 2 entries were pushed");
    pairs
        .push(("mcu", mcu_name))
        .expect("BUG: pairs has capacity for mcu= entry; static-key assert proves at most MAX_TXT_PAIRS - 2 entries were pushed");
    pairs
}

fn build_service<'a>(
    entry: &'a ServiceEntry,
    hostname: &'a str,
    txt_storage: &'a [(&'a str, &'a str)],
) -> Service<'a> {
    Service {
        name: hostname,
        priority: 0,
        weight: 0,
        service: entry.service,
        protocol: TCP_PROTOCOL,
        port: entry.port,
        service_subtypes: &[],
        txt_kvs: txt_storage,
    }
}

/// Announce `<hostname>.local.` plus every entry in `services` via mDNS.
///
/// `hostname` is the DNS-safe per-unit label used in the A and SRV
/// records. `board_name` and `mcu_name` reach every service's TXT record
/// as `board=<board_name>` and `mcu=<mcu_name>`.
///
/// Each iteration reads the stack's current IPv4 config afresh and waits
/// [`RETRY_DELAY_SECS`] when there is no lease or the responder returns
/// an error, so a DHCP renewal that changes the address is picked up on
/// the next restart and needs no reboot.
///
/// # Panics
///
/// Panics if `services.len()` exceeds [`MAX_SERVICES`]. Callers wire
/// `services` from a `static` slice fixed at firmware link time, so that
/// is a firmware bug and not a runtime input.
pub async fn run_mdns(
    stack: Stack<'static>,
    hostname: &'static str,
    board_name: &'static str,
    mcu_name: &'static str,
    services: &'static [&'static ServiceEntry],
) -> ! {
    let udp_buffers = UDP_BUFFERS.take();
    let udp = Udp::new(stack, udp_buffers);

    loop {
        let Some(config) = stack.config_v4() else {
            Timer::after(Duration::from_secs(RETRY_DELAY_SECS)).await;
            continue;
        };
        let host_ipv4 = config.address.address();
        let octets = host_ipv4.octets();
        info!(
            "mdns: starting on {}.{}.{}.{}",
            octets[0], octets[1], octets[2], octets[3]
        );

        // The elements must outlive `bind_and_run`, because the services
        // built below borrow slices into them.
        let mut txt_storage: Vec<Vec<(&str, &str), { MAX_TXT_PAIRS }>, { MAX_SERVICES }> =
            Vec::new();
        for entry in services {
            txt_storage
                .push(build_txt_pairs(entry, board_name, mcu_name))
                .expect("BUG: txt_storage sized for MAX_SERVICES entries; services slice must not exceed MAX_SERVICES");
        }

        let mut built: Vec<Service<'_>, { MAX_SERVICES }> = Vec::new();
        for (entry, txt) in services.iter().zip(txt_storage.iter()) {
            built
                .push(build_service(entry, hostname, txt.as_slice()))
                .expect("BUG: built sized for MAX_SERVICES entries; services slice must not exceed MAX_SERVICES");
        }

        if let Err(error) = bind_and_run(&udp, host_ipv4, hostname, built.as_slice()).await {
            warn!("mdns: {:?}", Debug2Format(&error));
        }

        Timer::after(Duration::from_secs(RETRY_DELAY_SECS)).await;
    }
}

async fn bind_and_run(
    udp: &Udp<'_>,
    host_ipv4: Ipv4Addr,
    hostname: &str,
    services: &[Service<'_>],
) -> Result<(), MdnsIoError<UdpError>> {
    let host = Host {
        hostname,
        ipv4: host_ipv4,
        // The AAAA record is skipped because this address is unspecified.
        ipv6: Ipv6Addr::UNSPECIFIED,
        ttl: Ttl::from_secs(TTL_SECS),
    };

    let mut socket = io::bind(udp, IPV4_DEFAULT_SOCKET, Some(Ipv4Addr::UNSPECIFIED), None).await?;

    let (recv, send) = socket.split();

    let handler = HostAnswersMdnsHandler::new(ServicesAnswers {
        host: &host,
        services,
    });

    let mdns = io::Mdns::<_, _, _, _, _, CriticalSectionRawMutex>::new(
        Some(Ipv4Addr::UNSPECIFIED),
        None,
        recv,
        send,
        &RECV_BUFFER,
        &SEND_BUFFER,
        CounterRng::new(hostname),
        &MDNS_SIGNAL,
    );

    mdns.run(handler).await
}

/// Length of the `-XX` per-unit suffix appended to `base` by
/// [`compose_hostname`].
const HOSTNAME_SUFFIX_LEN: usize = 3;

/// Compile-time guard: `CAP` must be at least `BASE_LEN + HOSTNAME_SUFFIX_LEN`.
///
/// Referencing `AssertCap::<CAP, BASE_LEN>::OK` evaluates the assertion
/// at build time, so an enlarged `MDNS_HOSTNAME_BASE` fails the build
/// instead of panicking at boot.
struct AssertCap<const CAP: usize, const BASE_LEN: usize>;

impl<const CAP: usize, const BASE_LEN: usize> AssertCap<CAP, BASE_LEN> {
    const OK: () = assert!(
        CAP >= BASE_LEN + HOSTNAME_SUFFIX_LEN,
        "MDNS_HOSTNAME_BASE leaves no room for the `-XX` serial suffix",
    );
}

/// Compose `<base>-<suffix:02x>` into `cell` and return it as
/// `&'static str`.
///
/// `cell` must be initialised exactly once for the lifetime of the
/// firmware. `BASE_LEN` must equal `base.len()`, which is what lets
/// `AssertCap` prove at build time that `CAP` leaves room.
///
/// Callers pass the low MAC byte as `suffix`: where the MAC is derived
/// per unit the hostname is unique, and where it is a const family MAC
/// the units of one family share a hostname and rely on RFC 6762 §9
/// conflict-resolution renames every boot. `Board::NAME` contains
/// spaces, so it cannot be used as a DNS label.
///
/// # Panics
///
/// Panics if `BASE_LEN` does not equal `base.len()`. Callers derive
/// `BASE_LEN` from the same `base` symbol, so this fires only on a
/// hand-written literal.
pub fn compose_hostname<const CAP: usize, const BASE_LEN: usize>(
    base: &'static str,
    suffix: u8,
    cell: &'static StaticCell<heapless::String<CAP>>,
) -> &'static str {
    // Referencing AssertCap::OK triggers the compile-time assertion.
    let _: () = AssertCap::<CAP, BASE_LEN>::OK;
    assert_eq!(
        base.len(),
        BASE_LEN,
        "BUG: BASE_LEN const generic must equal base.len()",
    );
    let mut buffer: heapless::String<CAP> = heapless::String::new();
    write!(&mut buffer, "{base}-{suffix:02x}")
        .expect("BUG: mDNS hostname does not fit in the buffer");
    cell.init(buffer).as_str()
}

/// Compose a hostname, preferring an operator-configured value over the
/// `<base>-<suffix:02x>` default.
///
/// An empty `configured` is the sentinel for the build-time default. A
/// value too long for `CAP` falls back to that default instead of
/// truncating or failing, so `CAP` must be at least `HOSTNAME_MAX` of
/// `ubos-config` for every operator hostname to survive.
///
/// The config layer validates the DNS-label charset, so `configured` is
/// not re-checked here. `cell` carries the same constraint as in
/// [`compose_hostname`].
pub fn compose_hostname_configured<const CAP: usize, const BASE_LEN: usize>(
    configured: &str,
    base: &'static str,
    suffix: u8,
    cell: &'static StaticCell<heapless::String<CAP>>,
) -> &'static str {
    if configured.is_empty() {
        return compose_hostname::<CAP, BASE_LEN>(base, suffix, cell);
    }
    let mut buffer: heapless::String<CAP> = heapless::String::new();
    let Ok(()) = buffer.push_str(configured) else {
        return compose_hostname::<CAP, BASE_LEN>(base, suffix, cell);
    };
    cell.init(buffer).as_str()
}
