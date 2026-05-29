//! Firmware entry point for the W6100-EVB-Pico-2 (RP2350 + W6100) with the
//! ubos-mining backend.
//!
//! Initialises SPI at 33 MHz, brings the W6100 up via `embassy-net-wiznet`,
//! starts the embassy-net stack with DHCP, and spawns the watchdog, LED
//! blink, ethernet runner, NTP sync, BTC price, and ubos-mining mining tasks
//! before entering the HTTP server loop. Built with `cargo build` from
//! this crate inside `nix develop .#arm`.
//!
//! Board docs: `../../../docs/OVERVIEW.md`
//! Wiring/build: `../../../docs/SETUP.md`
//! WFE wake-storm investigation: `../../../docs/rp2350-wake-storm.md`

#![no_std]
#![no_main]

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W6100;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::clocks::RoscRng;
use embassy_rp::flash::{Blocking as FlashBlocking, Flash};
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, FLASH, I2C1, SPI0, TRNG, UART1};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
use embassy_rp::trng::{self, Trng};
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma, i2c};
use embassy_time::Delay;
#[cfg(feature = "display")]
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::{ConstStaticCell, StaticCell};
use ubos_config::{FlashStorage, SyncFlash};
#[cfg(feature = "display")]
use ubos_display::sh1106::{Rotation, Sh1106};
use ubos_manager::Handle as ManagerHandle;
use ubos_mining::Handle as MiningHandle;
use ubos_thermal::Handle as ThermalHandle;
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

/// Software ring buffer size per direction for the core-1 `BufferedUart`.
/// The RP2350 HW FIFO is 32 bytes; 1 KiB gives slack for ETH-ISR jitter
/// at the BM1362 hashboard baud rate.
const UART_RING_BUFFER_SIZE: usize = 1024;

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

defmt::timestamp!("{=u64:iso8601s}", ubos_net::wall_clock_secs());

bind_interrupts!(struct Irqs {
    DMA_IRQ_0 =>
        dma::InterruptHandler<DMA_CH0>,
        dma::InterruptHandler<DMA_CH1>,
        #[cfg(feature = "display")]
        dma::InterruptHandler<DMA_CH2>;
    I2C1_IRQ => i2c::InterruptHandler<I2C1>;
    UART1_IRQ => BufferedInterruptHandler<UART1>;
    TRNG_IRQ => trng::InterruptHandler<TRNG>;
});

type EthernetRunner = Runner<
    'static,
    W6100,
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
async fn thermal_task(sensor: temperature::HashboardTemperature, manager: ManagerHandle) -> ! {
    ubos_thermal::run_thermal(sensor, manager).await
}

#[embassy_executor::task]
async fn manager_task(mining: MiningHandle) -> ! {
    ubos_manager::run_manager(mining).await
}

/// Storage backend type the [`config_task`] hands to
/// [`ubos_config::run`]. The chain is
/// `embassy-rp blocking Flash → SyncFlash → FlashStorage`.
type BoardConfigStorage =
    FlashStorage<SyncFlash<Flash<'static, FLASH, FlashBlocking, { config::Flash::SIZE }>>>;

#[embassy_executor::task]
async fn config_task(
    manager: ManagerHandle,
    mining: MiningHandle,
    storage: BoardConfigStorage,
) -> ! {
    ubos_config::run(manager, mining, storage).await
}

#[embassy_executor::task]
async fn cooling_task(fan: fan::BoardFan, thermal: ThermalHandle) -> ! {
    ubos_cooling::run_cooling(fan, thermal).await
}

#[embassy_executor::task]
async fn tach_task(hardware: fan::TachHardware) -> ! {
    fan::run_tach(hardware).await
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

#[embassy_executor::task]
#[allow(clippy::too_many_lines)]
async fn stratum_client_task(stack: embassy_net::Stack<'static>) -> ! {
    use defmt::warn;
    use embassy_futures::select::{select, Either};
    use embassy_time::Timer;
    use stratum_client::{RECONNECT_DELAY, SOCKET_TIMEOUT, STRATUM_TCP_BUFFER_SIZE};
    static TCP_RX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    static TCP_TX: ConstStaticCell<[u8; STRATUM_TCP_BUFFER_SIZE]> =
        ConstStaticCell::new([0; STRATUM_TCP_BUFFER_SIZE]);
    let rx_buffer = TCP_RX.take();
    let tx_buffer = TCP_TX.take();

    // SV2 secp context — initialised once even if the first pool entry
    // is Sv1, so a runtime protocol switch needs no late allocation.
    let secp_buffer = SECP_CTX_BUF.take();
    let secp_ctx = SECP_CTX.init(
        ii_sv2::security::CryptoCtx::preallocated(secp_buffer)
            .expect("BUG: SECP_CTX_SLOTS too small for AllPreallocated"),
    );

    let config_handle = ubos_config::handle();
    let restart = config_handle.stratum_restart();

    loop {
        let snapshot = config_handle.current();
        let Some(entry) = snapshot.pool.first() else {
            warn!("stratum-client: pool list empty, awaiting config update");
            restart.wait().await;
            continue;
        };

        let one_session = async {
            stratum_client::record_reconnect_attempt();
            let mut socket = match ubos_net::net::connect_stratum(
                stack,
                entry.host.as_str(),
                entry.port,
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
                    return;
                }
            };
            let mut state = stratum_client::StratumState::new();

            match &entry.protocol {
                ubos_config::ProtocolKind::Sv1 { password } => {
                    let sv1 = stratum_v1::Sv1Config::new(
                        entry.user.as_str(),
                        password.as_ref().map_or("", heapless::String::as_str),
                    )
                    .expect("BUG: PoolEntry user/password match Sv1Config buffer sizes");
                    let mut client = stratum_v1::StratumV1Client::new(&sv1);
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
                }
                ubos_config::ProtocolKind::Sv2 { pool_pubkey } => {
                    let sv2 = stratum_v2::Sv2Config::new(entry.user.as_str(), *pool_pubkey)
                        .expect("BUG: PoolEntry.user matches Sv2Config buffer size");
                    let mut client =
                        stratum_v2::StratumV2Client::new(&sv2, secp_ctx, Sv2BoardRng(RoscRng));
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
                }
            }

            Timer::after(RECONNECT_DELAY).await;
        };
        if matches!(
            select(one_session, restart.wait()).await,
            Either::Second(())
        ) {
            info!("stratum-client: pool config changed, reconnecting");
        }
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    ubos_metrics::cpu_load::run_cortex_m_executor::<0, _>(|spawner| {
        spawner.spawn(unwrap!(boot(spawner)));
    })
}

#[embassy_executor::task]
#[allow(clippy::too_many_lines, clippy::items_after_statements)]
async fn boot(spawner: Spawner) {
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());
    let hw = config::Board::take(peripherals);
    let mut rng = RoscRng;

    // Pin the ubos-mining serial worker to core 1 so embassy-net on core 0 stays
    // responsive. BufferedUart must be constructed on the core that polls
    // it — `init_buffers` unmasks the UART IRQ on the calling core's NVIC,
    // and the async write path pends the same IRQ to drain the SW TX ring
    // buffer into the HW FIFO. RP2350's Cortex-M33 has per-core NVICs, so
    // an unmask on one core and a pend on the other land on different
    // controllers and the FIFO is never written.
    let uart_parts = (hw.uart, hw.uart_tx, hw.uart_rx);
    embassy_rp::multicore::spawn_core1(hw.core1, CORE1_STACK.take(), move || {
        let (miner_tx, miner_rx) = {
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
                    config.baudrate = ubos_mining::consts::INITIAL_BAUDRATE;
                    config
                },
            );
            uart_baudrate::split_uart1(uart)
        };
        // Drive core 1 through `run_cortex_m_executor::<1, _>` so the
        // poll/wfe loop accumulates idle ticks into `IDLE_TICKS[1]`,
        // letting `run_cpu_load::<2>()` publish a real busy ratio for
        // this core instead of a constant 1.0.
        ubos_metrics::cpu_load::run_cortex_m_executor::<1, _>(|core1_spawner: Spawner| {
            core1_spawner.spawn(unwrap!(mining::serial_task(miner_rx, miner_tx)));
        });
    });

    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(Watchdog::new(
        hw.watchdog
    )))));

    let led = Output::new(hw.led, Level::Low);
    spawner.spawn(unwrap!(led_blink_task(led)));

    // Hashboard plug-detect input (GPIO3, pull-up). Hold the binding at
    // function scope so the GPIO stays driven for the lifetime of the
    // firmware; dropping `Input` would clear the pad and pull setting.
    let _hb_plug = Input::new(hw.hb_plug, Pull::Up);

    let thermal = ubos_thermal::handle();

    let manager_handle = ubos_manager::handle();

    let mining_handle = ubos_mining::handle();

    // BMM100 hashboard PSU on I2C1: shared bus, PSU watchdog, power
    // stats, and the miner bring-up task all run on core 0 so the I2C
    // bus mutex can stay NoopRawMutex.
    {
        use embassy_rp::i2c::{Config as I2cConfig, I2c};
        use embassy_sync::mutex::Mutex;

        static I2C1_BUS: StaticCell<psu::Bus> = StaticCell::new();
        let i2c = I2c::new_async(hw.i2c1, hw.scl, hw.sda, Irqs, I2cConfig::default());
        let bus: &'static psu::Bus = I2C1_BUS.init(Mutex::new(i2c));
        let (board_psu, psu_state, terminated) = psu::init(bus);
        spawner.spawn(unwrap!(psu::watchdog_task(psu_state, terminated)));
        spawner.spawn(unwrap!(psu::power_stats_task(board_psu)));
        let reset = mining::BoardResetPin::new(Output::new(hw.hb_rst, Level::Low));
        spawner.spawn(unwrap!(mining::miner_task(board_psu, reset)));

        // Temperature and fan monitor via shared I2C bus.
        let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
        let hashboard_temp = temperature::HashboardTemperature::new(temp_device);
        let board_fan = fan::BoardFan::new(hw.fan_pwm_slice, hw.fan_pwm_pin);
        spawner.spawn(unwrap!(thermal_task(hashboard_temp, manager_handle)));
        spawner.spawn(unwrap!(manager_task(mining_handle)));
        spawner.spawn(unwrap!(cooling_task(board_fan, thermal)));
    }

    let config_storage = FlashStorage::new(
        SyncFlash(Flash::<_, FlashBlocking, { config::Flash::SIZE }>::new_blocking(hw.flash)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(
        manager_handle,
        mining_handle,
        config_storage
    )));

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

    static RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        device,
        embassy_net::Config::dhcpv4(embassy_net::DhcpConfig::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(unwrap!(net_task(runner)));

    // Seed the session CSPRNG from the RP2350 hardware TRNG, not the
    // ring-oscillator-bit `RoscRng`. The TRNG runs three built-in
    // entropy health checks per draw and is NIST-aligned; `RoscRng`
    // takes one ROSC randombit per read with no oracle. The TRNG is
    // only used at boot, so the peripheral is dropped right after the
    // seed is taken — `bitcoin_price_task` continues to use `RoscRng`
    // for its TLS nonces because the RP2040 sister firmware has no
    // TRNG and we keep that code path uniform.
    let mut csprng_seed = [0u8; 32];
    {
        // Datasheet § 12.12.2: sample counts of 20-25 minimise generation
        // time but fail the on-die NIST entropy checks often enough to
        // log every boot. 100 trades a few hundred extra microseconds for
        // a quiet boot; the TRNG is only sampled once for the 32-byte
        // CSPRNG seed.
        let mut trng_config = trng::Config::default();
        trng_config.sample_count = 100;
        let mut trng = Trng::new(hw.trng, Irqs, trng_config);
        trng.fill_bytes(&mut csprng_seed).await;
    }
    ubos_api::seed_csprng(csprng_seed).await;

    spawner.spawn(unwrap!(bitcoin_price_task(stack, rng)));
    spawner.spawn(unwrap!(ntp_sync_task(stack)));
    spawner.spawn(unwrap!(metrics_task(stack)));

    static MDNS_HOSTNAME: StaticCell<heapless::String<16>> = StaticCell::new();
    let mdns_hostname =
        ubos_mdns::compose_hostname::<16, { config::Board::MDNS_HOSTNAME_BASE.len() }>(
            config::Board::MDNS_HOSTNAME_BASE,
            config::Board::MAC_ADDR[5],
            &MDNS_HOSTNAME,
        );
    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(tach_task(fan::TachHardware {
        slice: hw.fan_tach_slice,
        pin: hw.fan_tach_pin,
    })));

    spawner.spawn(unwrap!(cpu_load_task()));
    spawner.spawn(unwrap!(miner_metrics_task(mining_handle)));
    spawner.spawn(unwrap!(stratum_metrics_task()));
    spawner.spawn(unwrap!(hashrate_metrics_task()));

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

    // Core 1 hosts serial_task (ubos-mining backend); see spawn_core1 above.
    spawner.spawn(unwrap!(stratum_client_task(stack)));

    ubos_api::run(stack, config::Board::NAME, thermal, manager_handle).await;
}
