// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Firmware entry point for the W5500-EVB-Pico (RP2040 + W5500) with the
//! ubos-mining backend.
//!
//! Initialises SPI at 33 MHz, brings the W5500 up via `embassy-net-wiznet`,
//! starts the embassy-net stack with DHCP, and spawns the watchdog, LED
//! blink, ethernet runner, NTP sync, and ubos-mining mining tasks before
//! entering the HTTP server loop. Built with `cargo build` from this
//! crate inside `nix develop .#arm`.
//!
//! Board docs: `../../../docs/OVERVIEW.md`
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
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{Config as I2cConfig, I2c};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C1, SPI0, UART1};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma, i2c};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Delay;
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::{ConstStaticCell, StaticCell};
use ubos_config::{FlashStorage, SyncFlash};
#[cfg(feature = "display")]
use ubos_display::sh1106::{Rotation, Sh1106};
use ubos_eeprom::HashboardEeprom as _;

/// Custom panic handler: records a breadcrumb in SCRATCH2 and spins.
///
/// The breadcrumb distinguishes a panic-induced watchdog reset from a
/// slow-flash watchdog timeout: both set the watchdog TIMER reason bit,
/// but only a panic sets SCRATCH2 to `WRITE_PHASE_PANIC`. The probe is
/// detached during OTA uploads so defmt output is not observed; the
/// breadcrumb is the only durable record that survives the warm reset.
/// Spinning without feeding the watchdog causes the 2 s hardware watchdog
/// to fire, which resets the board with the TIMER reason bit set, and
/// SCRATCH2 carries the panic marker to the next boot. This holds only
/// once `watchdog_task` has armed the watchdog; a panic in early boot
/// before arming spins forever, the same window the prior `panic_probe`
/// handler had.
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

/// `HardFault` handler: defmt-logs the stacked exception frame.
///
/// A `HardFault` is distinct from a Rust panic and from a watchdog
/// timeout. cortex-m-rt's default handler executes `udf`, which probe-rs
/// reports only as a bare "Exception" with an unreliable unwound
/// backtrace. Logging the stacked PC and LR gives the true faulting
/// address. The frame is emitted over defmt-rtt before the board is
/// left to the hardware watchdog, so `cargo run` captures it even when
/// the unwinder cannot.
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
    // Spin without feeding the watchdog so the hardware watchdog resets the
    // board after the frame has been emitted.
    loop {
        cortex_m::asm::wfi();
    }
}

use ubos_net::net;

/// CPU cycles per core-1 park poll: 12 500 cycles at 125 MHz `clk_sys` is
/// 100 µs per iteration.
const PARK_POLL_DELAY_CYCLES: u32 = 12_500;

mod config;
mod fan;
mod mining;
mod psu;
mod temperature;
mod uart_baudrate;

static CORE1_STACK: ConstStaticCell<embassy_rp::multicore::Stack<{ config::Core1::STACK_BYTES }>> =
    ConstStaticCell::new(embassy_rp::multicore::Stack::new());

static OTA_TARGET: StaticCell<
    ubos_ota::RpOtaTarget<{ config::Flash::SIZE }, PARK_POLL_DELAY_CYCLES>,
> = StaticCell::new();

/// Software ring buffer size per direction for the core-1 `BufferedUart`.
/// The RP2040 HW FIFO is 32 bytes; 1 KiB gives slack for ETH-ISR jitter
/// at the BM1362 hashboard baud rate.
const UART_RING_BUFFER_SIZE: usize = 1024;

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
    DMA_IRQ_0 =>
        dma::InterruptHandler<DMA_CH0>,
        dma::InterruptHandler<DMA_CH1>,
        #[cfg(feature = "display")]
        dma::InterruptHandler<DMA_CH2>;
    I2C1_IRQ => i2c::InterruptHandler<I2C1>;
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

impl ubos_core::watchdog::Watchdog for BoardWatchdog {
    fn init(&mut self) {
        self.0.start(config::Watchdog::TIMEOUT);
    }

    fn feed(&mut self) {
        self.0.feed(config::Watchdog::TIMEOUT);
    }
}

ubos_firmware::ethernet_task!(EthernetRunner);

ubos_firmware::net_task!(Device<'static>);

ubos_core::watchdog_task!(BoardWatchdog);

ubos_core::led_blink_task!(Output<'static>);

ubos_net::ntp_sync_task!();

ubos_metrics::metrics_ingest_task!();

ubos_api::ota_watchdog_task!();

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

ubos_thermal::thermal_task!(temperature::HashboardTemperature);

ubos_manager::manager_task!();

/// Storage backend type the [`config_task`] hands to
/// [`ubos_config::run`]. The chain is
/// `shared blocking Flash → SharedBlockingFlash → SyncFlash → FlashStorage`.
/// The one [`Flash`] driver lives in the `'static` mutex the OTA updater
/// also partitions, so the config store and OTA share the single FLASH
/// peripheral.
type BoardConfigStorage =
    FlashStorage<SyncFlash<ubos_ota::SharedBlockingFlash<{ config::Flash::SIZE }>>>;

ubos_config::config_task!(BoardConfigStorage, config::BOARD);

ubos_cooling::cooling_task!(fan::BoardFan, fan::BoardTach);

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    ubos_metrics::cpu_load::run_cpu_load::<2>().await
}

ubos_firmware::stratum_metrics_task!(POOL_SLOTS);

ubos_mining::pool_weights_task!();

ubos_mining::miner_metrics_task!(POOL_SLOTS);

ubos_metrics::hashrate_metrics_task!();

#[cfg(feature = "display")]
type OledDisplay = Sh1106<
    ExclusiveDevice<Spi<'static, SPI1, Async>, Output<'static>, embedded_hal_bus::spi::NoDelay>,
    Output<'static>,
>;

#[cfg(feature = "display")]
ubos_display::display_task!(OledDisplay, Output<'static>);

ubos_core::reset_task!(cortex_m::peripheral::SCB::sys_reset());

ubos_api::sync_credentials_task!();

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

ubos_net::network_control_task!();

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
    let mut rng = RoscRng;

    // Read the flash unique id before moving the peripheral into the shared
    // mutex; `blocking_unique_id` is an inherent method on `Flash` and is
    // not reachable through the `NorFlash` trait exposed by
    // `SharedBlockingFlash`. Derive the MAC from the id, then move the
    // same `Flash` into the mutex so all later flash consumers (config
    // store and OTA updater) share the one driver.
    let mut flash = Flash::<_, FlashBlocking, { config::Flash::SIZE }>::new_blocking(hw.flash);
    let mac_addr = {
        let mut uid = [0u8; 8];
        if flash.blocking_unique_id(&mut uid).is_err() {
            defmt::warn!("flash unique id read failed; MAC hash falls back to zero");
        }
        ubos_net::mac::derive_mac(
            ubos_net::mac::DUMMY_OUI,
            config::MACHINE.control_board.mac_family,
            &uid,
        )
    };
    info!(
        "mac {=u8:02x}:{=u8:02x}:{=u8:02x}:{=u8:02x}:{=u8:02x}:{=u8:02x}",
        mac_addr[0], mac_addr[1], mac_addr[2], mac_addr[3], mac_addr[4], mac_addr[5],
    );
    ubos_net::net::handle().set_mac(mac_addr);

    // Place the flash driver in the shared mutex. Both the OTA target and
    // the config store reference this mutex, serialising individual flash
    // ops through the one FLASH peripheral.
    static SHARED_FLASH: StaticCell<ubos_ota::SharedFlashMutex<{ config::Flash::SIZE }>> =
        StaticCell::new();
    let shared_flash: &'static ubos_ota::SharedFlashMutex<{ config::Flash::SIZE }> =
        SHARED_FLASH.init(BlockingMutex::new(RefCell::new(flash)));

    // Capture the watchdog reset reason and OTA breadcrumb before anything
    // else touches the watchdog registers. This must happen before
    // `mark_booted` because `mark_booted` performs a flash op that feeds
    // the watchdog, and the reason register is read-once (a debugger warm
    // reset clears it). Reading here guarantees the values reflect the
    // most recent reset.
    {
        use embassy_rp::pac;
        use ubos_api::{OtaWritePhase, ResetReason};

        let watchdog = pac::WATCHDOG;
        let reason_reg = watchdog.reason().read();
        let scratch0 = watchdog.scratch0().read();
        let scratch1 = watchdog.scratch1().read();
        let scratch2 = watchdog.scratch2().read();

        // Determine the reset reason from the hardware bits. A debugger
        // warm reset also clears these bits, so the values are reliable
        // in production but may read as Clean after probe-rs attaches;
        // that is acceptable for diagnostic purposes.
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
        config::Ota::MAX_IMAGE_BYTES,
    ));

    // Pin the ubos-mining serial worker to core 1 so embassy-net on core 0 stays
    // responsive. BufferedUart must be constructed on the core that polls
    // it — `init_buffers` unmasks the UART IRQ on the calling core's NVIC,
    // and the async write path pends the same IRQ to drain the SW TX ring
    // buffer into the HW FIFO. RP2040's Cortex-M0+ has per-core NVICs, so
    // an unmask on one core and a pend on the other land on different
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
            core1_spawner.spawn(unwrap!(mining::serial_task(miner_serial, &ASIC_SESSION)));
        });
    });

    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(Watchdog::new(
        hw.watchdog
    )))));
    spawner.spawn(unwrap!(reset_task()));

    let led = Output::new(hw.led, Level::Low);
    spawner.spawn(unwrap!(led_blink_task(led)));

    // Hashboard plug-detect input (GPIO3, pull-up). Hold the binding at
    // function scope so the GPIO stays driven for the lifetime of the
    // firmware; dropping `Input` would clear the pad and pull setting.
    let _hb_plug = Input::new(hw.hb_plug, Pull::Up);

    // BMM101 hashboard PSU on I2C1: shared bus, PSU watchdog, power
    // stats, and the miner bring-up task all run on core 0 so the I2C
    // bus mutex can stay NoopRawMutex.
    let thermal = ubos_thermal::handle();
    let manager_handle = ubos_manager::handle();

    let mining_handle = ubos_mining::handle();

    {
        static I2C1_BUS: StaticCell<psu::Bus> = StaticCell::new();
        let i2c = I2c::new_async(hw.i2c, hw.scl, hw.sda, Irqs, I2cConfig::default());
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
        let reset = mining::BoardResetPin::new(Output::new(hw.hb_rst, Level::Low));
        spawner.spawn(unwrap!(mining::miner_task(board_psu, reset)));

        // Temperature and fan monitor via shared I2C bus.
        let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
        let hashboard_temp = temperature::new(temp_device);
        let board_fan = fan::BoardFan::new(hw.fan_pwm_slice, hw.fan_pwm_pin);
        let board_tach = fan::BoardTach::new(hw.fan_tach_slice, hw.fan_tach_pin);
        spawner.spawn(unwrap!(thermal_task(hashboard_temp, manager_handle)));
        spawner.spawn(unwrap!(manager_task(mining_handle)));
        spawner.spawn(unwrap!(cooling_task(board_fan, board_tach, thermal)));
    }

    let config_storage = FlashStorage::new(
        SyncFlash(ubos_ota::SharedBlockingFlash::new(shared_flash)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(config_storage)));

    let mut spi_config = SpiConfig::default();
    spi_config.frequency = config::Spi::FREQUENCY;
    let spi = Spi::new(
        hw.spi, hw.sck, hw.mosi, hw.miso, hw.rx_dma, hw.tx_dma, Irqs, spi_config,
    );
    let chip_select = Output::new(hw.cs, Level::High);
    let phy_int = Input::new(hw.phy_int, Pull::Up);
    let phy_reset = Output::new(hw.phy_reset, Level::High);

    static STATE: StaticCell<State<8, 8>> = StaticCell::new();
    let state = STATE.init(State::<8, 8>::new());
    let (device, runner) = embassy_net_wiznet::new(
        mac_addr,
        state,
        ExclusiveDevice::new(spi, chip_select, Delay),
        phy_int,
        phy_reset,
    )
    .await
    // Initialization succeeds when the chip is wired correctly and responds on SPI.
    .expect("BUG: W5500 initialization failed");
    spawner.spawn(unwrap!(ethernet_task(runner)));

    let seed = rng.next_u64();

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
    // responder, 1 webui listener socket on :80, and 1 REST API
    // listener socket on :8080, plus the metrics listener on :8081.
    // NTP opens a transient UDP socket every 60 s, and closed TCP
    // connections linger in TIME_WAIT until smoltcp reaps them. 16
    // provides headroom. The RP2040 is the tightest board: a 5/5 HTTP
    // pool overflows SRAM by ~50 KiB, so it stays at 1/1 listeners.
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

    // Seed the session CSPRNG. RP2040 has no hardware TRNG, so the seed
    // comes from RoscRng (ring-oscillator bits with no health checks).
    // These boards are demonstration hardware per CLAUDE.md.
    let mut csprng_seed = [0u8; 32];
    rng.fill_bytes(&mut csprng_seed);
    ubos_auth::csprng::seed_csprng(csprng_seed).await;

    // Independent entropy stream for the SV2 noise handshake, seeded from
    // the same RoscRng source as the session CSPRNG.
    let mut sv2_seed = [0u8; 32];
    rng.fill_bytes(&mut sv2_seed);
    let stratum_shared = ubos_firmware::init_stratum(sv2_seed);

    spawner.spawn(unwrap!(ntp_sync_task(stack)));
    spawner.spawn(unwrap!(metrics_ingest_task()));

    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(cpu_load_task()));
    spawner.spawn(unwrap!(miner_metrics_task(mining_handle)));
    spawner.spawn(unwrap!(stratum_metrics_task()));
    spawner.spawn(unwrap!(hashrate_metrics_task()));
    spawner.spawn(unwrap!(pool_weights_task()));

    // SH1106 OLED on SPI1: reset pulse, then async TX-only SPI with DMA.
    // The reset pin is moved into display_task so it stays driven high for the display's lifetime.
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
        let mut display = Sh1106::new(spi_device, display_dc, Rotation::Normal);
        // Display is reset and SPI is valid; init cannot fail.
        display.init().expect("BUG: SH1106 init failed");
        (rst, display)
    };

    #[cfg(feature = "display")]
    spawner.spawn(unwrap!(display_task(display, reset)));

    info!("waiting for dhcp");
    let config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", config.address.address());

    // Core 1 hosts serial_task (ubos-mining backend); see spawn_core1 above.
    for slot in 0..POOL_SLOTS {
        spawner.spawn(unwrap!(stratum_client_task(
            stack,
            slot,
            &ASIC_SESSION,
            stratum_shared
        )));
    }

    // Register the OTA target before spawning the REST API workers so the
    // OTA endpoints are available from the first request.
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
