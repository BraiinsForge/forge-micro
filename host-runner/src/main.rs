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
//! Pool wiring matches the firmware: `POOL_HOST`, `POOL_PORT`,
//! `POOL_PROTOCOL`, `POOL_USER`, and `POOL_SV1_PASS` env vars (read at
//! compile time) drive `eth_examples::pool_config::parse`; defaults
//! point at the public Braiins Pool so the binary works out of the
//! box. V2 host support is not wired yet — `POOL_PROTOCOL=sv2` is
//! rejected at boot.
//!
//! Host-side prerequisites (run once per boot): `./setup-network.sh up`
//! creates the TAP device and installs the NAT + forward rules. See
//! `README.md` for the manual equivalents and for `AF_PACKET` caveats.

use std::time::{SystemTime, UNIX_EPOCH};

use embassy_executor::{Executor, Spawner};
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Ticker, Timer};
use eth_examples::metrics::{Metric, Value};
use eth_examples::net;
use eth_examples::pool_config::{self, PoolConfig, ProtocolConfig};
use log::{error, info, warn};
use rand::rngs::OsRng;
use static_cell::StaticCell;
use stratum_client::AsicSession;
use ubos_monitor::MonitorConfig;

mod fan;
mod gpiod;
mod hosted_network;
mod miner;
mod mmm_serial;
mod psu;
mod temperature;

use hosted_network::HostDevice;

const SOCKET_COUNT: usize = 8;
const BOARD_NAME: &str = "host-runner";

/// Per-board fall-through for the worker identity. Used when
/// `POOL_USER` is not set in the environment.
const DEFAULT_POOL_USER: &str = "braiinstest.dumb_worker";

/// Defaults to the public Braiins Pool so the binary works out of
/// the box without requiring reach to the NUC regtest pool.
const DEFAULT_POOL_HOST: &str = "172.65.65.63";

/// Tunable monitor parameters. Mirrors the BMM model defaults from
/// bos-main; the PID gains and threshold temperatures live in
/// `MonitorConfig::DEFAULT`.
static MONITOR_CONFIG: MonitorConfig = MonitorConfig::DEFAULT;

static EXECUTOR: StaticCell<Executor> = StaticCell::new();
static RESOURCES: StaticCell<StackResources<SOCKET_COUNT>> = StaticCell::new();

/// Parsed pool config; populated once at boot from compile-time
/// env vars and held for the lifetime of the process.
static POOL_CONFIG_CELL: StaticCell<PoolConfig> = StaticCell::new();

/// V1 session config; only initialised when the parsed protocol is
/// `Sv1`. Held in a `StaticCell` so `StratumV1Client::new` gets the
/// `&'static Sv1Config` it expects.
static SV1_CONFIG_CELL: StaticCell<stratum_v1::Sv1Config> = StaticCell::new();

/// Shared work / solution rendezvous between the stratum-client
/// task and whichever ASIC driver is active (mmm BM1362 actor or
/// in-process CPU mock).
pub static ASIC_SESSION: AsicSession = AsicSession::new();

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, HostDevice>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn heartbeat_task(stack: Stack<'static>) {
    loop {
        Timer::after(Duration::from_secs(10)).await;
        let address = stack.config_v4().map(|config| config.address);
        let hashrate = eth_examples::hashrate::HashrateDisplay(eth_examples::hashrate::hashrate());
        info!("host-runner: tick, address={address:?}, hashrate={hashrate}");
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

/// CPU-side ASIC mock; spawned when no BM1362 hashboard is wired.
/// Consumes work from `ASIC_SESSION` and pushes solutions back.
#[embassy_executor::task]
async fn asic_cpu_task() -> ! {
    asic_cpu_mock::run_asic_cpu(&ASIC_SESSION).await
}

/// V1 stratum client task. Connects to `pool_config.host:port`,
/// runs the trait-based mining loop against `ASIC_SESSION`, and
/// reconnects with `RECONNECT_DELAY` between attempts.
#[embassy_executor::task]
async fn stratum_client_task(
    stack: Stack<'static>,
    pool_config: &'static PoolConfig,
    sv1_config: &'static stratum_v1::Sv1Config,
) -> ! {
    use embassy_net::tcp::TcpSocket;
    use embassy_net::IpAddress;

    const TCP_BUFFER_SIZE: usize = 2048;
    static TCP_RX: StaticCell<[u8; TCP_BUFFER_SIZE]> = StaticCell::new();
    static TCP_TX: StaticCell<[u8; TCP_BUFFER_SIZE]> = StaticCell::new();
    let rx_buffer = TCP_RX.init([0; TCP_BUFFER_SIZE]);
    let tx_buffer = TCP_TX.init([0; TCP_BUFFER_SIZE]);

    let host_quad = parse_ipv4_quad(pool_config.host.as_str())
        .expect("BUG: POOL_HOST is not a valid IPv4 literal (DNS not yet supported on host)");
    let [a, b, c, d] = host_quad;
    let address = IpAddress::v4(a, b, c, d);
    let port = pool_config.port;

    loop {
        let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(120)));
        info!("stratum-client: connecting to {a}.{b}.{c}.{d}:{port}");
        if let Err(error) = socket.connect((address, port)).await {
            warn!("stratum-client: connect failed {error:?}");
            Timer::after(pool_config::RECONNECT_DELAY).await;
            continue;
        }
        info!("stratum-client: connected to {a}.{b}.{c}.{d}:{port}");

        let mut client = stratum_v1::StratumV1Client::new(sv1_config);
        let mut state = stratum_client::StratumState::new();
        match stratum_client::run_stratum_client(
            &mut client,
            &mut socket,
            &ASIC_SESSION,
            &mut state,
        )
        .await
        {
            Ok(()) => {}
            Err(error) => warn!("stratum-client: session error {error:?}"),
        }
        Timer::after(pool_config::RECONNECT_DELAY).await;
    }
}

/// Parse `host` as an IPv4 dotted-quad. DNS resolution is deferred;
/// today the runner accepts only literal addresses.
fn parse_ipv4_quad(host: &str) -> Option<[u8; 4]> {
    let mut parts = host.split('.');
    let a = parts.next()?.parse::<u8>().ok()?;
    let b = parts.next()?.parse::<u8>().ok()?;
    let c = parts.next()?.parse::<u8>().ok()?;
    let d = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some([a, b, c, d])
}

/// Read the six `POOL_*` env vars at compile time (each falling
/// through to the host's defaults) and produce a typed `PoolConfig`.
fn pool_config_from_env() -> PoolConfig {
    pool_config::parse(
        option_env!("POOL_HOST").unwrap_or(DEFAULT_POOL_HOST),
        option_env!("POOL_PORT").unwrap_or(pool_config::DEFAULT_POOL_PORT_STR),
        option_env!("POOL_PROTOCOL").unwrap_or(pool_config::DEFAULT_POOL_PROTOCOL),
        option_env!("POOL_USER").unwrap_or(DEFAULT_POOL_USER),
        option_env!("POOL_SV1_PASS"),
        option_env!("POOL_SV2_PUBKEY"),
    )
    .expect("BUG: POOL_* env vars failed to parse")
}

/// Republishes `eth_examples::hashrate::hashrate()` as a Prometheus
/// metric on a fixed cadence. The EWMA hashrate is bumped
/// automatically every time a share is recorded via
/// `eth_examples::hashrate::record_share`.
#[embassy_executor::task]
async fn hashrate_metrics_task() -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(5));
    loop {
        ticker.next().await;
        let hashrate_hz = eth_examples::hashrate::hashrate();
        eth_examples::metrics::push(Metric {
            name: "board_hashrate_hz",
            // eth-examples still uses heapless 0.8 while host-runner needs
            // 0.9 for embassy-net 0.9's StaticConfigV4; renamed dep keeps
            // both in the build.
            labels: heapless_v08::Vec::new(),
            value: Value::Uint(hashrate_hz),
            timestamp_ms: 0,
        })
        .await;
    }
}

#[allow(clippy::single_match_else)]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // SIGINT signals `STOP_MINING` so the BM1362 driver tears the
    // chip down cleanly (when the mmm path is active), then exits
    // the process unconditionally. Without the explicit `exit`,
    // the embassy executor would keep running the network /
    // stratum / CPU-mock tasks indefinitely — none of them watch
    // `STOP_MINING`. The teardown vs. exit is racy on purpose;
    // this is a developer tool, not a production controller.
    ctrlc::set_handler(|| {
        info!("host-runner: SIGINT received, signalling miner stop");
        mmm::miner::STOP_MINING.signal(());
        std::process::exit(130);
    })
    .expect("BUG: ctrlc handler already installed");

    let pool_config = POOL_CONFIG_CELL.init(pool_config_from_env());
    info!(
        "host-runner: POOL_HOST={} POOL_PORT={} POOL_PROTOCOL={} POOL_USER={}",
        pool_config.host.as_str(),
        pool_config.port,
        match &pool_config.protocol {
            ProtocolConfig::Sv1 { .. } => "sv1",
            ProtocolConfig::Sv2 { .. } => "sv2",
        },
        pool_config.user.as_str(),
    );

    let sv1_config = match &pool_config.protocol {
        ProtocolConfig::Sv1 { password } => SV1_CONFIG_CELL.init(stratum_v1::Sv1Config {
            user: pool_config.user.as_str(),
            password: password.as_deref().unwrap_or(""),
        }),
        ProtocolConfig::Sv2 { .. } => {
            error!(
                "host-runner: POOL_PROTOCOL=sv2 is not yet supported on the host; \
                 set POOL_PROTOCOL=sv1 or leave it unset"
            );
            std::process::exit(1);
        }
    };

    let (device, config) = hosted_network::open_host_device();

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
        spawner.spawn(
            stratum_client_task(stack, pool_config, sv1_config)
                .expect("BUG: stratum_client_task token"),
        );
        spawner.spawn(ntp_sync_task(stack).expect("BUG: ntp_sync_task token"));
        spawner.spawn(bitcoin_price_task(stack).expect("BUG: bitcoin_price_task token"));
        spawner.spawn(metrics_task(stack).expect("BUG: metrics_task token"));
        spawner.spawn(hashrate_metrics_task().expect("BUG: hashrate_metrics_task token"));
        spawner.spawn(http_server_task(stack).expect("BUG: http_server_task token"));
        match fan::FanHwmonController::open() {
            Some(controller) => match temperature::HashboardTemperature::open() {
                Some(sensor) => {
                    spawner.spawn(
                        fan::pid_fan_task(controller, sensor, &MONITOR_CONFIG)
                            .expect("BUG: pid_fan_task token"),
                    );
                }
                None => {
                    warn!("host-runner: no LM75A bus; parking fans at 100%");
                    controller.park_at_full_speed();
                }
            },
            None => {
                info!("host-runner: no pwmfan device found; fan control disabled");
            }
        }
        match psu::open() {
            Some((mmm_psu, state, terminated)) => {
                spawner
                    .spawn(psu::watchdog_task(state, terminated).expect("BUG: psu watchdog token"));
                spawner.spawn(psu::power_stats_task(mmm_psu).expect("BUG: psu power_stats token"));
                spawner.spawn(mmm_serial::mmm_serial_task().expect("BUG: mmm_serial_task token"));
                spawner.spawn(miner::miner_task(mmm_psu).expect("BUG: miner_task token"));
            }
            None => {
                warn!(
                    "host-runner: {} unset or i2c open failed; mmm miner not spawned, falling back to in-process CPU mining",
                    psu::I2C_BUS_ENV
                );
                spawner.spawn(asic_cpu_task().expect("BUG: asic_cpu_task token"));
            }
        }
    });
}
