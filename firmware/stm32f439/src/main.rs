// Copyright (C) 2026  Braiins Systems s.r.o.
//
// All rights reserved. This file is proprietary and may not be copied,
// distributed, or disclosed except as authorized in writing.

//! Firmware entry point for the Nucleo-F439ZI (onboard LAN8742A over RMII)
//! with the BMM101/BM1362 ubos-mining backend.
//!
//! Starts the built-in MAC with an auto-probed PHY, runs the embassy-net
//! stack with DHCP, and spawns the watchdog, LED blink, NTP, ubos-mining
//! mining, and HTTP tasks. Built with `cargo build` from this crate
//! inside `nix develop .#arm`.
//!
//! Board docs: `../../../docs/OVERVIEW.md`
//! Wiring/build: `../../../docs/SETUP.md`

#![no_std]
#![no_main]

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::adc::{Adc, AdcChannel, Exten, RingBufferedAdc, SampleTime};
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue, Sma};
use embassy_stm32::flash::{Blocking as FlashBlocking, Flash};
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c};
#[cfg(feature = "display")]
use embassy_stm32::peripherals::DMA2_CH3;
use embassy_stm32::peripherals::{
    ADC1, DMA1_CH0, DMA1_CH1, DMA1_CH3, DMA1_CH6, DMA2_CH0, ETH, ETH_SMA, IWDG,
};
use embassy_stm32::rng::Rng;
#[cfg(feature = "display")]
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::timer::low_level::{MasterMode, RoundTo, Timer as LowLevelTimer};
use embassy_stm32::triggers::TIM3_TRGO;
use embassy_stm32::usart::{Config as UartConfig, InterruptHandler as UsartInterruptHandler, Uart};
use embassy_stm32::wdg::IndependentWatchdog;
use embassy_stm32::{bind_interrupts, dma, eth, peripherals, rng};
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Timer};
#[cfg(feature = "display")]
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::StaticCell;
use ubos_config::{FlashStorage, SyncFlash};
#[cfg(feature = "display")]
use ubos_display::sh1106::{Rotation, Sh1106};
use ubos_eeprom::HashboardEeprom as _;
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

/// Backing storage for the user-facing event ring, owned here so its size
/// follows this board's `EVENT_LOG_CAPACITY`. Placed in `.uninit`, which the
/// startup code does not zero, so the ring survives a warm reset; the validity
/// header in `RetainedRing` discards it after a cold boot leaves garbage.
#[unsafe(link_section = ".uninit")]
static mut EVENT_LOG_BACKING: ubos_events::RetainedRing<
    { config::MACHINE.control_board.event_log_capacity },
> = ubos_events::RetainedRing::new();

defmt::timestamp!("{=u64:iso8601s}", ubos_net::wall_clock_secs());

bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    HASH_RNG => rng::InterruptHandler<peripherals::RNG>;
    DMA2_STREAM0 => dma::InterruptHandler<DMA2_CH0>;
    USART3 => UsartInterruptHandler<peripherals::USART3>;
    DMA1_STREAM1 => dma::InterruptHandler<DMA1_CH1>;
    DMA1_STREAM3 => dma::InterruptHandler<DMA1_CH3>;
    DMA1_STREAM0 => dma::InterruptHandler<DMA1_CH0>;
    DMA1_STREAM6 => dma::InterruptHandler<DMA1_CH6>;
    I2C1_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C1>;
    I2C1_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C1>;
    #[cfg(feature = "display")]
    DMA2_STREAM3 => dma::InterruptHandler<DMA2_CH3>;
});

type Device = Ethernet<'static, ETH, GenericPhy<Sma<'static, ETH_SMA>>>;

struct BoardWatchdog(IndependentWatchdog<'static, IWDG>);

impl ubos_core::watchdog::Watchdog for BoardWatchdog {
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
    async fn read(&mut self) -> Option<ubos_core::units::Temperature> {
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
        i32::try_from(millicelsius)
            .ok()
            .map(ubos_core::units::Temperature::from_millicelsius)
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device>) -> ! {
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
async fn ntp_sync_task(stack: embassy_net::Stack<'static>) -> ! {
    net::run_ntp_sync(stack).await
}

#[embassy_executor::task]
async fn metrics_ingest_task() -> ! {
    ubos_metrics::metrics::run_ingest().await
}

const MDNS_SERVICES: &[&ubos_mdns::ServiceEntry] = &[
    &ubos_webui::WEBUI_SERVICE,
    &ubos_api::REST_API_SERVICE,
    &ubos_metrics::PROMETHEUS_SERVICE,
];

#[embassy_executor::task]
async fn mdns_task(stack: embassy_net::Stack<'static>, hostname: &'static str) -> ! {
    ubos_mdns::run_mdns(
        stack,
        hostname,
        config::MACHINE.control_board.name,
        config::MACHINE.control_board.platform.name,
        MDNS_SERVICES,
    )
    .await
}

#[embassy_executor::task]
async fn temperature_task(sensor: BoardTempSensor) -> ! {
    ubos_temperature::run_temperature(sensor).await
}

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    ubos_metrics::cpu_load::run_cpu_load::<1>().await
}

#[embassy_executor::task]
async fn stratum_metrics_task() -> ! {
    stratum_client::publishers::run_stratum_metrics_publisher(POOL_SLOTS).await
}

#[embassy_executor::task]
async fn pool_weights_task() -> ! {
    ubos_mining::scheduler::run_pool_weights_watcher().await
}

#[embassy_executor::task]
async fn miner_metrics_task(mining: MiningHandle) -> ! {
    ubos_mining::publishers::run_miner_metrics_publisher(mining, POOL_SLOTS).await
}

#[embassy_executor::task]
async fn hashrate_metrics_task() -> ! {
    ubos_metrics::hashrate::run_hashrate_publisher().await
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
/// `embassy-stm32 blocking Flash → SyncFlash → FlashStorage`.
type BoardConfigStorage = FlashStorage<SyncFlash<Flash<'static, FlashBlocking>>>;

#[embassy_executor::task]
async fn config_task(storage: BoardConfigStorage) -> ! {
    ubos_config::run(storage, config::BOARD).await
}

#[embassy_executor::task]
async fn cooling_task(fan: fan::BoardFan, tach: fan::Tach, thermal: ThermalHandle) -> ! {
    ubos_cooling::run_cooling(fan, tach, thermal).await
}

#[embassy_executor::task]
async fn tach_task(hardware: fan::TachHardware) -> ! {
    fan::run_tach(hardware).await
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

/// The reset pin is moved in and held for the task's lifetime so the SH1106
/// RESET line stays driven high; dropping it would release the pad and blank
/// the panel.
#[cfg(feature = "display")]
#[embassy_executor::task]
async fn display_task(
    mut display: OledDisplay,
    #[allow(clippy::used_underscore_binding)] _reset_pin: Output<'static>,
) -> ! {
    ubos_display::display::run(&mut display).await
}

/// Resets the MCU once a reboot has been requested through the HTTP API.
#[embassy_executor::task]
async fn reset_task() -> ! {
    ubos_core::reboot::wait_for_reboot_request().await;
    cortex_m::peripheral::SCB::sys_reset()
}

/// Keeps the credential store in sync with config changes at runtime.
#[embassy_executor::task]
async fn sync_credentials_task() -> ! {
    ubos_api::run_sync_credentials().await
}

ubos_http::http_worker_task!(
    api_worker_task,
    ubos_api::RestApiService,
    config::MACHINE.control_board.api_http_pool
);

ubos_http::http_worker_task!(
    webui_worker_task,
    ubos_webui::DashboardService,
    config::MACHINE.control_board.webui_http_pool
);

ubos_http::http_worker_task!(
    metrics_worker_task,
    ubos_metrics::metrics::PrometheusMetricsService,
    config::MACHINE.control_board.metrics_http_pool
);

#[embassy_executor::task]
async fn network_control_task(stack: embassy_net::Stack<'static>) -> ! {
    net::run_network_control(stack).await
}

/// Parallel stratum session slots this board runs — the full config
/// capacity; this board has the RAM for a session task per pool slot.
/// RAM-constrained boards may choose fewer (see stm32f407).
pub(crate) const POOL_SLOTS: usize = ubos_firmware::MAX_POOLS;

/// Shared ASIC session: the bridge between the per-slot stratum sessions and
/// the BM1362 serial worker. One instance per binary; referenced by every
/// stratum client task and the mining serial task.
static ASIC_SESSION: ubos_firmware::AsicSession<POOL_SLOTS> = ubos_firmware::AsicSession::new();

/// Board task shell around the shared stratum session loop, spawned once per
/// pool slot. The loop body lives in `ubos-firmware`; the
/// `#[embassy_executor::task]` shell stays per board because esp32s3 pins a
/// different `embassy-executor` version than the ARM boards, so the spawned
/// task type must come from each board's executor.
#[embassy_executor::task(pool_size = POOL_SLOTS)]
async fn stratum_client_task(
    stack: embassy_net::Stack<'static>,
    slot: usize,
    session: &'static ubos_firmware::AsicSession<POOL_SLOTS>,
    shared: ubos_firmware::StratumShared,
) -> ! {
    ubos_firmware::run_stratum_session(stack, slot, session, shared).await
}

// cortex-m-rt's startup only zeroes the standard `.bss`; CCM is a
// separate memory bank declared `NOLOAD` in `memory.x`, so its bytes
// are arbitrary at power-on. The metric map and HTTP scratch buffers
// relocated to CCM by the `ccmram-bss` feature rely on this initial
// zero, which the macro installs via `cortex_m_rt::pre_init`.
ubos_core::pre_init_zero_ccmram_bss!();

#[cortex_m_rt::entry]
fn main() -> ! {
    ubos_metrics::cpu_load::run_cortex_m_executor::<0, _>(|spawner| {
        spawner.spawn(unwrap!(boot(spawner)));
    })
}

#[embassy_executor::task]
#[allow(clippy::too_many_lines, clippy::items_after_statements)]
async fn boot(spawner: Spawner) {
    let peripherals = embassy_stm32::init(config::stm32_config());
    let hw = config::Board::take(peripherals);

    // Install the event ring and its decode-table registry before any task
    // that records events runs. wall_clock_secs returns uptime before NTP
    // sync and Unix time after; the ring stores it unconditionally.
    ubos_events::set_catalogs(ubos_firmware::EVENT_REGISTRY);
    // Safety: called once at boot before any task runs. EVENT_LOG_BACKING lives
    // in .uninit, so the startup code does not zero it and a warm reset
    // preserves its contents.
    ubos_events::init(
        unsafe { &mut *core::ptr::addr_of_mut!(EVENT_LOG_BACKING) },
        ubos_net::wall_clock_secs,
    );

    let watchdog = IndependentWatchdog::new(hw.iwdg, config::Watchdog::TIMEOUT_US);
    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(watchdog))));
    spawner.spawn(unwrap!(reset_task()));

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

    // Seed the SV2 ChaCha20 PRNG from the hardware RNG; the stratum-v2
    // noise handshake gets its own entropy stream independent of the
    // session CSPRNG seeded below.
    let stratum_shared = {
        let mut sv2_seed = [0u8; 32];
        rng.async_fill_bytes(&mut sv2_seed)
            .await
            .expect("BUG: RNG fill failed");
        ubos_firmware::init_stratum(sv2_seed)
    };

    // Derive a per-unit MAC from the 96-bit STM32 unique device id.
    let mac_addr = ubos_net::mac::derive_mac(
        ubos_net::mac::DUMMY_OUI,
        config::MACHINE.control_board.mac_family,
        &embassy_stm32::uid::uid(),
    );

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
        mac_addr,
        hw.eth_sma,
        hw.mdio,
        hw.mdc,
    );
    ubos_net::net::handle().set_mac(mac_addr);

    let manager_handle = ubos_manager::handle();
    let mining_handle = ubos_mining::handle();

    let config_storage = FlashStorage::new(
        SyncFlash(Flash::new_blocking(hw.flash)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(config_storage)));

    // The config task publishes the stored config on first boot. Wait
    // for it, then derive the stack config and hostname. Only the small
    // results escape the block, so this task's future never has to hold
    // the full Config or load_initial's decode buffer.
    static MDNS_HOSTNAME: StaticCell<heapless::String<32>> = StaticCell::new();
    let (net_config, mdns_hostname) = {
        let initial = ubos_config::handle().changed().await;
        let net_config = match &initial.network.mode {
            ubos_config::NetworkMode::Static {
                address,
                prefix_len,
                gateway,
                dns,
            } => ubos_net::config::static_ipv4(*address, *prefix_len, *gateway, dns),
            ubos_config::NetworkMode::Dhcp => {
                embassy_net::Config::dhcpv4(embassy_net::DhcpConfig::default())
            }
        };
        // Compose inside the block so only the &'static hostname escapes,
        // not a 32-byte copy held across the rest of boot.
        let mdns_hostname = ubos_mdns::compose_hostname_configured::<
            32,
            { config::Board::MDNS_HOSTNAME_BASE.len() },
        >(
            initial.network.hostname.as_str(),
            config::Board::MDNS_HOSTNAME_BASE,
            mac_addr[5],
            &MDNS_HOSTNAME,
        );
        (net_config, mdns_hostname)
    };

    // Socket-set capacity for the embassy-net stack. Seven sockets are
    // held continuously: DHCPv4, DNS, the stratum client, the mDNS
    // responder, 1 webui listener socket on :80, and 1 REST API listener
    // socket on :8080, plus the metrics listener on :8081. NTP sync opens
    // a transient UDP socket every 60 s, and closed TCP connections
    // linger in TIME_WAIT until smoltcp reaps them. 16 provides headroom.
    // A 5/5 HTTP pool needs ten worker futures whose combined stack
    // frames overflow this board's RAM, so it stays at 1/1.
    const STACK_SOCKETS: usize = 16;
    ubos_net::assert_stack_sockets!(
        STACK_SOCKETS,
        config::MACHINE.control_board.webui_http_pool,
        config::MACHINE.control_board.api_http_pool,
        config::MACHINE.control_board.metrics_http_pool,
    );
    static RESOURCES: StaticCell<StackResources<STACK_SOCKETS>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        device,
        net_config,
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(unwrap!(net_task(runner)));

    // Seed the session CSPRNG from the hardware RNG.
    let mut csprng_seed = [0u8; 32];
    rng.async_fill_bytes(&mut csprng_seed)
        .await
        .expect("BUG: RNG fill failed");
    ubos_auth::csprng::seed_csprng(csprng_seed).await;

    spawner.spawn(unwrap!(ntp_sync_task(stack)));
    spawner.spawn(unwrap!(metrics_ingest_task()));

    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(cpu_load_task()));
    spawner.spawn(unwrap!(stratum_metrics_task()));
    spawner.spawn(unwrap!(hashrate_metrics_task()));
    spawner.spawn(unwrap!(pool_weights_task()));

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

    // Hashboard plug-detect input (PE1, pull-up). Hold the binding at
    // function scope so the GPIO stays driven for the lifetime of the
    // firmware; dropping `Input` would clear the PUPDR pull setting.
    let _hb_plug = Input::new(hw.hb_plug, Pull::Up);

    let thermal = ubos_thermal::handle();

    spawner.spawn(unwrap!(miner_metrics_task(mining_handle)));

    // BMM101 hashboard PSU on I2C1: shared bus, PSU watchdog, power stats,
    // and the miner bring-up task all run on the single-core executor, so
    // the I2C bus mutex can stay `NoopRawMutex`.
    {
        static I2C1_BUS: StaticCell<psu::Bus> = StaticCell::new();
        let i2c = I2c::new(
            hw.i2c1,
            hw.scl,
            hw.sda,
            hw.i2c1_tx_dma,
            hw.i2c1_rx_dma,
            Irqs,
            I2cConfig::default(),
        );
        let bus: &'static psu::Bus = I2C1_BUS.init(Mutex::new(i2c));

        // Read the hashboard EEPROM once, before powering anything up, so
        // its identity is in the log even if bring-up later fails. The
        // parsed serial (or `None` on a failed read) is published for
        // `GET /api/system/info`; this board has one hashboard, so the
        // published array holds a single slot.
        if let Some(address) = config::BOARD.eeprom_i2c_address {
            let mut device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
            let serial = ubos_eeprom::i2c::I2cEeprom::new(address, &mut device)
                .log_identity()
                .await;
            let mut ids = ubos_api::HashboardIds::new();
            unwrap!(ids.push(serial).map_err(|_| ()));
            ubos_api::publish_hashboard_ids(ids);
        }

        let (board_psu, psu_state, terminated) = psu::init(bus);
        spawner.spawn(unwrap!(psu::watchdog_task(psu_state, terminated)));
        spawner.spawn(unwrap!(psu::power_stats_task(board_psu)));

        // Hashboard reset line (PE0, active-low).
        let reset_pin = Output::new(hw.hb_rst, Level::Low, Speed::Low);
        let reset = mining::BoardResetPin::new(reset_pin);
        spawner.spawn(unwrap!(mining::miner_task(board_psu, reset)));

        // Temperature and fan monitor via shared I2C1 bus.
        let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
        let hashboard_temp = temperature::new(temp_device);
        let board_fan = fan::BoardFan::new(hw.tim4, hw.fan_pwm_pin, hw.fan_tach_pin);
        let tach_hardware = fan::TachHardware::new(&board_fan);
        let tach = fan::Tach;
        spawner.spawn(unwrap!(thermal_task(hashboard_temp, manager_handle)));
        spawner.spawn(unwrap!(manager_task(mining_handle)));
        spawner.spawn(unwrap!(cooling_task(board_fan, tach, thermal)));
        spawner.spawn(unwrap!(tach_task(tach_hardware)));
    }

    // BM1362 UART on USART3 (PD8 TX, PD9 RX) with DMA ring buffer.
    // Serial task runs on the same executor as embassy-net because the
    // STM32F439 is single-core. USART3 uses DMA so ETH-ISR jitter affects
    // only the DMA transfer level; the 1 KiB ring buffer absorbs bursts.
    {
        static DMA_RX_BUF: StaticCell<[u8; 1024]> = StaticCell::new();
        let uart_config = {
            let mut config = UartConfig::default();
            config.baudrate = ubos_mining::consts::INITIAL_BAUDRATE;
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
        let miner_serial = uart_baudrate::MinerUart::new(uart_tx, ring_rx);
        spawner.spawn(unwrap!(mining::serial_task(miner_serial, &ASIC_SESSION)));
    }

    // SH1106 OLED on SPI1: reset pulse, then async TX-only SPI with DMA.
    // The reset pin is moved into display_task so it stays driven high for the display's lifetime.
    #[cfg(feature = "display")]
    let (reset, display) = {
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
    spawner.spawn(unwrap!(display_task(display, reset)));

    info!("waiting for dhcp");
    let config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", config.address.address());

    for slot in 0..POOL_SLOTS {
        spawner.spawn(unwrap!(stratum_client_task(
            stack,
            slot,
            &ASIC_SESSION,
            stratum_shared
        )));
    }

    spawner.spawn(unwrap!(network_control_task(stack)));

    // REST API on :8080 — seed credentials once, then spawn worker tasks.
    ubos_api::seed_api_credentials().await;
    static API_SERVICE: StaticCell<ubos_api::RestApiService> = StaticCell::new();
    let api_service: &'static ubos_api::RestApiService =
        API_SERVICE.init(ubos_api::RestApiService::new(
            config::MACHINE.control_board.name,
            &config::MINING_INFO,
            &config::BOUNDS,
            thermal,
            manager_handle,
            ubos_psu::handle(),
            stack,
        ));
    ubos_http::serve_http_pool!(
        spawner,
        stack,
        api_worker_task,
        api_service,
        config::MACHINE.control_board.api_http_pool,
        ubos_api::consts::Api
    );
    spawner.spawn(unwrap!(sync_credentials_task()));

    // Web UI on :80 — seed credentials once (the API listener's
    // sync_credentials_task keeps them current after first boot).
    ubos_webui::seed_webui_credentials().await;
    static WEBUI_SERVICE: ubos_webui::DashboardService = ubos_webui::DashboardService;
    ubos_http::serve_http_pool!(
        spawner,
        stack,
        webui_worker_task,
        &WEBUI_SERVICE,
        config::MACHINE.control_board.webui_http_pool,
        ubos_webui::consts::Http
    );

    static METRICS_SERVICE: ubos_metrics::metrics::PrometheusMetricsService =
        ubos_metrics::metrics::PrometheusMetricsService;
    ubos_http::serve_http_pool!(
        spawner,
        stack,
        metrics_worker_task,
        &METRICS_SERVICE,
        config::MACHINE.control_board.metrics_http_pool,
        ubos_metrics::consts::Metrics
    );
}
