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
use embassy_futures::select::{select, Either};
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
use ubos_thermal::Handle as ThermalHandle;

mod cpu_mock;
mod fan;
mod gpiod;
mod hosted_network;
mod miner;
mod miner_serial;
mod psu;
mod temperature;

use hosted_network::HostDevice;

const SOCKET_COUNT: usize = 9;
const BOARD_NAME: &str = "host-runner";

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
async fn metrics_task(stack: Stack<'static>) -> ! {
    ubos_metrics::metrics::run(stack).await
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

#[embassy_executor::task]
// run_rest_api's future crosses clippy's large_futures threshold on this
// 64-bit host; the allow inside ubos-api does not reach this await site. See
// run_rest_api for why the size is acceptable for a spawned-once task.
#[allow(clippy::large_futures)]
async fn rest_api_task(
    stack: Stack<'static>,
    thermal: ThermalHandle,
    manager: ManagerHandle,
    psu: ubos_psu::Handle,
) -> ! {
    ubos_api::run_rest_api(stack, BOARD_NAME, thermal, manager, psu).await
}

#[embassy_executor::task]
async fn network_apply_task(stack: Stack<'static>) -> ! {
    ubos_api::run_network_apply(stack).await
}

#[embassy_executor::task]
async fn webui_task(stack: Stack<'static>) -> ! {
    ubos_webui::run(stack).await
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
    ubos_config::run(storage).await
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
        spawner.spawn(metrics_task(stack).expect("BUG: metrics_task token"));
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
        spawner.spawn(
            rest_api_task(stack, thermal, manager_handle, ubos_psu::handle())
                .expect("BUG: rest_api_task token"),
        );
        spawner.spawn(
            network_apply_task(stack).expect("BUG: network_apply_task token"),
        );
        spawner.spawn(webui_task(stack).expect("BUG: webui_task token"));
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
