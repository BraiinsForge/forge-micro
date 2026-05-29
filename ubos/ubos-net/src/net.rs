//! Network tasks shared across all boards: DHCP wait, NTP, BTC price
//! fetch, and the HTTP server loop.
//!
//! Architecture: `../../../docs/OVERVIEW.md`.

use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Stack};
use embassy_time::{with_timeout, Duration, Instant, Timer};
use embedded_io_async::Write;
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use portable_atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use rand_core::{CryptoRng, RngCore};
use static_cell::ConstStaticCell;
use ubos_logging::{info, warn, Debug2Format};

use ubos_core::consts::{Btc, Net, Ntp, Tls};

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
pub static BTC_PRICE_CENTS: AtomicU32 = AtomicU32::new(0);
pub static NTP_EPOCH_OFFSET: AtomicU64 = AtomicU64::new(0);
/// Uptime seconds of the last successful BTC price fetch, or zero if none.
pub static LAST_BTC_FETCH_UPTIME_SECS: AtomicU64 = AtomicU64::new(0);
/// DHCP-assigned IPv4 address encoded via `Ipv4Address::to_bits()`. Zero
/// until the address is assigned; downstream readers treat zero as "no ip".
/// `0.0.0.0` round-trips through this encoding, but `DHCPv4` unicast leases
/// never assign it, so the sentinel does not collide in practice.
pub static LOCAL_IPV4: AtomicU32 = AtomicU32::new(0);
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
    debug_assert!(pool_index < 8, "pool_index must fit in the u8 bitmap");
    let bit = 1u8 << pool_index;
    if alive {
        POOL_ALIVE_BITMAP.fetch_or(bit, Ordering::Relaxed);
    } else {
        POOL_ALIVE_BITMAP.fetch_and(!bit, Ordering::Relaxed);
    }
}

static TCP_RX: ConstStaticCell<[u8; Tls::TCP_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Tls::TCP_BUFFER_SIZE]);
static TCP_TX: ConstStaticCell<[u8; Tls::TCP_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Tls::TCP_BUFFER_SIZE]);
// `TLS_READ` is CPU-only scratch filled by the embedded-tls record parser.
// `TlsConnection::read()` decrypts into this buffer using CPU memcpy paths
// inside embedded-tls; the MAC DMA only ever touches the smoltcp socket
// RX ring (the `TCP_RX` buffer handed to `TcpSocket::new`), which is a
// separate allocation. `TLS_READ` is never passed to any DMA descriptor, so
// it is safe to place in CCM on stm32f4.
#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
static TLS_READ: ConstStaticCell<[u8; Tls::READ_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Tls::READ_BUFFER_SIZE]);
// `TLS_WRITE` is CPU-only scratch for the embedded-tls record builder;
// the bytes are copied to the TCP TX buffer before DMA touches them,
// so it can live in CCM on stm32f4.
#[cfg_attr(feature = "ccmram-bss", link_section = ".ccmram.bss")]
static TLS_WRITE: ConstStaticCell<[u8; Tls::WRITE_BUFFER_SIZE]> =
    ConstStaticCell::new([0; Tls::WRITE_BUFFER_SIZE]);
/// Wait until the network stack receives a DHCP lease.
///
/// Polls `stack.config_v4()` at a 100 ms cadence rather than yielding
/// inside a tight loop, so the executor is not pegged for the entire
/// DHCP acquisition window.
pub async fn wait_for_config(stack: Stack<'static>) -> embassy_net::StaticConfigV4 {
    loop {
        if let Some(config) = stack.config_v4() {
            LOCAL_IPV4.store(config.address.address().to_bits(), Ordering::Relaxed);
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

/// Perform a single BTC price fetch over TLS.
///
/// # Errors
///
/// Returns a TLS error if DNS resolution, TCP connection, TLS handshake,
/// or HTTP request/response fails.
pub async fn fetch_btc_price<R: CryptoRng + RngCore>(
    stack: Stack<'static>,
    rng: &mut R,
    tcp_rx: &mut [u8],
    tcp_tx: &mut [u8],
    tls_read: &mut [u8],
    tls_write: &mut [u8],
) -> Result<(), embedded_tls::TlsError> {
    let addresses = with_timeout(
        DNS_QUERY_TIMEOUT,
        stack.dns_query(Btc::HOST, DnsQueryType::A),
    )
    .await
    .map_err(|_| embedded_tls::TlsError::InternalError)?
    .map_err(|_| embedded_tls::TlsError::InternalError)?;
    let address = *addresses
        .first()
        .ok_or(embedded_tls::TlsError::InternalError)?;

    let mut socket = TcpSocket::new(stack, tcp_rx, tcp_tx);
    socket.set_timeout(Some(Net::SOCKET_TIMEOUT));
    socket
        .connect((address, Btc::PORT))
        .await
        .map_err(|_| embedded_tls::TlsError::InternalError)?;

    let mut tls = TlsConnection::new(&mut socket, tls_read, tls_write);

    let config = TlsConfig::new()
        .with_server_name(Btc::HOST)
        .enable_rsa_signatures();
    tls.open(TlsContext::new(
        &config,
        UnsecureProvider::new::<Aes128GcmSha256>(rng),
    ))
    .await?;

    tls.write_all(Btc::REQUEST).await?;
    tls.flush().await?;

    let mut response_buffer = [0; Tls::RESPONSE_BUFFER_SIZE];
    let mut total = 0;
    loop {
        match tls.read(&mut response_buffer[total..]).await {
            Ok(0) | Err(embedded_tls::TlsError::ConnectionClosed) => break,
            Ok(count) => total += count,
            Err(error) => return Err(error),
        }
        if total >= response_buffer.len() {
            break;
        }
    }

    if let Some(cents) = ubos_pool::btc::parse_price_cents(&response_buffer[..total]) {
        let dollars = cents / 100;
        let remainder = cents % 100;
        info!("btc price updated: ${}.{:02}", dollars, remainder);
        BTC_PRICE_CENTS.store(cents, Ordering::Relaxed);
        LAST_BTC_FETCH_UPTIME_SECS.store(Instant::now().as_secs(), Ordering::Relaxed);
    }

    // Ignore close errors since we already have the data.
    let _ = tls.close().await;

    Ok(())
}

/// Periodically sync NTP time. Runs forever.
pub async fn run_ntp_sync(stack: Stack<'static>) -> ! {
    wait_for_config(stack).await;

    loop {
        if let Err(error) = fetch_ntp_time(stack).await {
            warn!("ntp sync failed: {}", error);
        }
        Timer::after(Ntp::SYNC_INTERVAL).await;
    }
}

/// Periodically fetch BTC price over TLS. Runs forever.
pub async fn run_bitcoin_price<R: CryptoRng + RngCore>(stack: Stack<'static>, mut rng: R) -> ! {
    wait_for_config(stack).await;

    let tcp_rx = TCP_RX.take();
    let tcp_tx = TCP_TX.take();
    let tls_read = TLS_READ.take();
    let tls_write = TLS_WRITE.take();

    loop {
        if fetch_btc_price(stack, &mut rng, tcp_rx, tcp_tx, tls_read, tls_write)
            .await
            .is_err()
        {
            warn!("btc fetch failed");
        }
        Timer::after(Btc::FETCH_INTERVAL).await;
    }
}
