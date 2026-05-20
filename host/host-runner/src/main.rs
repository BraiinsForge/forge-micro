//! Host-side runner for ubos-main tasks.
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
//! `POOL_PROTOCOL`, `POOL_USER`, `POOL_SV1_PASS`, `POOL_SV2_PUBKEY`,
//! and `POOL_SV2_TRUST_ANY_KEY` env vars (read at compile time) drive
//! `ubos_pool::pool_config::parse`;
//! defaults point at the public Braiins Pool so the binary works out of
//! the box. Both SV1 and SV2 are wired here — the workspace defaults
//! select SV2 against the Braiins V2 endpoint.
//!
//! Host-side prerequisites (run once per boot): `./setup-network.sh up`
//! creates the TAP device and installs the NAT + forward rules. See
//! `README.md` for the manual equivalents and for `AF_PACKET` caveats.

use std::time::{SystemTime, UNIX_EPOCH};

use embassy_executor::{Executor, Spawner};
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Ticker, Timer};
use ubos_main::metrics::{Metric, Value};
use ubos_main::net;
use ubos_pool::pool_config::{
    self, PoolConfig, ProtocolConfig, SOCKET_TIMEOUT, STRATUM_TCP_BUFFER_SIZE,
};
use log::{info, warn};
use rand::rngs::OsRng;
use rand_core::RngCore;
use static_cell::StaticCell;
use stratum_client::AsicSession;
use ubos_metrics::metrics::{Metric, Value};
use ubos_monitor::{MonitorConfig, BMM101_DEFAULT};
use ubos_net::net;
use ubos_pool::pool_config::{
    self, PoolConfig, ProtocolConfig, SOCKET_TIMEOUT, STRATUM_TCP_BUFFER_SIZE,
};

mod cpu_mock;
mod fan;
mod gpiod;
mod hosted_network;
mod miner;
mod mmm_serial;
mod psu;
mod temperature;

use hosted_network::HostDevice;

const SOCKET_COUNT: usize = 9;
const BOARD_NAME: &str = "host-runner";

/// Per-board fall-through for the worker identity. Used when
/// `POOL_USER` is not set in the environment.
const DEFAULT_POOL_USER: &str = "braiinstest.dumb_worker";

/// Tunable monitor parameters. The PID gains and threshold
/// temperatures live in `ubos_monitor::BMM101_DEFAULT`.
static MONITOR_CONFIG: MonitorConfig = BMM101_DEFAULT;

static EXECUTOR: StaticCell<Executor> = StaticCell::new();
static RESOURCES: StaticCell<StackResources<SOCKET_COUNT>> = StaticCell::new();

/// Parsed pool config; populated once at boot from compile-time
/// env vars and held for the lifetime of the process.
static POOL_CONFIG_CELL: StaticCell<PoolConfig> = StaticCell::new();

/// V1 session config; only initialised when the parsed protocol is
/// `Sv1`. Held in a `StaticCell` so `StratumV1Client::new` gets the
/// `&'static Sv1Config` it expects.
static SV1_CONFIG_CELL: StaticCell<stratum_v1::Sv1Config> = StaticCell::new();

/// V2 session config; only initialised when the parsed protocol is
/// `Sv2`. Held in a `StaticCell` so `StratumV2Client::new` gets the
/// `&'static Sv2Config` it expects.
static SV2_CONFIG_CELL: StaticCell<stratum_v2::Sv2Config> = StaticCell::new();

/// Heap-allocated secp256k1 context for the SV2 noise handshake.
/// The host has no SRAM ceiling, so the firmware's preallocated
/// `AllPreallocated` buffer is replaced with the standard
/// allocating context.
static SECP_CTX: StaticCell<ii_sv2::security::CryptoCtx<ii_sv2::security::secp256k1::All>> =
    StaticCell::new();

/// Adapter from `rand`'s `OsRng` onto the `ii_sv2::security::Rng`
/// trait the V2 noise handshake takes. Wraps the upstream `RngCore`
/// implementation without holding any state of its own.
struct HostSv2Rng(OsRng);

impl ii_sv2::security::Rng for HostSv2Rng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest);
    }
}

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
        let hashrate = ubos_main::hashrate::HashrateDisplay(ubos_main::hashrate::hashrate());
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
    ubos_main::metrics::run(stack).await
}

#[embassy_executor::task]
async fn mdns_task(stack: Stack<'static>) -> ! {
    // hostname and board_name share BOARD_NAME: host-runner has no per-unit
    // MAC suffix to compose a separate DNS label from.
    ubos_main::mdns::run_mdns(stack, BOARD_NAME, BOARD_NAME, "host").await
}

#[embassy_executor::task]
async fn http_server_task(stack: Stack<'static>) -> ! {
    net::run_http_server(stack, BOARD_NAME).await
}

/// CPU-side ASIC mock; spawned when no BM1362 hashboard is wired.
/// Consumes work from `ASIC_SESSION` and pushes solutions back.
#[embassy_executor::task]
async fn asic_cpu_task() -> ! {
    cpu_mock::run_asic_cpu(&ASIC_SESSION).await
}

/// Stratum client task. Resolves `pool_config.host`, opens a TCP
/// connection, and runs either the SV1 or SV2 trait-based mining loop
/// against `ASIC_SESSION`. Reconnects with `RECONNECT_DELAY` between
/// attempts. Mirrors the firmware shape so the SV1/SV2 switch and the
/// DNS path stay identical across host and bare-metal.
#[embassy_executor::task]
async fn stratum_client_task(stack: Stack<'static>, pool_config: &'static PoolConfig) -> ! {
    static TCP_RX: StaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> = StaticCell::new();
    static TCP_TX: StaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> = StaticCell::new();
    let rx_buffer = TCP_RX.init([0; STRATUM_TCP_BUFFER_SIZE]);
    let tx_buffer = TCP_TX.init([0; STRATUM_TCP_BUFFER_SIZE]);

    let port = pool_config.port;

    match &pool_config.protocol {
        ProtocolConfig::Sv1 { password } => {
            let sv1 = SV1_CONFIG_CELL.init(stratum_v1::Sv1Config {
                user: pool_config.user.as_str(),
                password: password.as_deref().unwrap_or(""),
            });
            loop {
                let mut socket = match net::connect_stratum(
                    stack,
                    pool_config.host.as_str(),
                    port,
                    SOCKET_TIMEOUT,
                    rx_buffer,
                    tx_buffer,
                )
                .await
                {
                    Ok(socket) => socket,
                    Err(error) => {
                        warn!("stratum-client: connect failed: {error:?}");
                        Timer::after(pool_config::RECONNECT_DELAY).await;
                        continue;
                    }
                };
                let mut state = stratum_client::StratumState::new();
                let mut client = stratum_v1::StratumV1Client::new(sv1);
                if let Err(error) = stratum_client::run_stratum_client(
                    &mut client,
                    &mut socket,
                    &ASIC_SESSION,
                    &mut state,
                )
                .await
                {
                    warn!("stratum-client: session error {error:?}");
                }
                Timer::after(pool_config::RECONNECT_DELAY).await;
            }
        }
        ProtocolConfig::Sv2 { pool_pubkey } => {
            let sv2 = SV2_CONFIG_CELL.init(
                stratum_v2::Sv2Config::new(pool_config.user.as_str(), *pool_pubkey)
                    // `pool_config::parse` rejects POOL_USER longer than
                    // `MAX_USER_LEN`, which equals `Sv2Config::MAX_USER_LEN`.
                    .expect("BUG: POOL_USER exceeds Sv2Config::MAX_USER_LEN"),
            );
            let ctx = SECP_CTX.init(ii_sv2::security::CryptoCtx::new());
            loop {
                let mut socket = match net::connect_stratum(
                    stack,
                    pool_config.host.as_str(),
                    port,
                    SOCKET_TIMEOUT,
                    rx_buffer,
                    tx_buffer,
                )
                .await
                {
                    Ok(socket) => socket,
                    Err(error) => {
                        warn!("stratum-client: connect failed: {error:?}");
                        Timer::after(pool_config::RECONNECT_DELAY).await;
                        continue;
                    }
                };
                let mut state = stratum_client::StratumState::new();
                let mut client = stratum_v2::StratumV2Client::new(sv2, ctx, HostSv2Rng(OsRng));
                if let Err(error) = stratum_client::run_stratum_client(
                    &mut client,
                    &mut socket,
                    &ASIC_SESSION,
                    &mut state,
                )
                .await
                {
                    warn!("stratum-client: session error {error:?}");
                }
                Timer::after(pool_config::RECONNECT_DELAY).await;
            }
        }
    }
}

/// Read the `POOL_*` env vars at compile time (each falling through
/// to shared defaults in `pool_config`) and produce a typed
/// `PoolConfig`. Identical to the firmware path so the workspace-wide
/// `.cargo/config.toml` POOL_* defaults reach every binary the same
/// way. When `POOL_SV2_PUBKEY` is unset, `DEFAULT_POOL_SV2_PUBKEY`
/// is used so the binary always validates the Braiins prod key by
/// default. Set `POOL_SV2_TRUST_ANY_KEY=1` to disable peer auth.
fn pool_config_from_env() -> PoolConfig {
    pool_config::parse(
        option_env!("POOL_HOST").unwrap_or(pool_config::DEFAULT_POOL_HOST),
        option_env!("POOL_PORT").unwrap_or(pool_config::DEFAULT_POOL_PORT_STR),
        option_env!("POOL_PROTOCOL").unwrap_or(pool_config::DEFAULT_POOL_PROTOCOL),
        option_env!("POOL_USER").unwrap_or(DEFAULT_POOL_USER),
        option_env!("POOL_SV1_PASS"),
        option_env!("POOL_SV2_PUBKEY").unwrap_or(pool_config::DEFAULT_POOL_SV2_PUBKEY),
        option_env!("POOL_SV2_TRUST_ANY_KEY"),
    )
    // Defaults are validated, so the default build always parses.
    // A malformed operator-supplied POOL_SV2_PUBKEY at build time
    // still panics here. The host has no watchdog reboot loop, so a
    // loud boot-time failure is preferred over the firmware's
    // log-and-skip path.
    .expect("BUG: POOL_* env vars failed to parse")
}

/// Republishes `ubos_main::hashrate::hashrate()` as a Prometheus
/// metric on a fixed cadence. The EWMA hashrate is bumped
/// automatically every time a share is recorded via
/// `ubos_main::hashrate::record_share`.
#[embassy_executor::task]
async fn hashrate_metrics_task() -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(5));
    loop {
        ticker.next().await;
        let hashrate_hz = ubos_main::hashrate::hashrate();
        ubos_main::metrics::push(Metric {
            name: "board_hashrate_hz",
            // the workspace ubos-* crates still use heapless 0.8 while
            // host-runner needs 0.9 for embassy-net 0.9's StaticConfigV4;
            // renamed dep keeps both in the build.
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
            stratum_client_task(stack, pool_config)
                .expect("BUG: stratum_client_task token"),
        );
        spawner.spawn(ntp_sync_task(stack).expect("BUG: ntp_sync_task token"));
        spawner.spawn(bitcoin_price_task(stack).expect("BUG: bitcoin_price_task token"));
        spawner.spawn(metrics_task(stack).expect("BUG: metrics_task token"));
        spawner.spawn(mdns_task(stack).expect("BUG: mdns_task token"));
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
