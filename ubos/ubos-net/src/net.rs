//! Network tasks shared across all boards: DHCP wait, NTP sync, stratum
//! TCP connect, and cross-board atomic state.
//!
//! Architecture: `../../../docs/OVERVIEW.md`.

use core::cell::Cell;

use embassy_futures::select::{Either, select};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{ConfigV4, DhcpConfig, IpAddress, Ipv4Address, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use portable_atomic::{AtomicU32, Ordering};
use ubos_config::{Message, NetworkConfig, NetworkMode, handle as config_handle};
use ubos_logging::{Debug2Format, info, warn};

use ubos_core::consts::Net;

use crate::ntp::Ntp;

/// Sockets the shared net stack occupies beyond a board's HTTP pools:
/// DHCP, DNS, mDNS, NTP, and the stratum client. A board sizes its
/// `StackResources` to cover these plus its pools; see
/// [`min_stack_sockets`] and the `assert_stack_sockets!` macro.
pub const FIXED_SOCKETS: usize = 5;

/// Minimum `StackResources` capacity for a board running the three HTTP
/// pools plus the [`FIXED_SOCKETS`]. smoltcp panics ("adding a socket to a
/// full `SocketSet`") when the stack is sized below the sockets bound at
/// once, and the last HTTP listener to bind is what trips it.
#[must_use]
pub const fn min_stack_sockets(webui_pool: usize, api_pool: usize, metrics_pool: usize) -> usize {
    webui_pool + api_pool + metrics_pool + FIXED_SOCKETS
}

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

/// Count of accepted HTTP requests since boot. Incremented through
/// [`Handle::record_request`] by the dashboard and API HTTP servers.
static REQUEST_COUNT: AtomicU32 = AtomicU32::new(0);
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
    /// applies by [`run_network_control`].
    pub ipv4: Option<Ipv4Address>,
    /// Board MAC, or `None` until [`Handle::set_mac`] runs at boot.
    pub mac: Option<[u8; 6]>,
    /// Seconds to add to the monotonic clock to reach Unix time, or `None`
    /// until NTP first syncs. Written by [`sync_ntp`] via
    /// [`Handle::set_ntp_offset`].
    pub ntp_offset: Option<u64>,
}

/// Subscriber capacity for the net-state watch. Every consumer today reads
/// point-in-time via [`Handle::current`], which consumes no receiver slot,
/// so one slot covers any future subscriber.
const WATCH_CAPACITY: usize = 1;

/// Module-internal state watch. Writers are [`wait_for_config`],
/// [`run_network_control`], and the boot MAC publish; readers obtain values
/// via [`Handle`].
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
    ///
    /// Records `ip_assigned` exactly once on a genuine address change. The
    /// compare-and-set runs inside `send_if_modified` under the Watch mutex, so
    /// concurrent publishers cannot fire the event twice for one transition and
    /// a re-publish of the same address wakes no receivers. The event itself is
    /// recorded after the closure returns to avoid nesting the ring's critical
    /// section inside the Watch's.
    pub fn set_ipv4(&self, ipv4: Ipv4Address) {
        let changed = Cell::new(false);
        self.state.sender().send_if_modified(|state| {
            let state = state.get_or_insert_with(NetState::default);
            let modified = state.ipv4 != Some(ipv4);
            changed.set(modified);
            state.ipv4 = Some(ipv4);
            modified
        });
        if changed.get() {
            let [octet0, octet1, octet2, octet3] = ipv4.octets();
            crate::events::ip_assigned(octet0, octet1, octet2, octet3);
        }
    }

    /// Publish the board MAC, leaving the IPv4 field untouched.
    ///
    /// Each firmware supplies its own six octets, derived from the chip id
    /// on the `WIZnet` boards or read from board config on the others.
    /// Calling this once at boot lets `/api/info` report the address without
    /// threading it through the network stack.
    pub fn set_mac(&self, mac: [u8; 6]) {
        self.state.sender().send_if_modified(|state| {
            let state = state.get_or_insert_with(NetState::default);
            if state.mac == Some(mac) {
                false
            } else {
                state.mac = Some(mac);
                true
            }
        });
    }

    /// Publish the NTP epoch offset after a successful sync, leaving the
    /// IPv4 and MAC fields untouched.
    pub fn set_ntp_offset(&self, offset: u64) {
        self.state.sender().send_if_modified(|state| {
            let state = state.get_or_insert_with(NetState::default);
            if state.ntp_offset == Some(offset) {
                false
            } else {
                state.ntp_offset = Some(offset);
                true
            }
        });
    }

    /// Seconds to add to the monotonic clock to reach Unix time, or 0 if
    /// NTP has not synced. The zero fallback matches the sentinel the bare
    /// atomic used before this moved behind the handle.
    #[must_use]
    pub fn ntp_epoch_offset(&self) -> u64 {
        self.current()
            .and_then(|state| state.ntp_offset)
            .unwrap_or(0)
    }

    /// Accepted HTTP requests since boot.
    #[must_use]
    pub fn request_count(&self) -> u32 {
        REQUEST_COUNT.load(Ordering::Relaxed)
    }

    /// Record one accepted HTTP request.
    pub fn record_request(&self) {
        REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
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
            // wait_for_config is awaited on both the boot path and by
            // run_ntp_sync; set_ipv4 records ip_assigned only on a genuine
            // address change, so calling it on every poll is safe.
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
    handle().set_ntp_offset(offset);

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

/// Cadence at which the live IPv4 address is observed and republished,
/// matching the prior reconcile loop. Long enough to stay well off the
/// executor hot path; `/api/info` and the display tolerate up to this
/// much lag after an address change.
const OBSERVE_INTERVAL: Duration = Duration::from_secs(2);

/// Hold-off between a config publish and switching the live stack.
///
/// `set_config_v4` tears down the TCP connection the apply request
/// arrived on, so this lets the API handler's HTTP ack flush first,
/// keeping the ack a reliable "command arrived" signal for the SPA.
/// See `docs/HTTP_LISTENERS.md`.
const REPLY_FLUSH_DELAY: Duration = Duration::from_secs(1);

/// Translate a stored [`NetworkConfig`] into the stack's [`ConfigV4`].
fn to_config_v4(network: &NetworkConfig) -> ConfigV4 {
    match &network.mode {
        NetworkMode::Static {
            address,
            prefix_len,
            gateway,
            dns,
        } => crate::config::static_ipv4(*address, *prefix_len, *gateway, dns).ipv4,
        NetworkMode::Dhcp => embassy_net::Config::dhcpv4(DhcpConfig::default()).ipv4,
    }
}

/// Own IP assignment: apply config-driven network changes to the live
/// stack and publish the resulting address. Runs forever; spawn once per
/// board beside the other net tasks.
///
/// Subscribes to the effective-network watch in `ubos-config`. On each
/// change it switches the stack and clears every browser session, since a
/// deliberate reconfiguration invalidates each session's CSRF token and
/// lands the SPA on `/login` — the unambiguous "it took effect" signal.
/// Between changes it polls the stack for the assigned address and
/// publishes it via [`Handle::set_ipv4`], which records `ip_assigned` once
/// per genuine change.
///
/// For a DHCP target a fresh lease is self-evident proof the change works,
/// so the task confirms the pending change once a lease lands; `ubos-config`
/// no-ops a confirm with nothing pending, so a boot lease is harmless. A
/// static address is never auto-confirmed here: only an operator round-trip
/// from the new address proves reachability.
pub async fn run_network_control(stack: Stack<'static>) -> ! {
    let mut network = config_handle().subscribe_network();
    // Seed from the committed config the firmware already applied at boot,
    // so only a genuine later transition triggers a switch.
    let mut applied = config_handle().current().network;
    let mut last_addr: Option<Ipv4Address> = None;
    let mut confirm_pending = false;

    loop {
        match select(network.changed(), Timer::after(OBSERVE_INTERVAL)).await {
            Either::First(new) => {
                if new != applied {
                    // Let the HTTP ack flush before the switch drops the
                    // connection the apply request arrived on.
                    Timer::after(REPLY_FLUSH_DELAY).await;
                    stack.set_config_v4(to_config_v4(&new));
                    ubos_auth::session::clear_all_sessions().await;
                    applied = new;
                    // Force the next poll to republish and, for DHCP, confirm
                    // the freshly acquired lease.
                    last_addr = None;
                    confirm_pending = matches!(applied.mode, NetworkMode::Dhcp);
                    info!("net: applied network change");
                }
            }
            Either::Second(()) => {
                if let Some(config) = stack.config_v4() {
                    let address = config.address.address();
                    if last_addr != Some(address) {
                        last_addr = Some(address);
                        handle().set_ipv4(address);
                    }
                    // A fresh DHCP lease confirms the pending change. Retry
                    // until the send lands: a dropped confirm would let the
                    // window expire and revert a working lease.
                    if confirm_pending && config_handle().try_send(Message::ConfirmNetwork) {
                        confirm_pending = false;
                    }
                }
            }
        }
    }
}
