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

//! Firmware entry point for the W6100-EVB-Pico2 (RP2350 + W6100) with the
//! ubos-mining backend.
//!
//! Initialises SPI at 33 MHz, brings the W6100 up via `embassy-net-wiznet`,
//! starts the embassy-net stack with DHCP, and spawns the watchdog, LED
//! blink, ethernet runner, NTP sync, and ubos-mining mining tasks before
//! entering the HTTP server loop. Built with `cargo build` from this
//! crate inside `nix develop .#arm`.
//!
//! Board docs: `../../../docs/OVERVIEW.md`
//! Wiring/build: `../../../docs/SETUP.md`
//! WFE wake-storm investigation: `../../../docs/rp2350-wake-storm.md`

#![no_std]
#![no_main]

use core::cell::RefCell;
use defmt::{info, unwrap};
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W6100;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::flash::{Blocking as FlashBlocking, Flash};
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{Config as I2cConfig, I2c};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C1, SPI0, TRNG, UART1};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
use embassy_rp::trng;
use embassy_rp::uart::BufferedInterruptHandler;
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

use ubos_net::net;

/// CPU cycles per core-1 park poll: 15 000 cycles at 150 MHz `clk_sys` is
/// 100 µs per iteration.
const PARK_POLL_DELAY_CYCLES: u32 = 15_000;

mod config;
mod mining;
mod psu;

static CORE1_STACK: ConstStaticCell<embassy_rp::multicore::Stack<{ config::Core1::STACK_BYTES }>> =
    ConstStaticCell::new(embassy_rp::multicore::Stack::new());

static OTA_TARGET: StaticCell<
    ubos_ota::RpOtaTarget<{ config::Flash::SIZE }, PARK_POLL_DELAY_CYCLES>,
> = StaticCell::new();

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
    TRNG_IRQ => trng::InterruptHandler<TRNG>;
});

type EthernetRunner = Runner<
    'static,
    W6100,
    ExclusiveDevice<Spi<'static, SPI0, Async>, Output<'static>, Delay>,
    Input<'static>,
    Output<'static>,
>;

ubos_firmware::ethernet_task!(EthernetRunner);

ubos_firmware::net_task!(Device<'static>);

ubos_core::watchdog_task!(ubos_rp::BoardWatchdog);

ubos_led::led_blink_task!(Output<'static>);

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

ubos_thermal::thermal_task!(
    ubos_thermal::SensorArray<psu::Device>,
    ubos_thermal::NoAmbient
);

ubos_manager::manager_task!();

/// Storage backend type the [`config_task`] hands to
/// [`ubos_config::run`]. The chain is
/// `shared blocking Flash → SharedBlockingFlash → SyncFlash → FlashStorage`.
/// The one [`Flash`] driver lives in the `'static` mutex the OTA updater
/// also partitions, so the config store and OTA share the single FLASH
/// peripheral.
type BoardConfigStorage =
    FlashStorage<SyncFlash<ubos_ota::SharedBlockingFlash<{ config::Flash::SIZE }>>>;

ubos_config::config_task!(BoardConfigStorage, config::BOARD, &config::MINING_INFO);

ubos_cooling::cooling_task!(ubos_rp::fan::BoardFan, ubos_rp::fan::BoardTach);

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    ubos_metrics::cpu_load::run_cpu_load::<2>().await
}

ubos_firmware::stratum_metrics_task!(POOL_SLOTS);

ubos_mining::pool_weights_task!(&mining::CHAINS);

ubos_mining::miner_metrics_task!(POOL_SLOTS);

#[cfg(feature = "display")]
type OledDisplay = Sh1106<
    ExclusiveDevice<Spi<'static, SPI1, Async>, Output<'static>, embedded_hal_bus::spi::NoDelay>,
    Output<'static>,
>;

#[cfg(feature = "display")]
ubos_display::display_task!(OledDisplay, Output<'static>);

ubos_core::reset_task!(cortex_m::peripheral::SCB::sys_reset());

ubos_api::sync_credentials_task!();

ubos_firmware::serve_all!(config::MACHINE, &config::MINING_INFO, &config::BOUNDS);

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
/// `#[embassy_executor::task]` shell stays per board because the attribute
/// expands a typed task pool sized by this board's `POOL_SLOTS`, and it
/// rejects the generic function a shared shell would have to be.
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

    // Safety: called once at boot before any task runs. EVENT_LOG_BACKING lives
    // in .uninit, so the startup code does not zero it and a warm reset
    // preserves its contents.
    ubos_firmware::events::install(unsafe { &mut *core::ptr::addr_of_mut!(EVENT_LOG_BACKING) });

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

    ubos_rp::capture_reset_diag();

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

    // BufferedUart must be constructed on the core that polls it, which the
    // macro does inside `spawn_core1`. This board drives core 1 through
    // `run_cortex_m_executor::<1, _>` so the poll/wfe loop accumulates idle
    // ticks into `IDLE_TICKS[1]`, letting `run_cpu_load::<2>()` publish a real
    // busy ratio for this core instead of a constant 1.0.
    ubos_rp::spawn_miner_core1!(
        core1 = hw.core1,
        stack = CORE1_STACK,
        irqs = Irqs,
        uart = (hw.uart, hw.uart_tx, hw.uart_rx),
        run = |miner_serial| {
            ubos_metrics::cpu_load::run_cortex_m_executor::<1, _>(|core1_spawner: Spawner| {
                core1_spawner.spawn(unwrap!(mining::serial_task(
                    &mining::CHAINS[0],
                    miner_serial,
                    &ASIC_SESSION,
                    ubos_asic::Bm13xxDriver::new(config::CHIP)
                )));
            });
        },
    );

    spawner.spawn(unwrap!(watchdog_task(ubos_rp::BoardWatchdog::new(
        Watchdog::new(hw.watchdog),
        config::Watchdog::TIMEOUT,
    ))));
    spawner.spawn(unwrap!(reset_task()));

    let led = Output::new(hw.led, Level::Low);
    spawner.spawn(unwrap!(led_blink_task(led)));

    // Hashboard plug-detect input on GPIO3, pulled up. `boot` returns once the
    // listeners are spawned, and dropping the input would release the pad
    // and clear the pull-up, so the binding is leaked: the pad keeps its
    // configuration for the firmware's lifetime and nothing else claims it.
    core::mem::forget(Input::new(hw.hb_plug, Pull::Up));

    let thermal = ubos_thermal::handle();

    let manager_handle = ubos_manager::handle();

    let mining_handle = ubos_mining::handle();

    // BMM101 hashboard PSU on I2C1: shared bus, PSU watchdog, power
    // stats, and the miner bring-up task all run on core 0 so the I2C
    // bus mutex can stay NoopRawMutex.
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
            ubos_api::publish_hashboard_id(serial);
        }

        let board_psu = psu::init(bus);
        spawner.spawn(unwrap!(psu::watchdog_task(board_psu)));
        spawner.spawn(unwrap!(psu::power_stats_task(board_psu)));
        let reset = mining::BoardResetPin::new(Output::new(hw.hb_rst, Level::Low));
        spawner.spawn(unwrap!(mining::miner_task(
            board_psu,
            ubos_mining::serial::Serial::wire(&mining::CHAINS, [reset])
        )));

        // Temperature and fan monitor via shared I2C bus.
        let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
        let hashboard_temp = ubos_thermal::SensorArray::new(temp_device, config::MACHINE.temp);
        let board_fan =
            ubos_rp::fan::BoardFan::new(hw.fan_pwm_slice, hw.fan_pwm_pin, config::Fan::PWM_TOP);
        let board_tach = ubos_rp::fan::BoardTach::new(
            hw.fan_tach_slice,
            hw.fan_tach_pin,
            config::Fan::TACH_PULSES_PER_REV,
        );
        spawner.spawn(unwrap!(thermal_task(
            hashboard_temp,
            ubos_thermal::NoAmbient,
            manager_handle
        )));
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

    let mac_addr = ubos_net::mac::publish_mac(
        config::MACHINE.control_board.mac_family,
        &ubos_rp::boot::chip_id(),
    );
    static STATE: StaticCell<State<8, 8>> = StaticCell::new();
    let state = STATE.init(State::<8, 8>::new());
    let (device, runner) = ubos_rp::boot::wiznet_or_reset(
        mac_addr,
        state,
        ExclusiveDevice::new(spi, chip_select, Delay),
        phy_int,
        phy_reset,
    )
    .await;
    spawner.spawn(unwrap!(ethernet_task(runner)));

    let seeds = ubos_rp::boot::seeds_from_trng(hw.trng, Irqs).await;

    let (net_config, mdns_hostname) = ubos_firmware::net::bring_up::<
        { config::Board::MDNS_HOSTNAME_BASE.len() },
    >(config::Board::MDNS_HOSTNAME_BASE, mac_addr[5])
    .await;

    // Socket-set capacity for the embassy-net stack. Seventeen sockets
    // are held continuously: DHCPv4, DNS, the stratum client, the mDNS
    // responder, 8 HTTP listener sockets on :80, and 5 metrics listener
    // sockets on :8081. NTP opens a transient UDP socket every 60 s, and
    // closed TCP connections linger in TIME_WAIT until smoltcp reaps
    // them. 24 provides headroom for concurrent connections and
    // TIME_WAIT, and holds more of it than it did before the merge.
    const STACK_SOCKETS: usize = 24;
    ubos_net::assert_stack_sockets!(
        STACK_SOCKETS,
        config::MACHINE.control_board.http_pool,
        config::MACHINE.control_board.metrics_http_pool,
    );
    static RESOURCES: StaticCell<StackResources<STACK_SOCKETS>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        device,
        net_config,
        RESOURCES.init(StackResources::new()),
        seeds.net,
    );
    spawner.spawn(unwrap!(net_task(runner)));

    ubos_auth::csprng::seed_csprng(seeds.csprng);
    let stratum_shared =
        ubos_firmware::init_stratum(seeds.sv2, config::CHIP.chip_descriptor().rolls_ntime);

    spawner.spawn(unwrap!(ntp_sync_task(stack)));
    spawner.spawn(unwrap!(metrics_ingest_task()));

    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(cpu_load_task()));
    spawner.spawn(unwrap!(miner_metrics_task(mining_handle)));
    spawner.spawn(unwrap!(stratum_metrics_task()));
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
    // OTA endpoints are available from the first request. The &'static mut
    // binding was established early in boot, before spawn_core1.
    ubos_api::register_ota_target(ota_target).await;

    // Reclaim abandoned chunk-ack OTA sessions so a client that vanishes
    // mid-upload does not strand the board with mining stopped.
    spawner.spawn(unwrap!(ota_watchdog_task()));
    spawner.spawn(unwrap!(network_control_task(stack)));

    serve_all(spawner, stack, thermal, manager_handle).await;
}
