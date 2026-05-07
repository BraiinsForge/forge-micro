//! Firmware entry point for the W6100-EVB-Pico-2. Initialises SPI at
//! 33 MHz, brings the W6100 up via `embassy-net-wiznet`, starts the
//! embassy-net stack with DHCP, and spawns the watchdog, LED blink,
//! ethernet runner, NTP sync, and BTC price tasks before entering
//! the HTTP server loop. Built with `cargo build` from this crate
//! inside `nix develop .#arm`.

#![no_std]
#![no_main]

#[cfg(any(
    all(feature = "backend-uart", feature = "backend-cpu"),
    all(feature = "backend-uart", feature = "backend-stratum-client"),
    all(feature = "backend-cpu", feature = "backend-stratum-client"),
))]
compile_error!(
    "features `backend-uart`, `backend-cpu`, and `backend-stratum-client` \
     are mutually exclusive; pick exactly one with \
     --no-default-features --features \"display,<backend>\""
);

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W6100;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::adc::{Adc, Async as AdcAsync, Channel as AdcChannel, Config as AdcConfig};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Input, Level, Output, Pull};
#[cfg(feature = "backend-uart")]
use embassy_rp::peripherals::UART1;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, SPI0};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
#[cfg(feature = "backend-uart")]
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{adc, bind_interrupts, dma};
use embassy_time::Delay;
#[cfg(feature = "display")]
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
#[cfg(feature = "display")]
use eth_examples::sh1106::{Rotation, Sh1106};
use static_cell::{ConstStaticCell, StaticCell};
use {defmt_rtt as _, panic_probe as _};

use eth_examples::net;
#[cfg(any(feature = "backend-cpu", feature = "backend-uart"))]
use ubos_backend::PoolConfig;

mod config;

#[cfg(any(feature = "backend-cpu", feature = "backend-uart"))]
static POOL_CONFIG: PoolConfig = PoolConfig {
    host_ip: config::Pool::HOST_IP,
    port: config::Pool::PORT,
    user: config::Pool::USER,
    password: config::Pool::PASS,
    reconnect_delay: config::Pool::RECONNECT_DELAY,
};

#[cfg(all(feature = "backend-stratum-client", not(feature = "protocol-sv2")))]
static SV1_CONFIG: stratum_v1::Sv1Config = stratum_v1::Sv1Config {
    user: config::Pool::USER,
    password: config::Pool::PASS,
};

#[cfg(feature = "protocol-sv2")]
static SV2_CONFIG: stratum_v2::Sv2Config = stratum_v2::Sv2Config {
    user: config::Pool::USER,
    pool_pubkey: config::Pool::POOL_PUBKEY,
};

// `secp256k1::AllPreallocated<'static>` precomputation buffer.
// `lowmemory` brings the verify table down to ~16 KB; the slot
// count is sized conservatively (covers signing + verification
// per `secp256k1_context_preallocated_size(All)` at the lowmemory
// flags). `CryptoCtx::preallocated` returns
// `Error::NotEnoughMemory` at runtime if undersized.
#[cfg(feature = "protocol-sv2")]
const SECP_CTX_SLOTS: usize = 4096;

#[cfg(feature = "protocol-sv2")]
static SECP_CTX_BUF: static_cell::ConstStaticCell<[ii_sv2::security::AlignedType; SECP_CTX_SLOTS]> =
    static_cell::ConstStaticCell::new([ii_sv2::security::AlignedType::ZERO; SECP_CTX_SLOTS]);

#[cfg(feature = "protocol-sv2")]
static SECP_CTX: static_cell::StaticCell<
    ii_sv2::security::CryptoCtx<ii_sv2::security::secp256k1::AllPreallocated<'static>>,
> = static_cell::StaticCell::new();

#[cfg(feature = "protocol-sv2")]
struct Sv2BoardRng(embassy_rp::clocks::RoscRng);

#[cfg(feature = "protocol-sv2")]
impl ii_sv2::security::Rng for Sv2BoardRng {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        // RoscRng has an inherent `fill_bytes` that routes
        // through the ROSC randombit-per-bit primitive — same
        // entropy source the rand_core impls expose.
        self.0.fill_bytes(dest);
    }
}

#[cfg(feature = "backend-stratum-client")]
static ASIC_SESSION: stratum_client::AsicSession = stratum_client::AsicSession::new();

defmt::timestamp!("{=u64:iso8601s}", eth_examples::wall_clock_secs());

bind_interrupts!(struct Irqs {
    DMA_IRQ_0 =>
        dma::InterruptHandler<DMA_CH0>,
        dma::InterruptHandler<DMA_CH1>,
        #[cfg(feature = "display")]
        dma::InterruptHandler<DMA_CH2>;
    ADC_IRQ_FIFO => adc::InterruptHandler;
    #[cfg(feature = "backend-uart")]
    UART1_IRQ => BufferedInterruptHandler<UART1>;
});

type EthernetRunner = Runner<
    'static,
    W6100,
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

struct BoardTempSensor {
    adc: Adc<'static, AdcAsync>,
    channel: AdcChannel<'static>,
}

impl eth_examples::temperature::TemperatureSensor for BoardTempSensor {
    async fn read_millicelsius(&mut self) -> Option<i32> {
        // RP2350 datasheet 12.4.6: same bipolar-diode sensor as RP2040 with
        // the same documented 0.706 V / 1.721 mV per °C calibration, so the
        // conversion math is identical.
        const VREF_UV: i64 = 3_300_000;
        const ADC_FULL_SCALE: i64 = 4095;
        const VSENSE_AT_27C_UV: i64 = 706_000;
        const UV_PER_C: i64 = 1_721;
        const CALIBRATION_MC: i64 = 27_000;

        let raw = self.adc.read(&mut self.channel).await.ok()?;
        let vsense_uv = i64::from(raw) * VREF_UV / ADC_FULL_SCALE;
        let millicelsius = CALIBRATION_MC - (vsense_uv - VSENSE_AT_27C_UV) * 1000 / UV_PER_C;
        i32::try_from(millicelsius).ok()
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
async fn temperature_task(sensor: BoardTempSensor) -> ! {
    eth_examples::temperature::run_temperature(sensor).await
}

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    eth_examples::cpu_load::run_cpu_load::<2>().await
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
async fn uart_worker_task(uart: BufferedUart) -> ! {
    let asic = eth_examples::uart_asic::UartAsic::new(uart);
    ubos_backend::run_worker(asic).await
}

#[cfg(any(feature = "backend-cpu", feature = "backend-uart"))]
#[embassy_executor::task]
async fn stratum_task(stack: embassy_net::Stack<'static>, config: &'static PoolConfig) -> ! {
    ubos_backend::run_stratum(stack, config).await
}

#[cfg(any(feature = "backend-cpu", feature = "backend-uart"))]
#[embassy_executor::task]
async fn ntime_roller_task() -> ! {
    ubos_backend::run_ntime_roller().await
}

#[cfg(feature = "backend-cpu")]
#[embassy_executor::task]
async fn cpu_worker_task() -> ! {
    ubos_backend::run_cpu_worker().await
}

#[cfg(feature = "backend-cpu")]
#[embassy_executor::task]
async fn cpu_miner_task() -> ! {
    cpu_miner::run_cpu_miner().await
}

#[cfg(feature = "backend-stratum-client")]
#[embassy_executor::task]
async fn stratum_client_task(stack: embassy_net::Stack<'static>) -> ! {
    use defmt::warn;
    use embassy_net::{tcp::TcpSocket, IpAddress};
    use embassy_time::Timer;
    use static_cell::ConstStaticCell;

    const TCP_BUFFER_SIZE: usize = 2048;
    static TCP_RX: ConstStaticCell<[u8; TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; TCP_BUFFER_SIZE]);
    static TCP_TX: ConstStaticCell<[u8; TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; TCP_BUFFER_SIZE]);
    let rx_buffer = TCP_RX.take();
    let tx_buffer = TCP_TX.take();

    let [a, b, c, d] = config::Pool::HOST_IP;
    let address = IpAddress::v4(a, b, c, d);

    // V2 adds a one-shot, alloc-free secp256k1 context backed by
    // a static AlignedType buffer. `lowmemory` keeps the verify
    // table at ~16 KB so RP2350 SRAM is not exhausted.
    #[cfg(feature = "protocol-sv2")]
    let secp_ctx: &'static ii_sv2::security::CryptoCtx<
        ii_sv2::security::secp256k1::AllPreallocated<'static>,
    > = {
        let buffer = SECP_CTX_BUF.take();
        let ctx = ii_sv2::security::CryptoCtx::preallocated(buffer)
            .expect("BUG: SECP_CTX_SLOTS too small for AllPreallocated");
        SECP_CTX.init(ctx)
    };

    loop {
        let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        socket.set_timeout(Some(embassy_time::Duration::from_secs(120)));
        if let Err(error) = socket.connect((address, config::Pool::PORT)).await {
            warn!(
                "stratum-client: connect failed {}",
                defmt::Debug2Format(&error)
            );
            Timer::after(config::Pool::RECONNECT_DELAY).await;
            continue;
        }
        info!(
            "stratum-client: connected to {}.{}.{}.{}:{}",
            a,
            b,
            c,
            d,
            config::Pool::PORT
        );

        #[cfg(not(feature = "protocol-sv2"))]
        let mut client = stratum_v1::StratumV1Client::new(&SV1_CONFIG);
        #[cfg(feature = "protocol-sv2")]
        let mut client =
            stratum_v2::StratumV2Client::new(&SV2_CONFIG, secp_ctx, Sv2BoardRng(RoscRng));

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
            Err(error) => warn!(
                "stratum-client: session error {}",
                defmt::Debug2Format(&error)
            ),
        }
        Timer::after(config::Pool::RECONNECT_DELAY).await;
    }
}

#[cfg(feature = "backend-stratum-client")]
#[embassy_executor::task]
async fn asic_cpu_task() -> ! {
    asic_cpu_mock::run_asic_cpu(&ASIC_SESSION).await
}

#[cortex_m_rt::entry]
fn main() -> ! {
    eth_examples::cpu_load::run_cortex_m_executor::<0, _>(|spawner| {
        spawner.spawn(unwrap!(boot(spawner)));
    })
}

#[embassy_executor::task]
#[allow(clippy::too_many_lines, clippy::items_after_statements)]
async fn boot(spawner: Spawner) {
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());
    let hw = config::Board::take(peripherals);
    let mut rng = RoscRng;

    // Pin the worker to core 1 so embassy-net on core 0 stays
    // responsive (cpu_miner hashes; uart_worker blocks on recv_share).
    // Core 1 runs the same wfe-bracketed executor as core 0 so its idle
    // ticks land in `IDLE_TICKS[1]`.
    static CORE1_STACK: ConstStaticCell<
        embassy_rp::multicore::Stack<{ config::Core1::STACK_BYTES }>,
    > = ConstStaticCell::new(embassy_rp::multicore::Stack::new());
    #[cfg(feature = "backend-uart")]
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
            static TX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
            static RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
            let (peripheral, tx_pin, rx_pin) = uart_parts;
            BufferedUart::new(
                peripheral,
                tx_pin,
                rx_pin,
                Irqs,
                TX_BUF.init([0; 1024]),
                RX_BUF.init([0; 1024]),
                {
                    let mut config = UartConfig::default();
                    config.baudrate = eth_examples::uart_asic::BAUD_RATE;
                    config
                },
            )
        };
        eth_examples::cpu_load::run_cortex_m_executor::<1, _>(|core1_spawner| {
            #[cfg(feature = "backend-cpu")]
            {
                core1_spawner.spawn(unwrap!(cpu_worker_task()));
                core1_spawner.spawn(unwrap!(cpu_miner_task()));
            }
            #[cfg(feature = "backend-uart")]
            core1_spawner.spawn(unwrap!(uart_worker_task(uart)));
            #[cfg(feature = "backend-stratum-client")]
            core1_spawner.spawn(unwrap!(asic_cpu_task()));
            #[cfg(not(any(
                feature = "backend-cpu",
                feature = "backend-uart",
                feature = "backend-stratum-client",
            )))]
            let _ = core1_spawner;
        });
    });

    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(Watchdog::new(
        hw.watchdog
    )))));

    let led = Output::new(hw.led, Level::Low);
    spawner.spawn(unwrap!(led_blink_task(led)));

    let mut spi_config = SpiConfig::default();
    spi_config.frequency = config::Spi::FREQUENCY;
    let spi = Spi::new(
        hw.spi, hw.sck, hw.mosi, hw.miso, hw.rx_dma, hw.tx_dma, Irqs, spi_config,
    );
    let chip_select = Output::new(hw.cs, Level::High);
    let w6100_int = Input::new(hw.w6100_int, Pull::Up);
    let w6100_reset = Output::new(hw.w6100_reset, Level::High);

    let mac_addr = config::Board::MAC_ADDR;
    static STATE: StaticCell<State<8, 8>> = StaticCell::new();
    let state = STATE.init(State::<8, 8>::new());
    let (device, runner) = embassy_net_wiznet::new(
        mac_addr,
        state,
        ExclusiveDevice::new(spi, chip_select, Delay),
        w6100_int,
        w6100_reset,
    )
    .await
    // Initialization succeeds when the chip is wired correctly and responds on SPI.
    .expect("BUG: W6100 initialization failed");
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
    spawner.spawn(unwrap!(cpu_load_task()));

    let adc = Adc::new(hw.adc, Irqs, AdcConfig::default());
    let temp_channel = AdcChannel::new_temp_sensor(hw.adc_temp_sensor);
    spawner.spawn(unwrap!(temperature_task(BoardTempSensor {
        adc,
        channel: temp_channel,
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

    #[cfg(any(feature = "backend-cpu", feature = "backend-uart"))]
    {
        // Core 1 hosts cpu_worker_task + cpu_miner_task (backend-cpu) or
        // uart_worker_task (backend-uart); see spawn_core1 above.
        spawner.spawn(unwrap!(stratum_task(stack, &POOL_CONFIG)));
        spawner.spawn(unwrap!(ntime_roller_task()));
    }
    #[cfg(feature = "backend-stratum-client")]
    {
        // Core 1 hosts asic_cpu_task; see spawn_core1 above.
        spawner.spawn(unwrap!(stratum_client_task(stack)));
    }

    net::run_http_server(stack, config::Board::NAME).await;
}
