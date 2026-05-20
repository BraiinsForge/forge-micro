//! mDNS responder advertising the board hostname and Prometheus service.
//!
//! Delegates the RFC 6762 probe phase from §8.1, the announcement phase from
//! §8.3, and query-response handling to edge-mdns. We provide:
//! - A: `<hostname>.local.` → DHCP-assigned IPv4
//! - PTR/SRV/TXT: `_prometheus._tcp.local.` on `Metrics::PORT` with TXT
//!   `path=/metrics`, `board=<board_name>`, `mcu=<mcu_name>`
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
use edge_mdns::HostAnswersMdnsHandler;
use edge_nal::UdpSplit;
use edge_nal_embassy::{Udp, UdpBuffers, UdpError};
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use rand_core_v010::TryRng;
use static_cell::{ConstStaticCell, StaticCell};
use ubos_logging::{info, warn, Debug2Format};

use crate::consts::{Mdns as MdnsConsts, Metrics};

/// `edge_mdns::io::Mdns::new` takes a `&Signal` to wake the responder
/// for unsolicited re-announces. We never signal it; address changes
/// are picked up via the socket-error restart loop in `run_mdns`. The
/// static still has to exist because the constructor needs a reference
/// it can borrow for the responder's lifetime.
static MDNS_SIGNAL: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Scratch buffer for incoming mDNS datagrams handed to `edge-mdns`.
static RECV_BUFFER: VecBufAccess<CriticalSectionRawMutex, { MdnsConsts::BUFFER_SIZE }> =
    VecBufAccess::new();
/// Scratch buffer for outgoing mDNS datagrams built by `edge-mdns`.
static SEND_BUFFER: VecBufAccess<CriticalSectionRawMutex, { MdnsConsts::BUFFER_SIZE }> =
    VecBufAccess::new();

/// Pool of raw UDP socket buffers consumed by `edge-nal-embassy`.
static UDP_BUFFERS: ConstStaticCell<
    UdpBuffers<
        { MdnsConsts::SOCKET_POOL_SIZE },
        { MdnsConsts::BUFFER_SIZE },
        { MdnsConsts::BUFFER_SIZE },
        { MdnsConsts::METADATA_SLOTS },
    >,
> = ConstStaticCell::new(UdpBuffers::new());

/// A counter-based RNG used only for the 0-250 ms probe-jitter delay.
///
/// RFC 6762 §8.1 requires a random delay before sending probes to reduce
/// multicast collisions. Cryptographic quality is not required; a simple
/// LCG seeded from boot time is sufficient and avoids pulling in an entropy
/// source on the no-std target. Two same-family boards booting together
/// will seed from nearly identical tick counts, so their probe-jitter
/// delays will be correlated rather than statistically independent.
struct CounterRng(u64);

impl CounterRng {
    /// Seed the RNG from the current monotonic clock tick.
    fn new() -> Self {
        Self(Instant::now().as_ticks())
    }
}

impl TryRng for CounterRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
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

/// Announce `<hostname>.local.` and `_prometheus._tcp.local.` via mDNS.
///
/// `hostname` is the DNS-safe per-unit label used in the A and SRV records;
/// `board_name` is the human-readable board family exposed in the TXT
/// `board=` key so scrapers can label samples by board kind rather than by
/// the per-unit hostname they already see in the SRV target.
///
/// Each iteration reads the stack's current IPv4 config; if no lease is
/// held the loop sleeps for [`MdnsConsts::RETRY_DELAY_SECS`] seconds and
/// tries again. On any socket or protocol error the same delay applies,
/// so an address change after a DHCP renewal is picked up on the next
/// restart without requiring a reboot.
pub async fn run_mdns(
    stack: Stack<'static>,
    hostname: &'static str,
    board_name: &'static str,
    mcu_name: &'static str,
) -> ! {
    let udp_buffers = UDP_BUFFERS.take();
    let udp = Udp::new(stack, udp_buffers);

    let txt_kvs: &[(&str, &str)] = &[
        ("path", MdnsConsts::METRICS_PATH),
        ("board", board_name),
        ("mcu", mcu_name),
    ];

    loop {
        let Some(config) = stack.config_v4() else {
            Timer::after(Duration::from_secs(MdnsConsts::RETRY_DELAY_SECS)).await;
            continue;
        };
        let host_ipv4 = config.address.address();
        let octets = host_ipv4.octets();
        info!(
            "mdns: starting on {}.{}.{}.{}",
            octets[0], octets[1], octets[2], octets[3]
        );

        if let Err(error) = bind_and_run(&udp, host_ipv4, hostname, txt_kvs).await {
            warn!("mdns: {:?}", Debug2Format(&error));
        }

        Timer::after(Duration::from_secs(MdnsConsts::RETRY_DELAY_SECS)).await;
    }
}

/// Bind the UDP socket, build the host and service records, and run the
/// edge-mdns responder until it returns an error.
async fn bind_and_run(
    udp: &Udp<'_>,
    host_ipv4: Ipv4Addr,
    hostname: &str,
    txt_kvs: &[(&str, &str)],
) -> Result<(), MdnsIoError<UdpError>> {
    let host = Host {
        hostname,
        ipv4: host_ipv4,
        ipv6: Ipv6Addr::UNSPECIFIED,
        ttl: Ttl::from_secs(MdnsConsts::TTL_SECS),
    };

    let service = Service {
        name: hostname,
        priority: 0,
        weight: 0,
        service: MdnsConsts::SERVICE_TYPE,
        protocol: MdnsConsts::SERVICE_PROTOCOL,
        port: Metrics::PORT,
        service_subtypes: &[],
        txt_kvs,
    };

    let mut socket = io::bind(udp, IPV4_DEFAULT_SOCKET, Some(Ipv4Addr::UNSPECIFIED), None).await?;

    let (recv, send) = socket.split();

    let handler = HostAnswersMdnsHandler::new(ServiceAnswers::new(&host, &service));

    let mdns = io::Mdns::<_, _, _, _, _, CriticalSectionRawMutex>::new(
        Some(Ipv4Addr::UNSPECIFIED),
        None,
        recv,
        send,
        &RECV_BUFFER,
        &SEND_BUFFER,
        CounterRng::new(),
        &MDNS_SIGNAL,
    );

    mdns.run(handler).await
}

/// Length of the `-XX` per-unit suffix appended to `base` by [`compose_hostname`].
const HOSTNAME_SUFFIX_LEN: usize = 3;

/// Compile-time guard: `CAP` must be at least `BASE_LEN + HOSTNAME_SUFFIX_LEN`.
///
/// Evaluated by referencing `AssertCap::<CAP, BASE_LEN>::OK` in the caller.
/// The associated const triggers a compile-time assertion rather than a
/// runtime panic, so a future enlargement of `MDNS_HOSTNAME_BASE` surfaces
/// at build time rather than at boot.
struct AssertCap<const CAP: usize, const BASE_LEN: usize>;

impl<const CAP: usize, const BASE_LEN: usize> AssertCap<CAP, BASE_LEN> {
    const OK: () = assert!(
        CAP >= BASE_LEN + HOSTNAME_SUFFIX_LEN,
        "MDNS_HOSTNAME_BASE leaves no room for the `-XX` serial suffix",
    );
}

/// Compose `<base>-<suffix:02x>` into `cell` and return it as `&'static str`.
///
/// `cell` must be initialised exactly once for the lifetime of the firmware;
/// callers reuse the returned slice as the mDNS hostname. `CAP` must be at
/// least `base.len() + HOSTNAME_SUFFIX_LEN`; `BASE_LEN` must equal
/// `base.len()` so the const assertion turns a future enlargement of `base`
/// into a build failure rather than a boot-time panic.
///
/// # Panics
///
/// Panics if `BASE_LEN` does not equal `base.len()`. Callers pass
/// `BASE_LEN` as a `const` derived from the same `base` symbol, so this
/// only fires when a caller hand-writes an inconsistent literal.
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
