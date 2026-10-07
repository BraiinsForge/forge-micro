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
//! file-backed storage. `Config::default()` seeds an SV1 entry at
//! the public Braiins endpoint with an empty user so the connection
//! is rejected until the operator configures their worker.
//!
//! Host-side prerequisites (run once per boot): `setup-network up`
//! creates the TAP device and installs the NAT + forward rules. See
//! `README.md` for the manual equivalents and for `AF_PACKET` caveats.

use std::time::{SystemTime, UNIX_EPOCH};

use embassy_executor::{Executor, Spawner};
use embassy_futures::select::{Either, select};
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use log::{info, warn};
use rand::rngs::OsRng;
use rand_core::RngCore;
use static_cell::{ConstStaticCell, StaticCell};
use stratum_client::AsicSession;
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

// Seventeen sockets are held continuously: DHCPv4, DNS, the stratum
// client, the mDNS responder, 8 HTTP listener sockets on :80, and 5
// metrics listener sockets on :8081. NTP opens a transient UDP socket,
// and closed TCP connections linger in TIME_WAIT until smoltcp reaps
// them. 24 provides headroom for concurrent connections and TIME_WAIT,
// matching the firmware boards. This crate calls `assert_stack_sockets!`
// nowhere, so nothing checks the value against the pools below; it is
// held level with the boards by hand.
const SOCKET_COUNT: usize = 24;
const BOARD_NAME: &str = "host-runner";

/// The host runner simulates the BMM101 hashboard, so it threads that
/// board descriptor through the mining, config, and API layers.
pub const BOARD: &ubos_asic::HashboardDescriptor = &ubos_asic::BMM101_HASHBOARD;
/// The tuners the host runner runs. The miner builds them, and
/// [`BOUNDS`] lists them for the config and the mining form.
pub type TunerSet = ubos_mining::tuner::Tuners;
pub const BOUNDS: ubos_config::MiningBounds = ubos_config::bounds(
    BOARD,
    <TunerSet as ubos_mining::tuner::MachineTuner<1>>::TUNERS,
);
pub const MINING_INFO: ubos_config::MiningInfo =
    ubos_config::mining_info(BOARD, &BOUNDS, ubos_config::DefaultTuning::Fixed);

/// Concurrent TCP connections the HTTP server (:80) accepts. It serves
/// the web UI and the REST API behind one router, and matches the depth
/// the RP2350 boards run.
const HTTP_POOL: usize = 8;
/// Concurrent TCP connections the Prometheus metrics server (:8081)
/// accepts. Matches the metrics pool the RP2350 boards run so a scrape
/// fan-in is served concurrently.
const METRICS_HTTP_POOL: usize = 5;

/// Exposition sizing for the host runner. A host has the RAM, so it
/// takes the larger of the two shapes the boards use and never drops a
/// series the firmware would have kept.
const MAX_METRICS: usize = 128;
const METRICS_RESPONSE_BYTES: usize = MAX_METRICS * ubos_metrics::metrics::METRIC_LINE_BYTES;

/// The web UI and the REST API behind one prefix, which is what the one
/// pool below serves.
type HttpService = ubos_http::PrefixRouter<ubos_api::RestApiService, ubos_webui::DashboardService>;

/// The exposition handler, sized to the statics it serves.
type MetricsService =
    ubos_metrics::metrics::PrometheusMetricsService<MAX_METRICS, METRICS_RESPONSE_BYTES>;

ubos_metrics::metrics_statics!(METRICS_STATICS, MAX_METRICS, METRICS_RESPONSE_BYTES);

ubos_http::http_worker_task!(http_worker_task, HttpService, HTTP_POOL);
ubos_http::http_worker_task!(metrics_worker_task, MetricsService, METRICS_HTTP_POOL);

static EXECUTOR: StaticCell<Executor> = StaticCell::new();
static RESOURCES: StaticCell<StackResources<SOCKET_COUNT>> = StaticCell::new();

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
/// task and whichever ASIC driver is active (ubos-mining BM1362 actor or
/// in-process CPU mock). Deliberately single-slot (`AsicSession<1>` via
/// the default): the host runner exercises one protocol session at a
/// time, so it has no use for the firmware's per-pool slots.
pub static ASIC_SESSION: AsicSession = AsicSession::new();

/// TCP transport source for one pool entry. Owns the stack handle and
/// host/port, borrows the task's static socket buffers; rebuilt per
/// config-snapshot iteration alongside the protocol client. Mirrors
/// the connector in `ubos-firmware/src/stratum.rs`.
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

/// Log a session failure, preserving the distinct connect-failed and
/// session-error lines the pre-connector code emitted.
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
        let hashrate = ubos_metrics::hashrate::HashrateDisplay(
            ubos_metrics::chain::handle().machine_hashrate(),
        );
        info!("host-runner: tick, address={address:?}, hashrate={hashrate}");
    }
}

#[embassy_executor::task]
async fn ntp_sync_task(stack: Stack<'static>) -> ! {
    net::run_ntp_sync(stack).await
}

#[embassy_executor::task]
async fn metrics_ingest_task() -> ! {
    ubos_metrics::metrics::run_ingest(&METRICS_STATICS).await
}

const MDNS_SERVICES: &[&ubos_mdns::ServiceEntry] = &[
    &ubos_webui::WEBUI_SERVICE,
    &ubos_api::REST_API_SERVICE,
    &ubos_metrics::PROMETHEUS_SERVICE,
];

#[embassy_executor::task]
async fn mdns_task(stack: Stack<'static>) -> ! {
    // hostname and board_name share BOARD_NAME: host-runner has no per-unit
    // MAC suffix to compose a separate DNS label from.
    ubos_mdns::run_mdns(stack, BOARD_NAME, BOARD_NAME, "host", MDNS_SERVICES).await
}

/// Keeps the credential store in sync with config changes at runtime.
#[embassy_executor::task]
async fn sync_credentials_task() -> ! {
    ubos_api::run_sync_credentials().await
}

#[embassy_executor::task]
async fn network_control_task(stack: Stack<'static>) -> ! {
    net::run_network_control(stack).await
}

#[embassy_executor::task]
async fn thermal_task(
    sensor: temperature::HashboardTemperature,
    manager_handle: ManagerHandle,
) -> ! {
    ubos_thermal::run_thermal(
        sensor,
        ubos_thermal::NoAmbient,
        manager_handle,
        ubos_config::handle(),
    )
    .await
}

#[embassy_executor::task]
async fn manager_task(mining: MiningHandle) -> ! {
    ubos_manager::run_manager(mining).await
}

#[embassy_executor::task]
async fn config_task(storage: FileStorage) -> ! {
    ubos_config::run(storage, BOARD, &MINING_INFO).await
}

/// CPU-side ASIC mock; spawned when no BM1362 hashboard is wired.
/// Consumes work from `ASIC_SESSION` and pushes solutions back.
#[embassy_executor::task]
async fn asic_cpu_task() -> ! {
    cpu_mock::run_asic_cpu(&ASIC_SESSION).await
}

/// Stratum client task. Reads the current pool config on each loop
/// iteration, opens a TCP connection, and runs either the SV1 or SV2
/// trait-based mining loop against `ASIC_SESSION`. Reconnects with
/// `RECONNECT_DELAY` between attempts. Mirrors the firmware shape so
/// the SV1/SV2 switch and the DNS path stay identical across host and
/// bare-metal.
#[embassy_executor::task]
#[allow(clippy::too_many_lines)]
async fn stratum_client_task(stack: Stack<'static>) -> ! {
    static TCP_RX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    static TCP_TX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    let rx_buffer = TCP_RX.take();
    let tx_buffer = TCP_TX.take();

    // SV2 secp context — initialised once even if the first pool entry
    // is Sv1, so a runtime protocol switch needs no late allocation.
    let secp_ctx = SECP_CTX.init(ii_sv2::security::CryptoCtx::new());

    let config_handle = ubos_config::handle();
    let mut config_generation = config_handle.subscribe_generation();

    loop {
        // Teardown boundary, mirroring the firmware session loop: never
        // leave a previous session's job in the work cell while no
        // session backs it, and drop the alive bit that a config-change
        // cancellation of `run_stratum_client` never cleared.
        ASIC_SESSION.pool_handle(0).clear_work();
        ASIC_SESSION.pool_handle(0).set_alive(false);

        let snapshot = config_handle.current();
        let Some(entry) = snapshot.pool.first() else {
            warn!("stratum-client: pool list empty, awaiting config update");
            config_generation.changed().await;
            continue;
        };

        let one_session = async {
            ASIC_SESSION.pool_handle(0).record_reconnect_attempt();
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
                        ASIC_SESSION.pool_handle(0),
                        &mut state,
                        ntime_roll.period_secs(),
                    )
                    .await
                }
                ubos_config::ProtocolKind::Sv2 { pool_pubkey } => {
                    let sv2 = stratum_v2::Sv2Config::new(entry.user.as_str(), *pool_pubkey)
                        .expect("BUG: PoolEntry.user matches Sv2Config buffer size");
                    let mut client =
                        stratum_v2::StratumV2Client::new(&sv2, secp_ctx, HostSv2Rng(OsRng));
                    run_stratum_session(
                        &mut connector,
                        &mut client,
                        ASIC_SESSION.pool_handle(0),
                        &mut state,
                        ntime_roll.period_secs(),
                    )
                    .await
                }
            };

            // Same backoff split as the firmware session loop: a pool
            // rejection does not fix itself, so retry it on the longer
            // cadence instead of the transport one.
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
            select(one_session, config_generation.changed()).await,
            Either::Second(_)
        ) {
            info!("stratum-client: pool config changed, reconnecting");
        }
    }
}

#[embassy_executor::task]
async fn stratum_metrics_task() -> ! {
    stratum_client::publishers::run_stratum_metrics_publisher(1).await
}

#[embassy_executor::task]
async fn miner_metrics_task(mining: MiningHandle) -> ! {
    ubos_mining::publishers::run_miner_metrics_publisher(mining, 1).await
}

#[embassy_executor::task]
async fn pool_weights_task() -> ! {
    ubos_mining::scheduler::run_pool_weights_watcher(&miner_serial::CHAINS).await
}

#[allow(
    clippy::single_match_else,
    clippy::items_after_statements,
    clippy::too_many_lines
)]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // SIGINT signals `STOP_MINING` so the BM1362 driver tears the
    // chip down cleanly (when the ubos-mining path is active), then exits
    // the process unconditionally. Without the explicit `exit`,
    // the embassy executor would keep running the network /
    // stratum / CPU-mock tasks indefinitely — none of them watch
    // `STOP_MINING`. The teardown vs. exit is racy on purpose;
    // this is a developer tool, not a production controller.
    ctrlc::set_handler(|| {
        info!("host-runner: SIGINT received, signalling miner stop");
        ubos_mining::miner::STOP_MINING.signal(());
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

    let thermal = ubos_thermal::handle();

    let manager_handle = ubos_manager::handle();

    let mining_handle = ubos_mining::handle();

    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner: Spawner| {
        // SpawnError only fires on reuse of a task token; each constructor
        // invocation mints a fresh one, so a first-time spawn cannot fail.
        spawner.spawn(net_task(runner).expect("BUG: net_task token"));
        spawner.spawn(heartbeat_task(stack).expect("BUG: heartbeat_task token"));
        spawner.spawn(stratum_client_task(stack).expect("BUG: stratum_client_task token"));
        spawner.spawn(ntp_sync_task(stack).expect("BUG: ntp_sync_task token"));
        spawner.spawn(metrics_ingest_task().expect("BUG: metrics_ingest_task token"));
        spawner.spawn(mdns_task(stack).expect("BUG: mdns_task token"));
        spawner.spawn(miner_metrics_task(mining_handle).expect("BUG: miner_metrics_task token"));
        spawner.spawn(stratum_metrics_task().expect("BUG: stratum_metrics_task token"));
        spawner.spawn(pool_weights_task().expect("BUG: pool_weights_task token"));
        let config_storage = FileStorage::new(
            std::env::var_os("UBOS_CONFIG_DIR")
                .map_or_else(|| std::path::PathBuf::from(".ubos-config"), Into::into),
        );
        spawner.spawn(config_task(config_storage).expect("BUG: config_task token"));
        // Spawned unconditionally because the manager drains its
        // command channel independent of any hardware: the HTTP server
        // below issues `manager.assert/clear` for the pause/resume
        // endpoints regardless of fan or LM75A presence, so the
        // channel must always have a consumer.
        spawner.spawn(manager_task(mining_handle).expect("BUG: manager_task token"));
        // Seed the session CSPRNG before the HTTP server task can handle
        // any request, matching the firmware boards' boot-time ordering.
        let mut csprng_seed = [0u8; 32];
        OsRng.fill_bytes(&mut csprng_seed);
        ubos_auth::csprng::seed_csprng(csprng_seed);

        // The web UI and the REST API on :80 — seed both credential
        // stores once, then spawn one worker task per pool slot so each
        // has its own static TaskStorage. The executor closure is not
        // async, so `block_on` drives the infallible bind to match the
        // firmware's boot-time ordering. This block and the metrics one
        // below hand-expand `serve_http_pool!`, which assumes an async
        // context; keep them in sync if that macro changes.
        ubos_api::seed_api_credentials();
        ubos_webui::seed_webui_credentials();
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
                thermal,
                manager_handle,
                ubos_psu::handle(),
                stack,
            ),
            fallback: ubos_webui::DashboardService,
        });
        let http_buffers = ubos_http::split_work_buffer::<HTTP_POOL>(
            HTTP_WORK.take(),
            ubos_http::consts::Http::REQUEST_BUFFER_SIZE,
        );
        for buffer in http_buffers {
            spawner.spawn(
                http_worker_task(
                    http_acceptor,
                    buffer,
                    http_service,
                    ubos_http::consts::Http::SOCKET_TIMEOUT,
                    ubos_http::consts::Http::IDLE_TIMEOUT,
                )
                .expect("BUG: http_worker_task token"),
            );
        }
        spawner.spawn(sync_credentials_task().expect("BUG: sync_credentials_task token"));

        spawner.spawn(network_control_task(stack).expect("BUG: network_control_task token"));

        // Prometheus metrics on :8081.
        static METRICS_TCP_BUFFERS: ConstStaticCell<
            edge_nal_embassy::TcpBuffers<
                METRICS_HTTP_POOL,
                { ubos_metrics::consts::Metrics::TX_BUFFER_SIZE },
                { ubos_metrics::consts::Metrics::RX_BUFFER_SIZE },
            >,
        > = ConstStaticCell::new(edge_nal_embassy::TcpBuffers::new());
        static METRICS_TCP_NAL: StaticCell<edge_nal_embassy::Tcp<'static>> = StaticCell::new();
        static METRICS_WORK: ConstStaticCell<
            [u8; ubos_metrics::consts::Metrics::REQUEST_BUFFER_SIZE * METRICS_HTTP_POOL],
        > = ConstStaticCell::new(
            [0; ubos_metrics::consts::Metrics::REQUEST_BUFFER_SIZE * METRICS_HTTP_POOL],
        );
        static METRICS_SERVICE: MetricsService = MetricsService::new(&METRICS_STATICS);
        let metrics_tcp: &'static edge_nal_embassy::Tcp<'static> =
            METRICS_TCP_NAL.init(edge_nal_embassy::Tcp::new(stack, METRICS_TCP_BUFFERS.take()));
        let metrics_acceptor = embassy_futures::block_on(ubos_http::bind_tcp(
            metrics_tcp,
            ubos_metrics::consts::Metrics::PORT,
        ));
        let metrics_buffers = ubos_http::split_work_buffer::<METRICS_HTTP_POOL>(
            METRICS_WORK.take(),
            ubos_metrics::consts::Metrics::REQUEST_BUFFER_SIZE,
        );
        for buffer in metrics_buffers {
            spawner.spawn(
                metrics_worker_task(
                    metrics_acceptor,
                    buffer,
                    &METRICS_SERVICE,
                    ubos_metrics::consts::Metrics::SOCKET_TIMEOUT,
                    ubos_metrics::consts::Metrics::IDLE_TIMEOUT,
                )
                .expect("BUG: metrics_worker_task token"),
            );
        }

        match fan::FanHwmonController::open() {
            Some(controller) => match temperature::HashboardTemperature::open() {
                Some(sensor) => {
                    spawner.spawn(
                        thermal_task(sensor, manager_handle).expect("BUG: thermal_task token"),
                    );
                    spawner.spawn(
                        fan::pid_fan_task(controller, thermal)
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
            Some(miner_psu) => {
                spawner.spawn(psu::watchdog_task(miner_psu).expect("BUG: psu watchdog token"));
                spawner
                    .spawn(psu::power_stats_task(miner_psu).expect("BUG: psu power_stats token"));
                spawner
                    .spawn(miner_serial::miner_serial_task().expect("BUG: miner_serial_task token"));
                spawner.spawn(miner::miner_task(miner_psu).expect("BUG: miner_task token"));
            }
            None => {
                warn!(
                    "host-runner: {} unset or i2c open failed; ubos-mining not spawned, falling back to in-process CPU mining",
                    psu::I2C_BUS_ENV
                );
                spawner.spawn(miner::miner_idle_task().expect("BUG: miner_idle_task token"));
                spawner.spawn(asic_cpu_task().expect("BUG: asic_cpu_task token"));
            }
        }
    });
}
