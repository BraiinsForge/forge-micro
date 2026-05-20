//! Firmware entry point for the Nucleo-F439ZI (onboard LAN8742A over RMII).
//!
//! Board docs: `../../../docs/OVERVIEW.md`
//! Wiring/build: `../../../docs/SETUP.md`

#![no_std]
#![no_main]

#[cfg(not(any(feature = "backend-uart", feature = "backend-cpu")))]
compile_error!("a backend feature must be selected: `backend-uart` or `backend-cpu`");

#[cfg(all(feature = "backend-uart", feature = "backend-cpu"))]
compile_error!(
    "features `backend-uart` and `backend-cpu` are mutually exclusive; \
     pick exactly one with --no-default-features --features \"<backend>\""
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
use embassy_stm32::peripherals::DMA2_CH3;
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
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use eth_examples::net;
#[cfg(feature = "display")]
use eth_examples::sh1106::{Rotation, Sh1106};

mod config;

// `eth_examples::pool_config::parse` runs at boot to copy seven
// compile-time env-var strings into typed fields. The result lives
// for the lifetime of the firmware; held in a `StaticCell` so the
// stratum task can take a `&'static PoolConfig`.
static POOL_CONFIG_CELL: StaticCell<eth_examples::pool_config::PoolConfig> = StaticCell::new();

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

// The stm32 RNG peripheral is single-owner and already claimed by
// the bitcoin-price TLS task. Seed a ChaCha20 PRNG once at boot
// from the hardware RNG; the SV2 client uses this PRNG for ephemeral
// keys and nonces across reconnects.
static SV2_PRNG: StaticCell<rand_chacha::ChaCha20Rng> = StaticCell::new();

struct Sv2BoardRng<'a>(&'a mut rand_chacha::ChaCha20Rng);

impl ii_sv2::security::Rng for Sv2BoardRng<'_> {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rand_chacha::rand_core::RngCore::fill_bytes(self.0, dest);
    }
}

static ASIC_SESSION: stratum_client::AsicSession = stratum_client::AsicSession::new();

defmt::timestamp!("{=u64:iso8601s}", eth_examples::wall_clock_secs());

bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    HASH_RNG => rng::InterruptHandler<peripherals::RNG>;
    DMA2_STREAM0 => dma::InterruptHandler<DMA2_CH0>;
    #[cfg(feature = "backend-uart")]
    USART3 => UsartInterruptHandler<peripherals::USART3>;
    #[cfg(feature = "backend-uart")]
    DMA1_STREAM1 => dma::InterruptHandler<DMA1_CH1>;
    #[cfg(feature = "backend-uart")]
    DMA1_STREAM3 => dma::InterruptHandler<DMA1_CH3>;
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

impl ubos_temperature::TemperatureSensor for BoardTempSensor {
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
    ubos_temperature::run_temperature(sensor).await
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
async fn asic_uart_task(
    uart: asic_uart_peer::SplitUart<RingBufferedUartRx<'static>, UartTx<'static, Async>>,
) -> ! {
    asic_uart_peer::run_asic_uart(&ASIC_SESSION, uart).await
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
) -> Result<eth_examples::pool_config::PoolConfig, eth_examples::pool_config::ConfigError> {
    use eth_examples::pool_config;
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
    pool_config: &'static eth_examples::pool_config::PoolConfig,
    sv2_prng: &'static mut rand_chacha::ChaCha20Rng,
) -> ! {
    use defmt::warn;
    use embassy_time::Timer;
    use eth_examples::pool_config::{
        ProtocolConfig, RECONNECT_DELAY, SOCKET_TIMEOUT, STRATUM_TCP_BUFFER_SIZE,
    };
    use static_cell::ConstStaticCell;

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
                let mut state = stratum_client::StratumState::new();
                let mut client =
                    stratum_v2::StratumV2Client::new(sv2, ctx, Sv2BoardRng(&mut *sv2_prng));
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

#[cfg(feature = "backend-cpu")]
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

    // Seed the SV2 ChaCha20 PRNG before the hardware RNG is moved into
    // the bitcoin-price TLS task; the stratum-v2 noise handshake then
    // has its own entropy stream that does not contend with TLS.
    let sv2_prng = {
        let mut sv2_seed = [0u8; 32];
        rng.async_fill_bytes(&mut sv2_seed)
            .await
            .expect("BUG: RNG fill failed");
        SV2_PRNG.init(
            <rand_chacha::ChaCha20Rng as rand_chacha::rand_core::SeedableRng>::from_seed(sv2_seed),
        )
    };

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

    #[cfg(feature = "backend-uart")]
    {
        static DMA_RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        let uart_config = {
            let mut config = UartConfig::default();
            config.baudrate = asic_uart_peer::BAUD_RATE;
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
        let split_uart = asic_uart_peer::SplitUart {
            rx: ring_rx,
            tx: uart_tx,
        };
        spawner.spawn(unwrap!(asic_uart_task(split_uart)));
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

    {
        match pool_config_from_env() {
            Ok(config) => {
                let pool_config = POOL_CONFIG_CELL.init(config);
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
                spawner.spawn(unwrap!(stratum_client_task(stack, pool_config, sv2_prng,)));
            }
            Err(error) => {
                defmt::error!(
                    "stratum-client: POOL_* env vars invalid: {}; not starting stratum task",
                    defmt::Debug2Format(&error),
                );
            }
        }
    }
    #[cfg(feature = "backend-cpu")]
    spawner.spawn(unwrap!(asic_cpu_task()));

    net::run_http_server(stack, config::Board::NAME).await;
}
