//! Host-side runner for eth-examples tasks.
//!
//! Brings up an embassy-net Stack over a host network device so the same
//! tasks that run on the RP2350 can be exercised on a host. By default the
//! Linux TAP driver from `embassy-net-tuntap` is used; setting
//! `HOST_NET_AFPACKET=<ifname>` switches to the `embassy-net-afpacket`
//! raw-socket driver bound to that real NIC. Either way, the static IPv4
//! config, the `net_task` runner, plus the stratum client, CPU solver,
//! hashrate publisher, NTP, BTC price, metrics, and HTTP server are
//! identical across the firmware and host paths.
//!
//! Host-side prerequisites (run once per boot): `./setup-network.sh up`
//! creates the TAP device and installs the NAT + forward rules. See
//! `README.md` for the manual equivalents and for `AF_PACKET` caveats.

use std::sync::atomic::Ordering;
use std::task::Context;
use std::time::{SystemTime, UNIX_EPOCH};

use embassy_executor::{Executor, Spawner};
use embassy_net::{
    Config, DhcpConfig, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4,
};
use embassy_net_afpacket::AfPacketDevice;
use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState};
use embassy_net_tuntap::TunTapDevice;
use embassy_time::{Duration, Ticker, Timer};
use embedded_serial_unix::Serial;
use eth_examples::metrics::{Metric, Value};
use eth_examples::net;
use rand::rngs::OsRng;
use static_cell::StaticCell;
use ubos_backend::{cpu_solver_task, hashrate_task, stratum_task, PoolConfig};

mod uart_loopback;

const TAP_NAME: &str = "tap0";
const HOST_IP: Ipv4Address = Ipv4Address::new(192, 168, 69, 2);
const PREFIX_LEN: u8 = 24;
const GATEWAY: Ipv4Address = Ipv4Address::new(192, 168, 69, 1);
const DNS_SERVER: Ipv4Address = Ipv4Address::new(1, 1, 1, 1);
const SOCKET_COUNT: usize = 8;
const BOARD_NAME: &str = "host-runner";

/// Serial device the loopback exercise opens. Hard-coded because every
/// host that wires up an external USB-to-UART bridge for this runner
/// uses the kernel's first ttyUSB; a missing device just disables the
/// loopback task while the rest of the runner keeps going.
const UART_LOOPBACK_PATH: &str = "/dev/ttyUSB0";

/// Baud rate for the loopback device. Matches the rp2350 firmware's
/// `UartLoopback::BAUD_RATE` so a host wired to the rp2350 exercises
/// the same line settings end-to-end.
const UART_LOOPBACK_BAUD: u32 = 115_200;

/// Last-resort MAC reported to the embassy stack on the `AF_PACKET`
/// path when neither the override env var nor the sysfs read succeed.
/// Locally-administered with bit 1 of byte 0 set and unicast with bit 0
/// clear; the rest spells "HR" + 0x000001 so the address is recognisable
/// in tcpdump and DHCP-server logs.
const AFPACKET_FALLBACK_MAC: [u8; 6] = [0x02, 0x48, 0x52, 0x00, 0x00, 0x01];

/// Env var that overrides MAC selection on the `AF_PACKET` path. Value
/// must parse as `aa:bb:cc:dd:ee:ff`; malformed values are ignored.
const AFPACKET_MAC_ENV: &str = "HOST_NET_AFPACKET_MAC";

/// Pool config. Defaults to public Braiins Pool so the host binary works
/// out of the box without requiring reach to the NUC regtest pool. Edit
/// in place to fire at the same regtest target the firmware uses.
static POOL_CONFIG: PoolConfig = PoolConfig {
    host_ip: [172, 65, 65, 63],
    port: 3333,
    user: "braiinstest.dumb_worker",
    password: "x",
    reconnect_delay: Duration::from_secs(5),
};

static EXECUTOR: StaticCell<Executor> = StaticCell::new();
static RESOURCES: StaticCell<StackResources<SOCKET_COUNT>> = StaticCell::new();

/// Sum type over the two host-side link drivers. embassy-net's `Stack` is
/// generic over a concrete driver, so we need a single named type that
/// dispatches at runtime to whichever variant was selected at startup.
enum HostDevice {
    Tap(TunTapDevice),
    AfPacket(AfPacketDevice),
}

/// Receive token paired with `HostDevice`. Matches `Driver::receive`'s
/// constraint that the token type is the same regardless of which variant
/// produced it.
enum HostRxToken {
    Tap(embassy_net_tuntap::RxToken),
    AfPacket(embassy_net_afpacket::RxToken),
}

/// Transmit token paired with `HostDevice`. Borrows from the underlying
/// device for the lifetime of the surrounding `receive`/`transmit` call.
enum HostTxToken<'a> {
    Tap(embassy_net_tuntap::TxToken<'a>),
    AfPacket(embassy_net_afpacket::TxToken<'a>),
}

impl Driver for HostDevice {
    type RxToken<'a>
        = HostRxToken
    where
        Self: 'a;
    type TxToken<'a>
        = HostTxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, context: &mut Context) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self {
            Self::Tap(device) => device
                .receive(context)
                .map(|(rx, tx)| (HostRxToken::Tap(rx), HostTxToken::Tap(tx))),
            Self::AfPacket(device) => device
                .receive(context)
                .map(|(rx, tx)| (HostRxToken::AfPacket(rx), HostTxToken::AfPacket(tx))),
        }
    }

    fn transmit(&mut self, context: &mut Context) -> Option<Self::TxToken<'_>> {
        match self {
            Self::Tap(device) => device.transmit(context).map(HostTxToken::Tap),
            Self::AfPacket(device) => device.transmit(context).map(HostTxToken::AfPacket),
        }
    }

    fn link_state(&mut self, context: &mut Context) -> LinkState {
        match self {
            Self::Tap(device) => device.link_state(context),
            Self::AfPacket(device) => device.link_state(context),
        }
    }

    fn capabilities(&self) -> Capabilities {
        match self {
            Self::Tap(device) => device.capabilities(),
            Self::AfPacket(device) => device.capabilities(),
        }
    }

    fn hardware_address(&self) -> HardwareAddress {
        match self {
            Self::Tap(device) => device.hardware_address(),
            Self::AfPacket(device) => device.hardware_address(),
        }
    }
}

impl embassy_net_driver::RxToken for HostRxToken {
    fn consume<R, F>(self, callback: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        match self {
            Self::Tap(token) => token.consume(callback),
            Self::AfPacket(token) => token.consume(callback),
        }
    }
}

impl embassy_net_driver::TxToken for HostTxToken<'_> {
    fn consume<R, F>(self, length: usize, callback: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        match self {
            Self::Tap(token) => token.consume(length, callback),
            Self::AfPacket(token) => token.consume(length, callback),
        }
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, HostDevice>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn heartbeat_task(stack: Stack<'static>) {
    loop {
        Timer::after(Duration::from_secs(10)).await;
        let link_up = stack.config_v4().is_some();
        let hashrate = ubos_backend::HASHRATE_HZ.load(Ordering::Relaxed);
        log::info!("host-runner: tick, link_up={link_up}, hashrate={hashrate} H/s");
    }
}

#[embassy_executor::task]
async fn ntp_sync_task(stack: Stack<'static>) -> ! {
    net::run_ntp_sync(stack).await
}

#[embassy_executor::task]
async fn bitcoin_price_task(stack: Stack<'static>) -> ! {
    net::run_bitcoin_price(stack, OsRng).await
}

#[embassy_executor::task]
async fn metrics_task(stack: Stack<'static>) -> ! {
    eth_examples::metrics::run(stack).await
}

#[embassy_executor::task]
async fn http_server_task(stack: Stack<'static>) -> ! {
    net::run_http_server(stack, BOARD_NAME).await
}

/// Mirror of `rp2350/main.rs` `hashrate_metrics_task`: republishes
/// `HASHRATE_HZ` as a Prometheus metric on a fixed cadence.
#[embassy_executor::task]
async fn hashrate_metrics_task() -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(5));
    loop {
        ticker.next().await;
        let hashrate_hz = ubos_backend::HASHRATE_HZ.load(Ordering::Relaxed);
        eth_examples::metrics::push(Metric {
            name: "board_hashrate_hz",
            // eth-examples still uses heapless 0.8 while host-runner needs
            // 0.9 for embassy-net 0.9's StaticConfigV4; renamed dep keeps
            // both in the build.
            labels: heapless_v08::Vec::new(),
            value: Value::Uint(u64::from(hashrate_hz)),
            timestamp_ms: 0,
        })
        .await;
    }
}

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
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
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
            log::info!("host-runner: {AFPACKET_MAC_ENV}=`{text}` accepted as override");
            return mac;
        }
        log::warn!("host-runner: {AFPACKET_MAC_ENV}=`{text}` is malformed, ignoring");
    }
    let Some(host_mac) = read_host_mac(ifname) else {
        log::warn!("host-runner: failed to read MAC for {ifname} from sysfs, using fallback");
        return AFPACKET_FALLBACK_MAC;
    };
    derive_synthetic_mac(host_mac)
}

/// Open the link driver and return the matching `embassy_net` config.
///
/// The TAP path keeps its setup-network.sh-aligned static IP. The
/// `AF_PACKET` path runs DHCP because the real LAN it attaches to has
/// its own server and the runner cannot know the subnet ahead of time.
fn open_host_device() -> (HostDevice, Config) {
    match std::env::var("HOST_NET_AFPACKET").ok() {
        Some(name) if !name.is_empty() => {
            let mac = afpacket_mac(&name);
            let [m0, m1, m2, m3, m4, m5] = mac;
            log::info!(
                "host-runner: HOST_NET_AFPACKET set, binding raw socket to {name} \
                 with synthetic MAC {m0:02x}:{m1:02x}:{m2:02x}:{m3:02x}:{m4:02x}:{m5:02x}"
            );
            // Failure here is unrecoverable: without a working link driver
            // there is nothing for the rest of the runner to do.
            let device =
                AfPacketDevice::with_synthetic_mac(&name, mac).expect("BUG: open AF_PACKET socket");
            (
                HostDevice::AfPacket(device),
                Config::dhcpv4(DhcpConfig::default()),
            )
        }
        _ => {
            log::info!("host-runner: starting on tap device {TAP_NAME}");
            let device = TunTapDevice::new(TAP_NAME).expect("BUG: open TAP device");
            let mut dns_servers: heapless::Vec<Ipv4Address, 3> = heapless::Vec::new();
            let _ = dns_servers.push(DNS_SERVER);
            let config = Config::ipv4_static(StaticConfigV4 {
                address: Ipv4Cidr::new(HOST_IP, PREFIX_LEN),
                gateway: Some(GATEWAY),
                dns_servers,
            });
            (HostDevice::Tap(device), config)
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let (device, config) = open_host_device();

    // Open the loopback serial up-front. A failure here just disables
    // the loopback task; the rest of the runner still comes up.
    let uart_loopback = match Serial::open(UART_LOOPBACK_PATH, UART_LOOPBACK_BAUD) {
        Ok(serial) => Some(serial.split()),
        Err(error) => {
            log::error!(
                "host-runner: failed to open {UART_LOOPBACK_PATH}: {error}; \
                 uart loopback disabled"
            );
            None
        }
    };

    // Seconds-since-epoch are enough entropy for TCP ISN randomness on a
    // development host; this is not the firmware's crypto RNG path.
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |delta| delta.as_secs());

    let (stack, runner) =
        embassy_net::new(device, config, RESOURCES.init(StackResources::new()), seed);

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner: Spawner| {
        // SpawnError only fires on reuse of a task token; each constructor
        // invocation mints a fresh one, so a first-time spawn cannot fail.
        spawner.spawn(net_task(runner).expect("BUG: net_task token"));
        spawner.spawn(heartbeat_task(stack).expect("BUG: heartbeat_task token"));
        spawner.spawn(stratum_task(stack, &POOL_CONFIG).expect("BUG: stratum_task token"));
        spawner.spawn(cpu_solver_task().expect("BUG: cpu_solver_task token"));
        spawner.spawn(hashrate_task().expect("BUG: hashrate_task token"));
        spawner.spawn(ntp_sync_task(stack).expect("BUG: ntp_sync_task token"));
        spawner.spawn(bitcoin_price_task(stack).expect("BUG: bitcoin_price_task token"));
        spawner.spawn(metrics_task(stack).expect("BUG: metrics_task token"));
        spawner.spawn(hashrate_metrics_task().expect("BUG: hashrate_metrics_task token"));
        spawner.spawn(http_server_task(stack).expect("BUG: http_server_task token"));
        if let Some((rx, tx)) = uart_loopback {
            spawner.spawn(uart_loopback::loopback_task(rx, tx).expect("BUG: uart loopback token"));
            spawner.spawn(
                uart_loopback::heartbeat_task().expect("BUG: uart loopback heartbeat token"),
            );
        }
    });
}
