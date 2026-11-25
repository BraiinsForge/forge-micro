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

//! Firmware entry point for the Forge Axe, an RP2350B with a W5500
//! ethernet controller, a four-chip single-domain BM1370 hashboard, and a
//! TPS53647-based `ForgeAxePsu` power stage.
//!
//! Board docs: `../../../docs/STRUCTURE.md`
//! Wiring/build: `../../../docs/SETUP.md`

#![no_std]
#![no_main]

use core::cell::RefCell;
use defmt::{error, info, unwrap, warn};
use defmt_rtt as _;
use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_net::{EthernetAddress, StackResources};
use embassy_net_wiznet::chip::W5500;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::flash::{Blocking as FlashBlocking, Flash};
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH3, I2C1, PIO1, SPI0, TRNG, UART1};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
use embassy_rp::trng;
use embassy_rp::uart::BufferedInterruptHandler;
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Delay;
#[cfg(feature = "display")]
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::{ConstStaticCell, StaticCell};
use ubos_config::{FlashStorage, SyncFlash};
use ubos_cooling::emc2302_1;
#[cfg(feature = "display")]
use ubos_display::sh1106::{Rotation, Sh1106};
use ubos_eeprom::forge::RECORD_LEN;
use ubos_eeprom::i2c::I2cEeprom;
use ubos_machine::forge_axe::{
    FORGE_AXE_EVENT_LOG_CAPACITY, FORGE_AXE_HTTP_POOL_SIZES, FORGE_AXE_MAX_SENSORS, ForgeAxeEeprom,
    forge_axe_control_board, forge_axe_hashboard, forge_axe_psu,
};
use ubos_machine::{ControlBoardDescriptor, MachineDescriptor};
use ubos_psu::ForgeAxePsuDescriptor;
use ubos_rp::i2c;
use ubos_rp::rgb::Ws2812Ring;

use ubos_net::net;

mod config;
mod mining;
mod psu;
mod rgb;

static CORE1_STACK: ConstStaticCell<embassy_rp::multicore::Stack<{ config::Core1::STACK_BYTES }>> =
    ConstStaticCell::new(embassy_rp::multicore::Stack::new());

/// CPU cycles per core-1 park poll: 15 000 cycles at 150 MHz `clk_sys` is
/// 100 µs per iteration.
const PARK_POLL_DELAY_CYCLES: u32 = 15_000;

static OTA_TARGET: StaticCell<
    ubos_ota::RpOtaTarget<{ config::Flash::SIZE }, PARK_POLL_DELAY_CYCLES>,
> = StaticCell::new();

/// `.uninit` is not zeroed by the startup code, so the ring survives a warm
/// reset.
#[unsafe(link_section = ".uninit")]
static mut EVENT_LOG_BACKING: ubos_events::RetainedRing<{ FORGE_AXE_EVENT_LOG_CAPACITY }> =
    ubos_events::RetainedRing::new();

defmt::timestamp!("{=u64:iso8601s}", ubos_net::wall_clock_secs());

bind_interrupts!(struct Irqs {
    DMA_IRQ_0 =>
        dma::InterruptHandler<DMA_CH0>,
        dma::InterruptHandler<DMA_CH1>,
        dma::InterruptHandler<DMA_CH3>,
        #[cfg(feature = "display")]
        dma::InterruptHandler<DMA_CH2>;
    I2C1_IRQ => embassy_rp::i2c::InterruptHandler<I2C1>;
    UART1_IRQ => BufferedInterruptHandler<UART1>;
    TRNG_IRQ => trng::InterruptHandler<TRNG>;
    PIO1_IRQ_0 => PioInterruptHandler<PIO1>;
});

type EthernetRunner = Runner<
    'static,
    W5500,
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

const MDNS_SERVICES: &[&ubos_mdns::ServiceEntry] = &[
    &ubos_webui::WEBUI_SERVICE,
    &ubos_api::REST_API_SERVICE,
    &ubos_metrics::PROMETHEUS_SERVICE,
];

#[embassy_executor::task]
async fn mdns_task(
    stack: embassy_net::Stack<'static>,
    hostname: &'static str,
    control_board: ControlBoardDescriptor,
) -> ! {
    ubos_mdns::run_mdns(
        stack,
        hostname,
        control_board.name,
        control_board.platform.name,
        MDNS_SERVICES,
    )
    .await
}

// This board's array is the sensors of the hashboard and the PSU that its
// EEPROMs name: a TMP102 and a TMP464 on the hashboard, and the PSU
// TMP102. The TMP464 reads each die through a remote diode, so the
// safety gate runs on the die readings.
ubos_thermal::thermal_task!(
    ubos_thermal::SensorArray<{ FORGE_AXE_MAX_SENSORS }, i2c::Device>,
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

ubos_cooling::cooling_task!(emc2302_1::Fan<i2c::Device>, emc2302_1::Tach<i2c::Device>);

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    ubos_metrics::cpu_load::run_cpu_load::<2>().await
}

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
    FORGE_AXE_HTTP_POOL_SIZES,
    &config::MINING_INFO,
    &config::BOUNDS,
    config::MAX_METRICS,
    config::METRICS_RESPONSE_BYTES,
);

ubos_net::network_control_task!();

/// The operator's slots plus the fee slot the build ships: this board
/// has the RAM for a session task per slot.
pub(crate) const POOL_SLOTS: usize = ubos_config::POOL_SLOTS;

/// `None` on an off build, so the source reads the same in both states.
pub(crate) const FEE_SLOT: Option<ubos_config::FeeSlot> =
    ubos_config::FeeSlot::last_of(config::FEE_POOL, POOL_SLOTS);

static ASIC_SESSION: ubos_firmware::AsicSession<POOL_SLOTS> = ubos_firmware::AsicSession::new();

static PARK: ubos_mining::Park = ubos_mining::Park::new();

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

/// Read the EEPROMs and return the machine descriptor composed of the
/// boards they specify, and the MAC address of the control board when its
/// EEPROM holds a valid one. Publishes the brand and device names that
/// the control-board EEPROM holds.
async fn read_machine(
    bus: &'static i2c::Bus,
) -> Option<(
    MachineDescriptor<ForgeAxePsuDescriptor>,
    Option<EthernetAddress>,
)> {
    let mut device = I2cDevice::new(bus);
    let mut raw = [[0; RECORD_LEN]; 4];
    let [hashboard, psu, control_board, front_panel] = &mut raw;
    let hashboard = I2cEeprom::new(ForgeAxeEeprom::HASHBOARD, &mut device)
        .read_record(hashboard)
        .await;
    let psu = I2cEeprom::new(ForgeAxeEeprom::PSU, &mut device)
        .read_record(psu)
        .await;
    let control_board = I2cEeprom::new(ForgeAxeEeprom::CONTROL_BOARD, &mut device)
        .read_record(control_board)
        .await;
    // TODO: the front panel takes the descriptor that its record names, with
    // the code that drives the panel.
    let front_panel = I2cEeprom::new(ForgeAxeEeprom::FRONT_PANEL, &mut device)
        .read_record(front_panel)
        .await;
    info!(
        "EEPROM: hashboard {}, PSU {}, control board {}, front panel {}",
        hashboard, psu, control_board, front_panel
    );
    let hashboard = hashboard.ok().and_then(|record| {
        forge_axe_hashboard(&record)
            .inspect_err(|e| error!("EEPROM: hashboard: {}", e))
            .ok()
    });
    let psu = psu.ok().and_then(|record| {
        forge_axe_psu(&record)
            .inspect_err(|e| error!("EEPROM: PSU: {}", e))
            .ok()
    });
    let control_board = control_board.ok().and_then(|record| {
        forge_axe_control_board(&record)
            .inspect_err(|e| error!("EEPROM: control board: {}", e))
            .ok()
    });
    // TODO: the LED ring shows the brand color of the control-board data.
    let (control_board, control_board_eeprom) = control_board?;
    let (hashboard, psu) = (hashboard?, psu?);
    if let Some(brand) = control_board_eeprom.and_then(|eeprom| eeprom.brand_name) {
        publish_name(brand, ubos_api::publish_brand_name);
    }
    if let Some(device) = control_board_eeprom.and_then(|eeprom| eeprom.device_name) {
        publish_name(device, ubos_api::publish_product_name);
    }
    let mac = control_board_eeprom.and_then(|eeprom| parse_mac(eeprom.mac_address));
    Some((
        MachineDescriptor::forge_axe(control_board, hashboard, psu),
        mac,
    ))
}

/// The MAC address that the control-board EEPROM holds, when the network
/// can take it: a unicast address other than zero. Warns otherwise.
fn parse_mac(mac_address: [u8; 6]) -> Option<EthernetAddress> {
    let mac = EthernetAddress(mac_address);
    if !mac.is_unicast() || mac == EthernetAddress::default() {
        warn!(
            "EEPROM: control board: the MAC address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} is zero or not unicast, so the board uses a derived MAC",
            mac.0[0], mac.0[1], mac.0[2], mac.0[3], mac.0[4], mac.0[5],
        );
        return None;
    }
    Some(mac)
}

/// Publish `name` with `publish`.
fn publish_name(name: &str, publish: fn(ubos_api::HwString)) {
    let name = &name[..name.floor_char_boundary(ubos_api::HW_STRING_LEN)];
    // The cut above makes the name fit, so the conversion cannot fail.
    publish(unwrap!(ubos_api::HwString::try_from(name)));
}

#[embassy_executor::task]
// `items_after_statements` fires on the `StaticCell` declarations that follow
// `let` bindings inside the I2C and network scopes; static items must sit at
// statement position there, so the ordering is unavoidable.
#[allow(clippy::too_many_lines, clippy::items_after_statements)]
async fn boot(spawner: Spawner) {
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());
    let hw = config::Board::take(peripherals);

    let (runners, handles) = ubos_firmware::Handles::take();

    // Safety: called once at boot, before any task that records an event runs.
    ubos_firmware::events::install(unsafe { &mut *core::ptr::addr_of_mut!(EVENT_LOG_BACKING) });

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
    spawner.spawn(unwrap!(metrics_ingest_task()));

    spawner.spawn(unwrap!(watchdog_task(ubos_rp::BoardWatchdog::new(
        Watchdog::new(hw.watchdog),
        config::Watchdog::TIMEOUT,
    ))));
    spawner.spawn(unwrap!(reset_task()));

    // The LEDs are active-low; start HIGH (LED off) so the initial blink
    // phase matches the active-low convention.
    let led = Output::new(hw.led_green1, Level::High);
    spawner.spawn(unwrap!(led_blink_task(led)));
    // The other three LEDs have no meaning yet. Forgetting the outputs
    // keeps them driven off, because dropping one would release the pin.
    core::mem::forget(Output::new(hw.led_red1, Level::High));
    core::mem::forget(Output::new(hw.led_red2, Level::High));
    core::mem::forget(Output::new(hw.led_green2, Level::High));

    use embassy_rp::i2c::{Config as I2cConfig, I2c};

    static I2C1_BUS: StaticCell<i2c::Bus> = StaticCell::new();
    let i2c = I2c::new_async(hw.i2c, hw.scl, hw.sda, Irqs, I2cConfig::default());
    let bus: &'static i2c::Bus = I2C1_BUS.init(Mutex::new(i2c));

    // PWR_EN (GPIO33) and HB_RST (GPIO32) start low before the EEPROMs
    // are read, so the rail stays off and the chips stay in reset until
    // the miner task brings them up.
    // TODO: hold both low after the boot stops below: returning drops
    // the outputs, and dropping an output releases its pin.
    let pwr_en = Output::new(hw.pwr_en, Level::Low);
    let hb_rst = Output::new(hw.hb_rst, Level::Low);

    // Read the EEPROMs before powering anything up: the boards they name
    // decide the PSU settings and the sensors the thermal monitor reads.
    // Without a known hashboard, PSU and control board, the hardware is not
    // known, so the rail stays off and the chips stay in reset. The
    // watchdog and the LED already run, so the board does not reboot-loop.
    let Some((machine, eeprom_mac)) = read_machine(bus).await else {
        // TODO: keep the network and OTA up without a known machine.
        error!("boot: the EEPROMs name no known machine, so it stays off");
        return;
    };

    let psu = psu::init(bus, machine.psu, pwr_en, Input::new(hw.vr_rdy, Pull::None));
    spawner.spawn(unwrap!(psu::psu_task(psu, runners.psu)));

    let reset = mining::BoardResetPin::new(hb_rst);
    spawner.spawn(unwrap!(mining::miner_task(
        handles.psu,
        ubos_mining::serial::Serial::wire(&mining::CHAINS, [reset]),
        runners.mining,
        handles.config
    )));

    let hashboard_temp =
        ubos_thermal::SensorArray::new(machine.sensors(), handles.thermal, || I2cDevice::new(bus));
    // Both fan connectors hang off the EMC2302-1 on the same bus.
    let (board_fan, board_tach) =
        emc2302_1::new_with_range(emc2302_1::Range::RpmMin500, || I2cDevice::new(bus)).await;
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

    // The 16-LED ARGB ring breathes a fixed color. The WS2812 program
    // object only parameterizes state machine setup, so it can drop at
    // the end of boot().
    let Pio {
        common: mut rgb_common,
        sm0: rgb_sm,
        ..
    } = Pio::new(hw.pio1, Irqs);
    let rgb_program = PioWs2812Program::new(&mut rgb_common);
    let status_ring: rgb::Ring = Ws2812Ring(PioWs2812::new(
        &mut rgb_common,
        rgb_sm,
        hw.rgb_dma,
        Irqs,
        hw.rgb_data,
        &rgb_program,
    ));
    spawner.spawn(unwrap!(rgb::rgb_task(status_ring, rgb::COLOR)));

    let config_storage = FlashStorage::new(
        SyncFlash(ubos_ota::SharedBlockingFlash::new(shared_flash, &PARK)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(runners.config, config_storage, None)));

    spawner.spawn(unwrap!(cpu_load_task()));

    // Placed before the network block so the display comes up early. The reset
    // pin moves into `display_task` so it stays driven high for the display's
    // lifetime.
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
    spawner.spawn(unwrap!(display_task(
        display,
        reset,
        handles.cooling,
        handles.net
    )));

    let mut spi_config = SpiConfig::default();
    spi_config.frequency = config::Spi::FREQUENCY;
    let spi = Spi::new(
        hw.spi, hw.sck, hw.mosi, hw.miso, hw.rx_dma, hw.tx_dma, Irqs, spi_config,
    );
    let chip_select = Output::new(hw.cs, Level::High);
    let phy_int = Input::new(hw.phy_int, Pull::Up);
    let phy_reset = Output::new(hw.phy_reset, Level::High);

    let mac_addr = match eeprom_mac {
        Some(EthernetAddress(mac)) => {
            info!(
                "mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} from the control-board EEPROM",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
            );
            runners.net.set_mac(mac);
            mac
        }
        None => runners
            .net
            .publish_mac(machine.control_board.mac_family, &ubos_rp::boot::chip_id()),
    };
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

    let (net_config, mdns_hostname) =
        ubos_firmware::net::bring_up::<{ config::Board::MDNS_HOSTNAME_BASE.len() }>(
            handles.config,
            config::Board::MDNS_HOSTNAME_BASE,
            mac_addr[5],
        )
        .await;

    // For the slack above the asserted minimum, see
    // `firmware/rp2040/src/main.rs`.
    const STACK_SOCKETS: usize = 24;
    ubos_net::assert_stack_sockets!(
        STACK_SOCKETS,
        FORGE_AXE_HTTP_POOL_SIZES.http,
        FORGE_AXE_HTTP_POOL_SIZES.metrics,
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

    spawner.spawn(unwrap!(mdns_task(
        stack,
        mdns_hostname,
        machine.control_board
    )));

    spawner.spawn(unwrap!(miner_metrics_task(handles.mining)));

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
    // OTA endpoints are available from the first request. The &'static mut
    // binding was established early in boot, before spawn_core1.
    ubos_api::register_ota_target(ota_target).await;

    spawner.spawn(unwrap!(ota_watchdog_task(handles.manager)));
    spawner.spawn(unwrap!(network_control_task(
        stack,
        runners.net,
        handles.config
    )));

    serve_all(spawner, stack, handles, machine.control_board.name).await;
}
