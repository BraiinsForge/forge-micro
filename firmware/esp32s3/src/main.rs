//! Firmware entry point for the ESP32-S3 `DevKit` with an external
//! W5100S over SPI. Configures SPI with DMA, brings up the W5100S via
//! `embassy-net-wiznet`, starts the embassy-net stack with DHCP, and
//! spawns the watchdog, ethernet runner, NTP, and BTC tasks before
//! entering the HTTP server loop. Built with `cargo build` from this
//! crate inside `nix develop .#esp`.

#![no_std]
#![no_main]

use defmt::info;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W5100S;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_time::{Delay, Instant};
#[cfg(feature = "display")]
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::peripherals::TIMG1;
use esp_hal::rng::Rng;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::timer::timg::{MwdtStage, TimerGroup, Wdt};
#[cfg(feature = "dummy-asic")]
use esp_hal::uart::{Config as UartConfig, Uart};
use esp_rtos::embassy::Callbacks;
use eth_examples::net;
#[cfg(feature = "display")]
use eth_examples::sh1106::Sh1106;
use portable_atomic::Ordering;
use rand_core::{CryptoRng, RngCore};
use static_cell::StaticCell;
use {defmt_rtt as _, esp_backtrace as _};

mod config;

defmt::timestamp!("{=u64:iso8601s}", eth_examples::wall_clock_secs());

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
        u64::from(self.0.random()) << 32 | u64::from(self.0.random())
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

impl eth_examples::watchdog::Watchdog for BoardWatchdog {
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
    eth_examples::watchdog::run_watchdog(watchdog).await
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
    eth_examples::metrics::run(stack).await
}

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    eth_examples::cpu_load::run_cpu_load::<1>().await
}

/// Adapts `esp32s3_tsens::Tsens` to the eth-examples `TemperatureSensor` trait.
struct BoardTempSensor(esp32s3_tsens::Tsens<'static>);

impl eth_examples::temperature::TemperatureSensor for BoardTempSensor {
    async fn read_millicelsius(&mut self) -> Option<i32> {
        self.0.read_millicelsius().await
    }
}

#[embassy_executor::task]
async fn temperature_task(sensor: BoardTempSensor) -> ! {
    eth_examples::temperature::run_temperature(sensor).await
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
    eth_examples::display::run(&mut display).await
}

#[cfg(feature = "dummy-asic")]
#[embassy_executor::task]
async fn dummy_asic_test(mut uart: Uart<'static, esp_hal::Async>) -> ! {
    let mut hasher = eth_examples::dummy_asic::SoftwareSha256d;
    let _ = eth_examples::dummy_asic::run_test(&mut uart, &mut hasher).await;
    eth_examples::dummy_asic::run_continuous_mining(&mut uart, &mut hasher).await
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
                CPU_INDEX < eth_examples::cpu_load::MAX_CPUS,
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
            eth_examples::cpu_load::IDLE_TICKS[CPU_INDEX].fetch_add(elapsed, Ordering::Relaxed);
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
    static RESOURCES: StaticCell<StackResources<7>> = StaticCell::new();

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let hw = config::Board::take(peripherals);

    let timg0 = TimerGroup::new(hw.timg0);
    esp_rtos::start(timg0.timer0);

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

    spawner.must_spawn(bitcoin_price_task(stack, TlsRng(rng)));
    spawner.must_spawn(ntp_sync_task(stack));
    spawner.must_spawn(metrics_task(stack));
    spawner.must_spawn(cpu_load_task());
    // No LED blink task: board wiring does not include an accessible LED.
    let tsens = esp32s3_tsens::Tsens::new(hw.sens, esp32s3_tsens::Range::Standard).await;
    spawner.must_spawn(temperature_task(BoardTempSensor(tsens)));

    #[cfg(feature = "dummy-asic")]
    {
        let uart_config = UartConfig::default().with_baudrate(eth_examples::dummy_asic::BAUD_RATE);
        // Valid UART peripheral and default-compatible baud rate; cannot fail.
        let uart = Uart::new(hw.uart0, uart_config)
            .expect("BUG: UART0 configuration failed")
            .with_tx(hw.uart_tx)
            .with_rx(hw.uart_rx)
            .into_async();
        spawner.must_spawn(dummy_asic_test(uart));
    }

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
        let mut display = Sh1106::new(spi_device, display_dc);
        // Display is reset and SPI is valid; init cannot fail.
        display.init_async().await.expect("BUG: SH1106 init failed");
        (rst, display)
    };

    #[cfg(feature = "display")]
    spawner.must_spawn(display_task(display));

    info!("waiting for dhcp");
    let config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", config.address.address());

    net::run_http_server(stack, config::Board::NAME).await;
}
