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
//! Configures SPI with DMA, brings up the W5100S via `embassy-net-wiznet`,
//! starts the embassy-net stack with DHCP, and spawns the watchdog,
//! ethernet runner, NTP, ubos-mining mining, and HTTP tasks.
//! Built with `cargo build` from this crate inside `nix develop .#esp`.
//!
//! The ESP32-S3 has two cores (`PRO_CPU` + `APP_CPU`). Embassy-net, the
//! mining state machine, and all peripheral tasks run on Core 0; the
//! ubos-mining `serial_task` runs on a separate executor on Core 1, mirroring
//! the rp2350 and bmm-pico dual-core split that keeps the chip UART loop
//! off the network path. `ASIC_SESSION` uses a `CriticalSectionRawMutex`,
//! so cross-core sharing needs no additional channel. Core 1 also accumulates
//! `IDLE_TICKS[1]` via `IdleAccounting::<1>` for the cpu-load metric.
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
use esp_hal::dma::{DmaRxBuf, DmaTxBuf};
use esp_hal::dma_buffers;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rng::Rng;
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::system::Stack;
use esp_hal::timer::timg::TimerGroup;
use static_cell::StaticCell;
use ubos_config::{FlashStorage, SyncFlash};
#[cfg(feature = "display")]
use ubos_display::sh1106::{Rotation, Sh1106};
use ubos_eeprom::HashboardEeprom as _;
use ubos_net::net;
use {defmt_rtt as _, esp_backtrace as _};

/// Backing storage for the user-facing event ring, owned here so its size
/// follows this board's `EVENT_LOG_CAPACITY`. Placed in RTC fast RAM, which
/// retains across a warm reset; the validity header in `RetainedRing` discards
/// it after a cold boot leaves garbage.
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
    ExclusiveDevice<
        esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>,
        Output<'static>,
        Delay,
    >,
    Input<'static>,
    Output<'static>,
>;

ubos_core::watchdog_task!(ubos_esp32::BoardWatchdog);

ubos_firmware::ethernet_task!(EthernetRunner);

ubos_firmware::net_task!(Device<'static>);

ubos_net::ntp_sync_task!();

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
async fn cpu_load_task() -> ! {
    ubos_metrics::cpu_load::run_cpu_load::<2>().await
}

ubos_firmware::stratum_metrics_task!(POOL_SLOTS);

ubos_mining::pool_weights_task!(&mining::CHAINS);

ubos_mining::miner_metrics_task!(POOL_SLOTS);

/// Adapts the MCU internal thermometer, `esp32s3_tsens::Tsens`, to the
/// `McuTemperatureSensor` trait.
struct McuTempSensor(esp32s3_tsens::Tsens<'static>);

impl ubos_metrics::mcu_temperature::McuTemperatureSensor for McuTempSensor {
    async fn read(&mut self) -> Option<ubos_core::units::Temperature> {
        self.0
            .read_millicelsius()
            .await
            .map(ubos_core::units::Temperature::from_millicelsius)
    }
}

ubos_metrics::mcu_temperature_task!(McuTempSensor);

ubos_thermal::thermal_task!(
    ubos_thermal::SensorArray<{ config::MACHINE.temp.len() }, psu::Device>,
    ubos_thermal::NoAmbient
);

ubos_manager::manager_task!();

/// Storage backend type the [`config_task`] hands to
/// [`ubos_config::run`]. The chain is
/// `esp-storage FlashStorage → SyncFlash → ubos-config FlashStorage`.
type BoardConfigStorage = FlashStorage<SyncFlash<ubos_ota::EspSharedFlash>>;

ubos_config::config_task!(BoardConfigStorage, config::BOARD, &config::MINING_INFO);

ubos_cooling::cooling_task!(ubos_esp32::fan::BoardFan, ubos_esp32::fan::BoardTach);

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
ubos_display::display_task!(OledDisplay, Output<'static>);

ubos_core::reset_task!(esp_hal::system::software_reset());

ubos_api::sync_credentials_task!();

ubos_firmware::serve_all!(
    config::MACHINE,
    &config::MINING_INFO,
    &config::BOUNDS,
    config::MAX_METRICS,
    config::METRICS_RESPONSE_BYTES,
);

ubos_net::network_control_task!();

ubos_api::ota_watchdog_task!();

/// Parallel stratum session slots this board runs — the full config
/// capacity; this board has the RAM for a session task per pool slot.
/// RAM-constrained boards may choose fewer (see stm32f407).
pub(crate) const POOL_SLOTS: usize = ubos_firmware::MAX_POOLS;

/// Rising edges the fan tach pin emits per revolution, per the Intel
/// 4-wire fan specification.
const TACH_PULSES_PER_REV: u64 = 2;

/// How long the board waits before it resets after a failed `WIZnet` init,
/// so the log line reaches the probe first.
const WIZNET_RETRY_DELAY: Duration = Duration::from_millis(500);

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
    // Socket-set capacity for the embassy-net stack. Eleven sockets are
    // held continuously: DHCPv4, DNS, the stratum client, the mDNS
    // responder, 4 HTTP listener sockets on :80, and 3 metrics listener
    // sockets on :8081. NTP opens a transient UDP socket every 60 s, and
    // closed TCP connections linger in TIME_WAIT until smoltcp reaps
    // them. 24 provides headroom for concurrent connections and
    // TIME_WAIT.
    const STACK_SOCKETS: usize = 24;
    ubos_net::assert_stack_sockets!(
        STACK_SOCKETS,
        config::MACHINE.control_board.http_pool,
        config::MACHINE.control_board.metrics_http_pool,
    );
    static RESOURCES: StaticCell<StackResources<STACK_SOCKETS>> = StaticCell::new();

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let hw = config::Board::take(peripherals);

    // Safety: called once at boot before any task runs. EVENT_LOG_BACKING lives
    // in RTC fast RAM (persistent), so a warm reset preserves its contents.
    ubos_firmware::events::install(unsafe { &mut (*core::ptr::addr_of_mut!(EVENT_LOG_BACKING)).0 });

    let timg0 = TimerGroup::new(hw.timg0);
    let software_interrupts = SoftwareInterruptControl::new(hw.sw_interrupt);
    esp_rtos::start(timg0.timer0, software_interrupts.software_interrupt0);

    // Pin the ubos-mining serial worker to Core 1 so embassy-net on Core 0
    // stays responsive. Mirrors the rp2350/bmm-pico dual-core split.
    // `ASIC_SESSION` uses `CriticalSectionRawMutex`, so the cross-core
    // share is safe without an additional pipe. Core 1's executor uses
    // `IdleAccounting::<1>` to populate `IDLE_TICKS[1]` for cpu-load metrics.
    static APP_EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();
    static APP_CORE_STACK: StaticCell<Stack<8192>> = StaticCell::new();
    ubos_esp32::spawn_miner_core1!(
        cpu_ctrl = hw.cpu_ctrl,
        sw_interrupt = software_interrupts.software_interrupt1,
        stack = APP_CORE_STACK,
        uart = (hw.uart0, hw.uart_tx, hw.uart_rx),
        run = |miner_serial| {
            let executor = APP_EXECUTOR.init(esp_rtos::embassy::Executor::new());
            executor.run_with_callbacks(
                |app_spawner| {
                    app_spawner.spawn(unwrap!(mining::serial_task(
                        &mining::CHAINS[0],
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

    // One esp-storage driver behind a mutex, shared by the config store and
    // the OTA writer: the chip has a single flash peripheral. The mutex
    // serialises individual flash ops between the two consumers, and
    // `multicore_auto_park` parks core 1 around each op so a flash write never
    // races the mining serial worker.
    static SHARED_FLASH: StaticCell<ubos_ota::EspFlashMutex> = StaticCell::new();
    let shared_flash: &'static ubos_ota::EspFlashMutex =
        SHARED_FLASH.init(ubos_ota::EspFlashMutex::new(core::cell::RefCell::new(
            esp_storage::FlashStorage::new(hw.flash).multicore_auto_park(),
        )));

    // Confirm the running image before the watchdog is armed so the
    // rollback-enabled bootloader stops arming a revert on the next reset. A
    // freshly OTA-activated slot boots as PendingVerify; this marks it Valid.
    // Running it pre-yield, before the feeder task can enable the 2 s window,
    // keeps the synchronous otadata write off the watchdog's clock.
    ubos_ota::confirm_running(ubos_ota::EspSharedFlash::new(shared_flash));

    let timg1 = TimerGroup::new(hw.timg1);
    spawner.spawn(unwrap!(watchdog_task(ubos_esp32::BoardWatchdog::new(
        timg1.wdt,
        config::Watchdog::TIMEOUT
    ))));
    spawner.spawn(unwrap!(reset_task()));

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
    let phy_int = Input::new(hw.phy_int, InputConfig::default().with_pull(Pull::Up));
    let phy_reset = Output::new(hw.phy_reset, Level::High, OutputConfig::default());

    // Fold the ESP32-S3 factory eFuse MAC into a stable per-unit MAC.
    // The WIZnet chip has no factory address; derive_mac keeps the fleet
    // MAC scheme consistent across all board families.
    let mac_addr = ubos_net::mac::publish_mac(
        config::MACHINE.control_board.mac_family,
        esp_hal::efuse::base_mac_address().as_bytes(),
    );
    let state = STATE.init(State::<8, 8>::new());
    // A failed init resets the board to try again. A failed SPI read is
    // almost always transient, a brownout garbling the bus rather than a
    // wiring fault. `new` consumes the SPI, both pins and the init-once
    // `State`, so it cannot be retried in place. A panic spins until the
    // watchdog fires, and the next boot then reports a watchdog stall for a
    // fault outside the firmware.
    let Ok((device, runner)) = embassy_net_wiznet::new(
        mac_addr,
        state,
        ExclusiveDevice::new(spi, chip_select, Delay),
        phy_int,
        phy_reset,
    )
    .await
    else {
        error!("WIZnet init failed; resetting to retry");
        Timer::after(WIZNET_RETRY_DELAY).await;
        esp_hal::system::software_reset();
    };
    spawner.spawn(unwrap!(ethernet_task(runner)));

    let manager_handle = ubos_manager::handle();
    let mining_handle = ubos_mining::handle();

    let config_storage = FlashStorage::new(
        SyncFlash(ubos_ota::EspSharedFlash::new(shared_flash)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(config_storage, None)));

    static OTA_TARGET: StaticCell<ubos_ota::EspOtaTarget> = StaticCell::new();
    let ota_target = OTA_TARGET.init(ubos_ota::OtaEngine::new(
        ubos_ota::EspBackend::new(ubos_ota::EspSharedFlash::new(shared_flash)),
        config::FIRMWARE_VERSION,
        config::Ota::MAX_IMAGE_BYTES,
    ));

    let mut rng = Rng::new();
    let seed = u64::from(rng.random()) << 32 | u64::from(rng.random());

    let (net_config, mdns_hostname) = ubos_firmware::net::bring_up::<
        { config::Board::MDNS_HOSTNAME_BASE.len() },
    >(config::Board::MDNS_HOSTNAME_BASE, mac_addr[5])
    .await;

    let (stack, runner) = embassy_net::new(
        device,
        net_config,
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(unwrap!(net_task(runner)));

    // Seed the session CSPRNG from the hardware RNG.
    let csprng_seed = ubos_esp32::seed_from_rng(&mut rng);
    ubos_auth::csprng::seed_csprng(csprng_seed);

    // Independent entropy stream for the SV2 noise handshake, drawn from
    // the same hardware RNG as the session CSPRNG.
    let sv2_seed = ubos_esp32::seed_from_rng(&mut rng);
    let stratum_shared =
        ubos_firmware::init_stratum(sv2_seed, config::CHIP.chip_descriptor().rolls_ntime);

    spawner.spawn(unwrap!(ntp_sync_task(stack)));
    spawner.spawn(unwrap!(metrics_ingest_task()));

    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(cpu_load_task()));
    spawner.spawn(unwrap!(stratum_metrics_task()));
    spawner.spawn(unwrap!(pool_weights_task()));
    // No LED blink task: board wiring does not include an accessible LED.
    let tsens = esp32s3_tsens::Tsens::new(hw.sens, esp32s3_tsens::Range::Standard).await;
    spawner.spawn(unwrap!(mcu_temperature_task(McuTempSensor(tsens))));

    // SH1106 OLED on SPI3: reset pulse, then async DMA SPI.
    // The reset pin is moved into display_task so it stays driven high for the display's lifetime.
    #[cfg(feature = "display")]
    let (reset, display) = {
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
        let mut display = Sh1106::new(spi_device, display_dc, Rotation::Normal);
        // Display is reset and SPI is valid; init cannot fail.
        display.init_async().await.expect("BUG: SH1106 init failed");
        (rst, display)
    };

    #[cfg(feature = "display")]
    spawner.spawn(unwrap!(display_task(display, reset)));

    let thermal = ubos_thermal::handle();

    spawner.spawn(unwrap!(miner_metrics_task(mining_handle)));

    // BMM101 hashboard PSU, LM75A temperature sensors and the carrier
    // TCA9554 I/O expander all share I2C0 (GPIO15 SDA, GPIO7 SCL). All
    // mining tasks run on Core 0 so the I2C bus mutex can stay
    // `NoopRawMutex`: every consumer is on the same executor.
    {
        static I2C0_BUS: StaticCell<psu::Bus> = StaticCell::new();
        // Valid I2C0 peripheral with GPIO15/7 routed via the GPIO matrix; cannot fail.
        let i2c = I2c::new(hw.i2c, I2cConfig::default())
            .expect("BUG: I2C0 configuration failed")
            .with_sda(hw.sda)
            .with_scl(hw.scl)
            .into_async();
        let bus: &'static psu::Bus = I2C0_BUS.init(Mutex::new(i2c));

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

        // Hashboard reset: active-low, driven through the carrier TCA9554
        // expander on EXIO5. Asserted (held low) here until the miner
        // task sequences the release during bring-up.
        let reset = ubos_esp32::expander::ExpanderResetPin::new(bus, mining::RESET_BIT).await;
        spawner.spawn(unwrap!(mining::miner_task(
            board_psu,
            ubos_mining::serial::Serial::wire(&mining::CHAINS, [reset])
        )));

        // Temperature and fan monitor via shared I2C0 bus.
        let hashboard_temp = ubos_thermal::SensorArray::new(
            const { config::MACHINE.temp.as_array().unwrap() },
            || embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus),
        );
        let board_fan = ubos_esp32::fan::BoardFan::new(hw.ledc, hw.fan_pwm_pin);
        let board_tach =
            ubos_esp32::fan::BoardTach::new(hw.pcnt, hw.fan_tach_pin, TACH_PULSES_PER_REV);
        spawner.spawn(unwrap!(thermal_task(
            hashboard_temp,
            ubos_thermal::NoAmbient,
            manager_handle
        )));
        spawner.spawn(unwrap!(manager_task(mining_handle)));
        spawner.spawn(unwrap!(cooling_task(board_fan, board_tach, thermal)));
    }

    // Hashboard plug-detect input on GPIO5, pulled up. `boot` returns once the
    // listeners are spawned and the binding drops there, but esp-hal gives
    // `Input` no `Drop`, so the pad keeps its configuration and the pull-up
    // for the firmware's lifetime. Nothing else claims the pad.
    let _hb_plug = Input::new(hw.hb_plug, InputConfig::default().with_pull(Pull::Up));

    info!("waiting for dhcp");
    let dhcp_config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", dhcp_config.address.address());

    for slot in 0..POOL_SLOTS {
        spawner.spawn(unwrap!(stratum_client_task(
            stack,
            slot,
            &ASIC_SESSION,
            stratum_shared
        )));
    }

    spawner.spawn(unwrap!(network_control_task(stack)));

    // Register the OTA target now that the network is up, then spawn the
    // session watchdog that aborts a stalled upload and resumes mining.
    ubos_api::register_ota_target(ota_target).await;
    spawner.spawn(unwrap!(ota_watchdog_task()));

    serve_all(spawner, stack, thermal, manager_handle).await;
}
