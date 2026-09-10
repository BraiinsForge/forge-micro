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

//! Firmware entry point for the `DevEBox` STM32F407VGT6 + WT32-ETH01
//! (LAN8720) with the BMM101/BM1362 ubos-mining backend.
//!
//! Enables the module's 50 MHz oscillator, starts the built-in MAC with
//! an auto-probed LAN8720 PHY, runs the embassy-net stack with DHCP, and
//! spawns the watchdog, LED blink, NTP, ubos-mining mining, and HTTP tasks.
//! Built with `cargo build` from this crate inside `nix develop .#arm`.
//!
//! Board docs: `../../../docs/OVERVIEW.md`
//! Wiring/build: `../../../docs/SETUP.md`

#![no_std]
#![no_main]

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::flash::{Blocking as FlashBlocking, Flash};
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_stm32::i2c::{Config as I2cConfig, I2c};
#[cfg(feature = "display")]
use embassy_stm32::peripherals::DMA1_CH5;
use embassy_stm32::peripherals::{DMA1_CH0, DMA1_CH1, DMA1_CH3, DMA1_CH6, DMA2_CH0};
use embassy_stm32::rng::Rng;
#[cfg(feature = "display")]
use embassy_stm32::spi::{self, Spi};
use embassy_stm32::usart::{Config as UartConfig, InterruptHandler as UsartInterruptHandler, Uart};
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
use {defmt_rtt as _, panic_probe as _};

use ubos_net::net;

mod config;
mod mining;
mod psu;

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
    RNG => rng::InterruptHandler<peripherals::RNG>;
    DMA2_STREAM0 => dma::InterruptHandler<DMA2_CH0>;
    USART3 => UsartInterruptHandler<peripherals::USART3>;
    DMA1_STREAM1 => dma::InterruptHandler<DMA1_CH1>;
    DMA1_STREAM3 => dma::InterruptHandler<DMA1_CH3>;
    DMA1_STREAM0 => dma::InterruptHandler<DMA1_CH0>;
    DMA1_STREAM6 => dma::InterruptHandler<DMA1_CH6>;
    I2C1_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C1>;
    I2C1_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C1>;
    #[cfg(feature = "display")]
    DMA1_STREAM5 => dma::InterruptHandler<DMA1_CH5>;
});

ubos_firmware::net_task!(ubos_stm32::Device);

ubos_core::watchdog_task!(ubos_stm32::BoardWatchdog);

ubos_core::led_blink_task!(Output<'static>);

ubos_net::ntp_sync_task!();

ubos_metrics::metrics_ingest_task!();

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

ubos_metrics::mcu_temperature_task!(ubos_stm32::mcu_temp::McuTempSensor);

#[embassy_executor::task]
async fn cpu_load_task() -> ! {
    ubos_metrics::cpu_load::run_cpu_load::<1>().await
}

ubos_firmware::stratum_metrics_task!(POOL_SLOTS);

ubos_mining::pool_weights_task!();

ubos_mining::miner_metrics_task!(POOL_SLOTS);

ubos_metrics::hashrate_metrics_task!();

ubos_thermal::thermal_task!(ubos_thermal::SensorArray<psu::Device>);

ubos_manager::manager_task!();

/// Storage backend type the [`config_task`] hands to
/// [`ubos_config::run`]. The chain is
/// `embassy-stm32 blocking Flash → SyncFlash → FlashStorage`.
type BoardConfigStorage = FlashStorage<SyncFlash<Flash<'static, FlashBlocking>>>;

ubos_config::config_task!(BoardConfigStorage, config::BOARD);

ubos_cooling::cooling_task!(ubos_stm32::fan::BoardFan, ubos_stm32::fan::BoardTach);

#[cfg(feature = "display")]
type OledDisplay = Sh1106<
    ExclusiveDevice<
        Spi<'static, embassy_stm32::mode::Async, embassy_stm32::spi::mode::Master>,
        Output<'static>,
        embedded_hal_bus::spi::NoDelay,
    >,
    Output<'static>,
>;

#[cfg(feature = "display")]
ubos_display::display_task!(OledDisplay, Output<'static>);

ubos_core::reset_task!(cortex_m::peripheral::SCB::sys_reset());

ubos_api::sync_credentials_task!();

ubos_firmware::serve_all!(config::MACHINE, &config::MINING_INFO, &config::BOUNDS);

ubos_net::network_control_task!();

/// Single-pool board: the F407's 128 KiB main SRAM cannot afford the
/// multipool per-slot session-task futures — the 3-slot build overflows
/// RAM by ~25 KiB — so this board runs one stratum session and ignores
/// the config's backup pool entries.
pub(crate) const POOL_SLOTS: usize = 1;

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

    // Safety: called once at boot before any task runs. EVENT_LOG_BACKING lives
    // in .uninit, so the startup code does not zero it and a warm reset
    // preserves its contents.
    ubos_firmware::events::install(unsafe { &mut *core::ptr::addr_of_mut!(EVENT_LOG_BACKING) });

    spawner.spawn(unwrap!(watchdog_task(ubos_stm32::BoardWatchdog::new(
        hw.iwdg,
        config::Watchdog::TIMEOUT_US
    ))));
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

    // Enable the 50 MHz oscillator on the WT32-ETH01 board.
    // Binding must remain alive for the oscillator to stay enabled.
    let _osc_enable = Output::new(hw.osc_enable, Level::High, Speed::Low);

    // Derive a per-unit MAC from the 96-bit STM32 unique device id.
    let mac_addr = ubos_net::mac::derive_mac(
        ubos_net::mac::DUMMY_OUI,
        config::MACHINE.control_board.mac_family,
        &embassy_stm32::uid::uid(),
    );

    // RMII pin mapping for DevEBox STM32F407VGT6 + WT32-ETH01 LAN8720 board.
    let device = ubos_stm32::ethernet_device!(
        eth = hw.eth,
        irqs = Irqs,
        ref_clk = hw.ref_clk,
        crs_dv = hw.crs_dv,
        rxd0 = hw.rxd0,
        rxd1 = hw.rxd1,
        txd0 = hw.txd0,
        txd1 = hw.txd1,
        tx_en = hw.tx_en,
        mac_addr = mac_addr,
        eth_sma = hw.eth_sma,
        mdio = hw.mdio,
        mdc = hw.mdc,
    );
    ubos_net::net::handle().set_mac(mac_addr);

    let manager_handle = ubos_manager::handle();
    let mining_handle = ubos_mining::handle();

    let config_storage = FlashStorage::new(
        SyncFlash(Flash::new_blocking(hw.flash)),
        config::Flash::CONFIG_RANGE,
    );
    spawner.spawn(unwrap!(config_task(config_storage)));

    let (net_config, mdns_hostname) = ubos_firmware::net::bring_up::<
        { config::Board::MDNS_HOSTNAME_BASE.len() },
    >(config::Board::MDNS_HOSTNAME_BASE, mac_addr[5])
    .await;

    // Socket-set capacity for the embassy-net stack. Seven sockets are
    // held continuously: DHCPv4, DNS, the stratum client, the mDNS
    // responder, 2 HTTP listener sockets on :80, plus the metrics
    // listener on :8081. NTP sync opens a transient UDP socket every
    // 60 s, and closed TCP connections linger in TIME_WAIT until smoltcp
    // reaps them. 16 provides headroom. The STM32F407 has only 128 KiB
    // main SRAM, so the second HTTP slot fits only because the pool
    // buffers sit in CCM, which the `ccmram-bss` feature this board
    // enables puts them in.
    const STACK_SOCKETS: usize = 16;
    ubos_net::assert_stack_sockets!(
        STACK_SOCKETS,
        config::MACHINE.control_board.http_pool,
        config::MACHINE.control_board.metrics_http_pool,
    );
    // In CCM: the socket set is pure CPU data — the ethernet DMA only
    // touches PACKETS, and frames are CPU-copied between there and the
    // sockets. A StaticCell is CCM-safe without a zero-image audit: its
    // pre-init state is a false flag plus uninitialized payload, and the
    // value is written by init() at runtime.
    #[unsafe(link_section = ".ccmram.bss")]
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
    ubos_auth::csprng::seed_csprng(csprng_seed);

    spawner.spawn(unwrap!(ntp_sync_task(stack)));
    spawner.spawn(unwrap!(metrics_ingest_task()));

    spawner.spawn(unwrap!(mdns_task(stack, mdns_hostname)));

    spawner.spawn(unwrap!(cpu_load_task()));
    spawner.spawn(unwrap!(stratum_metrics_task()));
    spawner.spawn(unwrap!(hashrate_metrics_task()));
    spawner.spawn(unwrap!(pool_weights_task()));

    static TEMP_RING_BUF: StaticCell<[u16; 2]> = StaticCell::new();
    let mcu_temp_sensor = ubos_stm32::mcu_temp::McuTempSensor::new(
        hw.adc,
        hw.adc_dma,
        hw.temp_trigger_timer,
        TEMP_RING_BUF.init([0u16; 2]),
        Irqs,
    );
    spawner.spawn(unwrap!(mcu_temperature_task(mcu_temp_sensor)));

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
            hw.i2c,
            hw.scl,
            hw.sda,
            hw.i2c_tx_dma,
            hw.i2c_rx_dma,
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

        let board_psu = psu::init(bus);
        spawner.spawn(unwrap!(psu::watchdog_task(board_psu)));
        spawner.spawn(unwrap!(psu::power_stats_task(board_psu)));

        // Hashboard reset line (PE0, active-low).
        let reset_pin = Output::new(hw.hb_rst, Level::Low, Speed::Low);
        let reset = mining::BoardResetPin::new(reset_pin);
        spawner.spawn(unwrap!(mining::miner_task(board_psu, reset)));

        // Temperature and fan monitor via shared I2C1 bus.
        let temp_device = embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice::new(bus);
        let hashboard_temp = ubos_thermal::SensorArray::new(temp_device, config::MACHINE.temp);
        let board_fan = ubos_stm32::fan::BoardFan::new(hw.tim4, hw.fan_pwm_pin, hw.fan_tach_pin);
        // Takes the fan by reference as proof that CH4 is configured and its
        // capture interrupt is armed.
        let tach = ubos_stm32::fan::BoardTach::new(&board_fan);
        spawner.spawn(unwrap!(thermal_task(hashboard_temp, manager_handle)));
        spawner.spawn(unwrap!(manager_task(mining_handle)));
        spawner.spawn(unwrap!(cooling_task(board_fan, tach, thermal)));
    }

    // BM1362 UART on USART3 (PD8 TX, PD9 RX) with DMA ring buffer.
    // Serial task runs on the same executor as embassy-net because the
    // STM32F407 is single-core. USART3 uses DMA so ETH-ISR jitter affects
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
        let miner_serial = ubos_stm32::uart::MinerUart::new(uart_tx, ring_rx);
        spawner.spawn(unwrap!(mining::serial_task(
            miner_serial,
            &ASIC_SESSION,
            ubos_asic::Bm13xxDriver::new(config::CHIP)
        )));
    }

    // SH1106 OLED on SPI3: reset pulse, then async TX-only SPI with DMA.
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

    serve_all(spawner, stack, thermal, manager_handle).await;
}
