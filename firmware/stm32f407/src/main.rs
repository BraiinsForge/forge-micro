//! Firmware entry point for the `DevEBox` STM32F407VGT6 board wired to
//! a WT32-ETH01 RMII module. Enables the module's 50 MHz oscillator,
//! starts the built-in MAC with an auto-probed LAN8720 PHY, runs the
//! embassy-net stack with DHCP, and spawns the watchdog, LED blink,
//! NTP, and BTC tasks before entering the HTTP server loop. Built
//! with `cargo build` from this crate inside `nix develop .#arm`.

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
use embassy_stm32::adc::{Adc, AdcChannel, Exten, RingBufferedAdc, SampleTime};
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue, Sma};
use embassy_stm32::gpio::{Level, Output, Speed};
#[cfg(feature = "backend-uart")]
use embassy_stm32::mode::Async;
#[cfg(feature = "display")]
use embassy_stm32::peripherals::DMA1_CH5;
use embassy_stm32::peripherals::{ADC1, DMA2_CH0, ETH, ETH_SMA, IWDG};
#[cfg(feature = "backend-uart")]
use embassy_stm32::peripherals::{DMA1_CH1, DMA1_CH3};
use embassy_stm32::rng::Rng;
#[cfg(feature = "display")]
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::timer::low_level::{MasterMode, RoundTo, Timer as LowLevelTimer};
use embassy_stm32::triggers::TIM3_TRGO;
#[cfg(feature = "backend-uart")]
use embassy_stm32::usart::{
    Config as UartConfig, InterruptHandler as UsartInterruptHandler, RingBufferedUartRx, Uart,
    UartTx,
};
use embassy_stm32::wdg::IndependentWatchdog;
use embassy_stm32::{bind_interrupts, dma, eth, peripherals, rng};
#[cfg(feature = "display")]
use embassy_time::{Duration, Timer};
#[cfg(feature = "display")]
use embedded_hal_bus::spi::ExclusiveDevice;
#[cfg(feature = "display")]
use eth_examples::sh1106::{Rotation, Sh1106};
use static_cell::StaticCell;
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

#[cfg(feature = "backend-stratum-client")]
static SV1_CONFIG: stratum_v1::Sv1Config = stratum_v1::Sv1Config {
    user: config::Pool::USER,
    password: config::Pool::PASS,
};

#[cfg(feature = "backend-stratum-client")]
static ASIC_SESSION: stratum_client::AsicSession = stratum_client::AsicSession::new();

defmt::timestamp!("{=u64:iso8601s}", eth_examples::wall_clock_secs());

bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    RNG => rng::InterruptHandler<peripherals::RNG>;
    DMA2_STREAM0 => dma::InterruptHandler<DMA2_CH0>;
    #[cfg(feature = "backend-uart")]
    USART3 => UsartInterruptHandler<peripherals::USART3>;
    #[cfg(feature = "backend-uart")]
    DMA1_STREAM1 => dma::InterruptHandler<DMA1_CH1>;
    #[cfg(feature = "backend-uart")]
    DMA1_STREAM3 => dma::InterruptHandler<DMA1_CH3>;
    #[cfg(feature = "display")]
    DMA1_STREAM5 => dma::InterruptHandler<DMA1_CH5>;
});

type Device = Ethernet<'static, ETH, GenericPhy<Sma<'static, ETH_SMA>>>;

struct BoardWatchdog(IndependentWatchdog<'static, IWDG>);

impl eth_examples::watchdog::Watchdog for BoardWatchdog {
    fn init(&mut self) {
        self.0.unleash();
    }

    fn feed(&mut self) {
        self.0.pet();
    }
}

/// TIM3 runs free as a TRGO source for the ADC. The timer peripheral is
/// moved into this wrapper to keep its RCC clock alive for as long as the
/// sensor lives. The field is never read.
struct BoardTempSensor {
    ring: RingBufferedAdc<'static, ADC1>,
    _timer: LowLevelTimer<'static, embassy_stm32::peripherals::TIM3>,
}

impl eth_examples::temperature::TemperatureSensor for BoardTempSensor {
    async fn read_millicelsius(&mut self) -> Option<i32> {
        // STM32F407 datasheet table 78: V_sense ≈ 0.76 V at 25 °C with an
        // average slope of 2.5 mV/°C, 12-bit ADC across VREF = 3.3 V.
        // T_mC = 25000 + (Vsense_uV - 760000) * 1000 / 2500.
        // i64 keeps raw * VREF_UV from overflowing i32.
        const VREF_UV: i64 = 3_300_000;
        const ADC_FULL_SCALE: i64 = 4095;
        const V25_UV: i64 = 760_000;
        const SLOPE_UV_PER_C: i64 = 2_500;
        const CALIBRATION_MC: i64 = 25_000;

        let mut samples = [0u16; 1];
        self.ring.read(&mut samples).await.ok()?;
        let vsense_uv = i64::from(samples[0]) * VREF_UV / ADC_FULL_SCALE;
        let millicelsius = CALIBRATION_MC + (vsense_uv - V25_UV) * 1000 / SLOPE_UV_PER_C;
        i32::try_from(millicelsius).ok()
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device>) -> ! {
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
async fn bitcoin_price_task(
    stack: embassy_net::Stack<'static>,
    rng: Rng<'static, peripherals::RNG>,
) -> ! {
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
    eth_examples::cpu_load::run_cpu_load::<1>().await
}

#[cfg(feature = "display")]
type OledDisplay = Sh1106<
    ExclusiveDevice<
        Spi<'static, embassy_stm32::mode::Async, embassy_stm32::spi::mode::Master>,
        Output<'static>,
        embedded_hal_bus::spi::NoDelay,
    >,
    Output<'static>,
>;

#[cfg(feature = "display")]
#[embassy_executor::task]
async fn display_task(mut display: OledDisplay) -> ! {
    eth_examples::display::run(&mut display).await
}

#[cfg(feature = "backend-uart")]
#[embassy_executor::task]
async fn uart_worker_task(
    uart: eth_examples::uart_asic::SplitUart<RingBufferedUartRx<'static>, UartTx<'static, Async>>,
) -> ! {
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
        let mut client = stratum_v1::StratumV1Client::new(&SV1_CONFIG);
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
    let peripherals = embassy_stm32::init(config::stm32_config());
    let hw = config::Board::take(peripherals);

    let watchdog = IndependentWatchdog::new(hw.iwdg, config::Watchdog::TIMEOUT_US);
    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(watchdog))));

    let led = Output::new(hw.led, Level::Low, Speed::Low);
    spawner.spawn(unwrap!(led_blink_task(led)));

    // Hardware RNG for DHCP and TCP sequence numbers.
    let mut rng = Rng::new(hw.rng, Irqs);
    let mut seed_bytes = [0u8; 8];
    // RNG peripheral is initialized and clocked via PLL; read cannot fail here.
    rng.async_fill_bytes(&mut seed_bytes)
        .await
        .expect("BUG: RNG fill failed");
    let seed = u64::from_le_bytes(seed_bytes);

    // Enable the 50 MHz oscillator on the WT32-ETH01 board.
    // Binding must remain alive for the oscillator to stay enabled.
    let _osc_enable = Output::new(hw.osc_enable, Level::High, Speed::Low);

    // RMII pin mapping for DevEBox STM32F407VGT6 + WT32-ETH01 LAN8720 board.
    static PACKETS: StaticCell<PacketQueue<4, 4>> = StaticCell::new();
    let device = Ethernet::new(
        PACKETS.init(PacketQueue::<4, 4>::new()),
        hw.eth,
        Irqs,
        hw.ref_clk,
        hw.crs_dv,
        hw.rxd0,
        hw.rxd1,
        hw.txd0,
        hw.txd1,
        hw.tx_en,
        config::Board::MAC_ADDR,
        hw.eth_sma,
        hw.mdio,
        hw.mdc,
    );

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

    // TIM3 emits TRGO once per second via master-mode UPDATE; the ADC runs
    // single-shot conversions on each rising edge, so the DMA ring buffer
    // receives exactly one temperature sample per trigger.
    let temp_timer = LowLevelTimer::new(hw.temp_trigger_timer);
    temp_timer.set_frequency(Hertz(1), RoundTo::Slower);
    temp_timer.set_master_mode(MasterMode::UPDATE);
    temp_timer.start();

    let adc = Adc::new(hw.adc);
    let temp_channel = adc.enable_temperature();
    static TEMP_RING_BUF: StaticCell<[u16; 2]> = StaticCell::new();
    let temp_ring = adc.into_ring_buffered(
        hw.adc_dma,
        TEMP_RING_BUF.init([0u16; 2]),
        Irqs,
        [(temp_channel.degrade_adc(), SampleTime::CYCLES480)].into_iter(),
        TIM3_TRGO,
        Exten::RISING_EDGE,
    );
    spawner.spawn(unwrap!(temperature_task(BoardTempSensor {
        ring: temp_ring,
        _timer: temp_timer,
    })));

    #[cfg(feature = "backend-uart")]
    {
        static DMA_RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        let uart_config = {
            let mut config = UartConfig::default();
            config.baudrate = eth_examples::uart_asic::BAUD_RATE;
            config
        };
        // Valid USART3 peripheral with type-checked DMA channels; cannot fail.
        let uart = Uart::new(
            hw.usart3,
            hw.usart3_rx,
            hw.usart3_tx,
            hw.usart3_tx_dma,
            hw.usart3_rx_dma,
            Irqs,
            uart_config,
        )
        .expect("BUG: USART3 configuration failed");
        let (uart_tx, uart_rx) = uart.split();
        let ring_rx = uart_rx.into_ring_buffered(DMA_RX_BUF.init([0; 1024]));
        let split_uart = eth_examples::uart_asic::SplitUart {
            rx: ring_rx,
            tx: uart_tx,
        };
        spawner.spawn(unwrap!(uart_worker_task(split_uart)));
    }

    // SH1106 OLED on SPI3: reset pulse, then async TX-only SPI with DMA.
    // Binding must stay alive so the reset pin remains driven high.
    #[cfg(feature = "display")]
    let (_rst, display) = {
        let rst = {
            let mut pin = Output::new(hw.display_rst, Level::Low, Speed::Low);
            Timer::after(Duration::from_millis(10)).await;
            pin.set_high();
            Timer::after(Duration::from_millis(10)).await;
            pin
        };
        let mut spi_config = spi::Config::default();
        spi_config.frequency = config::DisplaySpi::FREQUENCY;
        let spi = Spi::new_txonly(
            hw.display_spi,
            hw.display_sck,
            hw.display_mosi,
            hw.display_tx_dma,
            Irqs,
            spi_config,
        );
        let cs = Output::new(hw.display_cs, Level::High, Speed::Low);
        let dc = Output::new(hw.display_dc, Level::Low, Speed::Low);
        let spi_device = ExclusiveDevice::new_no_delay(spi, cs);
        let mut display = Sh1106::new(spi_device, dc, Rotation::Normal);
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
        spawner.spawn(unwrap!(stratum_task(stack, &POOL_CONFIG)));
        spawner.spawn(unwrap!(ntime_roller_task()));
    }
    #[cfg(feature = "backend-cpu")]
    {
        spawner.spawn(unwrap!(cpu_worker_task()));
        spawner.spawn(unwrap!(cpu_miner_task()));
    }
    #[cfg(feature = "backend-stratum-client")]
    {
        spawner.spawn(unwrap!(stratum_client_task(stack)));
        spawner.spawn(unwrap!(asic_cpu_task()));
    }

    net::run_http_server(stack, config::Board::NAME).await;
}
