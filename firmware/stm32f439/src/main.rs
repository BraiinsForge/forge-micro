//! Firmware entry point for the Nucleo-F439ZI board using its
//! onboard LAN8742A PHY and RJ45 over RMII. Starts the built-in
//! MAC with an auto-probed PHY, runs the embassy-net stack with
//! DHCP, and spawns the watchdog, LED blink, NTP, and BTC tasks
//! before entering the HTTP server loop. Built with `cargo build`
//! from this crate inside `nix develop .#arm`.

#![no_std]
#![no_main]

#[cfg(all(feature = "dummy-asic", feature = "cpu-mining"))]
compile_error!(
    "features `dummy-asic` and `cpu-mining` are mutually exclusive; \
     enable cpu-mining with --no-default-features --features \"display,cpu-mining\""
);

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::adc::{Adc, AdcChannel, Exten, RingBufferedAdc, SampleTime};
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue, Sma};
use embassy_stm32::gpio::{Level, Output, Speed};
#[cfg(feature = "dummy-asic")]
use embassy_stm32::hash;
#[cfg(feature = "display")]
use embassy_stm32::peripherals::DMA2_CH3;
use embassy_stm32::peripherals::{ADC1, DMA2_CH0, ETH, ETH_SMA, IWDG};
use embassy_stm32::rng::Rng;
#[cfg(feature = "display")]
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::timer::low_level::{MasterMode, RoundTo, Timer as LowLevelTimer};
use embassy_stm32::triggers::TIM3_TRGO;
#[cfg(feature = "dummy-asic")]
use embassy_stm32::usart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_stm32::wdg::IndependentWatchdog;
#[cfg(feature = "dummy-asic")]
use embassy_stm32::Peri;
use embassy_stm32::{bind_interrupts, dma, eth, peripherals, rng};
#[cfg(any(feature = "cpu-mining", feature = "display"))]
use embassy_time::Duration;
#[cfg(feature = "cpu-mining")]
use embassy_time::Ticker;
#[cfg(feature = "display")]
use embassy_time::Timer;
#[cfg(feature = "display")]
use embedded_hal_bus::spi::ExclusiveDevice;
#[cfg(feature = "cpu-mining")]
use eth_examples::metrics::{Metric, Value};
#[cfg(feature = "cpu-mining")]
use portable_atomic::Ordering;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use eth_examples::net;
#[cfg(feature = "display")]
use eth_examples::sh1106::Sh1106;
#[cfg(feature = "cpu-mining")]
use ubos_backend::PoolConfig;

mod config;

#[cfg(feature = "cpu-mining")]
static POOL_CONFIG: PoolConfig = PoolConfig {
    host_ip: config::Pool::HOST_IP,
    port: config::Pool::PORT,
    user: config::Pool::USER,
    password: config::Pool::PASS,
    reconnect_delay: config::Pool::RECONNECT_DELAY,
};

defmt::timestamp!("{=u64:iso8601s}", eth_examples::wall_clock_secs());

bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    HASH_RNG => rng::InterruptHandler<peripherals::RNG>,
        #[cfg(feature = "dummy-asic")]
        hash::InterruptHandler<peripherals::HASH>;
    DMA2_STREAM0 => dma::InterruptHandler<DMA2_CH0>;
    #[cfg(feature = "dummy-asic")]
    USART3 => BufferedInterruptHandler<peripherals::USART3>;
    #[cfg(feature = "display")]
    DMA2_STREAM3 => dma::InterruptHandler<DMA2_CH3>;
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
        // STM32F439 datasheet, F43x electrical characteristics: V_sense ≈ 0.76 V
        // at 25 °C with an average slope of 2.5 mV/°C, 12-bit ADC across
        // VREF = 3.3 V. T_mC = 25000 + (Vsense_uV - 760000) * 1000 / 2500.
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

/// Publish the current hashrate as a Prometheus metric and mirror it
/// into `eth_examples::hashrate` so the unified display and HTTP
/// surfaces render the cpu-miner rate.
#[cfg(feature = "cpu-mining")]
#[embassy_executor::task]
async fn hashrate_metrics_task() -> ! {
    let mut ticker = Ticker::every(Duration::from_secs(5));
    loop {
        ticker.next().await;
        let hashrate_hz = ubos_backend::HASHRATE_HZ.load(Ordering::Relaxed);
        // Saturating cast: a cpu miner saturating u32 H/s is out of scope.
        eth_examples::hashrate::record_external_rate(
            u32::try_from(hashrate_hz).unwrap_or(u32::MAX),
        );
        eth_examples::metrics::push(Metric {
            name: "board_hashrate_hz",
            labels: heapless::Vec::new(),
            value: Value::Uint(hashrate_hz),
            timestamp_ms: 0,
        })
        .await;
    }
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

#[cfg(feature = "dummy-asic")]
struct HardwareSha256d(hash::Hash<'static, peripherals::HASH, embassy_stm32::mode::Blocking>);

#[cfg(feature = "dummy-asic")]
impl eth_examples::dummy_asic::Sha256dHasher for HardwareSha256d {
    fn double_sha256(&mut self, header: &[u8; 80]) -> [u8; 32] {
        let first = hw_sha256(&mut self.0, header);
        hw_sha256(&mut self.0, &first)
    }
}

#[cfg(feature = "dummy-asic")]
fn hw_sha256(
    hash: &mut hash::Hash<'static, peripherals::HASH, embassy_stm32::mode::Blocking>,
    data: &[u8],
) -> [u8; 32] {
    let mut ctx = hash.start(hash::Algorithm::SHA256, hash::DataType::Width8, None);
    hash.update_blocking(&mut ctx, data);
    let mut digest = [0u8; 32];
    let _written = hash.finish_blocking(ctx, &mut digest);
    digest
}

#[cfg(feature = "dummy-asic")]
#[embassy_executor::task]
async fn dummy_asic_test(
    mut uart: BufferedUart<'static>,
    hash_peripheral: Peri<'static, peripherals::HASH>,
) -> ! {
    let mut hasher = HardwareSha256d(hash::Hash::new_blocking(hash_peripheral, Irqs));
    let _ = eth_examples::dummy_asic::run_test(&mut uart, &mut hasher).await;
    eth_examples::dummy_asic::run_continuous_mining(&mut uart, &mut hasher).await
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

    // RMII pin mapping for Nucleo-F439ZI onboard LAN8742A PHY.
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

    #[cfg(feature = "dummy-asic")]
    {
        static TX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        static RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        let uart = BufferedUart::new(
            hw.usart3,
            hw.usart3_rx,
            hw.usart3_tx,
            TX_BUF.init([0; 1024]),
            RX_BUF.init([0; 1024]),
            Irqs,
            {
                let mut config = UartConfig::default();
                config.baudrate = eth_examples::dummy_asic::BAUD_RATE;
                config
            },
        )
        // Valid USART peripheral and default-compatible baud rate; cannot fail.
        .expect("BUG: USART3 configuration failed");
        spawner.spawn(unwrap!(dummy_asic_test(uart, hw.hash)));
    }

    // SH1106 OLED on SPI1: reset pulse, then async TX-only SPI with DMA.
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
        let mut display = Sh1106::new(spi_device, dc);
        // Display is reset and SPI is valid; init cannot fail.
        display.init().expect("BUG: SH1106 init failed");
        (rst, display)
    };

    #[cfg(feature = "display")]
    spawner.spawn(unwrap!(display_task(display)));

    info!("waiting for dhcp");
    let config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", config.address.address());

    #[cfg(feature = "cpu-mining")]
    {
        spawner.spawn(unwrap!(ubos_backend::stratum_task(stack, &POOL_CONFIG)));
        spawner.spawn(unwrap!(ubos_backend::cpu_worker_task()));
        spawner.spawn(unwrap!(cpu_miner::cpu_miner_task()));
        spawner.spawn(unwrap!(ubos_backend::ntime_roller_task()));
        spawner.spawn(unwrap!(ubos_backend::hashrate_task()));
        spawner.spawn(unwrap!(hashrate_metrics_task()));
    }

    net::run_http_server(stack, config::Board::NAME).await;
}
