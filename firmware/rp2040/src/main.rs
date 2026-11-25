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

//! Firmware entry point for the W6100-EVB-Pico (RP2040 + W6100).
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
use embassy_net_wiznet::chip::W6100;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::flash::{Blocking as FlashBlocking, Flash};
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{Config as I2cConfig, I2c};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C1, SPI0, UART1};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
use embassy_rp::uart::BufferedInterruptHandler;
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma};
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
use ubos_rp::i2c;

use ubos_net::net;

/// CPU cycles per core-1 park poll: 12 500 cycles at 125 MHz `clk_sys` is
/// 100 µs per iteration.
const PARK_POLL_DELAY_CYCLES: u32 = 12_500;

mod config;
mod mining;
mod psu;

static CORE1_STACK: ConstStaticCell<embassy_rp::multicore::Stack<{ config::Core1::STACK_BYTES }>> =
    ConstStaticCell::new(embassy_rp::multicore::Stack::new());

static OTA_TARGET: StaticCell<
    ubos_ota::RpOtaTarget<{ config::Flash::SIZE }, PARK_POLL_DELAY_CYCLES>,
> = StaticCell::new();

/// `.uninit` is not zeroed by the startup code, so the ring survives a warm
/// reset.
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
    I2C1_IRQ => embassy_rp::i2c::InterruptHandler<I2C1>;
    UART1_IRQ => BufferedInterruptHandler<UART1>;
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

ubos_api::ota_watchdog_task!();

const MDNS_SERVICES: &[&ubos_mdns::ServiceEntry] =
    ubos_firmware::mdns_services![&ubos_webui::WEBUI_SERVICE, &ubos_api::REST_API_SERVICE];

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
    ubos_thermal::SensorArray<{ config::MACHINE.sensor_count() }, i2c::Device>,
    ubos_thermal::NoAmbient,
    ubos_thermal::NoMcu
);

ubos_manager::manager_task!();

type BoardConfigStorage =
    FlashStorage<SyncFlash<ubos_ota::SharedBlockingFlash<{ config::Flash::SIZE }>>>;

ubos_config::config_task!(
    BoardConfigStorage,
    &config::BOARD.defaults,
    &config::MINING_INFO,
    FEE_SLOT
);

ubos_cooling::cooling_task!(ubos_rp::fan::BoardFan, ubos_rp::fan::BoardTach);

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    ubos_metrics::cpu_load::run_cpu_load::<2>().await
}

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

ubos_firmware::serve_all!(
    config::MACHINE.control_board.http_pool_sizes,
    &config::MINING_INFO,
    &config::BOUNDS,
    config::MAX_METRICS,
    config::METRICS_RESPONSE_BYTES,
);

ubos_net::network_control_task!();

/// Three, because a fourth session task overflows the RAM. With the fee
/// switch on, the fee takes the last of them.
pub(crate) const POOL_SLOTS: usize = ubos_firmware::MAX_POOLS;

/// `None` on an off build, so the source reads the same in both states.
pub(crate) const FEE_SLOT: Option<ubos_config::FeeSlot> =
    ubos_config::FeeSlot::last_of(config::FEE_POOL, POOL_SLOTS);

static ASIC_SESSION: ubos_firmware::AsicSession<POOL_SLOTS> = ubos_firmware::AsicSession::new();

static PARK: ubos_mining::Park = ubos_mining::Park::new();

/// The shell stays per board because the attribute expands a typed task pool
/// sized by this board's `POOL_SLOTS`.
#[embassy_executor::task(pool_size = POOL_SLOTS)]
async fn stratum_client_task(
    stack: embassy_net::Stack<'static>,
    slot: usize,
    session: &'static ubos_firmware::AsicSession<POOL_SLOTS>,
    shared: ubos_firmware::StratumShared,
    config: ubos_config::Handle,
) -> ! {
    ubos_firmware::run_stratum_session(
        stack,
        slot,
        session,
        shared,
        config,
        &mut ubos_firmware::NoExtension,
    )
    .await
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

    let (runners, handles) = ubos_firmware::Handles::take();

    // Safety: called once at boot, before any task that records an event runs.
    ubos_firmware::events::install(unsafe { &mut *core::ptr::addr_of_mut!(EVENT_LOG_BACKING) });

    let mut flash = Flash::<_, FlashBlocking, { config::Flash::SIZE }>::new_blocking(hw.flash);
    let mac_addr = runners.net.publish_mac(
        config::MACHINE.control_board.mac_family,
        &ubos_rp::boot::flash_unique_id(&mut flash),
    );

    static SHARED_FLASH: StaticCell<ubos_ota::SharedFlashMutex<{ config::Flash::SIZE }>> =
        StaticCell::new();
    let shared_flash: &'static ubos_ota::SharedFlashMutex<{ config::Flash::SIZE }> =
        SHARED_FLASH.init(BlockingMutex::new(RefCell::new(flash)));

    ubos_rp::capture_reset_diag();

    info!("boot: firmware v{}", config::FIRMWARE_VERSION);

    // Runs before core 1 starts: the flash op pauses the other core.
    ubos_ota::mark_booted(shared_flash);

    let ota_target = OTA_TARGET.init(ubos_ota::OtaEngine::new(
        ubos_ota::RpBackend::new(shared_flash),
        &PARK,
        config::FIRMWARE_VERSION,
        config::Ota::MAX_IMAGE_BYTES,
    ));

    ubos_rp::spawn_miner_core1!(
        core1 = hw.core1,
        stack = CORE1_STACK,
        irqs = Irqs,
        uart = (hw.uart, hw.uart_tx, hw.uart_rx),
        run = |miner_serial| {
            ubos_metrics::cpu_load::run_cortex_m_executor::<1, _>(|core1_spawner: Spawner| {
                core1_spawner.spawn(unwrap!(mining::serial_task(
                    &mining::CHAINS[0],
                    &PARK,
                    miner_serial,
                    &ASIC_SESSION,
                    ubos_asic::Bm13xxDriver::new(config::CHIP)
                )));
            });
        },
    );

    // Before every pusher, for the reason `ubos_metrics::metrics::run_ingest`
    // gives.
    ubos_firmware::spawn_metrics_ingest!(spawner);

    spawner.spawn(unwrap!(watchdog_task(ubos_rp::BoardWatchdog::new(
        Watchdog::new(hw.watchdog),
        config::Watchdog::TIMEOUT,
    ))));
    spawner.spawn(unwrap!(reset_task()));

    let led = Output::new(hw.led, Level::Low);
    spawner.spawn(unwrap!(led_blink_task(led)));

    // Dropping the input would release the pad and clear the pull-up, so the
    // binding is leaked and the pad keeps its configuration for the firmware's
    // lifetime.
    core::mem::forget(Input::new(hw.hb_plug, Pull::Up));

    {
        static I2C1_BUS: StaticCell<i2c::Bus> = StaticCell::new();
        let i2c = I2c::new_async(hw.i2c, hw.scl, hw.sda, Irqs, I2cConfig::default());
        let bus: &'static i2c::Bus = I2C1_BUS.init(Mutex::new(i2c));

        // Before anything is powered; see `HashboardEeprom::log_identity`.
        if let Some(address) = config::BOARD.eeprom_i2c_address {
            let mut device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
            let serial = ubos_eeprom::i2c::I2cEeprom::new(address, &mut device)
                .log_identity()
                .await;
            ubos_api::publish_hashboard_id(serial);
        }

        let psu = psu::init(bus);
        spawner.spawn(unwrap!(psu::psu_task(psu, runners.psu)));
        let reset = mining::BoardResetPin::new(Output::new(hw.hb_rst, Level::Low));
        spawner.spawn(unwrap!(mining::miner_task(
            handles.psu,
            ubos_mining::serial::Serial::wire(&mining::CHAINS, [reset]),
            runners.mining,
            handles.config
        )));

        let hashboard_temp =
            ubos_thermal::SensorArray::new(config::MACHINE.sensors(), handles.thermal, || {
                embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus)
            });
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
            ubos_thermal::NoMcu,
            runners.thermal,
            handles.manager,
            handles.config
        )));
        spawner.spawn(unwrap!(manager_task(runners.manager, handles.mining)));
        spawner.spawn(unwrap!(cooling_task(
            board_fan,
            board_tach,
            runners.cooling,
            handles.thermal,
            handles.config
        )));
    };

    let config_storage = FlashStorage::new(
        SyncFlash(ubos_ota::SharedBlockingFlash::new(shared_flash, &PARK)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(runners.config, config_storage, None)));

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
    let (device, runner) = ubos_rp::boot::wiznet_or_reset(
        mac_addr,
        state,
        ExclusiveDevice::new(spi, chip_select, Delay),
        phy_int,
        phy_reset,
    )
    .await;
    spawner.spawn(unwrap!(ethernet_task(runner)));

    let seeds = ubos_rp::boot::seeds_from_rosc();

    let (net_config, mdns_hostname) =
        ubos_firmware::net::bring_up::<{ config::Board::MDNS_HOSTNAME_BASE.len() }>(
            handles.config,
            config::Board::MDNS_HOSTNAME_BASE,
            mac_addr[5],
        )
        .await;

    // The assert covers only the sockets bound at once; the slack above it
    // absorbs the TCP connections lingering in TIME_WAIT. 16 rather than the
    // 24 the RP2350 boards run, because the pool depth those boards carry
    // overflows SRAM here.
    const STACK_SOCKETS: usize = 16;
    ubos_net::assert_stack_sockets!(
        STACK_SOCKETS,
        config::MACHINE.control_board.http_pool_sizes.http,
        config::MACHINE.control_board.http_pool_sizes.metrics,
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

    spawner.spawn(unwrap!(ntp_sync_task(stack, runners.net)));

    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(cpu_load_task()));
    spawner.spawn(unwrap!(miner_metrics_task(handles.mining)));
    spawner.spawn(unwrap!(pool_weights_task(handles.config)));

    // The reset pin moves into `display_task` so it stays driven high for the
    // display's lifetime.
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
    spawner.spawn(unwrap!(display_task(
        display,
        reset,
        handles.cooling,
        handles.net
    )));

    info!("waiting for dhcp");
    let config = net::wait_for_config(stack, runners.net).await;
    info!("ip address: {:?}", config.address.address());

    for slot in 0..POOL_SLOTS {
        spawner.spawn(unwrap!(stratum_client_task(
            stack,
            slot,
            &ASIC_SESSION,
            stratum_shared,
            handles.config
        )));
    }

    // Register the OTA target before spawning the REST API workers so the
    // OTA endpoints are available from the first request.
    ubos_api::register_ota_target(ota_target).await;

    spawner.spawn(unwrap!(ota_watchdog_task(handles.manager)));
    spawner.spawn(unwrap!(network_control_task(
        stack,
        runners.net,
        handles.config
    )));

    serve_all(spawner, stack, handles, config::MACHINE.control_board.name).await;
}
