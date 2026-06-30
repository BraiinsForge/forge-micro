//! Host-side runner for ubos-main tasks.
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
//! Host-side prerequisites (run once per boot): `./setup-network.sh up`
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
use stratum_client::{RECONNECT_DELAY, SOCKET_TIMEOUT, STRATUM_TCP_BUFFER_SIZE};
use ubos_config::FileStorage;
use ubos_manager::Handle as ManagerHandle;
use ubos_mining::Handle as MiningHandle;
use ubos_net::net;

mod cpu_mock;
mod fan;
mod gpiod;
mod hosted_network;
mod miner;
mod miner_serial;
mod psu;
mod temperature;

use hosted_network::HostDevice;

// Nineteen sockets are held continuously: DHCPv4, DNS, the stratum
// client, the mDNS responder, 5 webui listener sockets on :80, 5 REST
// API listener sockets on :8080, and 5 metrics listener sockets on
// :8081. NTP opens a transient UDP socket, and closed TCP connections
// linger in TIME_WAIT until smoltcp reaps them. 24 provides headroom
// for concurrent connections and TIME_WAIT, matching the firmware boards.
const SOCKET_COUNT: usize = 24;
const BOARD_NAME: &str = "host-runner";

/// The host runner simulates the BMM100 hashboard, so it threads that
/// board descriptor through the mining, config, and API layers.
pub const BOARD: &ubos_asic::BoardDescriptor = &ubos_asic::BMM100;
pub const BOUNDS: ubos_config::MiningBounds = ubos_config::board_bounds(BOARD);
pub const MINING_INFO: ubos_config::MiningInfo = ubos_config::mining_info(BOARD, &BOUNDS);

/// Concurrent TCP connections the web-UI server (:80) accepts.
const WEBUI_HTTP_POOL: usize = 5;
/// Concurrent TCP connections the REST API server (:8080) accepts.
const API_HTTP_POOL: usize = 5;
/// Concurrent TCP connections the Prometheus metrics server (:8081)
/// accepts. Matches the webui and api pools so a scrape fan-in is
/// served concurrently.
const METRICS_HTTP_POOL: usize = 5;

ubos_http::http_worker_task!(api_worker_task, ubos_api::RestApiService, API_HTTP_POOL);
ubos_http::http_worker_task!(
    webui_worker_task,
    ubos_webui::DashboardService,
    WEBUI_HTTP_POOL
);
ubos_http::http_worker_task!(
    metrics_worker_task,
    ubos_metrics::metrics::PrometheusMetricsService,
    METRICS_HTTP_POOL
);

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
        let hashrate = ubos_metrics::hashrate::HashrateDisplay(ubos_metrics::hashrate::hashrate());
        info!("host-runner: tick, address={address:?}, hashrate={hashrate}");
    }
}

#[embassy_executor::task]
async fn ntp_sync_task(stack: Stack<'static>) -> ! {
    net::run_ntp_sync(stack).await
}

#[embassy_executor::task]
async fn metrics_ingest_task() -> ! {
    ubos_metrics::metrics::run_ingest().await
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
async fn thermal_task(sensor: temperature::HashboardTemperature, manager: ManagerHandle) -> ! {
    ubos_thermal::run_thermal(sensor, manager).await
}

#[embassy_executor::task]
async fn manager_task(mining: MiningHandle) -> ! {
    ubos_manager::run_manager(mining).await
}

#[embassy_executor::task]
async fn config_task(storage: FileStorage) -> ! {
    ubos_config::run(storage, BOARD).await
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
    let restart = config_handle.stratum_restart();

    loop {
        let snapshot = config_handle.current();
        let Some(entry) = snapshot.pool.first() else {
            warn!("stratum-client: pool list empty, awaiting config update");
            restart.wait().await;
            continue;
        };

        let one_session = async {
            stratum_client::record_reconnect_attempt();
            let mut socket = match net::connect_stratum(
                stack,
                entry.host.as_str(),
                entry.port,
                SOCKET_TIMEOUT,
                rx_buffer,
                tx_buffer,
            )
            .await
            {
                Ok(socket) => socket,
                Err(error) => {
                    warn!("stratum-client: connect failed: {error:?}");
                    Timer::after(RECONNECT_DELAY).await;
                    return;
                }
            };
            let mut state = stratum_client::StratumState::new();

            match &entry.protocol {
                ubos_config::ProtocolKind::Sv1 { password } => {
                    let sv1 = stratum_v1::Sv1Config::new(
                        entry.user.as_str(),
                        password.as_ref().map_or("", heapless::String::as_str),
                    )
                    .expect("BUG: PoolEntry user/password match Sv1Config buffer sizes");
                    let mut client = stratum_v1::StratumV1Client::new(&sv1);
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
                }
                ubos_config::ProtocolKind::Sv2 { pool_pubkey } => {
                    let sv2 = stratum_v2::Sv2Config::new(entry.user.as_str(), *pool_pubkey)
                        .expect("BUG: PoolEntry.user matches Sv2Config buffer size");
                    let mut client =
                        stratum_v2::StratumV2Client::new(&sv2, secp_ctx, HostSv2Rng(OsRng));
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
                }
            }

            Timer::after(RECONNECT_DELAY).await;
        };
        if matches!(
            select(one_session, restart.wait()).await,
            Either::Second(())
        ) {
            info!("stratum-client: pool config changed, reconnecting");
        }
    }
}

#[embassy_executor::task]
async fn stratum_metrics_task() -> ! {
    stratum_client::publishers::run_stratum_metrics_publisher().await
}

#[embassy_executor::task]
async fn miner_metrics_task(mining: MiningHandle) -> ! {
    ubos_mining::publishers::run_miner_metrics_publisher(mining).await
}

#[embassy_executor::task]
async fn hashrate_metrics_task() -> ! {
    ubos_metrics::hashrate::run_hashrate_publisher().await
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
        spawner.spawn(hashrate_metrics_task().expect("BUG: hashrate_metrics_task token"));
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
        // any request. Firmware boards seed in their async `boot` entry
        // before spawning; host-runner has no async entry, so
        // `block_on` is used here to match the boot-time ordering.
        let mut csprng_seed = [0u8; 32];
        OsRng.fill_bytes(&mut csprng_seed);
        embassy_futures::block_on(ubos_auth::csprng::seed_csprng(csprng_seed));

        // REST API on :8080 — seed credentials once, then spawn one worker
        // task per pool slot so each has its own static TaskStorage. The
        // executor closure is not async, so `block_on` drives the seed and
        // the infallible bind to match the firmware's boot-time ordering.
        // This block and the webui/metrics ones below hand-expand
        // `serve_http_pool!`, which assumes an async context; keep them in
        // sync if that macro changes.
        embassy_futures::block_on(ubos_api::seed_api_credentials());
        static API_TCP_BUFFERS: ConstStaticCell<
            edge_nal_embassy::TcpBuffers<
                API_HTTP_POOL,
                { ubos_api::consts::Api::TX_BUFFER_SIZE },
                { ubos_api::consts::Api::RX_BUFFER_SIZE },
            >,
        > = ConstStaticCell::new(edge_nal_embassy::TcpBuffers::new());
        static API_TCP_NAL: StaticCell<edge_nal_embassy::Tcp<'static>> = StaticCell::new();
        static API_WORK: ConstStaticCell<
            [u8; ubos_api::consts::Api::REQUEST_BUFFER_SIZE * API_HTTP_POOL],
        > = ConstStaticCell::new([0; ubos_api::consts::Api::REQUEST_BUFFER_SIZE * API_HTTP_POOL]);
        static API_SERVICE: StaticCell<ubos_api::RestApiService> = StaticCell::new();
        let api_tcp: &'static edge_nal_embassy::Tcp<'static> =
            API_TCP_NAL.init(edge_nal_embassy::Tcp::new(stack, API_TCP_BUFFERS.take()));
        let api_acceptor =
            embassy_futures::block_on(ubos_http::bind_tcp(api_tcp, ubos_api::consts::Api::PORT));
        let api_service: &'static ubos_api::RestApiService =
            API_SERVICE.init(ubos_api::RestApiService::new(
                BOARD_NAME,
                &MINING_INFO,
                &BOUNDS,
                thermal,
                manager_handle,
                ubos_psu::handle(),
                stack,
            ));
        let api_buffers = ubos_http::split_work_buffer::<API_HTTP_POOL>(
            API_WORK.take(),
            ubos_api::consts::Api::REQUEST_BUFFER_SIZE,
        );
        for buffer in api_buffers {
            spawner.spawn(
                api_worker_task(
                    api_acceptor,
                    buffer,
                    api_service,
                    ubos_api::consts::Api::SOCKET_TIMEOUT,
                )
                .expect("BUG: api_worker_task token"),
            );
        }
        spawner.spawn(sync_credentials_task().expect("BUG: sync_credentials_task token"));

        spawner.spawn(network_control_task(stack).expect("BUG: network_control_task token"));

        // Web UI on :80 — seed credentials once; sync_credentials_task
        // keeps the shared credential store current after first boot.
        embassy_futures::block_on(ubos_webui::seed_webui_credentials());
        static WEBUI_TCP_BUFFERS: ConstStaticCell<
            edge_nal_embassy::TcpBuffers<
                WEBUI_HTTP_POOL,
                { ubos_webui::consts::Http::TX_BUFFER_SIZE },
                { ubos_webui::consts::Http::RX_BUFFER_SIZE },
            >,
        > = ConstStaticCell::new(edge_nal_embassy::TcpBuffers::new());
        static WEBUI_TCP_NAL: StaticCell<edge_nal_embassy::Tcp<'static>> = StaticCell::new();
        static WEBUI_WORK: ConstStaticCell<
            [u8; ubos_webui::consts::Http::REQUEST_BUFFER_SIZE * WEBUI_HTTP_POOL],
        > = ConstStaticCell::new([0; ubos_webui::consts::Http::REQUEST_BUFFER_SIZE * WEBUI_HTTP_POOL]);
        static WEBUI_SERVICE: ubos_webui::DashboardService = ubos_webui::DashboardService;
        let webui_tcp: &'static edge_nal_embassy::Tcp<'static> =
            WEBUI_TCP_NAL.init(edge_nal_embassy::Tcp::new(stack, WEBUI_TCP_BUFFERS.take()));
        let webui_acceptor =
            embassy_futures::block_on(ubos_http::bind_tcp(webui_tcp, ubos_webui::consts::Http::PORT));
        let webui_buffers = ubos_http::split_work_buffer::<WEBUI_HTTP_POOL>(
            WEBUI_WORK.take(),
            ubos_webui::consts::Http::REQUEST_BUFFER_SIZE,
        );
        for buffer in webui_buffers {
            spawner.spawn(
                webui_worker_task(
                    webui_acceptor,
                    buffer,
                    &WEBUI_SERVICE,
                    ubos_webui::consts::Http::SOCKET_TIMEOUT,
                )
                .expect("BUG: webui_worker_task token"),
            );
        }

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
        static METRICS_SERVICE: ubos_metrics::metrics::PrometheusMetricsService =
            ubos_metrics::metrics::PrometheusMetricsService;
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
            Some((miner_psu, state, terminated)) => {
                spawner
                    .spawn(psu::watchdog_task(state, terminated).expect("BUG: psu watchdog token"));
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
