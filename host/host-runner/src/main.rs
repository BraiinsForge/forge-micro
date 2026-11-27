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

//! Host-side runner for the firmware tasks.
//!
//! Brings up an embassy-net Stack over a host network device so the same
//! tasks that run on the RP2350 can be exercised on a host. By default the
//! Linux TAP driver from `embassy-net-tuntap` is used; setting
//! `HOST_NET_AFPACKET=<ifname>` switches to the `embassy-net-afpacket`
//! raw-socket driver bound to that real NIC. Either way, the static IPv4
//! config, the `net_task` runner, plus the stratum client, CPU solver,
//! hashrate publisher, NTP, metrics, and HTTP server are identical
//! across the firmware and host paths.
//!
//! Pool config matches the firmware: the stratum task reads
//! `ubos_config::Config::pool[0]` on each reconnect, so an operator
//! POSTs credentials to `/api/config` and they persist in the host's
//! file-backed storage. `Config::for_board` seeds an SV1 entry at
//! the public Braiins endpoint with an empty user so the connection
//! is rejected until the operator configures their worker.
//!
//! Host-side prerequisites (run once per boot): `setup-network up`
//! creates the TAP device and installs the NAT + forward rules. See
//! `README.md` for the manual equivalents and for `AF_PACKET` caveats.

use std::time::{SystemTime, UNIX_EPOCH};

use embassy_executor::{Executor, Spawner};
use embassy_futures::select::{Either3, select, select3};
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use log::{info, warn};
use rand::TryRng;
use rand::rngs::SysRng;
use static_cell::{ConstStaticCell, StaticCell};
use stratum_client::{AsicSession, NoExtension};
use stratum_client::{
    NtimeRoll, PoolConnector, RECONNECT_DELAY, SESSION_REJECTED_DELAY, SOCKET_TIMEOUT,
    STRATUM_TCP_BUFFER_SIZE, SessionError, run_stratum_session,
};
use ubos_config::FileStorage;
use ubos_manager::Handle as ManagerHandle;
use ubos_mining::Handle as MiningHandle;
use ubos_net::net;

mod config;
mod cpu_mock;
mod fan;
mod gpiod;
mod hosted_network;
mod miner;
mod miner_serial;
mod psu;
mod temperature;

use hosted_network::HostDevice;

// Seventeen sockets are held continuously: DHCPv4, DNS, stratum, mDNS,
// 8 HTTP listeners and 5 metrics listeners, and the rest is headroom for
// the transient NTP socket and for TIME_WAIT. This crate calls
// `assert_stack_sockets!` nowhere, so nothing checks the value against
// the pools below and it is held level with the boards by hand.
const SOCKET_COUNT: usize = 24;
const BOARD_NAME: &str = "host-runner";

/// The host runner simulates the BMM101 hashboard, so it threads that
/// board descriptor through the mining, config, and API layers.
pub const BOARD: &ubos_asic::HashboardDescriptor = &ubos_asic::BMM101_HASHBOARD;
/// The tuners the host runner runs. The miner builds them, and
/// [`BOUNDS`] lists them for the config and the mining form.
pub type TunerSet = ubos_mining::tuner::Tuners;
pub const BOUNDS: ubos_config::MiningBounds = BOARD.bounds(
    <TunerSet as ubos_mining::tuner::MachineTuner<1>>::TUNERS,
    Some(ubos_machine::MINIMINER_INPUT_POWER),
);
pub const MINING_INFO: ubos_config::MiningInfo =
    BOARD.mining_info(&BOUNDS, ubos_config::DefaultTuning::Fixed);

/// Matches the HTTP connection depth the boards run.
const HTTP_POOL: usize = 8;
/// Matches the metrics pool the boards run, so a scrape fan-in is served
/// concurrently.
const METRICS_HTTP_POOL: usize = 5;

/// Exposition sizing for the host runner. A host has the RAM, so it takes
/// the larger of the two shapes the boards use and never drops a series
/// the firmware would have kept.
const MAX_METRICS: usize = 128;
const METRICS_RESPONSE_BYTES: usize = MAX_METRICS * ubos_metrics::metrics::METRIC_LINE_BYTES;

/// The web UI and the REST API behind one prefix.
type HttpService = ubos_http::PrefixRouter<ubos_api::RestApiService, ubos_webui::DashboardService>;

/// The exposition handler, sized to the statics it serves.
type MetricsService =
    ubos_prometheus::PrometheusMetricsService<MAX_METRICS, METRICS_RESPONSE_BYTES>;

ubos_metrics::metrics_statics!(METRICS_STATICS, MAX_METRICS, METRICS_RESPONSE_BYTES);

ubos_http::http_worker_task!(http_worker_task, HttpService, HTTP_POOL);
ubos_http::http_worker_task!(metrics_worker_task, MetricsService, METRICS_HTTP_POOL);

static EXECUTOR: StaticCell<Executor> = StaticCell::new();
static RESOURCES: StaticCell<StackResources<SOCKET_COUNT>> = StaticCell::new();

/// The park that the serial worker joins. The host has no flash writer, so
/// no one requests it.
static PARK: ubos_mining::Park = ubos_mining::Park::new();

/// The host has no SRAM ceiling, so the SV2 noise handshake takes the
/// standard allocating secp256k1 context rather than the firmware's
/// preallocated one.
static SECP_CTX: StaticCell<ii_sv2::security::CryptoCtx<ii_sv2::security::secp256k1::All>> =
    StaticCell::new();

struct HostSv2Rng(SysRng);

impl ii_sv2::security::Rng for HostSv2Rng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0
            .try_fill_bytes(dest)
            .expect("BUG: the OS random source failed");
    }
}

/// Single-slot by default, because the runner exercises one protocol
/// session at a time and has no use for the firmware's per-pool slots.
pub static ASIC_SESSION: AsicSession = AsicSession::new();

/// TCP transport for one pool entry. It borrows the task's static socket
/// buffers, so it is rebuilt per config snapshot alongside the client.
struct TcpConnector<'a> {
    stack: Stack<'static>,
    host: &'a str,
    port: u16,
    rx_buffer: &'a mut [u8],
    tx_buffer: &'a mut [u8],
}

impl PoolConnector for TcpConnector<'_> {
    type Conn<'c>
        = embassy_net::tcp::TcpSocket<'c>
    where
        Self: 'c;
    type Error = net::StratumConnectError;

    async fn connect(&mut self) -> Result<Self::Conn<'_>, Self::Error> {
        net::connect_stratum(
            self.stack,
            self.host,
            self.port,
            SOCKET_TIMEOUT,
            self.rx_buffer,
            self.tx_buffer,
        )
        .await
    }
}

fn log_session_error(error: &SessionError<net::StratumConnectError>) {
    match error {
        SessionError::Connect(error) => warn!("stratum-client: connect failed: {error:?}"),
        SessionError::Session(error) => warn!("stratum-client: session error {error:?}"),
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
        let address = stack.config_v4().map(|config| config.address);
        let hashrate = ubos_metrics::chain::handle().machine_hashrate();
        info!("host-runner: tick, address={address:?}, hashrate={hashrate}");
    }
}

#[embassy_executor::task]
async fn ntp_sync_task(stack: Stack<'static>, net_publisher: ubos_net::net::Publisher) -> ! {
    net::run_ntp_sync(stack, net_publisher).await
}

#[embassy_executor::task]
async fn metrics_ingest_task() -> ! {
    ubos_metrics::metrics::run_ingest(&METRICS_STATICS).await
}

const MDNS_SERVICES: &[&ubos_mdns::ServiceEntry] = &[
    &ubos_webui::WEBUI_SERVICE,
    &ubos_api::REST_API_SERVICE,
    &ubos_prometheus::PROMETHEUS_SERVICE,
];

#[embassy_executor::task]
async fn mdns_task(stack: Stack<'static>) -> ! {
    // hostname and board_name share BOARD_NAME: host-runner has no per-unit
    // MAC suffix to compose a separate DNS label from.
    ubos_mdns::run_mdns(stack, BOARD_NAME, BOARD_NAME, "host", MDNS_SERVICES).await
}

/// Seeds both credential stores from the first published config, then
/// spawns the HTTP workers, so the first request authenticates against
/// the credentials of the operator. A task, because the seeds wait for
/// the config task, which runs once the executor does.
#[embassy_executor::task]
async fn serve_http_task(
    spawner: Spawner,
    config_handle: ubos_config::Handle,
    acceptor: ubos_http::TcpAccept<'static>,
    buffers: [&'static mut [u8]; HTTP_POOL],
    service: &'static HttpService,
) {
    ubos_api::seed_api_credentials(config_handle).await;
    ubos_webui::seed_webui_credentials(config_handle).await;
    for buffer in buffers {
        spawner.spawn(
            http_worker_task(
                acceptor,
                buffer,
                service,
                ubos_http::consts::Http::SOCKET_TIMEOUT,
                ubos_http::consts::Http::IDLE_TIMEOUT,
            )
            .expect("BUG: http_worker_task token"),
        );
    }
}

#[embassy_executor::task]
async fn sync_credentials_task(config_handle: ubos_config::Handle) -> ! {
    ubos_api::run_sync_credentials(config_handle).await
}

#[embassy_executor::task]
async fn network_control_task(
    stack: Stack<'static>,
    net_publisher: ubos_net::net::Publisher,
    config_handle: ubos_config::Handle,
) -> ! {
    net::run_network_control(stack, net_publisher, config_handle).await
}

#[embassy_executor::task]
async fn thermal_task(
    sensor: temperature::HashboardTemperature,
    mut thermal_runner: ubos_thermal::Runner,
    manager_handle: ManagerHandle,
    config_handle: ubos_config::Handle,
) -> ! {
    ubos_thermal::run_thermal(
        sensor,
        ubos_thermal::NoAmbient,
        ubos_thermal::NoMcu,
        &mut thermal_runner,
        manager_handle,
        config_handle,
    )
    .await
}

#[embassy_executor::task]
async fn manager_task(mut manager_runner: ubos_manager::Runner, mining: MiningHandle) -> ! {
    ubos_manager::run_manager(&mut manager_runner, mining).await
}

#[embassy_executor::task]
async fn config_task(mut config_runner: ubos_config::Runner, storage: FileStorage) -> ! {
    ubos_config::run(
        &mut config_runner,
        storage,
        &BOARD.defaults,
        &MINING_INFO,
        None,
        None,
    )
    .await
}

#[embassy_executor::task]
async fn asic_cpu_task() -> ! {
    cpu_mock::run_asic_cpu(&ASIC_SESSION).await
}

/// Mirrors the firmware session loop, so the SV1/SV2 switch and the DNS
/// path stay identical across host and board.
#[embassy_executor::task]
#[allow(clippy::too_many_lines)]
async fn stratum_client_task(stack: Stack<'static>, config_handle: ubos_config::Handle) -> ! {
    static TCP_RX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    static TCP_TX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    let rx_buffer = TCP_RX.take();
    let tx_buffer = TCP_TX.take();

    // Initialised even when the first pool entry is Sv1, so a runtime
    // protocol switch needs no late allocation.
    let secp_ctx = SECP_CTX.init(ii_sv2::security::CryptoCtx::new());

    let mut config_generation = config_handle.subscribe_generation();
    // Set once the slot has had a pool, as in the firmware session loop.
    let mut had_pool = false;
    let pool = ASIC_SESSION.pool_handle(0);

    loop {
        // Never leave a previous session's job in the work cell while no
        // session backs it, and drop the alive bit that a config-change
        // cancellation never cleared.
        pool.clear_work();
        pool.set_alive(false);
        pool.clear_difficulty();

        let snapshot = config_handle.current().await;
        let Some(entry) = snapshot.pool.first() else {
            warn!("stratum-client: pool list empty, awaiting config update");
            if had_pool {
                select(
                    config_generation.changed(),
                    pool.push_counters_periodically(),
                )
                .await;
            } else {
                config_generation.changed().await;
            }
            continue;
        };
        had_pool = true;

        let one_session = async {
            pool.record_reconnect_attempt();
            let mut connector = TcpConnector {
                stack,
                host: entry.host.as_str(),
                port: entry.port,
                rx_buffer: &mut *rx_buffer,
                tx_buffer: &mut *tx_buffer,
            };
            let mut state = stratum_client::StratumState::new(0);
            let ntime_roll: NtimeRoll = config::CHIP.chip_descriptor().rolls_ntime.into();

            let result = match &entry.protocol {
                ubos_config::ProtocolKind::Sv1 { password } => {
                    let sv1 = stratum_v1::Sv1Config::new(
                        entry.user.as_str(),
                        password.as_ref().map_or("", heapless::String::as_str),
                    )
                    .expect("BUG: PoolEntry user/password match Sv1Config buffer sizes");
                    let mut client = stratum_v1::StratumV1Client::new(&sv1);
                    run_stratum_session(
                        &mut connector,
                        &mut client,
                        pool,
                        &mut state,
                        ntime_roll.period_secs(),
                        &mut NoExtension,
                    )
                    .await
                }
                ubos_config::ProtocolKind::Sv2 { pool_pubkey } => {
                    let sv2 = stratum_v2::Sv2Config::new(entry.user.as_str(), *pool_pubkey)
                        .expect("BUG: PoolEntry.user matches Sv2Config buffer size");
                    let mut client =
                        stratum_v2::StratumV2Client::new(&sv2, secp_ctx, HostSv2Rng(SysRng));
                    run_stratum_session(
                        &mut connector,
                        &mut client,
                        pool,
                        &mut state,
                        ntime_roll.period_secs(),
                        &mut NoExtension,
                    )
                    .await
                }
            };

            // A pool rejection does not fix itself, so it retries on the
            // longer cadence instead of the transport one.
            let delay = match &result {
                Err(error) => {
                    log_session_error(error);
                    if error.is_rejection() {
                        SESSION_REJECTED_DELAY
                    } else {
                        RECONNECT_DELAY
                    }
                }
                Ok(()) => RECONNECT_DELAY,
            };
            Timer::after(delay).await;
        };
        if matches!(
            select3(
                one_session,
                config_generation.changed(),
                pool.push_counters_periodically(),
            )
            .await,
            Either3::Second(_)
        ) {
            info!("stratum-client: pool config changed, reconnecting");
        }
    }
}

#[embassy_executor::task]
async fn miner_metrics_task(mining: MiningHandle) -> ! {
    ubos_mining::publishers::run_miner_metrics_publisher(mining, 1).await
}

#[embassy_executor::task]
async fn pool_weights_task(config_handle: ubos_config::Handle) -> ! {
    ubos_mining::scheduler::run_pool_weights_watcher(&miner_serial::CHAINS, config_handle).await
}

#[allow(
    clippy::single_match_else,
    clippy::items_after_statements,
    clippy::too_many_lines
)]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let (mining_runner, mining_handle) = ubos_mining::Handle::take();
    let (thermal_runner, thermal_handle) = ubos_thermal::Handle::take();
    let (cooling_runner, cooling_handle) = ubos_cooling::Handle::take();
    let (net_publisher, net_handle) = ubos_net::net::Handle::take();
    let (manager_runner, manager_handle) = ubos_manager::Handle::take();
    let (config_runner, config_handle) = ubos_config::Handle::take();
    let stratum_handle = stratum_client::PoolCountersHandle::take();

    // Only the miner watches `ubos_mining::Handle::stop`, so without the
    // explicit exit the executor would keep running the network, stratum
    // and CPU-mock tasks forever. The teardown and the exit race on
    // purpose: this is a developer tool, not a production controller.
    ctrlc::set_handler(move || {
        info!("host-runner: SIGINT received, signalling miner stop");
        mining_handle.stop();
        std::process::exit(130);
    })
    .expect("BUG: ctrlc handler already installed");

    let (device, config) = hosted_network::open_host_device();

    // Seconds-since-epoch are enough entropy for TCP ISN randomness on a
    // development host; this is not the firmware's crypto RNG path.
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |delta| delta.as_secs());

    let (stack, runner) =
        embassy_net::new(device, config, RESOURCES.init(StackResources::new()), seed);

    // Opened before the executor runs, because the HTTP service takes
    // the PSU handle whether or not a hashboard is attached.
    let psu = psu::open();
    let (psu_runner, psu_handle) = ubos_psu::Handle::take();

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner: Spawner| {
        // SpawnError only fires on reuse of a task token; each constructor
        // invocation mints a fresh one, so a first-time spawn cannot fail.
        spawner.spawn(net_task(runner).expect("BUG: net_task token"));
        spawner.spawn(heartbeat_task(stack).expect("BUG: heartbeat_task token"));
        spawner.spawn(
            stratum_client_task(stack, config_handle)
                .expect("BUG: stratum_client_task token"),
        );
        spawner.spawn(ntp_sync_task(stack, net_publisher).expect("BUG: ntp_sync_task token"));
        spawner.spawn(metrics_ingest_task().expect("BUG: metrics_ingest_task token"));
        spawner.spawn(mdns_task(stack).expect("BUG: mdns_task token"));
        spawner.spawn(miner_metrics_task(mining_handle).expect("BUG: miner_metrics_task token"));
        spawner.spawn(pool_weights_task(config_handle).expect("BUG: pool_weights_task token"));
        let config_storage = FileStorage::new(
            std::env::var_os("UBOS_CONFIG_DIR")
                .map_or_else(|| std::path::PathBuf::from(".ubos-config"), Into::into),
        );
        spawner.spawn(config_task(config_runner, config_storage).expect("BUG: config_task token"));
        // The pause and resume endpoints send the manager a message
        // whatever the fan and sensor presence, so the command channel
        // must always have a consumer.
        spawner.spawn(
            manager_task(manager_runner, mining_handle)
                .expect("BUG: manager_task token"),
        );
        // Seeded before the HTTP server task can handle any request.
        let mut csprng_seed = [0u8; 32];
        SysRng
            .try_fill_bytes(&mut csprng_seed).expect("BUG: the OS random source failed");
        ubos_auth::csprng::seed_csprng(csprng_seed);

        // The web UI and the REST API on :80 — seed both credential
        // stores, then spawn one worker task per pool slot so each has its
        // own static TaskStorage. `serve_http_task` does both once the
        // config is published. The executor closure is not
        // async, so `block_on` drives the infallible bind to match the
        // firmware's boot-time ordering. This block and the metrics one
        // below hand-expand `serve_http_pool!`, which assumes an async
        // context, so keep all three in step.
        static HTTP_TCP_BUFFERS: ConstStaticCell<
            edge_nal_embassy::TcpBuffers<
                HTTP_POOL,
                { ubos_http::consts::Http::TX_BUFFER_SIZE },
                { ubos_http::consts::Http::RX_BUFFER_SIZE },
            >,
        > = ConstStaticCell::new(edge_nal_embassy::TcpBuffers::new());
        static HTTP_TCP_NAL: StaticCell<edge_nal_embassy::Tcp<'static>> = StaticCell::new();
        static HTTP_WORK: ConstStaticCell<
            [u8; ubos_http::consts::Http::REQUEST_BUFFER_SIZE * HTTP_POOL],
        > = ConstStaticCell::new([0; ubos_http::consts::Http::REQUEST_BUFFER_SIZE * HTTP_POOL]);
        static HTTP_SERVICE: StaticCell<HttpService> = StaticCell::new();
        let http_tcp: &'static edge_nal_embassy::Tcp<'static> =
            HTTP_TCP_NAL.init(edge_nal_embassy::Tcp::new(stack, HTTP_TCP_BUFFERS.take()));
        let http_acceptor = embassy_futures::block_on(ubos_http::bind_tcp(
            http_tcp,
            ubos_http::consts::Http::PORT,
        ));
        let http_service: &'static HttpService = HTTP_SERVICE.init(ubos_http::PrefixRouter {
            prefix: ubos_api::PATH_PREFIX,
            matched: ubos_api::RestApiService::new(
                BOARD_NAME,
                env!("BUILD_ID"),
                &MINING_INFO,
                &BOUNDS,
                thermal_handle,
                manager_handle,
                psu_handle,
                cooling_handle,
                mining_handle,
                net_handle,
                config_handle,
                stratum_handle,
                stack,
            ),
            fallback: ubos_webui::DashboardService,
        });
        let http_buffers = ubos_http::split_work_buffer::<HTTP_POOL>(
            HTTP_WORK.take(),
            ubos_http::consts::Http::REQUEST_BUFFER_SIZE,
        );
        spawner.spawn(
            serve_http_task(spawner, config_handle, http_acceptor, http_buffers, http_service)
                .expect("BUG: serve_http_task token"),
        );
        spawner.spawn(
            sync_credentials_task(config_handle)
                .expect("BUG: sync_credentials_task token"),
        );

        spawner.spawn(
            network_control_task(stack, net_publisher, config_handle)
                .expect("BUG: network_control_task token"),
        );

        static METRICS_TCP_BUFFERS: ConstStaticCell<
            edge_nal_embassy::TcpBuffers<
                METRICS_HTTP_POOL,
                { ubos_prometheus::Prometheus::TX_BUFFER_SIZE },
                { ubos_prometheus::Prometheus::RX_BUFFER_SIZE },
            >,
        > = ConstStaticCell::new(edge_nal_embassy::TcpBuffers::new());
        static METRICS_TCP_NAL: StaticCell<edge_nal_embassy::Tcp<'static>> = StaticCell::new();
        static METRICS_WORK: ConstStaticCell<
            [u8; ubos_prometheus::Prometheus::REQUEST_BUFFER_SIZE * METRICS_HTTP_POOL],
        > = ConstStaticCell::new(
            [0; ubos_prometheus::Prometheus::REQUEST_BUFFER_SIZE * METRICS_HTTP_POOL],
        );
        static METRICS_SERVICE: StaticCell<MetricsService> = StaticCell::new();
        let metrics_service: &'static MetricsService =
            METRICS_SERVICE.init(MetricsService::new(&METRICS_STATICS, net_handle));
        let metrics_tcp: &'static edge_nal_embassy::Tcp<'static> =
            METRICS_TCP_NAL.init(edge_nal_embassy::Tcp::new(stack, METRICS_TCP_BUFFERS.take()));
        let metrics_acceptor = embassy_futures::block_on(ubos_http::bind_tcp(
            metrics_tcp,
            ubos_prometheus::Prometheus::PORT,
        ));
        let metrics_buffers = ubos_http::split_work_buffer::<METRICS_HTTP_POOL>(
            METRICS_WORK.take(),
            ubos_prometheus::Prometheus::REQUEST_BUFFER_SIZE,
        );
        for buffer in metrics_buffers {
            spawner.spawn(
                metrics_worker_task(
                    metrics_acceptor,
                    buffer,
                    metrics_service,
                    ubos_prometheus::Prometheus::SOCKET_TIMEOUT,
                    ubos_prometheus::Prometheus::IDLE_TIMEOUT,
                )
                .expect("BUG: metrics_worker_task token"),
            );
        }

        match fan::FanHwmonController::open() {
            Some(controller) => match temperature::HashboardTemperature::open(thermal_handle) {
                Some(sensor) => {
                    spawner.spawn(
                        thermal_task(sensor, thermal_runner, manager_handle, config_handle)
                            .expect("BUG: thermal_task token"),
                    );
                    spawner.spawn(
                        fan::pid_fan_task(controller, cooling_runner, thermal_handle, config_handle)
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
        match psu {
            Some(psu) => {
                spawner.spawn(psu::psu_task(psu, psu_runner).expect("BUG: psu_task token"));
                spawner
                    .spawn(
                        miner_serial::miner_serial_task(&PARK)
                            .expect("BUG: miner_serial_task token"),
                    );
                spawner.spawn(
                    miner::miner_task(psu_handle, mining_runner, config_handle)
                        .expect("BUG: miner_task token"),
                );
            }
            None => {
                warn!(
                    "host-runner: {} unset or i2c open failed; ubos-mining not spawned, falling back to in-process CPU mining",
                    psu::I2C_BUS_ENV
                );
                spawner.spawn(
                    miner::miner_idle_task(mining_runner)
                        .expect("BUG: miner_idle_task token"),
                );
                spawner.spawn(asic_cpu_task().expect("BUG: asic_cpu_task token"));
            }
        }
    });
}
