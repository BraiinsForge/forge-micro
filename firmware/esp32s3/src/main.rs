//! Firmware entry point for the ESP32-S3 `DevKit` + external W5100S (SPI)
//! with the BMM100/BM1362 ubos-miner backend.
//!
//! Configures SPI with DMA, brings up the W5100S via `embassy-net-wiznet`,
//! starts the embassy-net stack with DHCP, and spawns the watchdog,
//! ethernet runner, NTP, BTC, ubos-miner mining, and HTTP tasks.
//! Built with `cargo build` from this crate inside `nix develop .#esp`.
//!
//! The ESP32-S3 has two cores (`PRO_CPU` + `APP_CPU`). Embassy-net, the
//! mining state machine, and all peripheral tasks run on Core 0; the
//! ubos-miner `serial_task` runs on a separate executor on Core 1, mirroring
//! the rp2350 and bmm-pico dual-core split that keeps the chip UART loop
//! off the network path. `ASIC_SESSION` uses a `CriticalSectionRawMutex`,
//! so cross-core sharing needs no additional channel. Core 1 also accumulates
//! `IDLE_TICKS[1]` via `IdleAccounting::<1>` for the cpu-load metric.
//!
//! Board docs: `../../../docs/OVERVIEW.md`
//! Wiring/build: `../../../docs/SETUP.md`

#![no_std]
#![no_main]

use defmt::info;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W5100S;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_sync::mutex::Mutex;
use embassy_time::{Delay, Instant};
#[cfg(feature = "display")]
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::peripherals::TIMG1;
use esp_hal::rng::Rng;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::system::Stack;
use esp_hal::timer::timg::{MwdtStage, TimerGroup, Wdt};
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_rtos::embassy::Callbacks;
use portable_atomic::Ordering;
use rand_core::{CryptoRng, RngCore};
use static_cell::StaticCell;
#[cfg(feature = "display")]
use ubos_display::sh1106::{Rotation, Sh1106};
use ubos_monitor::{MonitorConfig, BMM101_DEFAULT};
use ubos_net::net;
use {defmt_rtt as _, esp_backtrace as _};

// `ubos_pool::pool_config::parse` runs at boot to copy seven
// compile-time env-var strings into typed fields. The result lives
// for the lifetime of the firmware; held in a `StaticCell` so the
// stratum task can take a `&'static PoolConfig`.
static POOL_CONFIG_CELL: StaticCell<ubos_pool::pool_config::PoolConfig> = StaticCell::new();

// `Sv1Config` references &'static str borrowed from the parsed
// `PoolConfig`; allocated at boot once the protocol is known to be
// `Sv1`. Empty when the parsed protocol is `Sv2`.
static SV1_CONFIG_CELL: StaticCell<stratum_v1::Sv1Config> = StaticCell::new();

// `Sv2Config::new` copies the worker identity into the owned
// `heapless::String<128>`. Held in a `StaticCell` to give
// `StratumV2Client::new` a `&'static Sv2Config`. Only populated
// when the parsed protocol is `Sv2`.
static SV2_CONFIG_CELL: StaticCell<stratum_v2::Sv2Config> = StaticCell::new();

// `secp256k1::AllPreallocated<'static>` precomputation buffer.
// `lowmemory` brings the verify table down to ~16 KB; the slot
// count is sized conservatively (covers signing + verification
// per `secp256k1_context_preallocated_size(All)` at the lowmemory
// flags). `CryptoCtx::preallocated` returns
// `Error::NotEnoughMemory` at runtime if undersized.
const SECP_CTX_SLOTS: usize = 4096;

static SECP_CTX_BUF: static_cell::ConstStaticCell<[ii_sv2::security::AlignedType; SECP_CTX_SLOTS]> =
    static_cell::ConstStaticCell::new([ii_sv2::security::AlignedType::ZERO; SECP_CTX_SLOTS]);

static SECP_CTX: StaticCell<
    ii_sv2::security::CryptoCtx<ii_sv2::security::secp256k1::AllPreallocated<'static>>,
> = StaticCell::new();

struct Sv2BoardRng(esp_hal::rng::Rng);

impl ii_sv2::security::Rng for Sv2BoardRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        // esp-hal `Rng` exposes a u32-at-a-time hardware RNG; fill
        // 4-byte chunks and truncate the trailing chunk to fit.
        for chunk in dest.chunks_mut(4) {
            let bytes = self.0.random().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}

static ASIC_SESSION: stratum_client::AsicSession = stratum_client::AsicSession::new();

static MONITOR_CONFIG: MonitorConfig = BMM101_DEFAULT;

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

/// Wrapper that bridges esp-hal `Rng` to `rand_core` 0.6 `CryptoRng`. The esp-hal
/// `Rng` only implements `CryptoRng` for `rand_core` 0.9, but `embedded-tls` 0.18
/// requires 0.6. Certificate verification is already skipped via `UnsecureProvider`.
struct TlsRng(Rng);

impl RngCore for TlsRng {
    fn next_u32(&mut self) -> u32 {
        self.0.random()
    }

    fn next_u64(&mut self) -> u64 {
        (u64::from(self.0.random()) << 32) | u64::from(self.0.random())
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for chunk in dest.chunks_mut(4) {
            let bytes = self.0.random().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

// SAFETY: Not used for real certificate verification. UnsecureProvider skips
// all certificate checks, so this CryptoRng marker is a formality.
impl CryptoRng for TlsRng {}

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
async fn bitcoin_price_task(stack: embassy_net::Stack<'static>, rng: TlsRng) -> ! {
    net::run_bitcoin_price(stack, rng).await
}

#[embassy_executor::task]
async fn ntp_sync_task(stack: embassy_net::Stack<'static>) -> ! {
    net::run_ntp_sync(stack).await
}

#[embassy_executor::task]
async fn metrics_task(stack: embassy_net::Stack<'static>) -> ! {
    ubos_metrics::metrics::run(stack).await
}

const MDNS_SERVICES: &[&ubos_mdns::ServiceEntry] = &[
    &ubos_api::HTTP_API_SERVICE,
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
async fn miner_metrics_task() -> ! {
    ubos_miner::publishers::run_miner_metrics_publisher().await
}

#[embassy_executor::task]
async fn hashrate_metrics_task() -> ! {
    ubos_metrics::hashrate::run_hashrate_publisher().await
}

/// Adapts `esp32s3_tsens::Tsens` to the `TemperatureSensor` trait.
struct BoardTempSensor(esp32s3_tsens::Tsens<'static>);

impl ubos_temperature::TemperatureSensor for BoardTempSensor {
    async fn read_millicelsius(&mut self) -> Option<i32> {
        self.0.read_millicelsius().await
    }
}

#[embassy_executor::task]
async fn temperature_task(sensor: BoardTempSensor) -> ! {
    ubos_temperature::run_temperature(sensor).await
}

#[embassy_executor::task]
async fn monitor_task(sensor: temperature::HashboardTemperature, board_fan: fan::BoardFan) -> ! {
    ubos_monitor::run_monitor(sensor, board_fan, &MONITOR_CONFIG).await
}

#[embassy_executor::task]
async fn tach_task(hardware: fan::TachHardware) -> ! {
    fan::run_tach(hardware).await
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

#[cfg(feature = "display")]
#[embassy_executor::task]
async fn display_task(mut display: OledDisplay) -> ! {
    ubos_display::display::run(&mut display).await
}

/// Read the `POOL_*` env vars at compile time (each falling through
/// to its default if unset) and produce a typed `PoolConfig`. The
/// result is stored in `POOL_CONFIG_CELL` and borrowed `'static` by
/// the stratum task. Returns the parse error instead of panicking so
/// the boot path can log and skip the stratum task on a bad
/// operator-supplied value (e.g. a malformed `POOL_SV2_PUBKEY`).
/// When `POOL_SV2_PUBKEY` is unset, `DEFAULT_POOL_SV2_PUBKEY` is
/// used so the firmware always validates the Braiins prod key by
/// default. Set `POOL_SV2_TRUST_ANY_KEY=1` to disable peer auth.
fn pool_config_from_env(
) -> Result<ubos_pool::pool_config::PoolConfig, ubos_pool::pool_config::ConfigError> {
    use ubos_pool::pool_config;
    pool_config::parse(
        option_env!("POOL_HOST").unwrap_or(pool_config::DEFAULT_POOL_HOST),
        option_env!("POOL_PORT").unwrap_or(pool_config::DEFAULT_POOL_PORT_STR),
        option_env!("POOL_PROTOCOL").unwrap_or(pool_config::DEFAULT_POOL_PROTOCOL),
        option_env!("POOL_USER").unwrap_or(config::DEFAULT_POOL_USER),
        option_env!("POOL_SV1_PASS"),
        option_env!("POOL_SV2_PUBKEY").unwrap_or(pool_config::DEFAULT_POOL_SV2_PUBKEY),
        option_env!("POOL_SV2_TRUST_ANY_KEY"),
    )
}

#[embassy_executor::task]
#[allow(clippy::too_many_lines)]
async fn stratum_client_task(
    stack: embassy_net::Stack<'static>,
    pool_config: &'static ubos_pool::pool_config::PoolConfig,
) -> ! {
    use defmt::warn;
    use embassy_time::Timer;
    use static_cell::ConstStaticCell;
    use ubos_pool::pool_config::{
        ProtocolConfig, RECONNECT_DELAY, SOCKET_TIMEOUT, STRATUM_TCP_BUFFER_SIZE,
    };

    static TCP_RX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    static TCP_TX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    let rx_buffer = TCP_RX.take();
    let tx_buffer = TCP_TX.take();

    let port = pool_config.port;

    match &pool_config.protocol {
        ProtocolConfig::Sv1 { password } => {
            let sv1 = SV1_CONFIG_CELL.init(stratum_v1::Sv1Config {
                user: pool_config.user.as_str(),
                password: password.as_deref().unwrap_or(""),
            });
            loop {
                stratum_client::record_reconnect_attempt();
                let mut socket = match ubos_net::net::connect_stratum(
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
                        warn!(
                            "stratum-client: connect failed: {}",
                            defmt::Debug2Format(&error)
                        );
                        Timer::after(RECONNECT_DELAY).await;
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
                    warn!(
                        "stratum-client: session error {}",
                        defmt::Debug2Format(&error)
                    );
                }
                Timer::after(RECONNECT_DELAY).await;
            }
        }
        ProtocolConfig::Sv2 { pool_pubkey } => {
            let sv2 = SV2_CONFIG_CELL.init(
                stratum_v2::Sv2Config::new(pool_config.user.as_str(), *pool_pubkey)
                    // `pool_config::parse` already rejects POOL_USER longer
                    // than `MAX_USER_LEN`, which equals `Sv2Config::MAX_USER_LEN`.
                    .expect("BUG: POOL_USER exceeds Sv2Config::MAX_USER_LEN"),
            );
            let buffer = SECP_CTX_BUF.take();
            let ctx = SECP_CTX.init(
                ii_sv2::security::CryptoCtx::preallocated(buffer)
                    // `SECP_CTX_SLOTS` is sized for `AllPreallocated` with
                    // `lowmemory`; undersizing would be caught here at boot.
                    .expect("BUG: SECP_CTX_SLOTS too small for AllPreallocated"),
            );
            loop {
                stratum_client::record_reconnect_attempt();
                let mut socket = match ubos_net::net::connect_stratum(
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
                        warn!(
                            "stratum-client: connect failed: {}",
                            defmt::Debug2Format(&error)
                        );
                        Timer::after(RECONNECT_DELAY).await;
                        continue;
                    }
                };
                let mut state = stratum_client::StratumState::new();
                let mut client = stratum_v2::StratumV2Client::new(
                    sv2,
                    ctx,
                    Sv2BoardRng(esp_hal::rng::Rng::new()),
                );
                if let Err(error) = stratum_client::run_stratum_client(
                    &mut client,
                    &mut socket,
                    &ASIC_SESSION,
                    &mut state,
                )
                .await
                {
                    warn!(
                        "stratum-client: session error {}",
                        defmt::Debug2Format(&error)
                    );
                }
                Timer::after(RECONNECT_DELAY).await;
            }
        }
    }
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
        |spawner| spawner.must_spawn(boot(spawner)),
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
    static RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let hw = config::Board::take(peripherals);

    let timg0 = TimerGroup::new(hw.timg0);
    esp_rtos::start(timg0.timer0);

    // Pin the ubos-miner serial worker to Core 1 so embassy-net on Core 0
    // stays responsive. Mirrors the rp2350/bmm-pico dual-core split.
    // `ASIC_SESSION` uses `CriticalSectionRawMutex`, so the cross-core
    // share is safe without an additional pipe. Core 1's executor uses
    // `IdleAccounting::<1>` to populate `IDLE_TICKS[1]` for cpu-load metrics.
    static APP_EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
    static APP_CORE_STACK: StaticCell<Stack<8192>> = StaticCell::new();
    let software_interrupts = SoftwareInterruptControl::new(hw.sw_interrupt);
    let uart_parts = (hw.uart0, hw.uart_tx, hw.uart_rx);
    esp_rtos::start_second_core(
        hw.cpu_ctrl,
        software_interrupts.software_interrupt0,
        software_interrupts.software_interrupt1,
        APP_CORE_STACK.init(Stack::new()),
        move || {
            let (miner_rx, miner_tx) = {
                let (peripheral, tx_pin, rx_pin) = uart_parts;
                let uart_config = UartConfig::default()
                    // Valid baud rate for UART0; cannot fail.
                    .with_baudrate(ubos_miner::consts::INITIAL_BAUDRATE);
                // Valid UART0 peripheral with default-compatible baud rate; cannot fail.
                let uart = Uart::new(peripheral, uart_config)
                    .expect("BUG: UART0 configuration failed")
                    .with_tx(tx_pin)
                    .with_rx(rx_pin)
                    .into_async();
                let (uart_rx, uart_tx) = uart.split();
                uart_baudrate::split_uart0(uart_rx, uart_tx)
            };
            let executor = APP_EXECUTOR.init(esp_rtos::embassy::Executor::new());
            executor.run_with_callbacks(
                |app_spawner| {
                    app_spawner.must_spawn(mining::serial_task(miner_rx, miner_tx));
                },
                IdleAccounting::<1>::new(),
            );
        },
    );

    let timg1 = TimerGroup::new(hw.timg1);
    spawner.must_spawn(watchdog_task(BoardWatchdog(timg1.wdt)));

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

    let mac_addr = config::Board::MAC_ADDR;
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
    spawner.must_spawn(ethernet_task(runner));

    let rng = Rng::new();
    let seed = u64::from(rng.random()) << 32 | u64::from(rng.random());

    let (stack, runner) = embassy_net::new(
        device,
        embassy_net::Config::dhcpv4(embassy_net::DhcpConfig::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.must_spawn(net_task(runner));

    // Seed the session CSPRNG before rng is moved into the bitcoin-price TLS task.
    // esp-hal Rng produces one u32 per call; 8 calls fill the 32-byte seed.
    let csprng_seed = {
        let mut seed = [0u8; 32];
        for chunk in seed.chunks_exact_mut(4) {
            chunk.copy_from_slice(&rng.random().to_le_bytes());
        }
        seed
    };
    ubos_api::seed_csprng(csprng_seed).await;

    spawner.must_spawn(bitcoin_price_task(stack, TlsRng(rng)));
    spawner.must_spawn(ntp_sync_task(stack));
    spawner.must_spawn(metrics_task(stack));

    static MDNS_HOSTNAME: StaticCell<heapless::String<16>> = StaticCell::new();
    let mdns_hostname =
        ubos_mdns::compose_hostname::<16, { config::Board::MDNS_HOSTNAME_BASE.len() }>(
            config::Board::MDNS_HOSTNAME_BASE,
            config::Board::MAC_ADDR[5],
            &MDNS_HOSTNAME,
        );
    spawner.must_spawn(mdns_task(stack, mdns_hostname));

    spawner.must_spawn(cpu_load_task());
    spawner.must_spawn(miner_metrics_task());
    spawner.must_spawn(stratum_metrics_task());
    spawner.must_spawn(hashrate_metrics_task());
    // No LED blink task: board wiring does not include an accessible LED.
    let tsens = esp32s3_tsens::Tsens::new(hw.sens, esp32s3_tsens::Range::Standard).await;
    spawner.must_spawn(temperature_task(BoardTempSensor(tsens)));

    // SH1106 OLED on SPI3: reset pulse, then async DMA SPI.
    // Binding must stay alive so the reset pin remains driven high.
    #[cfg(feature = "display")]
    let (_rst, display) = {
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
    spawner.must_spawn(display_task(display));

    // BMM100 hashboard PSU on I2C0 (GPIO8 SDA, GPIO9 SCL).
    // All mining tasks run on Core 0 so the I2C bus mutex can stay
    // `NoopRawMutex`: every consumer is on the same executor.
    {
        static I2C0_BUS: StaticCell<psu::Bus> = StaticCell::new();
        // Valid I2C0 peripheral with conventional GPIO8/9 pins; cannot fail.
        let i2c = I2c::new(hw.i2c0, I2cConfig::default())
            .expect("BUG: I2C0 configuration failed")
            .with_sda(hw.i2c_sda)
            .with_scl(hw.i2c_scl)
            .into_async();
        let bus: &'static psu::Bus = I2C0_BUS.init(Mutex::new(i2c));
        let (board_psu, psu_state, terminated) = psu::init(bus);
        spawner.must_spawn(psu::watchdog_task(psu_state, terminated));
        spawner.must_spawn(psu::power_stats_task(board_psu));

        // Hashboard reset line (GPIO4, active-low).
        let reset = mining::make_reset_pin(hw.hb_rst);
        spawner.must_spawn(mining::miner_task(board_psu, reset));

        // Temperature and fan monitor via shared I2C0 bus.
        let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
        let hashboard_temp = temperature::HashboardTemperature::new(temp_device);
        let board_fan = fan::BoardFan::new(hw.ledc, hw.fan_pwm_pin);
        let tach_hardware = fan::TachHardware::new(hw.pcnt, hw.fan_tach_pin);
        spawner.must_spawn(monitor_task(hashboard_temp, board_fan));
        spawner.must_spawn(tach_task(tach_hardware));
    }

    // Plug-detect input; hold the peripheral so it is not reclaimed.
    let _hb_plug = Input::new(hw.hb_plug, InputConfig::default().with_pull(Pull::Up));

    info!("waiting for dhcp");
    let dhcp_config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", dhcp_config.address.address());

    match pool_config_from_env() {
        Ok(config) => {
            let pool_config = POOL_CONFIG_CELL.init(config);
            info!(
                "stratum-client: POOL_HOST={=str} POOL_PORT={=u16} POOL_PROTOCOL={=str} POOL_USER={=str}",
                pool_config.host.as_str(),
                pool_config.port,
                match &pool_config.protocol {
                    ubos_pool::pool_config::ProtocolConfig::Sv1 { .. } => "sv1",
                    ubos_pool::pool_config::ProtocolConfig::Sv2 { .. } => "sv2",
                },
                pool_config.user.as_str(),
            );
            spawner.must_spawn(stratum_client_task(stack, pool_config));
        }
        Err(error) => {
            defmt::error!(
                "stratum-client: POOL_* env vars invalid: {}; not starting stratum task",
                defmt::Debug2Format(&error),
            );
        }
    }

    ubos_api::run(stack, config::Board::NAME).await;
}
