//! mDNS responder advertising the board hostname and its DNS-SD services.
//!
//! Delegates the RFC 6762 probe phase from §8.1, the announcement phase from
//! §8.3, and query-response handling to `edge-mdns`. We provide:
//! - A: `<hostname>.local.` → DHCP-assigned IPv4
//! - PTR/SRV/TXT for each entry in the `services` slice the caller passes
//!   to [`run_mdns`]. Each entry is a `&ServiceEntry` declared by the crate
//!   that owns the port via [`declare_service!`](crate::declare_service).
//!   Two runtime TXT entries — `board=<board_name>` and `mcu=<mcu_name>` —
//!   are appended to every service's TXT record by the responder.
//!
//! The responder reads `stack.config_v4()` on every loop iteration, so a
//! DHCP renewal with a new lease becomes visible on the next socket-error
//! restart.

use core::convert::Infallible;
use core::net::{Ipv4Addr, Ipv6Addr};

use edge_mdns::buf::VecBufAccess;
use edge_mdns::domain::base::Ttl;
use edge_mdns::host::{Host, Service, ServiceAnswers};
use edge_mdns::io::{self, MdnsIoError, IPV4_DEFAULT_SOCKET};
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
use ubos_logging::{info, warn, Debug2Format};

use crate::registry::ServiceEntry;

/// Transport protocol used by every service the responder advertises.
const TCP_PROTOCOL: &str = "_tcp";
/// A record TTL in seconds. RFC 6762 §10 recommends 120 s for volatile
/// addresses such as DHCP leases.
const TTL_SECS: u32 = 120;
/// Seconds to wait before retrying after a bind or run error.
const RETRY_DELAY_SECS: u64 = 5;
/// Buffer size for mDNS datagrams. RFC 6762 §17 caps mDNS messages at
/// the interface UDP payload max.
const BUFFER_SIZE: usize = 1472;
/// Packet-metadata slots per direction for the mDNS UDP socket.
const METADATA_SLOTS: usize = 4;
/// Sockets in the UDP pool. One is sufficient because the responder
/// holds a single socket for the lifetime of each run attempt.
const SOCKET_POOL_SIZE: usize = 1;
/// Maximum number of services that the responder can announce in a
/// single run. Eight entries occupy at most a few hundred bytes of
/// stack and leave room for all planned service crates without
/// revisiting this constant.
const MAX_SERVICES: usize = 8;
/// Maximum number of TXT key-value pairs per service entry, including
/// the two runtime-appended `board=` and `mcu=` pairs. Four static
/// pairs plus two runtime pairs is sufficient for any service planned
/// in this workspace.
const MAX_TXT_PAIRS: usize = 6;

/// `edge_mdns::io::Mdns::new` takes a `&Signal` to wake the responder
/// for unsolicited re-announces. We never signal it; address changes
/// are picked up via the socket-error restart loop in `run_mdns`. The
/// static still has to exist because the constructor needs a reference
/// it can borrow for the responder's lifetime.
static MDNS_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Scratch buffer for incoming mDNS datagrams handed to `edge-mdns`.
static RECV_BUFFER: VecBufAccess<CriticalSectionRawMutex, { BUFFER_SIZE }> = VecBufAccess::new();
/// Scratch buffer for outgoing mDNS datagrams built by `edge-mdns`.
static SEND_BUFFER: VecBufAccess<CriticalSectionRawMutex, { BUFFER_SIZE }> = VecBufAccess::new();

/// Pool of raw UDP socket buffers consumed by `edge-nal-embassy`.
static UDP_BUFFERS: ConstStaticCell<
    UdpBuffers<{ SOCKET_POOL_SIZE }, { BUFFER_SIZE }, { BUFFER_SIZE }, { METADATA_SLOTS }>,
> = ConstStaticCell::new(UdpBuffers::new());

/// A counter-based RNG used only for the 0-250 ms probe-jitter delay.
///
/// RFC 6762 §8.1 requires a random delay before sending probes to reduce
/// multicast collisions. Cryptographic quality is not required; a simple
/// LCG seeded from boot time plus the per-board hostname is sufficient
/// and avoids pulling in an entropy source on the no-std target.
/// `compose_hostname` folds one MAC byte into the hostname. On the
/// boards whose MAC is derived per unit that byte differs between
/// same-family boards, so they seed distinct LCGs even when their boot
/// ticks line up. On boards with a const family MAC the byte is shared,
/// so same-family units rely instead on differing boot ticks and the
/// `edge-mdns` conflict-resolution path.
struct CounterRng(u64);

impl CounterRng {
    /// Seed the RNG by mixing the current monotonic tick with `hostname`.
    ///
    /// The hostname seeds the high half of the LCG state through a
    /// FNV-1a fold; the boot tick seeds the low half. Boards with
    /// distinct hostnames (and so distinct MAC suffixes) start the
    /// LCG at different points even when they boot simultaneously.
    fn new(hostname: &str) -> Self {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash: u64 = FNV_OFFSET;
        for &byte in hostname.as_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        let ticks = Instant::now().as_ticks();
        // rotate_left(32) spreads the lower-bits-dominant tick count into the high half so identical-boot-time boards still pick different probe jitter.
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
        // Returns the high 32 bits of the 64-bit LCG state; the explicit
        // narrowing cast drops the low half by construction, not by
        // accident.
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
/// shared `host`. Adding another `_…._tcp` advert means adding an entry to
/// the registry that the caller builds; the responder loop and call sites do
/// not change.
///
/// Each visited [`ServiceAnswers`] re-emits the host's A record (AAAA is
/// skipped because `Host::ipv6` is `UNSPECIFIED`), so an A query response
/// includes one copy per service. Receivers collapse duplicate records with
/// the same name, type, and rdata into a single cache entry per RFC 6762 §10
/// (cache management); stripping them on our side would require
/// reimplementing `Service::visit_answers`.
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

/// Build the TXT key-value pairs for a registry entry by zipping the
/// entry's parallel key/value slices and appending the runtime
/// `board=` and `mcu=` pairs.
///
/// Returns a `heapless::Vec` capped at [`MAX_TXT_PAIRS`].
/// Two slots are reserved for the runtime `board=` and `mcu=` entries,
/// leaving `MAX_TXT_PAIRS - 2` = 4 slots for static keys. Panics if
/// the registry entry exceeds that budget; `declare_service!` data is
/// static so the panic is deterministic on first call rather than a
/// runtime-data-dependent failure. Every caller in the workspace
/// currently provides 1 static key.
fn build_txt_pairs<'a>(
    entry: &'a ServiceEntry,
    board_name: &'a str,
    mcu_name: &'a str,
) -> Vec<(&'a str, &'a str), { MAX_TXT_PAIRS }> {
    // Static keys must leave room for the two runtime entries appended
    // below. Overflow is a declare_service! bug; the assert surfaces it
    // before .take() would silently truncate.
    let mut pairs: Vec<(&str, &str), { MAX_TXT_PAIRS }> = Vec::new();
    // Parallel slices: a length mismatch would let zip() silently drop
    // trailing entries on the longer slice, advertising fewer TXT pairs
    // than the caller wrote in declare_service!. The debug_assert
    // surfaces the inconsistency in tests without spending flash on a
    // release-build panic for what is a compile-time-static bug.
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
        // take(static_cap) bounds the iterator to at most static_cap entries,
        // which is MAX_TXT_PAIRS - 2, so pairs always has capacity for each push.
        pairs
            .push((key, value))
            .expect("BUG: pairs sized for MAX_TXT_PAIRS entries; take(static_cap) bounds iteration to static_cap = MAX_TXT_PAIRS - 2");
    }
    // The assert above proves entry.txt_keys.len() <= static_cap = MAX_TXT_PAIRS - 2,
    // leaving exactly two free slots for the board= and mcu= runtime entries.
    pairs
        .push(("board", board_name))
        .expect("BUG: pairs has capacity for board= entry; static-key assert proves at most MAX_TXT_PAIRS - 2 entries were pushed");
    pairs
        .push(("mcu", mcu_name))
        .expect("BUG: pairs has capacity for mcu= entry; static-key assert proves at most MAX_TXT_PAIRS - 2 entries were pushed");
    pairs
}

/// Build an `edge_mdns::Service` from a registry entry.
///
/// `txt_storage` is a reference to the already-built TXT pairs for
/// this entry; it must outlive the returned `Service`.
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
/// records. `board_name` and `mcu_name` are the human-readable board
/// and MCU identifiers appended to every service's TXT record as
/// `board=<board_name>` and `mcu=<mcu_name>`. `services` is the list
/// of advertisements composed by the firmware; the responder iterates
/// it on every loop pass to build the SRV and TXT records.
///
/// Each iteration reads the stack's current IPv4 config; if no lease
/// is held the loop sleeps for [`RETRY_DELAY_SECS`]
/// seconds and tries again. On any socket or protocol error the same
/// delay applies, so an address change after a DHCP renewal is picked
/// up on the next restart without requiring a reboot.
///
/// # Panics
///
/// Panics if `services.len()` exceeds [`MAX_SERVICES`]:
/// the per-service `txt_storage` and the `built` answer-record vec are
/// sized for that bound. Callers wire `services` from a `static` slice
/// determined at firmware link time, so this is a firmware bug, not a
/// runtime input.
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

        // Build TXT storage for each caller-supplied entry on the stack.
        // The Vec elements live for the duration of `bind_and_run`;
        // `build_service` borrows slices into these Vecs.
        let mut txt_storage: Vec<Vec<(&str, &str), { MAX_TXT_PAIRS }>, { MAX_SERVICES }> =
            Vec::new();
        for entry in services {
            // `services` is bounded to MAX_SERVICES by the caller's static slice.
            txt_storage
                .push(build_txt_pairs(entry, board_name, mcu_name))
                .expect("BUG: txt_storage sized for MAX_SERVICES entries; services slice must not exceed MAX_SERVICES");
        }

        let mut built: Vec<Service<'_>, { MAX_SERVICES }> = Vec::new();
        for (entry, txt) in services.iter().zip(txt_storage.iter()) {
            // zip() produces at most services.len() pairs, which is bounded
            // to MAX_SERVICES by the same invariant as txt_storage above.
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

/// Bind the UDP socket, build the host record, and run the edge-mdns
/// responder over the caller's service list until it returns an error.
async fn bind_and_run(
    udp: &Udp<'_>,
    host_ipv4: Ipv4Addr,
    hostname: &str,
    services: &[Service<'_>],
) -> Result<(), MdnsIoError<UdpError>> {
    let host = Host {
        hostname,
        ipv4: host_ipv4,
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
/// Evaluated by referencing `AssertCap::<CAP, BASE_LEN>::OK` in the
/// caller. The associated const triggers a compile-time assertion
/// rather than a runtime panic, so a future enlargement of
/// `MDNS_HOSTNAME_BASE` surfaces at build time rather than at boot.
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
/// firmware; callers reuse the returned slice as the mDNS hostname.
/// `CAP` must be at least `base.len() + HOSTNAME_SUFFIX_LEN`;
/// `BASE_LEN` must equal `base.len()` so the const assertion turns a
/// future enlargement of `base` into a build failure rather than a
/// boot-time panic.
///
/// `base` is hardcoded per board family. Callers pass the low MAC byte
/// as `suffix`: on boards whose MAC is derived per unit it differs
/// between units, so same-family units get distinct hostnames; on
/// boards with a const family MAC it is `MAC_ADDR[5]`, a per-family
/// constant, so same-family units share the hostname across every
/// advertised service and rely on RFC 6762 §9 conflict-resolution
/// renames every boot. `Board::NAME` is human-readable and contains
/// spaces, so it cannot be used as a DNS label.
///
/// # Panics
///
/// Panics if `BASE_LEN` does not equal `base.len()`. Callers pass
/// `BASE_LEN` as a `const` derived from the same `base` symbol, so
/// this only fires when a caller hand-writes an inconsistent literal.
pub fn compose_hostname<const CAP: usize, const BASE_LEN: usize>(
    base: &'static str,
    suffix: u8,
    cell: &'static StaticCell<heapless::String<CAP>>,
) -> &'static str {
    use core::fmt::Write;
    // Referencing AssertCap::OK triggers the compile-time assertion.
    let _: () = AssertCap::<CAP, BASE_LEN>::OK;
    assert_eq!(
        base.len(),
        BASE_LEN,
        "BUG: BASE_LEN const generic must equal base.len()",
    );
    let mut buffer: heapless::String<CAP> = heapless::String::new();
    // `AssertCap` above proves `CAP >= BASE_LEN + HOSTNAME_SUFFIX_LEN`,
    // so writing `<base>-<suffix:02x>` always fits.
    write!(&mut buffer, "{base}-{suffix:02x}")
        .expect("BUG: mDNS hostname does not fit in the buffer");
    cell.init(buffer).as_str()
}
