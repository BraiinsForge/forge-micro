//! Firmware entry point for the BMM Adapter (RP2350 + W5500). Initialises
//! SPI at 33 MHz, brings the W5500 up via `embassy-net-wiznet`, starts the
//! embassy-net stack with DHCP, and spawns the watchdog, LED blink,
//! ethernet runner, NTP sync, and BTC price tasks before entering
//! the HTTP server loop. Built with `cargo build` from this crate
//! inside `nix develop .#arm`.

#![no_std]
#![no_main]

#[cfg(all(feature = "backend-uart", feature = "backend-cpu"))]
compile_error!(
    "features `backend-uart` and `backend-cpu` are mutually exclusive; \
     pick exactly one with --no-default-features --features \"display,<backend>\""
);
#[cfg(all(feature = "backend-mmm", feature = "backend-uart"))]
compile_error!("features `backend-mmm` and `backend-uart` are mutually exclusive");
#[cfg(all(feature = "backend-mmm", feature = "backend-cpu"))]
compile_error!("features `backend-mmm` and `backend-cpu` are mutually exclusive");

use defmt::{info, unwrap};
use embassy_executor::Executor;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W5500;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Input, Level, Output, Pull};
#[cfg(not(feature = "backend-mmm"))]
use embassy_rp::i2c::{Config as I2cConfig, I2c};
#[cfg(any(feature = "backend-uart", feature = "backend-mmm"))]
use embassy_rp::peripherals::UART1;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C1, SPI0};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
#[cfg(any(feature = "backend-uart", feature = "backend-mmm"))]
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma, i2c};
use embassy_time::Delay;
#[cfg(feature = "display")]
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
#[cfg(feature = "display")]
use eth_examples::sh1106::{Rotation, Sh1106};
use static_cell::{ConstStaticCell, StaticCell};
use ubos_monitor::{MonitorConfig, BMM101_DEFAULT};
use {defmt_rtt as _, panic_probe as _};

use eth_examples::net;

mod config;
mod fan;
#[cfg(feature = "backend-mmm")]
mod mining;
#[cfg(feature = "backend-mmm")]
mod psu;
mod temperature;
#[cfg(feature = "backend-mmm")]
mod uart_baudrate;

static CORE1_STACK: ConstStaticCell<embassy_rp::multicore::Stack<{ config::Core1::STACK_BYTES }>> =
    ConstStaticCell::new(embassy_rp::multicore::Stack::new());

static CORE1_EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// Software ring buffer size per direction for the core-1 `BufferedUart`.
/// The RP2040/RP2350 HW FIFO is 32 bytes; 1 KiB gives plenty of slack for
/// ETH-ISR jitter at 921600 baud (backend-uart) and 3 Mbaud (backend-mmm).
#[cfg(any(feature = "backend-uart", feature = "backend-mmm"))]
const UART_RING_BUFFER_SIZE: usize = 1024;

// `eth_examples::pool_config::parse` runs at boot to copy seven
// compile-time env-var strings into typed fields. The result lives
// for the lifetime of the firmware; held in a `StaticCell` so the
// stratum task can take a `&'static PoolConfig`.
static POOL_CONFIG_CELL: StaticCell<eth_examples::pool_config::PoolConfig> = StaticCell::new();

// `Sv1Config` references &'static str borrowed from the parsed
// `PoolConfig`; allocated at boot once the protocol is known to be
// `Sv1`. Empty when the parsed protocol is `Sv2`.
static SV1_CONFIG_CELL: StaticCell<stratum_v1::Sv1Config> = StaticCell::new();

#[cfg(any(
    feature = "backend-uart",
    feature = "backend-cpu",
    feature = "backend-mmm"
))]
static ASIC_SESSION: stratum_client::AsicSession = stratum_client::AsicSession::new();

static MONITOR_CONFIG: MonitorConfig = BMM101_DEFAULT;

defmt::timestamp!("{=u64:iso8601s}", eth_examples::wall_clock_secs());

bind_interrupts!(struct Irqs {
    DMA_IRQ_0 =>
        dma::InterruptHandler<DMA_CH0>,
        dma::InterruptHandler<DMA_CH1>,
        #[cfg(feature = "display")]
        dma::InterruptHandler<DMA_CH2>;
    I2C1_IRQ => i2c::InterruptHandler<I2C1>;
    #[cfg(any(feature = "backend-uart", feature = "backend-mmm"))]
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

impl eth_examples::watchdog::Watchdog for BoardWatchdog {
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
    eth_examples::watchdog::run_watchdog(watchdog).await
}

#[embassy_executor::task]
async fn led_blink_task(led: Output<'static>) -> ! {
    eth_examples::led::run_led_blink(led).await
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
    eth_examples::metrics::run(stack).await
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
    eth_examples::display::run(&mut display).await
}

#[cfg(feature = "backend-uart")]
#[embassy_executor::task]
async fn asic_uart_task(uart: BufferedUart) -> ! {
    asic_uart_peer::run_asic_uart(&ASIC_SESSION, uart).await
}

/// Read the six `POOL_*` env vars at compile time (each falling
/// through to its default if unset) and produce a typed
/// `PoolConfig`. The result is stored in `POOL_CONFIG_CELL` and
/// borrowed `'static` by the stratum task.
#[cfg(any(
    feature = "backend-uart",
    feature = "backend-cpu",
    feature = "backend-mmm"
))]
fn pool_config_from_env() -> eth_examples::pool_config::PoolConfig {
    use eth_examples::pool_config;
    pool_config::parse(
        option_env!("POOL_HOST").unwrap_or(pool_config::DEFAULT_POOL_HOST),
        option_env!("POOL_PORT").unwrap_or(pool_config::DEFAULT_POOL_PORT_STR),
        option_env!("POOL_PROTOCOL").unwrap_or(pool_config::DEFAULT_POOL_PROTOCOL),
        option_env!("POOL_USER").unwrap_or(config::DEFAULT_POOL_USER),
        option_env!("POOL_SV1_PASS"),
        option_env!("POOL_SV2_PUBKEY"),
    )
    // All inputs have validated defaults, so parse cannot fail.
    .expect("BUG: POOL_* env vars failed to parse")
}

#[cfg(any(
    feature = "backend-uart",
    feature = "backend-cpu",
    feature = "backend-mmm"
))]
#[embassy_executor::task]
async fn stratum_client_task(
    stack: embassy_net::Stack<'static>,
    pool_config: &'static eth_examples::pool_config::PoolConfig,
) -> ! {
    use defmt::warn;
    use embassy_time::Timer;
    use eth_examples::pool_config::{
        ProtocolConfig, RECONNECT_DELAY, SOCKET_TIMEOUT, STRATUM_TCP_BUFFER_SIZE,
    };
    static TCP_RX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    static TCP_TX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    let rx_buffer = TCP_RX.take();
    let tx_buffer = TCP_TX.take();

    let port = pool_config.port;

    // These boards have no SV2 client; if the user sets
    // POOL_PROTOCOL=sv2, fall back to V1 with a warning.
    let sv1 = match &pool_config.protocol {
        ProtocolConfig::Sv1 { password } => SV1_CONFIG_CELL.init(stratum_v1::Sv1Config {
            user: pool_config.user.as_str(),
            password: password.as_deref().unwrap_or(""),
        }),
        ProtocolConfig::Sv2 { .. } => {
            warn!(
                "stratum-client: POOL_PROTOCOL=sv2 but no SV2 client on this board; \
                 falling back to V1 with empty password"
            );
            SV1_CONFIG_CELL.init(stratum_v1::Sv1Config {
                user: pool_config.user.as_str(),
                password: "",
            })
        }
    };

    loop {
        let mut socket = match eth_examples::net::connect_stratum(
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

        let mut client = stratum_v1::StratumV1Client::new(sv1);
        let mut state = stratum_client::StratumState::new();
        if let Err(error) =
            stratum_client::run_stratum_client(&mut client, &mut socket, &ASIC_SESSION, &mut state)
                .await
        {
            warn!(
                "stratum-client: session error {}",
                defmt::Debug2Format(&error)
            );
        }
    }
}

#[cfg(feature = "backend-cpu")]
#[embassy_executor::task]
async fn asic_cpu_task() -> ! {
    asic_cpu_mock::run_asic_cpu(&ASIC_SESSION).await
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());
    let hw = config::Board::take(peripherals);
    let mut rng = RoscRng;

    // Pin the worker to core 1 so embassy-net on core 0 stays
    // responsive.
    {
        #[cfg(any(feature = "backend-uart", feature = "backend-mmm"))]
        let uart_parts = (hw.uart, hw.uart_tx, hw.uart_rx);
        embassy_rp::multicore::spawn_core1(hw.core1, CORE1_STACK.take(), move || {
            // BufferedUart must be constructed on the core that polls it.
            // `init_buffers` unmasks the UART IRQ on the calling core's NVIC,
            // and the async write path manually pends the same IRQ to shovel
            // the SW TX ringbuffer into the HW FIFO. RP2350's Cortex-M33 has
            // a per-core NVIC, so an unmask on one core and a pend on the
            // other land on different controllers and the FIFO is never
            // written.
            #[cfg(feature = "backend-uart")]
            let uart = {
                static TX_BUF: StaticCell<[u8; UART_RING_BUFFER_SIZE]> = StaticCell::new();
                static RX_BUF: StaticCell<[u8; UART_RING_BUFFER_SIZE]> = StaticCell::new();
                let (peripheral, tx_pin, rx_pin) = uart_parts;
                BufferedUart::new(
                    peripheral,
                    tx_pin,
                    rx_pin,
                    Irqs,
                    TX_BUF.init([0; UART_RING_BUFFER_SIZE]),
                    RX_BUF.init([0; UART_RING_BUFFER_SIZE]),
                    {
                        let mut config = UartConfig::default();
                        config.baudrate = asic_uart_peer::BAUD_RATE;
                        config
                    },
                )
            };
            #[cfg(feature = "backend-mmm")]
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
                #[cfg(feature = "backend-cpu")]
                core1_spawner.spawn(unwrap!(asic_cpu_task()));
                #[cfg(feature = "backend-uart")]
                core1_spawner.spawn(unwrap!(asic_uart_task(uart)));
                #[cfg(feature = "backend-mmm")]
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
    #[cfg(feature = "backend-mmm")]
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

    static RESOURCES: StaticCell<StackResources<7>> = StaticCell::new();
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

    #[cfg(not(feature = "backend-mmm"))]
    {
        let i2c = I2c::new_async(hw.i2c, hw.i2c_scl, hw.i2c_sda, Irqs, I2cConfig::default());
        let hashboard_temp = temperature::HashboardTemperature::new(i2c);
        let board_fan = fan::BoardFan::new(hw.fan_pwm_slice, hw.fan_pwm);
        spawner.spawn(unwrap!(monitor_task(hashboard_temp, board_fan)));
    }

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
        let mut display = Sh1106::new(spi_device, display_dc, Rotation::Rotate180);
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
        // Core 1 hosts asic_uart_task (backend-uart), asic_cpu_task
        // (backend-cpu), or serial_task (backend-mmm); see spawn_core1 above.
        let pool_config = POOL_CONFIG_CELL.init(pool_config_from_env());
        info!(
            "stratum-client: POOL_HOST={=str} POOL_PORT={=u16} POOL_PROTOCOL={=str} POOL_USER={=str}",
            pool_config.host.as_str(),
            pool_config.port,
            match &pool_config.protocol {
                eth_examples::pool_config::ProtocolConfig::Sv1 { .. } => "sv1",
                eth_examples::pool_config::ProtocolConfig::Sv2 { .. } => "sv2",
            },
            pool_config.user.as_str(),
        );
        spawner.spawn(unwrap!(stratum_client_task(stack, pool_config)));
    }

    net::run_http_server(stack, config::Board::NAME).await;
}
