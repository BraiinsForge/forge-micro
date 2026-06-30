//! Firmware entry point for the ESP32-S3 `DevKit` + external W5100S (SPI)
//! with the BMM100/BM1362 ubos-mining backend.
//!
//! Configures SPI with DMA, brings up the W5100S via `embassy-net-wiznet`,
//! starts the embassy-net stack with DHCP, and spawns the watchdog,
//! ethernet runner, NTP, ubos-mining mining, and HTTP tasks.
//! Built with `cargo build` from this crate inside `nix develop .#esp`.
//!
//! The ESP32-S3 has two cores (`PRO_CPU` + `APP_CPU`). Embassy-net, the
//! mining state machine, and all peripheral tasks run on Core 0; the
//! ubos-mining `serial_task` runs on a separate executor on Core 1, mirroring
//! the rp2350 and bmm-pico dual-core split that keeps the chip UART loop
//! off the network path. `ASIC_SESSION` uses a `CriticalSectionRawMutex`,
//! so cross-core sharing needs no additional channel. Core 1 also accumulates
//! `IDLE_TICKS[1]` via `IdleAccounting::<1>` for the cpu-load metric.
//!
//! Board docs: `../../../docs/OVERVIEW.md`
//! Wiring/build: `../../../docs/SETUP.md`

#![no_std]
#![no_main]

use defmt::{info, unwrap, warn};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W5100S;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_sync::mutex::Mutex;
use embassy_time::{Delay, Duration, Instant, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::peripherals::TIMG1;
use esp_hal::rng::Rng;
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::system::Stack;
use esp_hal::timer::timg::{MwdtStage, TimerGroup, Wdt};
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_rtos::embassy::Callbacks;
use portable_atomic::Ordering;
use static_cell::StaticCell;
use ubos_config::{FlashStorage, SyncFlash};
#[cfg(feature = "display")]
use ubos_display::sh1106::{Rotation, Sh1106};
use ubos_manager::Handle as ManagerHandle;
use ubos_mining::Handle as MiningHandle;
use ubos_net::net;
use ubos_thermal::Handle as ThermalHandle;
use {defmt_rtt as _, esp_backtrace as _};

/// Wraps the event-ring backing so it can carry the esp-hal `Persistable`
/// marker that `#[ram(persistent)]` requires. `RetainedRing` is `repr(C)` over
/// `u32` and `u8` fields, so every bit pattern is a valid value.
#[repr(transparent)]
struct PersistentRing(ubos_events::RetainedRing<{ config::Board::EVENT_LOG_CAPACITY }>);

// Safety: every bit pattern of `PersistentRing` is a valid value and it needs
// no initialisation beyond the validity header `init` checks, so persisting it
// uninitialised across a reset is sound.
unsafe impl esp_hal::Persistable for PersistentRing {}

/// Backing storage for the user-facing event ring, owned here so its size
/// follows this board's `EVENT_LOG_CAPACITY`. Placed in RTC fast RAM, which
/// retains across a warm reset; the validity header in `RetainedRing` discards
/// it after a cold boot leaves garbage.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut EVENT_LOG_BACKING: PersistentRing = PersistentRing(ubos_events::RetainedRing::new());

mod config;
mod fan;
mod mining;
mod psu;
mod temperature;
mod uart_baudrate;

defmt::timestamp!("{=u64:iso8601s}", ubos_net::wall_clock_secs());

esp_bootloader_esp_idf::esp_app_desc!();

type EthernetRunner = Runner<
    'static,
    W5100S,
    ExclusiveDevice<
        esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>,
        Output<'static>,
        Delay,
    >,
    Input<'static>,
    Output<'static>,
>;

struct BoardWatchdog(Wdt<TIMG1<'static>>);

impl ubos_core::watchdog::Watchdog for BoardWatchdog {
    fn init(&mut self) {
        self.0.enable();
        self.0
            .set_timeout(MwdtStage::Stage0, config::Watchdog::TIMEOUT);
    }

    fn feed(&mut self) {
        self.0.feed();
    }
}

#[embassy_executor::task]
async fn watchdog_task(watchdog: BoardWatchdog) -> ! {
    ubos_core::watchdog::run_watchdog(watchdog).await
}

#[embassy_executor::task]
async fn ethernet_task(runner: EthernetRunner) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device<'static>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn ntp_sync_task(stack: embassy_net::Stack<'static>) -> ! {
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
async fn mdns_task(stack: embassy_net::Stack<'static>, hostname: &'static str) -> ! {
    ubos_mdns::run_mdns(
        stack,
        hostname,
        config::Board::NAME,
        config::Board::MCU,
        MDNS_SERVICES,
    )
    .await
}

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    ubos_metrics::cpu_load::run_cpu_load::<2>().await
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

/// Adapts `esp32s3_tsens::Tsens` to the `TemperatureSensor` trait.
struct BoardTempSensor(esp32s3_tsens::Tsens<'static>);

impl ubos_temperature::TemperatureSensor for BoardTempSensor {
    async fn read(&mut self) -> Option<ubos_core::units::Temperature> {
        self.0
            .read_millicelsius()
            .await
            .map(ubos_core::units::Temperature::from_millicelsius)
    }
}

#[embassy_executor::task]
async fn temperature_task(sensor: BoardTempSensor) -> ! {
    ubos_temperature::run_temperature(sensor).await
}

#[embassy_executor::task]
async fn thermal_task(sensor: temperature::HashboardTemperature, manager: ManagerHandle) -> ! {
    ubos_thermal::run_thermal(sensor, manager).await
}

#[embassy_executor::task]
async fn manager_task(mining: MiningHandle) -> ! {
    ubos_manager::run_manager(mining).await
}

/// Storage backend type the [`config_task`] hands to
/// [`ubos_config::run`]. The chain is
/// `esp-storage FlashStorage → SyncFlash → ubos-config FlashStorage`.
type BoardConfigStorage = FlashStorage<SyncFlash<ubos_ota::EspSharedFlash>>;

#[embassy_executor::task]
async fn config_task(storage: BoardConfigStorage) -> ! {
    ubos_config::run(storage, config::BOARD).await
}

#[embassy_executor::task]
async fn cooling_task(
    board_fan: fan::BoardFan,
    board_tach: fan::BoardTach,
    thermal: ThermalHandle,
) -> ! {
    ubos_cooling::run_cooling(board_fan, board_tach, thermal).await
}

#[cfg(feature = "display")]
type OledDisplay = Sh1106<
    ExclusiveDevice<
        esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>,
        Output<'static>,
        embedded_hal_bus::spi::NoDelay,
    >,
    Output<'static>,
>;

/// The reset pin is moved in and held for the task's lifetime so the SH1106
/// RESET line stays driven high; dropping it would release the pad and blank
/// the panel.
#[cfg(feature = "display")]
#[embassy_executor::task]
async fn display_task(
    mut display: OledDisplay,
    #[allow(clippy::used_underscore_binding)] _reset_pin: Output<'static>,
) -> ! {
    ubos_display::display::run(&mut display).await
}

/// Resets the MCU once a reboot has been requested through the HTTP API.
#[embassy_executor::task]
async fn reset_task() -> ! {
    ubos_core::reboot::wait_for_reboot_request().await;
    esp_hal::system::software_reset()
}

/// Keeps the credential store in sync with config changes at runtime.
#[embassy_executor::task]
async fn sync_credentials_task() -> ! {
    ubos_api::run_sync_credentials().await
}

ubos_http::http_worker_task!(
    api_worker_task,
    ubos_api::RestApiService,
    config::Board::API_HTTP_POOL
);

ubos_http::http_worker_task!(
    webui_worker_task,
    ubos_webui::DashboardService,
    config::Board::WEBUI_HTTP_POOL
);

ubos_http::http_worker_task!(
    metrics_worker_task,
    ubos_metrics::metrics::PrometheusMetricsService,
    config::Board::METRICS_HTTP_POOL
);

#[embassy_executor::task]
async fn network_control_task(stack: embassy_net::Stack<'static>) -> ! {
    net::run_network_control(stack).await
}

#[embassy_executor::task]
async fn ota_watchdog_task() -> ! {
    ubos_api::run_ota_session_watchdog().await
}

/// Board task shell around the shared stratum session loop. The loop body
/// lives in `ubos-firmware`; the `#[embassy_executor::task]` shell stays per
/// board because esp32s3 pins a different `embassy-executor` version than the
/// ARM boards, so the spawned task type must come from each board's executor.
#[embassy_executor::task]
async fn stratum_client_task(
    stack: embassy_net::Stack<'static>,
    sv2_prng: &'static mut ubos_firmware::ChaCha20Rng,
) -> ! {
    ubos_firmware::run_stratum_session(stack, sv2_prng).await
}

/// Brackets `flags.wait()` inside `esp_rtos::embassy::Executor::run_with_callbacks`.
/// `on_idle` records the entry instant; `before_poll` accumulates the
/// elapsed ticks on the next iteration into `IDLE_TICKS[CPU_INDEX]`.
/// `run_with_callbacks` only fires `on_idle` when the wait will
/// actually block, so spurious wakes do not skew the count.
struct IdleAccounting<const CPU_INDEX: usize> {
    idle_start: Option<Instant>,
}

impl<const CPU_INDEX: usize> IdleAccounting<CPU_INDEX> {
    const fn new() -> Self {
        const {
            assert!(
                CPU_INDEX < ubos_metrics::cpu_load::MAX_CPUS,
                "CPU_INDEX exceeds MAX_CPUS"
            );
        }
        Self { idle_start: None }
    }
}

impl<const CPU_INDEX: usize> Callbacks for IdleAccounting<CPU_INDEX> {
    fn before_poll(&mut self) {
        if let Some(start) = self.idle_start.take() {
            let elapsed = (Instant::now() - start).as_ticks();
            ubos_metrics::cpu_load::IDLE_TICKS[CPU_INDEX].fetch_add(elapsed, Ordering::Relaxed);
        }
    }

    fn on_idle(&mut self) {
        self.idle_start = Some(Instant::now());
    }
}

#[esp_hal::main]
fn main() -> ! {
    static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
    let executor = EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run_with_callbacks(
        |spawner| spawner.spawn(unwrap!(boot(spawner))),
        IdleAccounting::<0>::new(),
    )
}

#[embassy_executor::task]
#[allow(
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::similar_names
)]
async fn boot(spawner: Spawner) {
    static STATE: StaticCell<State<8, 8>> = StaticCell::new();
    // Socket-set capacity for the embassy-net stack. Thirteen sockets are
    // held continuously: DHCPv4, DNS, the stratum client, the mDNS
    // responder, 3 webui listener sockets on :80, 3 REST API listener
    // sockets on :8080, and 3 metrics listener sockets on :8081.
    // NTP opens a transient UDP socket every 60 s, and closed TCP
    // connections linger in TIME_WAIT until smoltcp reaps them.
    // 24 provides headroom for concurrent connections and TIME_WAIT.
    const STACK_SOCKETS: usize = 24;
    ubos_net::assert_stack_sockets!(
        STACK_SOCKETS,
        config::Board::WEBUI_HTTP_POOL,
        config::Board::API_HTTP_POOL,
        config::Board::METRICS_HTTP_POOL,
    );
    static RESOURCES: StaticCell<StackResources<STACK_SOCKETS>> = StaticCell::new();

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let hw = config::Board::take(peripherals);

    // Install the event ring and its decode-table registry before any task
    // that records events runs. wall_clock_secs returns uptime before NTP
    // sync and Unix time after; the ring stores it unconditionally.
    ubos_events::set_catalogs(ubos_firmware::EVENT_REGISTRY);
    // Safety: called once at boot before any task runs. EVENT_LOG_BACKING lives
    // in RTC fast RAM (persistent), so a warm reset preserves its contents.
    ubos_events::init(
        unsafe { &mut (*core::ptr::addr_of_mut!(EVENT_LOG_BACKING)).0 },
        ubos_net::wall_clock_secs,
    );

    let timg0 = TimerGroup::new(hw.timg0);
    let software_interrupts = SoftwareInterruptControl::new(hw.sw_interrupt);
    esp_rtos::start(timg0.timer0, software_interrupts.software_interrupt0);

    // Pin the ubos-mining serial worker to Core 1 so embassy-net on Core 0
    // stays responsive. Mirrors the rp2350/bmm-pico dual-core split.
    // `ASIC_SESSION` uses `CriticalSectionRawMutex`, so the cross-core
    // share is safe without an additional pipe. Core 1's executor uses
    // `IdleAccounting::<1>` to populate `IDLE_TICKS[1]` for cpu-load metrics.
    static APP_EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
    static APP_CORE_STACK: StaticCell<Stack<8192>> = StaticCell::new();
    let uart_parts = (hw.uart0, hw.uart_tx, hw.uart_rx);
    esp_rtos::start_second_core(
        hw.cpu_ctrl,
        software_interrupts.software_interrupt1,
        APP_CORE_STACK.init(Stack::new()),
        move || {
            let miner_serial = {
                let (peripheral, tx_pin, rx_pin) = uart_parts;
                let uart_config = UartConfig::default()
                    // Valid baud rate for UART0; cannot fail.
                    .with_baudrate(ubos_mining::consts::INITIAL_BAUDRATE);
                // Valid UART0 peripheral with default-compatible baud rate; cannot fail.
                let uart = Uart::new(peripheral, uart_config)
                    .expect("BUG: UART0 configuration failed")
                    .with_tx(tx_pin)
                    .with_rx(rx_pin)
                    .into_async();
                uart_baudrate::MinerUart::new(uart)
            };
            let executor = APP_EXECUTOR.init(esp_rtos::embassy::Executor::new());
            executor.run_with_callbacks(
                |app_spawner| {
                    app_spawner.spawn(unwrap!(mining::serial_task(miner_serial)));
                },
                IdleAccounting::<1>::new(),
            );
        },
    );

    // Report why the chip last reset and publish it for the OTA reset
    // diagnostics. A watchdog reason means the previous run stalled the
    // executor past config::Watchdog::TIMEOUT; a power-on or external reset
    // reads as None. Unlike the RP boards there is no reset-surviving scratch
    // register, so the OTA breadcrumb fields are absent: the two-slot otadata
    // scheme is self-healing, so a reset is never an in-progress OTA write.
    use esp_hal::rtc_cntl::SocResetReason;
    use ubos_api::{OtaWritePhase, ResetReason};
    let reset_reason = match esp_hal::system::reset_reason() {
        Some(
            SocResetReason::CoreMwdt0
            | SocResetReason::CoreMwdt1
            | SocResetReason::CpuMwdt0
            | SocResetReason::CpuMwdt1
            | SocResetReason::CoreRtcWdt
            | SocResetReason::CpuRtcWdt
            | SocResetReason::SysRtcWdt
            | SocResetReason::SysSuperWdt,
        ) => {
            warn!("last reset: watchdog");
            ResetReason::WatchdogTimeout
        }
        Some(SocResetReason::SysBrownOut) => {
            warn!("last reset: brownout");
            ResetReason::Unknown
        }
        Some(SocResetReason::CoreSw | SocResetReason::CpuSw) => {
            info!("last reset: software");
            ResetReason::Clean
        }
        Some(SocResetReason::ChipPowerOn) | None => {
            info!("last reset: power-on or external");
            ResetReason::Clean
        }
        Some(
            SocResetReason::CoreDeepSleep
            | SocResetReason::SysClkGlitch
            | SocResetReason::CoreEfuseCrc
            | SocResetReason::CoreUsbUart
            | SocResetReason::CoreUsbJtag
            | SocResetReason::CorePwrGlitch,
        ) => {
            info!("last reset: other");
            ResetReason::Unknown
        }
    };
    ubos_api::publish_reset_diag(reset_reason, false, 0, OtaWritePhase::None);

    // One esp-storage driver behind a mutex, shared by the config store and
    // the OTA writer: the chip has a single flash peripheral. The mutex
    // serialises individual flash ops between the two consumers, and
    // `multicore_auto_park` parks core 1 around each op so a flash write never
    // races the mining serial worker.
    static SHARED_FLASH: StaticCell<ubos_ota::EspFlashMutex> = StaticCell::new();
    let shared_flash: &'static ubos_ota::EspFlashMutex =
        SHARED_FLASH.init(ubos_ota::EspFlashMutex::new(core::cell::RefCell::new(
            esp_storage::FlashStorage::new(hw.flash).multicore_auto_park(),
        )));

    // Confirm the running image before the watchdog is armed so the
    // rollback-enabled bootloader stops arming a revert on the next reset. A
    // freshly OTA-activated slot boots as PendingVerify; this marks it Valid.
    // Running it pre-yield, before the feeder task can enable the 2 s window,
    // keeps the synchronous otadata write off the watchdog's clock.
    ubos_ota::confirm_running(ubos_ota::EspSharedFlash::new(shared_flash));

    let timg1 = TimerGroup::new(hw.timg1);
    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(timg1.wdt))));
    spawner.spawn(unwrap!(reset_task()));

    // SPI wiring via LCD header on Waveshare ESP32-S3-LCD-Driver-Board.
    // LCD_MOSI=GPIO2, LCD_MISO=GPIO42, LCD_SCLK=GPIO1,
    // LCD_CS=GPIO39, LCD_DC=GPIO41 as INT, LCD_RST=GPIO40 as RST.
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = dma_buffers!(4096);
    // dma_buffers! produces correctly-sized aligned descriptors and buffers,
    // so DmaRxBuf::new and DmaTxBuf::new only fail on a programming error.
    let dma_rx_buf =
        DmaRxBuf::new(rx_descriptors, rx_buffer).expect("BUG: DMA RX buffer creation failed");
    let dma_tx_buf =
        DmaTxBuf::new(tx_descriptors, tx_buffer).expect("BUG: DMA TX buffer creation failed");

    let spi = Spi::new(
        hw.spi,
        SpiConfig::default()
            .with_frequency(config::Spi::FREQUENCY)
            .with_mode(Mode::_0),
    )
    // SPI peripheral configuration uses only valid Mode and Rate values.
    .expect("BUG: SPI configuration failed")
    .with_sck(hw.sck)
    .with_mosi(hw.mosi)
    .with_miso(hw.miso)
    .with_dma(hw.dma)
    .with_buffers(dma_rx_buf, dma_tx_buf)
    .into_async();

    let chip_select = Output::new(hw.cs, Level::High, OutputConfig::default());
    let w5500_int = Input::new(hw.w5500_int, InputConfig::default().with_pull(Pull::Up));
    let w5500_reset = Output::new(hw.w5500_reset, Level::High, OutputConfig::default());

    // Fold the ESP32-S3 factory eFuse MAC into a stable per-unit MAC.
    // The WIZnet chip has no factory address; derive_mac keeps the fleet
    // MAC scheme consistent across all board families.
    let mac_addr = ubos_net::mac::derive_mac(
        ubos_net::mac::DUMMY_OUI,
        config::Board::MAC_FAMILY,
        esp_hal::efuse::base_mac_address().as_bytes(),
    );
    info!(
        "mac {=u8:02x}:{=u8:02x}:{=u8:02x}:{=u8:02x}:{=u8:02x}:{=u8:02x}",
        mac_addr[0], mac_addr[1], mac_addr[2], mac_addr[3], mac_addr[4], mac_addr[5],
    );
    ubos_net::net::handle().set_mac(mac_addr);
    let state = STATE.init(State::<8, 8>::new());
    let (device, runner) = embassy_net_wiznet::new(
        mac_addr,
        state,
        ExclusiveDevice::new(spi, chip_select, Delay),
        w5500_int,
        w5500_reset,
    )
    .await
    // Initialization succeeds when the chip is wired correctly and responds on SPI.
    .expect("BUG: W5100S initialization failed");
    spawner.spawn(unwrap!(ethernet_task(runner)));

    let manager_handle = ubos_manager::handle();
    let mining_handle = ubos_mining::handle();

    let config_storage = FlashStorage::new(
        SyncFlash(ubos_ota::EspSharedFlash::new(shared_flash)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(config_storage)));

    static OTA_TARGET: StaticCell<ubos_ota::EspOtaTarget> = StaticCell::new();
    let ota_target = OTA_TARGET.init(ubos_ota::OtaEngine::new(
        ubos_ota::EspBackend::new(ubos_ota::EspSharedFlash::new(shared_flash)),
        config::FIRMWARE_VERSION,
    ));

    let rng = Rng::new();
    let seed = u64::from(rng.random()) << 32 | u64::from(rng.random());

    // The config task publishes the stored config on first boot. Wait
    // for it, then derive the stack config and hostname. Only the small
    // results escape the block, so this task's future never has to hold
    // the full Config or load_initial's decode buffer.
    static MDNS_HOSTNAME: StaticCell<heapless::String<32>> = StaticCell::new();
    let (net_config, mdns_hostname) = {
        let initial = ubos_config::handle().changed().await;
        let net_config = match &initial.network.mode {
            ubos_config::NetworkMode::Static {
                address,
                prefix_len,
                gateway,
                dns,
            } => ubos_net::config::static_ipv4(*address, *prefix_len, *gateway, dns),
            ubos_config::NetworkMode::Dhcp => {
                embassy_net::Config::dhcpv4(embassy_net::DhcpConfig::default())
            }
        };
        // Compose inside the block so only the &'static hostname escapes,
        // not a 32-byte copy held across the rest of boot.
        let mdns_hostname = ubos_mdns::compose_hostname_configured::<
            32,
            { config::Board::MDNS_HOSTNAME_BASE.len() },
        >(
            initial.network.hostname.as_str(),
            config::Board::MDNS_HOSTNAME_BASE,
            mac_addr[5],
            &MDNS_HOSTNAME,
        );
        (net_config, mdns_hostname)
    };

    let (stack, runner) = embassy_net::new(
        device,
        net_config,
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(unwrap!(net_task(runner)));

    // Seed the session CSPRNG. esp-hal Rng produces one u32 per call;
    // 8 calls fill the 32-byte seed.
    let csprng_seed = {
        let mut seed = [0u8; 32];
        for chunk in seed.chunks_exact_mut(4) {
            chunk.copy_from_slice(&rng.random().to_le_bytes());
        }
        seed
    };
    ubos_auth::csprng::seed_csprng(csprng_seed).await;

    // Independent entropy stream for the SV2 noise handshake, drawn from
    // the same hardware RNG as the session CSPRNG.
    let sv2_seed = {
        let mut seed = [0u8; 32];
        for chunk in seed.chunks_exact_mut(4) {
            chunk.copy_from_slice(&rng.random().to_le_bytes());
        }
        seed
    };
    let sv2_prng = ubos_firmware::seed_sv2_prng(sv2_seed);

    spawner.spawn(unwrap!(ntp_sync_task(stack)));
    spawner.spawn(unwrap!(metrics_ingest_task()));

    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(cpu_load_task()));
    spawner.spawn(unwrap!(stratum_metrics_task()));
    spawner.spawn(unwrap!(hashrate_metrics_task()));
    // No LED blink task: board wiring does not include an accessible LED.
    let tsens = esp32s3_tsens::Tsens::new(hw.sens, esp32s3_tsens::Range::Standard).await;
    spawner.spawn(unwrap!(temperature_task(BoardTempSensor(tsens))));

    // SH1106 OLED on SPI3: reset pulse, then async DMA SPI.
    // The reset pin is moved into display_task so it stays driven high for the display's lifetime.
    #[cfg(feature = "display")]
    let (reset, display) = {
        let rst = {
            let mut pin = Output::new(hw.display_rst, Level::Low, OutputConfig::default());
            Timer::after(Duration::from_millis(10)).await;
            pin.set_high();
            Timer::after(Duration::from_millis(10)).await;
            pin
        };
        let (disp_rx_buf, disp_rx_desc, disp_tx_buf, disp_tx_desc) = dma_buffers!(2048);
        // dma_buffers! produces correctly-sized aligned descriptors and buffers,
        // so DmaRxBuf::new and DmaTxBuf::new only fail on a programming error.
        let disp_dma_rx =
            DmaRxBuf::new(disp_rx_desc, disp_rx_buf).expect("BUG: display DMA RX buffer failed");
        let disp_dma_tx =
            DmaTxBuf::new(disp_tx_desc, disp_tx_buf).expect("BUG: display DMA TX buffer failed");
        let display_spi = Spi::new(
            hw.display_spi,
            SpiConfig::default()
                .with_frequency(config::DisplaySpi::FREQUENCY)
                .with_mode(Mode::_0),
        )
        // SPI peripheral configuration uses only valid Mode and Rate values.
        .expect("BUG: SPI3 configuration failed")
        .with_sck(hw.display_sck)
        .with_mosi(hw.display_mosi)
        .with_dma(hw.display_dma)
        .with_buffers(disp_dma_rx, disp_dma_tx)
        .into_async();
        let display_cs = Output::new(hw.display_cs, Level::High, OutputConfig::default());
        let display_dc = Output::new(hw.display_dc, Level::Low, OutputConfig::default());
        let spi_device = ExclusiveDevice::new_no_delay(display_spi, display_cs);
        let mut display = Sh1106::new(spi_device, display_dc, Rotation::Normal);
        // Display is reset and SPI is valid; init cannot fail.
        display.init_async().await.expect("BUG: SH1106 init failed");
        (rst, display)
    };

    #[cfg(feature = "display")]
    spawner.spawn(unwrap!(display_task(display, reset)));

    let thermal = ubos_thermal::handle();

    spawner.spawn(unwrap!(miner_metrics_task(mining_handle)));

    // BMM100 hashboard PSU, LM75A temperature sensors and the carrier
    // TCA9554 I/O expander all share I2C0 (GPIO15 SDA, GPIO7 SCL). All
    // mining tasks run on Core 0 so the I2C bus mutex can stay
    // `NoopRawMutex`: every consumer is on the same executor.
    {
        static I2C0_BUS: StaticCell<psu::Bus> = StaticCell::new();
        // Valid I2C0 peripheral with GPIO15/7 routed via the GPIO matrix; cannot fail.
        let i2c = I2c::new(hw.i2c0, I2cConfig::default())
            .expect("BUG: I2C0 configuration failed")
            .with_sda(hw.i2c_sda)
            .with_scl(hw.i2c_scl)
            .into_async();
        let bus: &'static psu::Bus = I2C0_BUS.init(Mutex::new(i2c));
        let (board_psu, psu_state, terminated) = psu::init(bus);
        spawner.spawn(unwrap!(psu::watchdog_task(psu_state, terminated)));
        spawner.spawn(unwrap!(psu::power_stats_task(board_psu)));

        // Hashboard reset: active-low, driven through the carrier TCA9554
        // expander on EXIO5. Asserted (held low) here until the miner
        // task sequences the release during bring-up.
        let reset = mining::BoardResetPin::new(bus).await;
        spawner.spawn(unwrap!(mining::miner_task(board_psu, reset)));

        // Temperature and fan monitor via shared I2C0 bus.
        let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
        let hashboard_temp = temperature::HashboardTemperature::new(temp_device);
        let board_fan = fan::BoardFan::new(hw.ledc, hw.fan_pwm_pin);
        let board_tach = fan::BoardTach::new(hw.pcnt, hw.fan_tach_pin);
        spawner.spawn(unwrap!(thermal_task(hashboard_temp, manager_handle)));
        spawner.spawn(unwrap!(manager_task(mining_handle)));
        spawner.spawn(unwrap!(cooling_task(board_fan, board_tach, thermal)));
    }

    // Plug-detect input; hold the peripheral so it is not reclaimed.
    let _hb_plug = Input::new(hw.hb_plug, InputConfig::default().with_pull(Pull::Up));

    info!("waiting for dhcp");
    let dhcp_config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", dhcp_config.address.address());

    spawner.spawn(unwrap!(stratum_client_task(stack, sv2_prng)));

    spawner.spawn(unwrap!(network_control_task(stack)));

    // Register the OTA target now that the network is up, then spawn the
    // session watchdog that aborts a stalled upload and resumes mining.
    ubos_api::register_ota_target(ota_target).await;
    spawner.spawn(unwrap!(ota_watchdog_task()));

    // REST API on :8080 — seed credentials once, then spawn one worker
    // task per pool slot so each has its own static TaskStorage.
    ubos_api::seed_api_credentials().await;
    static API_SERVICE: StaticCell<ubos_api::RestApiService> = StaticCell::new();
    let api_service: &'static ubos_api::RestApiService =
        API_SERVICE.init(ubos_api::RestApiService::new(
            config::Board::NAME,
            &config::MINING_INFO,
            &config::BOUNDS,
            thermal,
            manager_handle,
            ubos_psu::handle(),
            stack,
        ));
    ubos_http::serve_http_pool!(
        spawner,
        stack,
        api_worker_task,
        api_service,
        config::Board::API_HTTP_POOL,
        ubos_api::consts::Api
    );
    spawner.spawn(unwrap!(sync_credentials_task()));

    // Web UI on :80 — seed credentials once (the API listener's
    // sync_credentials_task keeps them current after first boot).
    ubos_webui::seed_webui_credentials().await;
    static WEBUI_SERVICE: ubos_webui::DashboardService = ubos_webui::DashboardService;
    ubos_http::serve_http_pool!(
        spawner,
        stack,
        webui_worker_task,
        &WEBUI_SERVICE,
        config::Board::WEBUI_HTTP_POOL,
        ubos_webui::consts::Http
    );

    static METRICS_SERVICE: ubos_metrics::metrics::PrometheusMetricsService =
        ubos_metrics::metrics::PrometheusMetricsService;
    ubos_http::serve_http_pool!(
        spawner,
        stack,
        metrics_worker_task,
        &METRICS_SERVICE,
        config::Board::METRICS_HTTP_POOL,
        ubos_metrics::consts::Metrics
    );
}
