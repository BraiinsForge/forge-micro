//! Network tasks shared across all boards: DHCP wait, NTP sync, stratum
//! TCP connect, and cross-board atomic state.
//!
//! Architecture: `../../../docs/OVERVIEW.md`.

use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Ipv4Address, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use portable_atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use ubos_logging::{info, warn, Debug2Format};

use ubos_core::consts::Net;

use crate::ntp::Ntp;

/// Maximum number of DNS A-record results retained for connect attempts.
/// Pool DNS responses are typically two or three addresses; four covers the
/// largest observed Braiins record set with one slot to spare.
const MAX_DNS_RESULTS: usize = 4;

/// Cap on how long [`connect_stratum`] waits for the DNS resolver. The TCP
/// connect already has its own per-socket timeout; this prevents an
/// unresponsive resolver from hanging the stratum task indefinitely between
/// reconnect attempts.
const DNS_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Error returned by [`connect_stratum`] when the host cannot be reached.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum StratumConnectError {
    /// DNS query timed out or returned an error from the resolver.
    DnsQueryFailed,
    /// DNS query succeeded but returned no A records.
    NoDnsResults,
    /// Every resolved address was tried and all TCP connects failed.
    AllConnectsFailed,
}

/// Resolve `host` and open a TCP connection to one of the resulting
/// addresses on `port`. If `host` is an IPv4 literal, attempts only that
/// address. Otherwise queries DNS A records and tries each in order,
/// returning the first that connects. Logs a warn line for each address
/// that fails to connect before falling through to the next.
///
/// # Errors
///
/// Returns [`StratumConnectError::DnsQueryFailed`] if the DNS resolver
/// returns an error, [`StratumConnectError::NoDnsResults`] if the query
/// returns no A records, and [`StratumConnectError::AllConnectsFailed`] if
/// every resolved address (or the IPv4 literal) fails to connect.
pub async fn connect_stratum<'b>(
    stack: Stack<'static>,
    host: &str,
    port: u16,
    timeout: Duration,
    rx_buffer: &'b mut [u8],
    tx_buffer: &'b mut [u8],
) -> Result<TcpSocket<'b>, StratumConnectError> {
    let mut candidates: heapless::Vec<IpAddress, MAX_DNS_RESULTS> = heapless::Vec::new();
    if let Ok(ipv4) = host.parse::<core::net::Ipv4Addr>() {
        // `push` cannot fail on the first insertion into an empty Vec.
        let _ = candidates.push(IpAddress::from(ipv4));
    } else {
        let query = with_timeout(DNS_QUERY_TIMEOUT, stack.dns_query(host, DnsQueryType::A)).await;
        match query {
            Ok(Ok(found)) => {
                for address in found.iter().take(candidates.capacity()) {
                    let _ = candidates.push(*address);
                }
            }
            Ok(Err(error)) => {
                warn!(
                    "stratum-client: DNS query failed: {:?}",
                    Debug2Format(&error)
                );
                return Err(StratumConnectError::DnsQueryFailed);
            }
            Err(_) => {
                warn!("stratum-client: DNS query timed out for {}", host);
                return Err(StratumConnectError::DnsQueryFailed);
            }
        }
    }
    if candidates.is_empty() {
        warn!("stratum-client: no DNS results for {}", host);
        return Err(StratumConnectError::NoDnsResults);
    }

    // One socket reused across attempts: under embassy-net 0.9.0 a failed
    // `connect` leaves the smoltcp state machine back in `Closed`, so the
    // next iteration can drive another SYN through the same socket. The
    // buffers are borrowed exactly once. Reverify this assumption on every
    // embassy-net version bump.
    let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
    socket.set_timeout(Some(timeout));
    for address in &candidates {
        info!(
            "stratum-client: connecting to {}:{} ({:?})",
            host,
            port,
            Debug2Format(address)
        );
        match socket.connect((*address, port)).await {
            Ok(()) => {
                info!(
                    "stratum-client: connected to {}:{} ({:?})",
                    host,
                    port,
                    Debug2Format(address)
                );
                return Ok(socket);
            }
            Err(error) => warn!(
                "stratum-client: connect to {:?} failed: {:?}",
                Debug2Format(address),
                Debug2Format(&error)
            ),
        }
    }
    Err(StratumConnectError::AllConnectsFailed)
}

/// Count of accepted HTTP requests since boot, incremented by the
/// dashboard HTTP server in `ubos-api::dashboard`.
pub static REQUEST_COUNT: AtomicU32 = AtomicU32::new(0);
pub static NTP_EPOCH_OFFSET: AtomicU64 = AtomicU64::new(0);
type CSMutex = CriticalSectionRawMutex;

/// Cross-module network identity published by `ubos-net`: the live IPv4
/// address and the board MAC. Read by the display, the REST API, and the
/// system endpoint through [`Handle`].
///
/// Each field is `None` until its producer runs, so a consumer reading the
/// state before DHCP assigns an address still sees the MAC, and vice versa.
#[derive(Clone, Copy, Default)]
pub struct NetState {
    /// Live IPv4 address, or `None` until DHCP or a static config assigns
    /// one. Written at boot by [`wait_for_config`] and refreshed on live
    /// applies by the network-apply task.
    pub ipv4: Option<Ipv4Address>,
    /// Board MAC, or `None` until [`Handle::set_mac`] runs at boot.
    pub mac: Option<[u8; 6]>,
}

/// Subscriber capacity for the net-state watch. Every consumer today reads
/// point-in-time via [`Handle::current`], which consumes no receiver slot,
/// so one slot covers any future subscriber.
const WATCH_CAPACITY: usize = 1;

/// Module-internal state watch. Writers are [`wait_for_config`], the
/// network-apply task, and the boot MAC publish; readers obtain values via
/// [`Handle`].
static STATE_WATCH: Watch<CSMutex, NetState, WATCH_CAPACITY> = Watch::new();

/// Per-task subscription returned by [`Handle::subscribe`].
pub type NetSubscription = Receiver<'static, CSMutex, NetState, WATCH_CAPACITY>;

/// Obtain a handle to the network-identity state. `Copy`; callers freely
/// pass it to multiple tasks.
#[must_use]
pub fn handle() -> Handle {
    Handle {
        state: &STATE_WATCH,
    }
}

/// Consumer- and producer-side handle for the network-identity state.
///
/// `Copy` because it holds only a `&'static` reference. Every task takes
/// its own value, mirroring the `handle()` accessors on the thermal and
/// PSU watches.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static Watch<CSMutex, NetState, WATCH_CAPACITY>,
}

impl Handle {
    /// Most recent published state, or `None` if neither the IP nor the MAC
    /// has been published yet.
    #[must_use]
    pub fn current(&self) -> Option<NetState> {
        self.state.try_get()
    }

    /// Per-task subscription. Call once per consumer task before entering
    /// its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&self) -> NetSubscription {
        self.state
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Publish the live IPv4 address, leaving the MAC field untouched.
    pub fn set_ipv4(&self, ipv4: Ipv4Address) {
        self.state
            .sender()
            .send_modify(|state| state.get_or_insert_with(NetState::default).ipv4 = Some(ipv4));
    }

    /// Publish the board MAC, leaving the IPv4 field untouched.
    ///
    /// Each firmware supplies its own six octets, derived from the chip id
    /// on the `WIZnet` boards or read from board config on the others.
    /// Calling this once at boot lets `/api/info` report the address without
    /// threading it through the network stack.
    pub fn set_mac(&self, mac: [u8; 6]) {
        self.state
            .sender()
            .send_modify(|state| state.get_or_insert_with(NetState::default).mac = Some(mac));
    }
}

/// Per-pool reachability bitmap, one bit per pool index. Set bits
/// render as `Alive` on the dashboard; cleared bits as `Dead`.
/// Written by the stratum task via [`set_pool_alive`].
///
/// Only bit 0 (primary pool) is driven today. The dashboard reserves
/// rows for backup pools; their bits stay zero and render `Dead`
/// until secondary/tertiary stratum clients are wired up.
pub static POOL_ALIVE_BITMAP: AtomicU8 = AtomicU8::new(0);

/// Set or clear the reachability bit for `pool_index` (0 = primary).
///
/// Thread-safe; uses a relaxed RMW so the bitmap stays coherent even
/// when multiple tasks race on separate pool indexes.
pub fn set_pool_alive(pool_index: u8, alive: bool) {
    // The debug_assert traps a bad index in tests; the guard keeps an
    // out-of-range shift from writing a bogus bit in release firmware.
    debug_assert!(pool_index < 8, "pool_index must fit in the u8 bitmap");
    if pool_index >= 8 {
        return;
    }
    let bit = 1u8 << pool_index;
    if alive {
        POOL_ALIVE_BITMAP.fetch_or(bit, Ordering::Relaxed);
    } else {
        POOL_ALIVE_BITMAP.fetch_and(!bit, Ordering::Relaxed);
    }
}

/// Wait until the network stack receives a DHCP lease.
///
/// Polls `stack.config_v4()` at a 100 ms cadence rather than yielding
/// inside a tight loop, so the executor is not pegged for the entire
/// DHCP acquisition window.
pub async fn wait_for_config(stack: Stack<'static>) -> embassy_net::StaticConfigV4 {
    loop {
        if let Some(config) = stack.config_v4() {
            handle().set_ipv4(config.address.address());
            return config.clone();
        }
        // 100 ms is short enough to keep DHCP re-acquisition responsive but
        // long enough to keep this poll off the executor hot path. Previously
        // a `yield_now()`, which was a CPU-bound spin.
        Timer::after(Duration::from_millis(100)).await;
    }
}

/// Perform a single NTP time synchronization.
///
/// # Errors
///
/// Returns an error string if DNS resolution, UDP communication, or NTP
/// response parsing fails.
pub async fn fetch_ntp_time(stack: Stack<'static>) -> Result<(), &'static str> {
    let addresses = with_timeout(
        DNS_QUERY_TIMEOUT,
        stack.dns_query(Ntp::HOST, DnsQueryType::A),
    )
    .await
    .map_err(|_| "dns query timeout")?
    .map_err(|_| "dns query failed")?;
    let address = *addresses.first().ok_or("no dns results")?;

    let mut rx_meta = [PacketMetadata::EMPTY; 1];
    let mut rx_buffer = [0; Ntp::PACKET_SIZE];
    let mut tx_meta = [PacketMetadata::EMPTY; 1];
    let mut tx_buffer = [0; Ntp::PACKET_SIZE];
    let mut socket = UdpSocket::new(
        stack,
        &mut rx_meta,
        &mut rx_buffer,
        &mut tx_meta,
        &mut tx_buffer,
    );
    socket.bind(0).map_err(|_| "udp bind failed")?;

    // SNTP request: first byte 0x1b selects version 3 client mode, rest zeros.
    let mut request = [0u8; Ntp::PACKET_SIZE];
    request[0] = 0x1b;
    socket
        .send_to(&request, (address, Ntp::PORT))
        .await
        .map_err(|_| "udp send failed")?;

    let mut response = [0u8; Ntp::PACKET_SIZE];
    let (length, _remote) = with_timeout(Net::SOCKET_TIMEOUT, socket.recv_from(&mut response))
        .await
        .map_err(|_| "ntp recv timeout")?
        .map_err(|_| "udp recv failed")?;

    let unix_secs =
        crate::ntp::parse_timestamp_secs(&response[..length]).ok_or("ntp parse failed")?;
    let uptime_secs = Instant::now().as_secs();
    let offset = unix_secs.saturating_sub(uptime_secs);
    info!("ntp time synced: offset {}s", offset);
    NTP_EPOCH_OFFSET.store(offset, Ordering::Relaxed);

    Ok(())
}

/// Periodically sync NTP time. Runs forever.
pub async fn run_ntp_sync(stack: Stack<'static>) -> ! {
    wait_for_config(stack).await;
    Timer::after(Ntp::INITIAL_DELAY).await;

    loop {
        if let Err(error) = fetch_ntp_time(stack).await {
            warn!("ntp sync failed: {}", error);
        }
        Timer::after(Ntp::SYNC_INTERVAL).await;
    }
}
