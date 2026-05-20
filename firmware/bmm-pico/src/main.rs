//! Firmware entry point for the BMM Adapter (RP2350 + W5500).
//!
//! Board docs: `../../../docs/OVERVIEW.md`
//! Wiring/build: `../../../docs/SETUP.md`
//! Mining backend: `../../../docs/MMM.md`

#![no_std]
#![no_main]

use defmt::{info, unwrap};
use embassy_executor::Executor;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W5500;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C1, SPI0, UART1};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma, i2c};
use embassy_time::Delay;
#[cfg(feature = "display")]
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::{ConstStaticCell, StaticCell};
#[cfg(feature = "display")]
use ubos_display::sh1106::{Rotation, Sh1106};
use ubos_monitor::{MonitorConfig, BMM101_DEFAULT};
use {defmt_rtt as _, panic_probe as _};

use ubos_net::net;

mod config;
mod fan;
mod mining;
mod psu;
mod temperature;
mod uart_baudrate;

static CORE1_STACK: ConstStaticCell<embassy_rp::multicore::Stack<{ config::Core1::STACK_BYTES }>> =
    ConstStaticCell::new(embassy_rp::multicore::Stack::new());

static CORE1_EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// Software ring buffer size per direction for the core-1 `BufferedUart`.
/// The RP2040/RP2350 HW FIFO is 32 bytes; 1 KiB gives plenty of slack for
/// ETH-ISR jitter at 3 Mbaud (MMM backend).
const UART_RING_BUFFER_SIZE: usize = 1024;

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

static SECP_CTX_BUF: ConstStaticCell<[ii_sv2::security::AlignedType; SECP_CTX_SLOTS]> =
    ConstStaticCell::new([ii_sv2::security::AlignedType::ZERO; SECP_CTX_SLOTS]);

static SECP_CTX: StaticCell<
    ii_sv2::security::CryptoCtx<ii_sv2::security::secp256k1::AllPreallocated<'static>>,
> = StaticCell::new();

struct Sv2BoardRng(embassy_rp::clocks::RoscRng);

impl ii_sv2::security::Rng for Sv2BoardRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        // RoscRng has an inherent `fill_bytes` that routes
        // through the ROSC randombit-per-bit primitive — same
        // entropy source the rand_core impls expose.
        self.0.fill_bytes(dest);
    }
}

static ASIC_SESSION: stratum_client::AsicSession = stratum_client::AsicSession::new();

static MONITOR_CONFIG: MonitorConfig = BMM101_DEFAULT;

defmt::timestamp!("{=u64:iso8601s}", ubos_net::wall_clock_secs());

bind_interrupts!(struct Irqs {
    DMA_IRQ_0 =>
        dma::InterruptHandler<DMA_CH0>,
        dma::InterruptHandler<DMA_CH1>,
        #[cfg(feature = "display")]
        dma::InterruptHandler<DMA_CH2>;
    I2C1_IRQ => i2c::InterruptHandler<I2C1>;
    UART1_IRQ => BufferedInterruptHandler<UART1>;
});

type EthernetRunner = Runner<
    'static,
    W5500,
    ExclusiveDevice<Spi<'static, SPI0, Async>, Output<'static>, Delay>,
    Input<'static>,
    Output<'static>,
>;

struct BoardWatchdog(Watchdog);

impl ubos_core::watchdog::Watchdog for BoardWatchdog {
    fn init(&mut self) {
        self.0.start(config::Watchdog::TIMEOUT);
    }

    fn feed(&mut self) {
        self.0.feed(config::Watchdog::TIMEOUT);
    }
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
async fn watchdog_task(watchdog: BoardWatchdog) -> ! {
    ubos_core::watchdog::run_watchdog(watchdog).await
}

#[embassy_executor::task]
async fn led_blink_task(led: Output<'static>) -> ! {
    ubos_core::led::run_led_blink(led).await
}

#[embassy_executor::task]
async fn bitcoin_price_task(stack: embassy_net::Stack<'static>, rng: RoscRng) -> ! {
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

#[embassy_executor::task]
async fn mdns_task(stack: embassy_net::Stack<'static>, hostname: &'static str) -> ! {
    ubos_net::mdns::run_mdns(stack, hostname, config::Board::NAME, config::Board::MCU).await
}

#[embassy_executor::task]
async fn monitor_task(sensor: temperature::HashboardTemperature, fan: fan::BoardFan) -> ! {
    ubos_monitor::run_monitor(sensor, fan, &MONITOR_CONFIG).await
}

#[embassy_executor::task]
async fn tach_task(hardware: fan::TachHardware) -> ! {
    fan::run_tach(hardware).await
}

#[cfg(feature = "display")]
type OledDisplay = Sh1106<
    ExclusiveDevice<Spi<'static, SPI1, Async>, Output<'static>, embedded_hal_bus::spi::NoDelay>,
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
                let mut client = stratum_v2::StratumV2Client::new(sv2, ctx, Sv2BoardRng(RoscRng));
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

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());
    let hw = config::Board::take(peripherals);
    let mut rng = RoscRng;

    // Pin the MMM serial worker to core 1 so embassy-net on core 0 stays
    // responsive. BufferedUart must be constructed on the core that polls
    // it — `init_buffers` unmasks the UART IRQ on the calling core's NVIC,
    // and the async write path manually pends the same IRQ to shovel
    // the SW TX ringbuffer into the HW FIFO. RP2350's Cortex-M33 has
    // a per-core NVIC, so an unmask on one core and a pend on the
    // other land on different controllers and the FIFO is never
    // written.
    {
        let uart_parts = (hw.uart, hw.uart_tx, hw.uart_rx);
        embassy_rp::multicore::spawn_core1(hw.core1, CORE1_STACK.take(), move || {
            let (mmm_tx, mmm_rx) = {
                static TX_BUF: StaticCell<[u8; UART_RING_BUFFER_SIZE]> = StaticCell::new();
                static RX_BUF: StaticCell<[u8; UART_RING_BUFFER_SIZE]> = StaticCell::new();
                let (peripheral, tx_pin, rx_pin) = uart_parts;
                let uart = BufferedUart::new(
                    peripheral,
                    tx_pin,
                    rx_pin,
                    Irqs,
                    TX_BUF.init([0; UART_RING_BUFFER_SIZE]),
                    RX_BUF.init([0; UART_RING_BUFFER_SIZE]),
                    {
                        let mut config = UartConfig::default();
                        config.baudrate = mmm::consts::INITIAL_BAUDRATE;
                        config
                    },
                );
                uart_baudrate::split_uart1(uart)
            };
            let executor = CORE1_EXECUTOR.init(Executor::new());
            executor.run(|core1_spawner: Spawner| {
                core1_spawner.spawn(unwrap!(mining::serial_task(mmm_rx, mmm_tx)));
            })
        });
    }

    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(Watchdog::new(
        hw.watchdog
    )))));

    let led = Output::new(hw.led, Level::Low);
    spawner.spawn(unwrap!(led_blink_task(led)));

    // BMM100 hashboard PSU on I2C1: shared bus, PSU watchdog, power
    // stats, and the miner bring-up task all run on core 0 so the I2C
    // bus mutex can stay NoopRawMutex.
    {
        use embassy_rp::i2c::{Config as I2cConfig, I2c};
        use embassy_sync::mutex::Mutex;

        static I2C1_BUS: StaticCell<psu::Bus> = StaticCell::new();
        let i2c = I2c::new_async(hw.i2c, hw.i2c_scl, hw.i2c_sda, Irqs, I2cConfig::default());
        let bus: &'static psu::Bus = I2C1_BUS.init(Mutex::new(i2c));
        let (board_psu, psu_state, terminated) = psu::init(bus);
        spawner.spawn(unwrap!(psu::watchdog_task(psu_state, terminated)));
        spawner.spawn(unwrap!(psu::power_stats_task(board_psu)));
        let reset = mining::BoardResetPin::new(Output::new(hw.hb_rst, Level::Low));
        spawner.spawn(unwrap!(mining::miner_task(board_psu, reset)));

        // Temperature + fan monitor via shared I2C bus.
        let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
        let hashboard_temp = temperature::HashboardTemperature::new(temp_device);
        let board_fan = fan::BoardFan::new(hw.fan_pwm_slice, hw.fan_pwm);
        spawner.spawn(unwrap!(monitor_task(hashboard_temp, board_fan)));
    }

    let mut spi_config = SpiConfig::default();
    spi_config.frequency = config::Spi::FREQUENCY;
    let spi = Spi::new(
        hw.spi, hw.sck, hw.mosi, hw.miso, hw.rx_dma, hw.tx_dma, Irqs, spi_config,
    );
    let chip_select = Output::new(hw.cs, Level::High);
    let w5500_int = Input::new(hw.w5500_int, Pull::Up);
    let w5500_reset = Output::new(hw.w5500_reset, Level::High);

    let mac_addr = config::Board::MAC_ADDR;
    static STATE: StaticCell<State<8, 8>> = StaticCell::new();
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
    .expect("BUG: W5500 initialization failed");
    spawner.spawn(unwrap!(ethernet_task(runner)));

    let seed = rng.next_u64();

    static RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        device,
        embassy_net::Config::dhcpv4(embassy_net::DhcpConfig::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(unwrap!(net_task(runner)));

    spawner.spawn(unwrap!(bitcoin_price_task(stack, rng)));
    spawner.spawn(unwrap!(ntp_sync_task(stack)));
    spawner.spawn(unwrap!(metrics_task(stack)));

    static MDNS_HOSTNAME: StaticCell<heapless::String<16>> = StaticCell::new();
    let mdns_hostname =
        ubos_net::mdns::compose_hostname::<16, { config::Board::MDNS_HOSTNAME_BASE.len() }>(
            config::Board::MDNS_HOSTNAME_BASE,
            config::Board::MAC_ADDR[5],
            &MDNS_HOSTNAME,
        );
    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(tach_task(fan::TachHardware {
        slice: hw.fan_rpm_slice,
        pin: hw.fan_rpm,
    })));

    // SH1106 OLED on SPI1: reset pulse, then async TX-only SPI with DMA.
    // Binding must stay alive so the reset pin remains driven high.
    #[cfg(feature = "display")]
    let (_rst, display) = {
        let rst = {
            let mut pin = Output::new(hw.display_rst, Level::Low);
            Timer::after(Duration::from_millis(10)).await;
            pin.set_high();
            Timer::after(Duration::from_millis(10)).await;
            pin
        };
        let mut display_spi_config = SpiConfig::default();
        display_spi_config.frequency = config::DisplaySpi::FREQUENCY;
        let display_spi = Spi::new_txonly(
            hw.display_spi,
            hw.display_sck,
            hw.display_mosi,
            hw.display_tx_dma,
            Irqs,
            display_spi_config,
        );
        let display_cs = Output::new(hw.display_cs, Level::High);
        let display_dc = Output::new(hw.display_dc, Level::Low);
        let spi_device = ExclusiveDevice::new_no_delay(display_spi, display_cs);
        let mut display = Sh1106::new(spi_device, display_dc, Rotation::Normal);
        // Display is reset and SPI is valid; init cannot fail.
        display.init().expect("BUG: SH1106 init failed");
        (rst, display)
    };

    #[cfg(feature = "display")]
    spawner.spawn(unwrap!(display_task(display)));

    info!("waiting for dhcp");
    let config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", config.address.address());

    {
        // Core 1 hosts serial_task (MMM backend); see spawn_core1 above.
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
                spawner.spawn(unwrap!(stratum_client_task(stack, pool_config)));
            }
            Err(error) => {
                defmt::error!(
                    "stratum-client: POOL_* env vars invalid: {}; not starting stratum task",
                    defmt::Debug2Format(&error),
                );
            }
        }
    }

    ubos_metrics::http::run_http_server(stack, config::Board::NAME).await;
}
