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

//! Network tasks shared across all boards: DHCP wait, NTP sync, stratum
//! TCP connect, and cross-board atomic state.
//!
//! Architecture: `../../../docs/OVERVIEW.md`.

use core::cell::Cell;
use core::convert::Infallible;

use embassy_futures::join::join;
use embassy_futures::select::{Either, select};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{ConfigV4, DhcpConfig, IpAddress, Ipv4Address, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Ticker, Timer, with_timeout};
use ubos_config::{Message, NetworkConfig, NetworkMode};
use ubos_logging::{Debug2Format, info, warn};
use ubos_metrics::consts::Metrics;
use ubos_metrics::metric;
use ubos_metrics::value::{Count, Flag};

use ubos_core::consts::Net;

use crate::ntp::Ntp;

/// Sockets the shared net stack occupies beyond a board's HTTP pools:
/// DHCP, DNS, mDNS, NTP, and the stratum client. A board sizes its
/// `StackResources` to cover these plus its pools; see
/// [`min_stack_sockets`] and the `assert_stack_sockets!` macro.
pub const FIXED_SOCKETS: usize = 5;

/// Minimum `StackResources` capacity for a board running the two HTTP
/// pools plus the [`FIXED_SOCKETS`]. smoltcp panics ("adding a socket to a
/// full `SocketSet`") when the stack is sized below the sockets bound at
/// once, and the last HTTP listener to bind is what trips it.
#[must_use]
pub const fn min_stack_sockets(http_pool: usize, metrics_pool: usize) -> usize {
    http_pool + metrics_pool + FIXED_SOCKETS
}

/// Maximum number of DNS A-record results retained for connect attempts.
/// Pool DNS responses are typically two or three addresses; four covers the
/// largest observed Braiins record set with one slot to spare.
const MAX_DNS_RESULTS: usize = 4;

/// Cap on how long a stratum or NTP DNS query waits for the resolver. The
/// TCP connect already has its own per-socket timeout; this prevents an
/// unresponsive resolver from hanging a reconnect loop indefinitely.
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
    // `connect` leaves the smoltcp state machine in `Closed`, so the next
    // iteration can drive another SYN through it. The buffers are
    // borrowed once.
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
    /// Board MAC, or `None` until [`Publisher::set_mac`] runs at boot.
    pub mac: Option<[u8; 6]>,
    /// Seconds to add to the monotonic clock to reach Unix time, or `None`
    /// until NTP first syncs. Written by [`sync_ntp`] via
    /// [`Publisher::set_ntp_offset`].
    pub ntp_offset: Option<u64>,
}

/// Subscriber capacity for the net-state watch. Every consumer today reads
/// point-in-time via [`Handle::current`], which consumes no receiver slot,
/// so one slot covers any future subscriber.
const WATCH_CAPACITY: usize = 1;

type StateWatch = Watch<CSMutex, NetState, WATCH_CAPACITY>;

/// Module-internal state watch. Writers are [`wait_for_config`],
/// [`run_network_control`], and the boot MAC publish; readers obtain values
/// via [`Handle`].
static STATE_WATCH: StateWatch = Watch::new();

/// Per-task subscription returned by [`Handle::subscribe`].
pub type NetSubscription = Receiver<'static, CSMutex, NetState, WATCH_CAPACITY>;

static TAKEN: ubos_core::take_once::TakeOnce = ubos_core::take_once::TakeOnce::new("net");

/// Consumer-side handle for the network-identity state. It holds no data:
/// a firmware takes it once and passes a copy to each task that uses it.
#[derive(Clone, Copy)]
pub struct Handle(());

/// Producer-side handle for the network-identity state.
///
/// It is `Copy` because the board and two tasks publish: the board its
/// MAC, the DHCP wait and the network control the address, and the NTP
/// sync the clock. It goes only to those, so a reader cannot publish.
#[derive(Clone, Copy)]
pub struct Publisher(());

impl Handle {
    /// The firmware's one network handle, and the publisher of the state it
    /// reads. A second call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> (Publisher, Self) {
        TAKEN.take();
        (Publisher(()), Self(()))
    }

    /// Most recent published state, or `None` until any producer has
    /// published.
    #[must_use]
    pub fn current(&self) -> Option<NetState> {
        STATE_WATCH.try_get()
    }

    /// Per-task subscription. Call once per consumer task before entering
    /// its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&self) -> NetSubscription {
        STATE_WATCH
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Seconds to add to the monotonic clock to reach Unix time, or 0 if
    /// NTP has not synced.
    #[must_use]
    pub fn ntp_epoch_offset(&self) -> u64 {
        epoch_offset()
    }

    /// Seconds since the Unix epoch, or `None` while NTP has not synced.
    #[must_use]
    pub fn unix_secs(&self) -> Option<u64> {
        match self.ntp_epoch_offset() {
            0 => None,
            offset => Some(Instant::now().as_secs() + offset),
        }
    }
}

impl Publisher {
    /// Publish the live IPv4 address, leaving the MAC field untouched.
    ///
    /// Records `ip_assigned` exactly once per genuine change: the compare
    /// inside `send_if_modified` runs under the Watch mutex, so concurrent
    /// publishers cannot double-fire and a re-publish wakes no receivers.
    /// The event is recorded after the closure returns, to keep the
    /// ring's critical section from nesting inside the Watch's.
    pub fn set_ipv4(&self, ipv4: Ipv4Address) {
        let changed = Cell::new(false);
        STATE_WATCH.sender().send_if_modified(|state| {
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
    /// Each firmware supplies its own six octets, derived from the chip id,
    /// or read from the board EEPROM or from board config.
    /// Calling this once at boot lets `/api/info` report the address without
    /// threading it through the network stack.
    pub fn set_mac(&self, mac: [u8; 6]) {
        STATE_WATCH.sender().send_if_modified(|state| {
            let state = state.get_or_insert_with(NetState::default);
            if state.mac == Some(mac) {
                false
            } else {
                state.mac = Some(mac);
                true
            }
        });
    }

    /// Derive the board MAC from a per-chip id, log it and publish it, for
    /// a control board that carries no address of its own; `mac_family` is
    /// the descriptor's byte for this control board and `chip_id` is
    /// whatever unique id the chip offers.
    #[must_use]
    pub fn publish_mac(&self, mac_family: u8, chip_id: &[u8]) -> [u8; 6] {
        let mac = crate::mac::derive_mac(crate::mac::DUMMY_OUI, mac_family, chip_id);
        info!(
            "mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
        );
        self.set_mac(mac);
        mac
    }

    /// Publish the NTP epoch offset after a successful sync, leaving the
    /// IPv4 and MAC fields untouched.
    pub fn set_ntp_offset(&self, offset: u64) {
        STATE_WATCH.sender().send_if_modified(|state| {
            let state = state.get_or_insert_with(NetState::default);
            if state.ntp_offset == Some(offset) {
                false
            } else {
                state.ntp_offset = Some(offset);
                true
            }
        });
    }
}

/// The logging clock: seconds since Unix epoch if NTP has synced, or seconds
/// since boot otherwise.
///
/// Free, because a board's `defmt::timestamp!` and the clock it gives the
/// event ring take a plain function and can receive no handle. A reader
/// that has a [`Handle`] reads [`Handle::unix_secs`], which tells a synced
/// clock from the uptime fallback.
#[inline]
#[must_use]
pub fn wall_clock_secs() -> u64 {
    Instant::now().as_secs() + epoch_offset()
}

fn epoch_offset() -> u64 {
    STATE_WATCH
        .try_get()
        .and_then(|state| state.ntp_offset)
        .unwrap_or(0)
}

/// Wait until the network stack receives a DHCP lease and publish it on
/// `publisher`.
///
/// Polls `stack.config_v4()` at a 100 ms cadence rather than yielding
/// inside a tight loop, so the executor is not pegged for the entire
/// DHCP acquisition window.
pub async fn wait_for_config(
    stack: Stack<'static>,
    publisher: Publisher,
) -> embassy_net::StaticConfigV4 {
    loop {
        if let Some(config) = stack.config_v4() {
            // Safe to call on every poll: see Publisher::set_ipv4.
            publisher.set_ipv4(config.address.address());
            return config.clone();
        }
        // 100 ms is short enough to keep DHCP re-acquisition responsive but
        // long enough to keep this poll off the executor hot path.
        Timer::after(Duration::from_millis(100)).await;
    }
}

/// Perform a single NTP time synchronization.
///
/// # Errors
///
/// Returns an error string if DNS resolution, UDP communication, or NTP
/// response parsing fails.
pub async fn fetch_ntp_time(
    stack: Stack<'static>,
    publisher: Publisher,
) -> Result<(), &'static str> {
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
    publisher.set_ntp_offset(offset);

    Ok(())
}

/// Periodically sync NTP time and export the clock state. Runs forever.
///
/// The export loop starts at once, so the series exist before the lease
/// does. It is joined to the sync loop and not a task of its own, so no
/// board has a spawn line for it.
pub async fn run_ntp_sync(stack: Stack<'static>, publisher: Publisher) -> ! {
    let (never, _) = join(sync_clock(stack, publisher), export_clock_state()).await;
    match never {}
}

/// A failed fetch is retried on [`Ntp::RETRY_INTERVAL`], for the reason
/// that constant gives, rather than the steady-state
/// [`Ntp::SYNC_INTERVAL`]. There is no attempt limit: an unreachable
/// pool is a condition to keep trying through, not one to give up on.
async fn sync_clock(stack: Stack<'static>, publisher: Publisher) -> Infallible {
    wait_for_config(stack, publisher).await;
    Timer::after(Ntp::INITIAL_DELAY).await;

    loop {
        let interval = match fetch_ntp_time(stack, publisher).await {
            Ok(()) => Ntp::SYNC_INTERVAL,
            Err(error) => {
                warn!("ntp sync failed: {}", error);
                Ntp::RETRY_INTERVAL
            }
        };
        Timer::after(interval).await;
    }
}

/// Pushes the uptime and whether NTP has synced, the two series of the
/// clock this module owns. A push that finds the channel full is dropped,
/// and the next tick replaces it.
async fn export_clock_state() -> Infallible {
    let mut ticker = Ticker::every(EXPORT_INTERVAL);
    loop {
        metric!("uptime_seconds", Count(Instant::now().as_secs()))
            .state(EXPORT_TTL)
            .try_push();
        metric!("ntp_synced", Flag(epoch_offset() != 0))
            .state(EXPORT_TTL)
            .try_push();
        ticker.next().await;
    }
}

/// Task shell around [`run_ntp_sync`], which keeps the wall clock
/// aligned so event and log timestamps are absolute rather than
/// uptime-relative.
#[macro_export]
macro_rules! ntp_sync_task {
    () => {
        #[embassy_executor::task]
        async fn ntp_sync_task(
            stack: embassy_net::Stack<'static>,
            publisher: ubos_net::net::Publisher,
        ) -> ! {
            ubos_net::net::run_ntp_sync(stack, publisher).await
        }
    };
}

/// Cadence at which the clock series are pushed.
const EXPORT_INTERVAL: Duration = Duration::from_secs(1);
/// The series leave the exposition when this task stops, after the
/// pushes that [`Metrics::TTL_INTERVALS`] allows to miss.
const EXPORT_TTL: Duration =
    Duration::from_secs(EXPORT_INTERVAL.as_secs() * Metrics::TTL_INTERVALS as u64);

/// Cadence at which the live IPv4 address is observed and republished.
/// Long enough to stay well off the executor hot path; `/api/info` and
/// the display tolerate up to this much lag after an address change.
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
/// publishes it via [`Publisher::set_ipv4`].
///
/// For a DHCP target a fresh lease is self-evident proof the change works,
/// so the task confirms the pending change once a lease lands; `ubos-config`
/// no-ops a confirm with nothing pending, so a boot lease is harmless. A
/// static address is never auto-confirmed here: only an operator round-trip
/// from the new address proves reachability.
pub async fn run_network_control(
    stack: Stack<'static>,
    publisher: Publisher,
    config: ubos_config::Handle,
) -> ! {
    let mut network = config.subscribe_network();
    // Seed from the committed config the firmware already applied at boot,
    // so only a genuine later transition triggers a switch.
    let mut applied = config.current().await.network;
    let mut last_addr: Option<Ipv4Address> = None;
    let mut confirm_pending = false;

    loop {
        match select(network.changed(), Timer::after(OBSERVE_INTERVAL)).await {
            Either::First(new) => {
                if new != applied {
                    Timer::after(REPLY_FLUSH_DELAY).await;
                    stack.set_config_v4(to_config_v4(&new));
                    ubos_auth::session::clear_all_sessions();
                    applied = new;
                    // Force the next poll to republish and, for DHCP, confirm
                    // the freshly acquired lease.
                    last_addr = None;
                    confirm_pending = matches!(applied.mode, NetworkMode::Dhcp);
                    info!("net: applied network change");
                }
            }
            Either::Second(()) => {
                if let Some(lease) = stack.config_v4() {
                    let address = lease.address.address();
                    if last_addr != Some(address) {
                        last_addr = Some(address);
                        publisher.set_ipv4(address);
                    }
                    // A fresh DHCP lease confirms the pending change. Retry
                    // until the send lands: a dropped confirm would let the
                    // window expire and revert a working lease.
                    if confirm_pending && config.try_send(Message::ConfirmNetwork) {
                        confirm_pending = false;
                    }
                }
            }
        }
    }
}

/// Task shell around [`run_network_control`], which applies network
/// configuration changes to the running stack.
#[macro_export]
macro_rules! network_control_task {
    () => {
        #[embassy_executor::task]
        async fn network_control_task(
            stack: embassy_net::Stack<'static>,
            publisher: ubos_net::net::Publisher,
            config: ubos_config::Handle,
        ) -> ! {
            ubos_net::net::run_network_control(stack, publisher, config).await
        }
    };
}
