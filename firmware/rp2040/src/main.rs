//! Firmware entry point for the W5500-EVB-Pico. Initialises SPI at
//! 33 MHz, brings the W5500 up via `embassy-net-wiznet`, starts the
//! embassy-net stack with DHCP, and spawns the watchdog, LED blink,
//! ethernet runner, NTP sync, and BTC price tasks before entering
//! the HTTP server loop. Built with `cargo build` from this crate
//! inside `nix develop .#arm`.

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
use embassy_net_wiznet::chip::W5500;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::adc::{Adc, Async as AdcAsync, Channel as AdcChannel, Config as AdcConfig};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Input, Level, Output, Pull};
#[cfg(feature = "dummy-asic")]
use embassy_rp::peripherals::UART1;
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, SPI0};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
#[cfg(feature = "dummy-asic")]
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{adc, bind_interrupts, dma};
use embassy_time::Delay;
#[cfg(any(feature = "cpu-mining", feature = "display"))]
use embassy_time::Duration;
#[cfg(feature = "cpu-mining")]
use embassy_time::Ticker;
#[cfg(feature = "display")]
use embassy_time::Timer;
use embedded_hal_bus::spi::ExclusiveDevice;
#[cfg(feature = "cpu-mining")]
use eth_examples::metrics::{Metric, Value};
#[cfg(feature = "cpu-mining")]
use portable_atomic::Ordering;
use static_cell::{ConstStaticCell, StaticCell};
#[cfg(feature = "cpu-mining")]
use ubos_backend::PoolConfig;
use {defmt_rtt as _, panic_probe as _};

use eth_examples::net;
#[cfg(feature = "display")]
use eth_examples::sh1106::Sh1106;

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
    DMA_IRQ_0 =>
        dma::InterruptHandler<DMA_CH0>,
        dma::InterruptHandler<DMA_CH1>,
        #[cfg(feature = "display")]
        dma::InterruptHandler<DMA_CH2>;
    ADC_IRQ_FIFO => adc::InterruptHandler;
    #[cfg(feature = "dummy-asic")]
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

struct BoardTempSensor {
    adc: Adc<'static, AdcAsync>,
    channel: AdcChannel<'static>,
}

impl eth_examples::temperature::TemperatureSensor for BoardTempSensor {
    async fn read_millicelsius(&mut self) -> Option<i32> {
        // RP2040 datasheet 4.9.5: Vsense_uV = raw * VREF_uV / full_scale,
        // T_mC = CALIBRATION_mC - (Vsense_uV - VSENSE27_uV) * 1000 / uV_PER_C.
        // i64 intermediates keep raw * VREF_uV from overflowing i32. The
        // final try_from is defensive against future tweaks to the
        // calibration constants.
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
    ExclusiveDevice<Spi<'static, SPI1, Async>, Output<'static>, embedded_hal_bus::spi::NoDelay>,
    Output<'static>,
>;

#[cfg(feature = "display")]
#[embassy_executor::task]
async fn display_task(mut display: OledDisplay) -> ! {
    eth_examples::display::run(&mut display).await
}

#[cfg(feature = "dummy-asic")]
#[embassy_executor::task]
async fn dummy_asic_test(mut uart: BufferedUart) -> ! {
    let mut hasher = eth_examples::dummy_asic::SoftwareSha256d;
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
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());
    let hw = config::Board::take(peripherals);
    let mut rng = RoscRng;

    // Run an empty embassy executor on core 1 so its wfe-bracketed idle
    // wait feeds `IDLE_TICKS[1]`. The board has no work for core 1; the
    // metric exists to confirm the second core is reachable and parked.
    static CORE1_STACK: ConstStaticCell<
        embassy_rp::multicore::Stack<{ config::Core1::STACK_BYTES }>,
    > = ConstStaticCell::new(embassy_rp::multicore::Stack::new());
    embassy_rp::multicore::spawn_core1(hw.core1, CORE1_STACK.take(), || {
        eth_examples::cpu_load::run_cortex_m_executor::<1, _>(|_spawner| {});
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
    spawner.spawn(unwrap!(cpu_load_task()));

    let adc = Adc::new(hw.adc, Irqs, AdcConfig::default());
    let temp_channel = AdcChannel::new_temp_sensor(hw.adc_temp_sensor);
    spawner.spawn(unwrap!(temperature_task(BoardTempSensor {
        adc,
        channel: temp_channel,
    })));

    #[cfg(feature = "dummy-asic")]
    {
        static TX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        static RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        let uart = BufferedUart::new(
            hw.uart,
            hw.uart_tx,
            hw.uart_rx,
            Irqs,
            TX_BUF.init([0; 1024]),
            RX_BUF.init([0; 1024]),
            {
                let mut config = UartConfig::default();
                config.baudrate = eth_examples::dummy_asic::BAUD_RATE;
                config
            },
        );
        spawner.spawn(unwrap!(dummy_asic_test(uart)));
    }

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
        let mut display = Sh1106::new(spi_device, display_dc);
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
        spawner.spawn(unwrap!(cpu_miner::cpu_miner_task(
            &ubos_backend::TOTAL_HASHES
        )));
        spawner.spawn(unwrap!(ubos_backend::ntime_roller_task()));
        spawner.spawn(unwrap!(ubos_backend::hashrate_task()));
        spawner.spawn(unwrap!(hashrate_metrics_task()));
    }

    net::run_http_server(stack, config::Board::NAME).await;
}
