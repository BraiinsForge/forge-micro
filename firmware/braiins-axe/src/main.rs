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

//! Firmware entry point for the Braiins Axe, a Raspberry Pi Pico 2
//! (RP2350A) with a W5500 ethernet controller, the ubos-mining backend
//! driving a four-chip single-domain BM1370 hashboard, and a
//! TPS53647-based `BraiinsAxePsu` power stage.
//!
//! Initialises SPI at 33 MHz, brings the W5500 up via `embassy-net-wiznet`,
//! starts the embassy-net stack with DHCP, and spawns the watchdog,
//! ethernet runner, NTP sync, and ubos-mining mining tasks
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
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH3, I2C0, PIO0, PIO1, SPI0, TRNG, UART1};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, SPI1};
use embassy_rp::pio::{InterruptHandler as PioInterruptHandler, Pio};
use embassy_rp::pio_programs::ws2812::{PioWs2812, PioWs2812Program};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
use embassy_rp::trng::{self, Trng};
use embassy_rp::uart::BufferedInterruptHandler;
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma, i2c};
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
use ubos_eeprom::HashboardEeprom as _;

use ubos_net::net;

mod config;
mod fan;
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
        dma::InterruptHandler<DMA_CH3>,
        #[cfg(feature = "display")]
        dma::InterruptHandler<DMA_CH2>;
    I2C0_IRQ => i2c::InterruptHandler<I2C0>;
    UART1_IRQ => BufferedInterruptHandler<UART1>;
    TRNG_IRQ => trng::InterruptHandler<TRNG>;
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
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

// This board's array is two TMP102 PCB sensors, from the
// `ubos_machine::BRAIINS_AXE` sensor table. There is no die-attached
// remote diode on this board, so the safety gate runs on the PCB
// readings plus their PCB-to-die offset.
ubos_thermal::thermal_task!(ubos_thermal::SensorArray<psu::Device>);

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

ubos_firmware::serve_all!(config::MACHINE, &config::MINING_INFO, &config::BOUNDS);

ubos_net::network_control_task!();

/// Parallel stratum session slots this board runs — the full config
/// capacity; this board has the RAM for a session task per pool slot.
/// RAM-constrained boards may choose fewer (see stm32f407).
pub(crate) const POOL_SLOTS: usize = ubos_firmware::MAX_POOLS;

/// Shared ASIC session: the bridge between the per-slot stratum sessions and
/// the BM1370 serial worker. One instance per binary; referenced by every
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
// `items_after_statements` fires on the `StaticCell` declarations that follow
// `let` bindings inside the I2C and network scopes; static items must sit at
// statement position there, so the ordering is unavoidable.
#[allow(clippy::too_many_lines, clippy::items_after_statements)]
async fn boot(spawner: Spawner) {
    // --- 1. Core peripherals — always ----------------------------------------
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());
    let hw = config::Board::take(peripherals);

    // Safety: called once at boot before any task runs. EVENT_LOG_BACKING lives
    // in .uninit, so the startup code does not zero it and a warm reset
    // preserves its contents.
    ubos_firmware::events::install(unsafe { &mut *core::ptr::addr_of_mut!(EVENT_LOG_BACKING) });

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

    // --- 2. Core1 + serial_task ------------------------------------------------
    // Pin the ubos-mining serial worker to core 1 so embassy-net on core 0
    // stays responsive. BufferedUart must be constructed on the core that
    // polls it — `init_buffers` unmasks the UART IRQ on the calling core's
    // NVIC, and the async write path pends the same IRQ to drain the SW TX
    // ring buffer into the HW FIFO. RP2350's Cortex-M33 has per-core NVICs,
    // so an unmask on one core and a pend on the other land on different
    // controllers and the FIFO is never written.
    ubos_rp::spawn_miner_core1!(
        core1 = hw.core1,
        stack = CORE1_STACK,
        irqs = Irqs,
        uart = (hw.uart, hw.uart_tx, hw.uart_rx),
        run = |miner_serial| {
            // Drive core 1 through `run_cortex_m_executor::<1, _>` so the
            // poll/wfe loop accumulates idle ticks into `IDLE_TICKS[1]`,
            // letting `run_cpu_load::<2>()` publish a real busy ratio for
            // this core instead of a constant 1.0.
            ubos_metrics::cpu_load::run_cortex_m_executor::<1, _>(|core1_spawner: Spawner| {
                core1_spawner.spawn(unwrap!(mining::serial_task(miner_serial, &ASIC_SESSION)));
            });
        },
    );

    // --- 3. Watchdog — always (no status LED on this board) -------------------
    spawner.spawn(unwrap!(watchdog_task(ubos_rp::BoardWatchdog::new(
        Watchdog::new(hw.watchdog),
        config::Watchdog::TIMEOUT,
    ))));
    spawner.spawn(unwrap!(reset_task()));

    // --- 4. Handle refs — always (compiled even when gated tasks are off) -----
    let thermal = ubos_thermal::handle();
    let manager_handle = ubos_manager::handle();
    let mining_handle = ubos_mining::handle();

    // I2C0 bus + PSU + mining + sensors.
    // The TPS53647 rail is gated by PWR_EN (GPIO0, ENABLE pin) plus the
    // OPERATION on-bit; the 0V8/1V2 LDOs by LDO_EN (GPIO1). The BoardPsu
    // owns those pins; only the miner task raises them, during bring-up,
    // and drops them again when mining halts, so an idle board never
    // holds the chip powered in reset.
    use embassy_rp::i2c::{Config as I2cConfig, I2c};

    static I2C0_BUS: StaticCell<psu::Bus> = StaticCell::new();
    let i2c = I2c::new_async(hw.i2c, hw.scl, hw.sda, Irqs, I2cConfig::default());
    let bus: &'static psu::Bus = I2C0_BUS.init(Mutex::new(i2c));

    // Read the hashboard EEPROM once, before powering anything up, so its
    // identity is in the log even if bring-up later fails.
    if let Some(address) = config::BOARD.eeprom_i2c_address {
        let mut device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
        ubos_eeprom::i2c::I2cEeprom::new(address, &mut device)
            .log_identity()
            .await;
    }

    // PWR_EN and LDO_EN start low; the rails stay off until the miner task
    // enables them. `_terminated` is intentionally discarded: nothing here
    // polls the flag to surface a termination event.
    let (board_psu, _terminated) = psu::init(
        bus,
        Output::new(hw.pwr_en, Level::Low),
        Output::new(hw.ldo_en, Level::Low),
        Input::new(hw.vr_rdy, Pull::None),
    );
    spawner.spawn(unwrap!(psu::power_stats_task(board_psu)));

    // GPIO2: HIGH = enabled/out-of-reset; start LOW (in reset) until the
    // miner task is ready to bring up the hashboard.
    let reset = mining::BoardResetPin::new(Output::new(hw.hb_rst, Level::Low));
    spawner.spawn(unwrap!(mining::miner_task(board_psu, reset)));

    // Temperature and fan monitor via shared I2C bus.
    let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
    let hashboard_temp = ubos_thermal::SensorArray::new(temp_device, config::MACHINE.temp);
    let board_fan = fan::BoardFan::new(hw.fan_pwm_slice, hw.fan_pwm_pin, hw.fan_pwm_pin_b);
    // Each connector's tachometer is measured by a PIO0 state machine; the
    // cooling loop drains the latest period per fan and publishes one
    // labeled board_fan_rpm series each.
    let Pio {
        mut common,
        sm0,
        sm1,
        ..
    } = Pio::new(hw.pio, Irqs);
    let board_tach =
        fan::BoardTach::new(&mut common, sm0, sm1, hw.fan_tach_pin_a, hw.fan_tach_pin_b);
    spawner.spawn(unwrap!(thermal_task(hashboard_temp, manager_handle)));
    spawner.spawn(unwrap!(manager_task(mining_handle)));
    spawner.spawn(unwrap!(cooling_task(board_fan, board_tach, thermal)));

    // The 16-LED ARGB ring breathes a fixed color; see rgb.rs for why
    // the ring runs on PIO1 rather than sharing PIO0 with the tachs.
    // The WS2812 program object only parameterizes state machine setup,
    // so it can drop at the end of boot().
    let Pio {
        common: mut rgb_common,
        sm0: rgb_sm,
        ..
    } = Pio::new(hw.pio1, Irqs);
    let rgb_program = PioWs2812Program::new(&mut rgb_common);
    let status_ring: rgb::Ring = PioWs2812::new(
        &mut rgb_common,
        rgb_sm,
        hw.rgb_dma,
        Irqs,
        hw.rgb_data,
        &rgb_program,
    );
    spawner.spawn(unwrap!(rgb::rgb_task(status_ring)));

    // --- 7. Config — always ---------------------------------------------------
    let config_storage = FlashStorage::new(
        SyncFlash(ubos_ota::SharedBlockingFlash::new(shared_flash)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(config_storage)));

    // --- 8. Board health tasks — always (no net/psu deps) ---------------------
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
    let phy_int = Input::new(hw.phy_int, Pull::Up);
    let phy_reset = Output::new(hw.phy_reset, Level::High);

    // Fold the RP2350 OTP chip id into a stable per-board MAC. The WIZnet
    // chip has no factory address, so the firmware supplies one.
    let mac_addr = {
        let chip_id = embassy_rp::otp::get_chipid().unwrap_or_else(|_| {
            defmt::warn!("OTP chip id read failed; MAC hash falls back to zero");
            0
        });
        ubos_net::mac::derive_mac(
            ubos_net::mac::DUMMY_OUI,
            config::MACHINE.control_board.mac_family,
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
        phy_int,
        phy_reset,
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

    let (net_config, mdns_hostname) = ubos_firmware::net::bring_up::<
        { config::Board::MDNS_HOSTNAME_BASE.len() },
    >(config::Board::MDNS_HOSTNAME_BASE, mac_addr[5])
    .await;

    // 16 was one short of even a single pool generation; 24 matches the
    // rp2350 board running the same pool sizes. The assert is shared so
    // every board gets the same smoltcp socket-exhaustion guarantee.
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
    let stratum_shared = ubos_firmware::init_stratum(sv2_seed);

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
