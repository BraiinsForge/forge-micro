//! Host-side runner for eth-examples tasks.
//!
//! Brings up an embassy-net Stack over a Linux TAP device so the same tasks
//! that run on the RP2350 can be exercised on a host. This step wires the
//! TAP driver, static IPv4 config, the `net_task` runner, plus the stratum
//! client, CPU solver, and hashrate publisher from `ubos-backend`. Later
//! steps add NTP, BTC price, metrics, and HTTP on top.
//!
//! Host-side prerequisites (run once per boot): `./setup-network.sh up`
//! creates the TAP device and installs the NAT + forward rules. See
//! `README.md` for the manual equivalents.

use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use embassy_executor::{Executor, Spawner};
use embassy_net::{Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4};
use embassy_net_tuntap::TunTapDevice;
use embassy_time::{Duration, Ticker, Timer};
use eth_examples::metrics::{Metric, Value};
use eth_examples::net;
use rand::rngs::OsRng;
use static_cell::StaticCell;
use ubos_backend::{cpu_solver_task, hashrate_task, stratum_task, PoolConfig};

const TAP_NAME: &str = "tap0";
const HOST_IP: Ipv4Address = Ipv4Address::new(192, 168, 69, 2);
const PREFIX_LEN: u8 = 24;
const GATEWAY: Ipv4Address = Ipv4Address::new(192, 168, 69, 1);
const DNS_SERVER: Ipv4Address = Ipv4Address::new(1, 1, 1, 1);
const SOCKET_COUNT: usize = 8;
const BOARD_NAME: &str = "host-runner";

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

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, TunTapDevice>) -> ! {
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

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("host-runner starting on tap device {TAP_NAME}");

    let device = TunTapDevice::new(TAP_NAME).expect("BUG: open TAP device");

    let mut dns_servers: heapless::Vec<Ipv4Address, 3> = heapless::Vec::new();
    let _ = dns_servers.push(DNS_SERVER);
    let config = Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(HOST_IP, PREFIX_LEN),
        gateway: Some(GATEWAY),
        dns_servers,
    });

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
    });
}
