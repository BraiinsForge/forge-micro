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

//! Firmware entry point for the Waveshare ESP32-S3-LCD-Driver-Board +
//! external W5100S (SPI) with the BMM101/BM1362 ubos-mining backend.
//!
//! Core 0 runs embassy-net, the mining state machine and every peripheral
//! task; Core 1 runs only the chip serial worker, see the `mining` module.
//!
//! Board docs: `../../../docs/OVERVIEW.md`
//! Wiring/build: `../../../docs/SETUP.md`

#![no_std]
#![no_main]

use defmt::{error, info, unwrap};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W5100S;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_sync::mutex::Mutex;
use embassy_time::{Delay, Duration, Timer};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::rng::Rng;
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::system::Stack;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{dma_rx_buffer, dma_tx_buffer};
use static_cell::StaticCell;
use ubos_config::{FlashStorage, SyncFlash};
#[cfg(feature = "display")]
use ubos_display::sh1106::{Rotation, Sh1106};
use ubos_eeprom::HashboardEeprom as _;
use ubos_esp32::i2c;
use ubos_net::net;
use {defmt_rtt as _, esp_backtrace as _};

/// Placed in RTC fast RAM, which retains across a warm reset. See
/// [`ubos_firmware::events::install`].
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut EVENT_LOG_BACKING: ubos_esp32::PersistentRing<
    { config::MACHINE.control_board.event_log_capacity },
> = ubos_esp32::PersistentRing(ubos_events::RetainedRing::new());

mod config;
mod mining;
mod psu;

defmt::timestamp!("{=u64:iso8601s}", ubos_net::wall_clock_secs());

esp_bootloader_esp_idf::esp_app_desc!();

type EthernetRunner = Runner<
    'static,
    W5100S,
    ExclusiveDevice<esp_hal::spi::master::SpiDma<'static, esp_hal::Async>, Output<'static>, Delay>,
    Input<'static>,
    Output<'static>,
>;

ubos_core::watchdog_task!(ubos_esp32::BoardWatchdog);

ubos_firmware::ethernet_task!(EthernetRunner);

ubos_firmware::net_task!(Device<'static>);

ubos_net::ntp_sync_task!();

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

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    ubos_metrics::cpu_load::run_cpu_load::<2>().await
}

ubos_mining::pool_weights_task!(&mining::CHAINS);

ubos_mining::miner_metrics_task!(POOL_SLOTS);

struct McuTempSensor(esp32s3_tsens::Tsens<'static>);

impl ubos_thermal::McuTemperatureSensor for McuTempSensor {
    async fn read(&mut self) -> Option<ubos_core::units::Temperature> {
        self.0.read_temperature().await
    }
}

ubos_thermal::thermal_task!(
    ubos_thermal::SensorArray<{ config::MACHINE.sensor_count() }, i2c::Device>,
    ubos_thermal::NoAmbient,
    McuTempSensor
);

ubos_manager::manager_task!();

/// Storage chain behind [`config_task`]: `esp-storage FlashStorage →
/// SyncFlash → ubos-config FlashStorage`.
type BoardConfigStorage = FlashStorage<SyncFlash<ubos_ota::EspSharedFlash>>;

ubos_config::config_task!(
    BoardConfigStorage,
    &config::BOARD.defaults,
    &config::MINING_INFO,
    FEE_SLOT
);

ubos_cooling::cooling_task!(ubos_esp32::fan::BoardFan, ubos_esp32::fan::BoardTach);

#[cfg(feature = "display")]
type OledDisplay = Sh1106<
    ExclusiveDevice<
        esp_hal::spi::master::SpiDma<'static, esp_hal::Async>,
        Output<'static>,
        embedded_hal_bus::spi::NoDelay,
    >,
    Output<'static>,
>;

#[cfg(feature = "display")]
ubos_display::display_task!(OledDisplay, Output<'static>);

ubos_core::reset_task!(esp_hal::system::software_reset());

ubos_api::sync_credentials_task!();

ubos_firmware::serve_all!(
    config::MACHINE.control_board.http_pool_sizes,
    &config::MINING_INFO,
    &config::BOUNDS,
    config::MAX_METRICS,
    config::METRICS_RESPONSE_BYTES,
);

ubos_net::network_control_task!();

ubos_api::ota_watchdog_task!();

/// Three, because a fourth session task pushes DRAM below the boot stack
/// floor the linker holds. With the fee switch on, the fee takes the last
/// of them.
pub(crate) const POOL_SLOTS: usize = ubos_firmware::MAX_POOLS;

/// `None` on an off build, so the source reads the same in both states.
pub(crate) const FEE_SLOT: Option<ubos_config::FeeSlot> =
    ubos_config::FeeSlot::last_of(config::FEE_POOL, POOL_SLOTS);

/// Rising edges the fan tach pin emits per revolution, per the Intel
/// 4-wire fan specification.
const TACH_PULSES_PER_REV: u64 = 2;

/// How long the board waits before it resets after a failed `WIZnet` init,
/// so the log line reaches the probe first.
const WIZNET_RETRY_DELAY: Duration = Duration::from_millis(500);

static ASIC_SESSION: ubos_firmware::AsicSession<POOL_SLOTS> = ubos_firmware::AsicSession::new();

static PARK: ubos_mining::Park = ubos_mining::Park::new();

/// Board shell for [`ubos_firmware::run_stratum_session`]: the
/// `#[embassy_executor::task]` attribute sizes a typed task pool from this
/// board's `POOL_SLOTS` and rejects the generic function a shared shell
/// would be.
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

#[esp_hal::main]
fn main() -> ! {
    static EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
    let executor = EXECUTOR.init(esp_rtos::embassy::Executor::new());
    executor.run_with_callbacks(
        |spawner| spawner.spawn(unwrap!(boot(spawner))),
        ubos_esp32::IdleAccounting::<0>::new(),
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
    // For the slack above the asserted minimum, see
    // `firmware/rp2040/src/main.rs`.
    const STACK_SOCKETS: usize = 24;
    ubos_net::assert_stack_sockets!(
        STACK_SOCKETS,
        config::MACHINE.control_board.http_pool_sizes.http,
        config::MACHINE.control_board.http_pool_sizes.metrics,
    );
    static RESOURCES: StaticCell<StackResources<STACK_SOCKETS>> = StaticCell::new();

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let hw = config::Board::take(peripherals);

    let (runners, handles) = ubos_firmware::Handles::take();

    // Safety: called once at boot, before any task runs.
    ubos_firmware::events::install(unsafe { &mut (*core::ptr::addr_of_mut!(EVENT_LOG_BACKING)).0 });

    let timg0 = TimerGroup::new(hw.timg0);
    esp_rtos::start(timg0.timer0, hw.scheduler_interrupt0);

    static APP_EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
    static APP_CORE_STACK: StaticCell<Stack<8192>> = StaticCell::new();
    ubos_esp32::spawn_miner_core1!(
        cpu_ctrl = hw.cpu_ctrl,
        sw_interrupt = hw.scheduler_interrupt1,
        stack = APP_CORE_STACK,
        uart = (hw.uart0, hw.uart_tx, hw.uart_rx),
        run = |miner_serial| {
            let executor = APP_EXECUTOR.init(esp_rtos::embassy::Executor::new());
            executor.run_with_callbacks(
                |app_spawner| {
                    app_spawner.spawn(unwrap!(mining::serial_task(
                        &mining::CHAINS[0],
                        &PARK,
                        miner_serial,
                        &ASIC_SESSION,
                        ubos_asic::Bm13xxDriver::new(config::CHIP)
                    )));
                },
                ubos_esp32::IdleAccounting::<1>::new(),
            );
        },
    );

    ubos_esp32::capture_reset_diag();

    // The chip has one flash peripheral, so the config store and the OTA
    // writer share this driver; `multicore_auto_park` parks core 1 around
    // each op so a flash write never races the mining serial worker.
    static SHARED_FLASH: StaticCell<ubos_ota::EspFlashMutex> = StaticCell::new();
    let shared_flash: &'static ubos_ota::EspFlashMutex =
        SHARED_FLASH.init(ubos_ota::EspFlashMutex::new(core::cell::RefCell::new(
            esp_storage::FlashStorage::new(hw.flash).multicore_auto_park(),
        )));

    // Run before the watchdog is armed and before the first yield, so the
    // synchronous otadata write stays off the watchdog's clock.
    ubos_ota::confirm_running(ubos_ota::EspSharedFlash::new(shared_flash));

    // Before every pusher, for the reason `ubos_metrics::metrics::run_ingest`
    // gives.
    ubos_firmware::spawn_metrics_ingest!(spawner);

    let timg1 = TimerGroup::new(hw.timg1);
    spawner.spawn(unwrap!(watchdog_task(ubos_esp32::BoardWatchdog::new(
        timg1.wdt,
        config::Watchdog::TIMEOUT
    ))));
    spawner.spawn(unwrap!(reset_task()));

    // The W5100S rides the carrier's LCD header, so the PHY interrupt is on
    // the LCD_DC pad and the PHY reset on the LCD_RST pad.
    // The macros size and align the descriptors and the buffer, so these
    // fail only on a programming error.
    let dma_rx_buf = dma_rx_buffer!(4096).expect("BUG: DMA RX buffer creation failed");
    let dma_tx_buf = dma_tx_buffer!(4096).expect("BUG: DMA TX buffer creation failed");

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
    let phy_int = Input::new(hw.phy_int, InputConfig::default().with_pull(Pull::Up));
    let phy_reset = Output::new(hw.phy_reset, Level::High, OutputConfig::default());

    let mac_addr = runners.net.publish_mac(
        config::MACHINE.control_board.mac_family,
        esp_hal::efuse::base_mac_address().as_bytes(),
    );
    let state = STATE.init(State::<8, 8>::new());
    // A failed init is almost always a transient bus fault, and `new`
    // consumes the SPI, both pins and the init-once `State`, so it cannot
    // be retried in place. A panic in its stead would make the next boot
    // report a watchdog stall for a fault outside the firmware.
    let Ok(ethernet_spi) = ExclusiveDevice::new(spi, chip_select, Delay);
    let Ok((device, runner)) =
        embassy_net_wiznet::new(mac_addr, state, ethernet_spi, phy_int, phy_reset).await
    else {
        error!("WIZnet init failed; resetting to retry");
        Timer::after(WIZNET_RETRY_DELAY).await;
        esp_hal::system::software_reset();
    };
    spawner.spawn(unwrap!(ethernet_task(runner)));

    let config_storage = FlashStorage::new(
        SyncFlash(ubos_ota::EspSharedFlash::new(shared_flash)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(runners.config, config_storage, None)));

    static OTA_TARGET: StaticCell<ubos_ota::EspOtaTarget> = StaticCell::new();
    let ota_target = OTA_TARGET.init(ubos_ota::OtaEngine::new(
        ubos_ota::EspBackend::new(ubos_ota::EspSharedFlash::new(shared_flash)),
        &PARK,
        config::FIRMWARE_VERSION,
        config::Ota::MAX_IMAGE_BYTES,
    ));

    let mut rng = Rng::new();
    let seed = u64::from(rng.random()) << 32 | u64::from(rng.random());

    let (net_config, mdns_hostname) =
        ubos_firmware::net::bring_up::<{ config::Board::MDNS_HOSTNAME_BASE.len() }>(
            handles.config,
            config::Board::MDNS_HOSTNAME_BASE,
            mac_addr[5],
        )
        .await;

    let (stack, runner) = embassy_net::new(
        device,
        net_config,
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(unwrap!(net_task(runner)));

    let csprng_seed = ubos_esp32::seed_from_rng(&mut rng);
    ubos_auth::csprng::seed_csprng(csprng_seed);

    let sv2_seed = ubos_esp32::seed_from_rng(&mut rng);
    let stratum_shared =
        ubos_firmware::init_stratum(sv2_seed, config::CHIP.chip_descriptor().rolls_ntime);

    spawner.spawn(unwrap!(ntp_sync_task(stack, runners.net)));

    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(cpu_load_task()));
    spawner.spawn(unwrap!(pool_weights_task(handles.config)));
    // No LED blink task: board wiring does not include an accessible LED.
    let tsens = esp32s3_tsens::Tsens::new(hw.sens, esp32s3_tsens::Range::Standard).await;

    #[cfg(feature = "display")]
    let (reset, display) = {
        let rst = {
            let mut pin = Output::new(hw.display_rst, Level::Low, OutputConfig::default());
            Timer::after(Duration::from_millis(10)).await;
            pin.set_high();
            Timer::after(Duration::from_millis(10)).await;
            pin
        };
        // The macros size and align the descriptors and the buffer, so these
        // fail only on a programming error.
        let disp_dma_rx = dma_rx_buffer!(2048).expect("BUG: display DMA RX buffer failed");
        let disp_dma_tx = dma_tx_buffer!(2048).expect("BUG: display DMA TX buffer failed");
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
        let Ok(spi_device) = ExclusiveDevice::new_no_delay(display_spi, display_cs);
        let mut display = Sh1106::new(spi_device, display_dc, Rotation::Normal);
        // Display is reset and SPI is valid; init cannot fail.
        display.init_async().await.expect("BUG: SH1106 init failed");
        (rst, display)
    };

    #[cfg(feature = "display")]
    spawner.spawn(unwrap!(display_task(
        display,
        reset,
        handles.cooling,
        handles.net
    )));

    spawner.spawn(unwrap!(miner_metrics_task(handles.mining)));

    {
        static I2C0_BUS: StaticCell<i2c::Bus> = StaticCell::new();
        // Valid I2C0 peripheral with GPIO15/7 routed via the GPIO matrix; cannot fail.
        let i2c = I2c::new(hw.i2c, I2cConfig::default())
            .expect("BUG: I2C0 configuration failed")
            .with_sda(hw.sda)
            .with_scl(hw.scl)
            .into_async();
        let bus: &'static i2c::Bus = I2C0_BUS.init(Mutex::new(i2c));

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

        // Held asserted until the miner task sequences the release during
        // bring-up.
        let reset = ubos_esp32::expander::ExpanderResetPin::new(bus, mining::RESET_BIT).await;
        spawner.spawn(unwrap!(mining::miner_task(
            handles.psu,
            ubos_mining::serial::Serial::wire(&mining::CHAINS, [reset]),
            runners.mining,
            handles.thermal,
            handles.config
        )));

        let hashboard_temp =
            ubos_thermal::SensorArray::new(config::MACHINE.sensors(), handles.thermal, || {
                embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus)
            });
        let board_fan = ubos_esp32::fan::BoardFan::new(hw.ledc, hw.fan_pwm_pin);
        let board_tach =
            ubos_esp32::fan::BoardTach::new(hw.pcnt, hw.fan_tach_pin, TACH_PULSES_PER_REV);
        spawner.spawn(unwrap!(thermal_task(
            hashboard_temp,
            ubos_thermal::NoAmbient,
            McuTempSensor(tsens),
            runners.thermal,
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

    // esp-hal gives `Input` no `Drop`, so the pad keeps its configuration and
    // its pull-up after this binding falls out of `boot`.
    let _hb_plug = Input::new(hw.hb_plug, InputConfig::default().with_pull(Pull::Up));

    info!("waiting for dhcp");
    let dhcp_config = net::wait_for_config(stack, runners.net).await;
    info!("ip address: {:?}", dhcp_config.address.address());

    for slot in 0..POOL_SLOTS {
        spawner.spawn(unwrap!(stratum_client_task(
            stack,
            slot,
            &ASIC_SESSION,
            stratum_shared,
            handles.config
        )));
    }

    spawner.spawn(unwrap!(network_control_task(
        stack,
        runners.net,
        handles.config
    )));

    // Registered once the network is up; the watchdog aborts a stalled
    // upload and resumes mining.
    ubos_api::register_ota_target(ota_target).await;
    spawner.spawn(unwrap!(ota_watchdog_task(handles.manager)));

    serve_all(spawner, stack, handles, config::MACHINE.control_board.name).await;
}
