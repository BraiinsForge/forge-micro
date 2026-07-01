//! Firmware entry point for the Hashnode, an RP2350 with a W5500
//! ethernet controller, the ubos-mining backend driving a 4-chip BM1370
//! hashboard, and a `SIC45x`-based `BfmPsu` power stage.
//!
//! Initialises SPI at 33 MHz, brings the W5500 up via `embassy-net-wiznet`,
//! starts the embassy-net stack with DHCP, and spawns the watchdog, LED
//! blink, ethernet runner, NTP sync, and ubos-mining mining tasks
//! before entering the HTTP server loop. Built with `cargo build` from
//! this crate inside `nix develop .#arm`.
//!
//! Board docs: `../../../docs/STRUCTURE.md`
//! Wiring/build: `../../../docs/SETUP.md`

#![no_std]
#![no_main]

use core::cell::RefCell;
use defmt::{info, unwrap};
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W5500;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::clocks::RoscRng;
use embassy_rp::flash::{Blocking as FlashBlocking, Flash};
use embassy_rp::gpio::{AnyPin, Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C1, SPI0, TRNG, UART1};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
use embassy_rp::trng::{self, Trng};
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma, i2c, Peri};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex;
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

use ubos_net::net;

/// Custom panic handler: leaves a breadcrumb in watchdog SCRATCH2, then spins.
///
/// The board has no debug probe in the field, so defmt panic output is not
/// observed. SCRATCH2 survives the warm reset the spin triggers (the armed
/// hardware watchdog fires once the feed stops), so the next clean boot reads
/// it and surfaces "last reset was a panic" over the REST reset-diag endpoint.
/// A panic before `watchdog_task` arms the watchdog spins forever — the same
/// early-boot window the previous `panic_probe` handler had.
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    ubos_ota::record_panic_breadcrumb();
    defmt::error!("firmware panic");
    // Spin without feeding the watchdog so the hardware watchdog fires and
    // resets the board, leaving SCRATCH2 set for the next boot to read.
    loop {
        cortex_m::asm::wfi();
    }
}

/// `HardFault` handler: defmt-logs the stacked exception frame, then leaves the
/// board to the hardware watchdog. cortex-m-rt's default handler only executes
/// `udf`, which a probe reports as a bare exception with an unreliable
/// backtrace; logging PC/LR gives the true faulting address.
#[cortex_m_rt::exception]
unsafe fn HardFault(frame: &cortex_m_rt::ExceptionFrame) -> ! {
    defmt::error!(
        "HardFault: pc={=u32:#x} lr={=u32:#x} xpsr={=u32:#x} r0={=u32:#x} r1={=u32:#x} r2={=u32:#x} r3={=u32:#x} r12={=u32:#x}",
        frame.pc(),
        frame.lr(),
        frame.xpsr(),
        frame.r0(),
        frame.r1(),
        frame.r2(),
        frame.r3(),
        frame.r12(),
    );
    loop {
        cortex_m::asm::wfi();
    }
}

mod config;
mod fan;
mod mining;
mod psu;
mod temperature;
mod uart_baudrate;

static CORE1_STACK: ConstStaticCell<embassy_rp::multicore::Stack<{ config::Core1::STACK_BYTES }>> =
    ConstStaticCell::new(embassy_rp::multicore::Stack::new());

/// CPU cycles per core-1 park poll: 15 000 cycles at 150 MHz `clk_sys` is
/// 100 µs per iteration.
const PARK_POLL_DELAY_CYCLES: u32 = 15_000;

static OTA_TARGET: StaticCell<
    ubos_ota::RpOtaTarget<{ config::Flash::SIZE }, PARK_POLL_DELAY_CYCLES>,
> = StaticCell::new();

/// Software ring buffer size per direction for the core-1 `BufferedUart`.
/// The RP2350 HW FIFO is 32 bytes; 1 KiB gives slack for ETH-ISR jitter
/// at the BM1370 hashboard baud rate.
const UART_RING_BUFFER_SIZE: usize = 1024;

/// Backing storage for the user-facing event ring, owned here so its size
/// follows this board's `EVENT_LOG_CAPACITY`. Placed in `.uninit`, which the
/// startup code does not zero, so the ring survives a warm reset; the validity
/// header in `RetainedRing` discards it after a cold boot leaves garbage.
#[unsafe(link_section = ".uninit")]
static mut EVENT_LOG_BACKING: ubos_events::RetainedRing<{ config::Board::EVENT_LOG_CAPACITY }> =
    ubos_events::RetainedRing::new();

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
async fn ntp_sync_task(stack: embassy_net::Stack<'static>) -> ! {
    net::run_ntp_sync(stack).await
}

#[embassy_executor::task]
async fn metrics_ingest_task() -> ! {
    ubos_metrics::metrics::run_ingest().await
}

#[embassy_executor::task]
async fn ota_watchdog_task() -> ! {
    ubos_api::run_ota_session_watchdog().await
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
/// `shared blocking Flash → SharedBlockingFlash → SyncFlash → FlashStorage`.
/// The one [`Flash`] driver lives in the `'static` mutex the OTA updater
/// also partitions, so the config store and OTA share the single FLASH
/// peripheral.
type BoardConfigStorage =
    FlashStorage<SyncFlash<ubos_ota::SharedBlockingFlash<{ config::Flash::SIZE }>>>;

#[embassy_executor::task]
async fn config_task(storage: BoardConfigStorage) -> ! {
    ubos_config::run(storage, config::BOARD).await
}

#[embassy_executor::task]
async fn cooling_task(fan: fan::BoardFan, tach: fan::BoardTach, thermal: ThermalHandle) -> ! {
    ubos_cooling::run_cooling(fan, tach, thermal).await
}

#[embassy_executor::task(pool_size = 3)]
async fn tach_task(pin: Peri<'static, AnyPin>, index: usize) -> ! {
    fan::run_tach(pin, index).await
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

#[embassy_executor::task]
async fn network_control_task(stack: embassy_net::Stack<'static>) -> ! {
    net::run_network_control(stack).await
}

ubos_http::http_worker_task!(
    api_worker_task,
    ubos_api::RestApiService,
    config::Board::API_HTTP_POOL
);

ubos_http::http_worker_task!(
    webui_worker_task,
    ubos_webui::DashboardService,
    config::Board::WEBUI_HTTP_POOL
);

ubos_http::http_worker_task!(
    metrics_worker_task,
    ubos_metrics::metrics::PrometheusMetricsService,
    config::Board::METRICS_HTTP_POOL
);

/// Board task shell around the shared stratum session loop. The loop body
/// lives in `ubos-firmware`; the `#[embassy_executor::task]` shell stays per
/// board because esp32s3 pins a different `embassy-executor` version than the
/// ARM boards, so the spawned task type must come from each board's executor.
#[embassy_executor::task]
async fn stratum_client_task(
    stack: embassy_net::Stack<'static>,
    sv2_prng: &'static mut ubos_firmware::ChaCha20Rng,
) -> ! {
    ubos_firmware::run_stratum_session(stack, sv2_prng).await
}

#[cortex_m_rt::entry]
fn main() -> ! {
    ubos_metrics::cpu_load::run_cortex_m_executor::<0, _>(|spawner| {
        spawner.spawn(unwrap!(boot(spawner)));
    })
}

#[embassy_executor::task]
// `items_after_statements` fires on the `StaticCell` declarations that follow
// `let` bindings inside the I2C and network scopes; static items must sit at
// statement position there, so the ordering is unavoidable.
#[allow(clippy::too_many_lines, clippy::items_after_statements)]
async fn boot(spawner: Spawner) {
    // --- 1. Core peripherals — always ----------------------------------------
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());
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

    // `rng` seeds the embassy-net stack. Declared here so it lives for the
    // full boot scope regardless of gating.
    let mut rng = RoscRng;

    // Build the single blocking FLASH driver and place it in a `'static`
    // mutex shared by the config store and the OTA updater. Both the
    // `SharedBlockingFlash` config backend and the OTA `BlockingPartition`s
    // borrow this one mutex, so the one FLASH peripheral serves both
    // consumers; the mutex serialises individual flash ops between them.
    static SHARED_FLASH: StaticCell<ubos_ota::SharedFlashMutex<{ config::Flash::SIZE }>> =
        StaticCell::new();
    let shared_flash: &'static ubos_ota::SharedFlashMutex<{ config::Flash::SIZE }> = SHARED_FLASH
        .init(BlockingMutex::new(RefCell::new(Flash::<
            _,
            FlashBlocking,
            { config::Flash::SIZE },
        >::new_blocking(
            hw.flash
        ))));

    // Capture the watchdog reset reason and OTA breadcrumb before anything
    // else touches the watchdog registers. This must happen before
    // `mark_booted` because `mark_booted` performs a flash op that feeds
    // the watchdog, and the reason register is read-once on RP2350 (a
    // debugger warm reset clears it). Reading here guarantees the values
    // reflect the most recent reset.
    {
        use embassy_rp::pac;
        use ubos_api::{OtaWritePhase, ResetReason};

        let watchdog = pac::WATCHDOG;
        let reason_reg = watchdog.reason().read();
        let scratch0 = watchdog.scratch0().read();
        let scratch1 = watchdog.scratch1().read();
        let scratch2 = watchdog.scratch2().read();

        // Determine the reset reason from the hardware bits. On RP2350 a
        // debugger warm reset also clears these bits, so the values are
        // reliable in production but may read as Clean after probe-rs
        // attaches; that is acceptable for diagnostic purposes.
        let reason = if reason_reg.timer() {
            ResetReason::WatchdogTimeout
        } else if reason_reg.force() {
            ResetReason::WatchdogForced
        } else {
            ResetReason::Clean
        };

        // The breadcrumb is active when SCRATCH0 holds the magic value.
        let ota_was_active = scratch0 == ubos_ota::OTA_BREADCRUMB_MAGIC;
        let ota_offset = if ota_was_active { scratch1 } else { 0 };

        // SCRATCH2 holds the write-phase code written by `write_chunk` or
        // the panic handler. Unknown values map to None.
        let write_phase = OtaWritePhase::from_u32(scratch2);

        info!(
            "reset reason: {:?}, ota_active={}, ota_offset={}, write_phase={:?}",
            reason as u8, ota_was_active, ota_offset, write_phase as u8,
        );

        ubos_api::publish_reset_diag(reason, ota_was_active, ota_offset, write_phase);

        // Clear SCRATCH0 and SCRATCH2 so a subsequent clean reset does not
        // re-report stale OTA state from this boot cycle.
        watchdog.scratch0().write_value(0u32);
        watchdog.scratch2().write_value(0u32);
    }

    // Log which image booted, so an OTA swap is observable on the defmt
    // console: the new version appears here after a successful update.
    info!("boot: firmware v{}", config::FIRMWARE_VERSION);

    // Confirm the running image to the bootloader before core 1 starts and
    // while flash is quiescent. A prior `Swap` state means the bootloader
    // just swapped in an OTA image; marking it booted stops a revert on the
    // next reset. The flash op pauses core 1, but core 1 is not up yet.
    ubos_ota::mark_booted(shared_flash);

    let ota_target = OTA_TARGET.init(ubos_ota::OtaEngine::new(
        ubos_ota::RpBackend::new(shared_flash),
        config::FIRMWARE_VERSION,
    ));

    // --- 2. Core1 + serial_task ------------------------------------------------
    // Pin the ubos-mining serial worker to core 1 so embassy-net on core 0
    // stays responsive. BufferedUart must be constructed on the core that
    // polls it — `init_buffers` unmasks the UART IRQ on the calling core's
    // NVIC, and the async write path pends the same IRQ to drain the SW TX
    // ring buffer into the HW FIFO. RP2350's Cortex-M33 has per-core NVICs,
    // so an unmask on one core and a pend on the other land on different
    // controllers and the FIFO is never written.
    let uart_parts = (hw.uart, hw.uart_tx, hw.uart_rx);
    embassy_rp::multicore::spawn_core1(hw.core1, CORE1_STACK.take(), move || {
        let miner_serial = {
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
            uart_baudrate::MinerUart::new(uart)
        };
        // Drive core 1 through `run_cortex_m_executor::<1, _>` so the
        // poll/wfe loop accumulates idle ticks into `IDLE_TICKS[1]`,
        // letting `run_cpu_load::<2>()` publish a real busy ratio for
        // this core instead of a constant 1.0.
        ubos_metrics::cpu_load::run_cortex_m_executor::<1, _>(|core1_spawner: Spawner| {
            core1_spawner.spawn(unwrap!(mining::serial_task(miner_serial)));
        });
    });

    // --- 3. Watchdog + LED — always -------------------------------------------
    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(Watchdog::new(
        hw.watchdog
    )))));
    spawner.spawn(unwrap!(reset_task()));

    // GPIO6 is active-low; start HIGH (LED off) so the initial blink
    // phase matches the active-low convention.
    let led = Output::new(hw.led, Level::High);
    spawner.spawn(unwrap!(led_blink_task(led)));

    // --- 4. Handle refs — always (compiled even when gated tasks are off) -----
    let thermal = ubos_thermal::handle();
    let manager_handle = ubos_manager::handle();
    let mining_handle = ubos_mining::handle();

    // I2C1 bus + PSU + mining + sensors.
    // The `SIC45x` rail is gated by SIC_EN (GPIO30, active-high, 10k pull-down
    // holds it off at power-on). The BoardPsu owns that pin; only the miner
    // task raises it, during bring-up, and drops it again when mining halts,
    // so an idle board never holds the chip powered in reset.
    use embassy_rp::i2c::{Config as I2cConfig, I2c};

    static I2C1_BUS: StaticCell<psu::Bus> = StaticCell::new();
    let i2c = I2c::new_async(hw.i2c1, hw.scl, hw.sda, Irqs, I2cConfig::default());
    let bus: &'static psu::Bus = I2C1_BUS.init(Mutex::new(i2c));

    // SIC_EN starts low; the rail stays off until the miner task enables it.
    // `_terminated` is intentionally discarded: nothing here polls the flag
    // to surface a termination event.
    let (board_psu, _terminated) = psu::init(bus, Output::new(hw.sic_en, Level::Low));
    spawner.spawn(unwrap!(psu::power_stats_task(board_psu)));

    // GPIO32: HIGH = enabled/out-of-reset; start LOW (in reset) until the
    // miner task is ready to bring up the hashboard.
    let reset = mining::BoardResetPin::new(Output::new(hw.hb_rst, Level::Low));
    spawner.spawn(unwrap!(mining::miner_task(board_psu, reset)));

    // Temperature and fan monitor via shared I2C bus.
    let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
    let hashboard_temp = temperature::new(temp_device);
    let board_fan = fan::BoardFan::new(hw.fan_pwm_slice, hw.fan_pwm_pin, hw.fan_pwm_pin_b);
    spawner.spawn(unwrap!(thermal_task(hashboard_temp, manager_handle)));
    spawner.spawn(unwrap!(manager_task(mining_handle)));
    spawner.spawn(unwrap!(cooling_task(board_fan, fan::BoardTach, thermal)));

    // --- 7. Config — always ---------------------------------------------------
    let config_storage = FlashStorage::new(
        SyncFlash(ubos_ota::SharedBlockingFlash::new(shared_flash)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(config_storage)));

    // --- 8. Board health tasks — always (no net/psu deps) ---------------------
    // Each tach stores into its FAN_RPM_SAMPLE slot; the cooling loop reads
    // all three and publishes one labeled board_fan_rpm series per fan.
    spawner.spawn(unwrap!(tach_task(hw.fan_tach_pin_a.into(), 0)));
    spawner.spawn(unwrap!(tach_task(hw.fan_tach_pin_b.into(), 1)));
    spawner.spawn(unwrap!(tach_task(hw.fan_tach_pin_c.into(), 2)));
    spawner.spawn(unwrap!(cpu_load_task()));

    // --- 9. Display — feature-gated on the cargo `display` feature -----------
    // Positioned before the network block so the display comes up early.
    // SH1106 OLED on SPI1: reset pulse, then async TX-only SPI with DMA.
    // The reset pin moves into display_task so it stays driven high for
    // the display's lifetime.
    #[cfg(feature = "display")]
    let (reset, display) = {
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
    spawner.spawn(unwrap!(display_task(display, reset)));

    // --- 10. Network -----------------------------------------------------------
    let mut spi_config = SpiConfig::default();
    spi_config.frequency = config::Spi::FREQUENCY;
    let spi = Spi::new(
        hw.spi, hw.sck, hw.mosi, hw.miso, hw.rx_dma, hw.tx_dma, Irqs, spi_config,
    );
    let chip_select = Output::new(hw.cs, Level::High);
    let w5500_int = Input::new(hw.w5500_int, Pull::Up);
    let w5500_reset = Output::new(hw.w5500_reset, Level::High);

    // Fold the RP2350 OTP chip id into a stable per-board MAC. The WIZnet
    // chip has no factory address, so the firmware supplies one.
    let mac_addr = {
        let chip_id = embassy_rp::otp::get_chipid().unwrap_or_else(|_| {
            defmt::warn!("OTP chip id read failed; MAC hash falls back to zero");
            0
        });
        ubos_net::mac::derive_mac(
            ubos_net::mac::DUMMY_OUI,
            config::Board::MAC_FAMILY,
            &chip_id.to_be_bytes(),
        )
    };
    info!(
        "mac {=u8:02x}:{=u8:02x}:{=u8:02x}:{=u8:02x}:{=u8:02x}:{=u8:02x}",
        mac_addr[0], mac_addr[1], mac_addr[2], mac_addr[3], mac_addr[4], mac_addr[5],
    );
    ubos_net::net::handle().set_mac(mac_addr);
    static STATE: StaticCell<State<8, 8>> = StaticCell::new();
    let state = STATE.init(State::<8, 8>::new());
    let (device, runner) = match embassy_net_wiznet::new(
        mac_addr,
        state,
        ExclusiveDevice::new(spi, chip_select, Delay),
        w5500_int,
        w5500_reset,
    )
    .await
    {
        Ok(pair) => pair,
        Err(error) => {
            // A correctly wired W5500 does not fail here. On a hardware fault,
            // park instead of panicking: a panic would reset-loop forever with
            // no network to surface the breadcrumb, whereas parking keeps the
            // board alive (LED heartbeat, watchdog fed) and probe-debuggable.
            defmt::error!(
                "W5500 init failed: {:?}; parking with no network",
                defmt::Debug2Format(&error)
            );
            core::future::pending().await
        }
    };
    spawner.spawn(unwrap!(ethernet_task(runner)));

    let seed = rng.next_u64();

    // 16 was one short of even a single pool generation; 24 matches the
    // rp2350 board running the same pool sizes. The assert is shared so
    // every board gets the same smoltcp socket-exhaustion guarantee.
    const STACK_SOCKETS: usize = 24;
    ubos_net::assert_stack_sockets!(
        STACK_SOCKETS,
        config::Board::WEBUI_HTTP_POOL,
        config::Board::API_HTTP_POOL,
        config::Board::METRICS_HTTP_POOL,
    );
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

    static RESOURCES: StaticCell<StackResources<STACK_SOCKETS>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        device,
        net_config,
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(unwrap!(net_task(runner)));

    // Seed the session CSPRNG and the SV2 noise-handshake PRNG from the
    // RP2350 hardware TRNG, not the ring-oscillator-bit `RoscRng`. The TRNG
    // runs three built-in entropy health checks per draw and is NIST-aligned;
    // `RoscRng` takes one ROSC randombit per read with no oracle. The TRNG is
    // only used at boot, so the peripheral is dropped right after the seeds
    // are taken.
    let mut csprng_seed = [0u8; 32];
    let mut sv2_seed = [0u8; 32];
    {
        // Datasheet § 12.12.2: sample counts of 20-25 minimise generation
        // time but fail the on-die NIST entropy checks often enough to
        // log every boot. 100 trades a few hundred extra microseconds for
        // a quiet boot; the TRNG is only sampled at boot for the two seeds.
        let mut trng_config = trng::Config::default();
        trng_config.sample_count = 100;
        let mut trng = Trng::new(hw.trng, Irqs, trng_config);
        trng.fill_bytes(&mut csprng_seed).await;
        trng.fill_bytes(&mut sv2_seed).await;
    }
    ubos_auth::csprng::seed_csprng(csprng_seed).await;
    let sv2_prng = ubos_firmware::seed_sv2_prng(sv2_seed);

    spawner.spawn(unwrap!(ntp_sync_task(stack)));
    spawner.spawn(unwrap!(metrics_ingest_task()));

    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(miner_metrics_task(mining_handle)));
    spawner.spawn(unwrap!(stratum_metrics_task()));
    spawner.spawn(unwrap!(hashrate_metrics_task()));

    info!("waiting for dhcp");
    let config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", config.address.address());

    // Core 1 hosts serial_task (ubos-mining backend); see spawn_core1 above.
    spawner.spawn(unwrap!(stratum_client_task(stack, sv2_prng)));

    // Register the OTA target before spawning the REST API workers so the
    // OTA endpoints are available from the first request. The &'static mut
    // binding was established early in boot, before spawn_core1.
    ubos_api::register_ota_target(ota_target).await;

    // Reclaim abandoned chunk-ack OTA sessions so a client that vanishes
    // mid-upload does not strand the board with mining stopped.
    spawner.spawn(unwrap!(ota_watchdog_task()));
    spawner.spawn(unwrap!(network_control_task(stack)));

    // REST API on :8080 — seed credentials once, then spawn one worker
    // task per pool slot so each has its own static TaskStorage.
    ubos_api::seed_api_credentials().await;
    static API_SERVICE: StaticCell<ubos_api::RestApiService> = StaticCell::new();
    let api_service: &'static ubos_api::RestApiService =
        API_SERVICE.init(ubos_api::RestApiService::new(
            config::Board::NAME,
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
        config::Board::API_HTTP_POOL,
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
        config::Board::WEBUI_HTTP_POOL,
        ubos_webui::consts::Http
    );

    static METRICS_SERVICE: ubos_metrics::metrics::PrometheusMetricsService =
        ubos_metrics::metrics::PrometheusMetricsService;
    ubos_http::serve_http_pool!(
        spawner,
        stack,
        metrics_worker_task,
        &METRICS_SERVICE,
        config::Board::METRICS_HTTP_POOL,
        ubos_metrics::consts::Metrics
    );
}
