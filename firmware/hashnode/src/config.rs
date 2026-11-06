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

//! Board constants and peripheral extraction for the Hashnode, an
//! RP2350 with a W5500 running the ubos-mining backend and a `SIC45x`-based
//! `BfmPsu`.
//! `Board::take` consumes the RP2350 `Peripherals` once and returns every
//! singleton the board's schematic claims, keeping pin assignments in one
//! place rather than scattered through `main`.
//!
//! Wiring/pinout: `../../../docs/SETUP.md`

pub use ubos_machine::HASHNODE as MACHINE;

/// How many hashboard chains this board drives. Every array sized over
/// the chains takes its length from here, so a machine that gains a
/// chain fails to build until each is wired.
pub const CHAINS: usize = MACHINE.chains;

use embassy_rp::Peri;
use embassy_rp::peripherals::{
    CORE1, DMA_CH0, DMA_CH1, FLASH, I2C1, PIN_2, PIN_3, PIN_4, PIN_5, PIN_6, PIN_10, PIN_11,
    PIN_16, PIN_17, PIN_18, PIN_19, PIN_20, PIN_21, PIN_23, PIN_24, PIN_25, PIN_30, PIN_32, PIN_38,
    PIN_43, PIN_44, PIO0, PIO1, PWM_SLICE5, PWM_SLICE9, SPI0, TRNG, UART1, WATCHDOG,
};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, PIN_15, PIN_26, PIN_27, PIN_28, PIN_29, SPI1};
use embassy_time::Duration;

/// Every peripheral singleton the board consumes, extracted in one place.
pub struct Board {
    pub watchdog: Peri<'static, WATCHDOG>,
    /// Status LED on GPIO6, active-low (10k pull-up to 3V3; DS-LED off when
    /// driven high). The blink task toggles this pin, so the initial level
    /// only sets the phase of the blink pattern.
    pub led: Peri<'static, PIN_6>,
    pub spi: Peri<'static, SPI0>,
    /// W5500 SPI0: SCLK=GPIO18, MOSI=GPIO19, MISO=GPIO16.
    pub sck: Peri<'static, PIN_18>,
    pub mosi: Peri<'static, PIN_19>,
    pub miso: Peri<'static, PIN_16>,
    /// W5500 chip-select (GPIO17, active-low; driven as GPIO Output).
    pub cs: Peri<'static, PIN_17>,
    /// W5500 interrupt line (GPIO20, active-low input).
    pub phy_int: Peri<'static, PIN_20>,
    /// W5500 reset line (GPIO21, active-low output).
    pub phy_reset: Peri<'static, PIN_21>,
    pub rx_dma: Peri<'static, DMA_CH0>,
    pub tx_dma: Peri<'static, DMA_CH1>,
    /// UART1 TX pin, drives BM1370 chip serial.
    pub uart: Peri<'static, UART1>,
    pub uart_tx: Peri<'static, PIN_4>,
    pub uart_rx: Peri<'static, PIN_5>,
    /// I2C1 bus: SDA=GPIO2, SCL=GPIO3. Sensors: INA230 0x43, TMP102 ×2,
    /// TMP461, `SIC45x` 0x10.
    pub i2c: Peri<'static, I2C1>,
    pub sda: Peri<'static, PIN_2>,
    pub scl: Peri<'static, PIN_3>,
    /// Hashboard reset (GPIO32). HIGH = enabled / out-of-reset;
    /// LOW = held in reset. Drive LOW at boot until the miner task
    /// is ready to bring up the hashboard.
    pub hb_rst: Peri<'static, PIN_32>,
    /// Fan PWM outputs (25 kHz). `PWM_SLICE5` channel A = GPIO10
    /// (`RP_PWM_A`), channel B = GPIO11 (`RP_PWM_B`); `PWM_SLICE9`
    /// channel B = GPIO43 (`RP_PWM_C`, the `FAN_CONTROL` header). All run
    /// at the same duty so every fan tracks together.
    pub fan_pwm_slice: Peri<'static, PWM_SLICE5>,
    pub fan_pwm_pin: Peri<'static, PIN_10>,
    pub fan_pwm_pin_b: Peri<'static, PIN_11>,
    pub fan_pwm_slice_c: Peri<'static, PWM_SLICE9>,
    pub fan_pwm_pin_c: Peri<'static, PIN_43>,
    /// Fan tachometer inputs, one per connector: GPIO24 (`RP_TACH_A`),
    /// GPIO25 (`RP_TACH_B`), GPIO23 (`RP_TACH_C`), GPIO38 (`RP_TACH_D`),
    /// GPIO44 (`RP_TACH_E`, the `FAN_CONTROL` header). None map to a `PWM`
    /// `ChannelB`, so the slice edge counter is unusable; each is measured
    /// by a PIO state machine instead.
    pub fan_tach_pin_a: Peri<'static, PIN_24>,
    pub fan_tach_pin_b: Peri<'static, PIN_25>,
    pub fan_tach_pin_c: Peri<'static, PIN_23>,
    pub fan_tach_pin_d: Peri<'static, PIN_38>,
    pub fan_tach_pin_e: Peri<'static, PIN_44>,
    /// PIO block driving the tach counters for GPIO23/24/25. A PIO
    /// block addresses one 32-pin GPIO window, so the two tachs above
    /// GPIO31 live on [`Self::pio1`] instead.
    pub pio: Peri<'static, PIO0>,
    /// Second PIO block for the GPIO38/GPIO44 tach counters.
    pub pio1: Peri<'static, PIO1>,
    /// FLASH peripheral for the on-board QSPI; ubos-config persists the
    /// runtime config record in the last two sectors of this device.
    pub flash: Peri<'static, FLASH>,
    /// `SIC45x` enable (GPIO30, active-high). A 10k pull-down holds it off at
    /// power-on; DS3 LED lights when driven high. Drive HIGH before
    /// miner/psu bring-up to gate the `SIC45x` power stage. There is no
    /// MCU-gated logic rail on Hashnode — the logic DC/DC is always-on from 12V.
    pub sic_en: Peri<'static, PIN_30>,
    #[cfg(feature = "display")]
    pub display_spi: Peri<'static, SPI1>,
    /// Display SPI1: SCK=GPIO26, MOSI=GPIO27.
    #[cfg(feature = "display")]
    pub display_sck: Peri<'static, PIN_26>,
    #[cfg(feature = "display")]
    pub display_mosi: Peri<'static, PIN_27>,
    /// Display SPI CS (GPIO29, active-low, driven as GPIO Output).
    #[cfg(feature = "display")]
    pub display_cs: Peri<'static, PIN_29>,
    /// Display data/command select (GPIO28).
    #[cfg(feature = "display")]
    pub display_dc: Peri<'static, PIN_28>,
    /// Display reset (GPIO15, active-low output).
    #[cfg(feature = "display")]
    pub display_rst: Peri<'static, PIN_15>,
    #[cfg(feature = "display")]
    pub display_tx_dma: Peri<'static, DMA_CH2>,
    pub core1: Peri<'static, CORE1>,
    /// Hardware TRNG peripheral. Used at boot to seed the session
    /// CSPRNG in `ubos_api`. RP2350-specific; the RP2040 has no
    /// equivalent and seeds from `RoscRng` instead.
    pub trng: Peri<'static, TRNG>,
}

/// The board descriptor this firmware targets: the 4-chip BM1370 hashnode.
/// Every crate that needs board facts takes this explicitly rather than
/// selecting a board behind a cargo feature.
pub const BOARD: &ubos_asic::HashboardDescriptor = &MACHINE.hashboard;

/// The Bitmain chip model this board's hashboard carries; injected into
/// the mining actor as the concrete chain driver.
pub const CHIP: ubos_asic::Chip = ubos_asic::Chip::Bm1370;

/// The tuners this board runs. The miner builds them, and [`BOUNDS`]
/// lists them for the config and the mining form. No power tuner: nobody
/// has measured what this board may draw.
pub type TunerSet = ubos_mining::tuner::fixed::FixedTuner;

/// Accepted tuning ranges and tuners for [`BOARD`]; the API validates
/// against these.
pub const BOUNDS: ubos_config::MiningBounds = ubos_config::bounds(
    BOARD,
    <TunerSet as ubos_mining::tuner::MachineTuner<CHAINS>>::TUNERS,
    None,
);

/// Static mining capability metadata, built from [`BOARD`].
pub const MINING_INFO: ubos_config::MiningInfo =
    ubos_config::mining_info(BOARD, &BOUNDS, ubos_config::DefaultTuning::Fixed);

/// The fee pool this board mines for: the Braiins fee pool with the
/// worker the pool reads as an unregistered device, under the build's
/// `DEVFEE_*` overrides.
pub const FEE_POOL: ubos_config::FeePool = ubos_config::fee::BRAIINS_POOL.with_overrides();

/// Distinct series the Prometheus exposition map holds. The map is keyed
/// by the label-expanded series, and a series that does not fit is
/// dropped with a warning. Must be a power of two, which the map requires.
pub const MAX_METRICS: usize = 128;

/// Bytes of the buffer one scrape is rendered into. It holds every series
/// in the map at once, so it moves with [`MAX_METRICS`].
pub const METRICS_RESPONSE_BYTES: usize = MAX_METRICS * ubos_metrics::metrics::METRIC_LINE_BYTES;

impl Board {
    /// DNS-safe base hostname for mDNS. See `ubos_mdns::compose_hostname`
    /// for the per-unit suffix and RFC 6762 §9 conflict-resolution semantics.
    pub const MDNS_HOSTNAME_BASE: &str = "hashnode";

    pub fn take(peripherals: embassy_rp::Peripherals) -> Self {
        Self {
            watchdog: peripherals.WATCHDOG,
            led: peripherals.PIN_6,
            spi: peripherals.SPI0,
            sck: peripherals.PIN_18,
            mosi: peripherals.PIN_19,
            miso: peripherals.PIN_16,
            cs: peripherals.PIN_17,
            phy_int: peripherals.PIN_20,
            phy_reset: peripherals.PIN_21,
            rx_dma: peripherals.DMA_CH0,
            tx_dma: peripherals.DMA_CH1,
            uart: peripherals.UART1,
            uart_tx: peripherals.PIN_4,
            uart_rx: peripherals.PIN_5,
            i2c: peripherals.I2C1,
            sda: peripherals.PIN_2,
            scl: peripherals.PIN_3,
            hb_rst: peripherals.PIN_32,
            fan_pwm_slice: peripherals.PWM_SLICE5,
            fan_pwm_pin: peripherals.PIN_10,
            fan_pwm_pin_b: peripherals.PIN_11,
            fan_pwm_slice_c: peripherals.PWM_SLICE9,
            fan_pwm_pin_c: peripherals.PIN_43,
            fan_tach_pin_a: peripherals.PIN_24,
            fan_tach_pin_b: peripherals.PIN_25,
            fan_tach_pin_c: peripherals.PIN_23,
            fan_tach_pin_d: peripherals.PIN_38,
            fan_tach_pin_e: peripherals.PIN_44,
            pio: peripherals.PIO0,
            pio1: peripherals.PIO1,
            flash: peripherals.FLASH,
            sic_en: peripherals.PIN_30,
            #[cfg(feature = "display")]
            display_spi: peripherals.SPI1,
            #[cfg(feature = "display")]
            display_sck: peripherals.PIN_26,
            #[cfg(feature = "display")]
            display_mosi: peripherals.PIN_27,
            #[cfg(feature = "display")]
            display_cs: peripherals.PIN_29,
            #[cfg(feature = "display")]
            display_dc: peripherals.PIN_28,
            #[cfg(feature = "display")]
            display_rst: peripherals.PIN_15,
            #[cfg(feature = "display")]
            display_tx_dma: peripherals.DMA_CH2,
            core1: peripherals.CORE1,
            trng: peripherals.TRNG,
        }
    }
}

pub struct Spi;
impl Spi {
    pub const FREQUENCY: u32 = 33_000_000;
}

#[cfg(feature = "display")]
pub struct DisplaySpi;
#[cfg(feature = "display")]
impl DisplaySpi {
    pub const FREQUENCY: u32 = 10_000_000;
}

pub struct Watchdog;
impl Watchdog {
    pub const TIMEOUT: Duration = Duration::from_secs(2);
}

/// Fan PWM and tachometer parameters. Default `clk_sys` on the RP2350 is
/// 150 MHz, so `(PWM_TOP + 1) × divider = 6000` cycles per period puts
/// the PWM frequency at exactly 25 kHz (the Intel 4-wire fan spec).
pub struct Fan;
impl Fan {
    pub const PWM_TOP: u16 = 5999;
    pub const TACH_PULSES_PER_REV: u64 = 2;
    /// GPIO of the `RP_TACH_D` / `RP_TACH_E` inputs on PIO1. Both sit
    /// above GPIO31, so PIO1 runs in the base-16 window and the fan
    /// driver rebases their `jmp pin` by [`Self::PIO_HIGH_WINDOW_BASE`].
    pub const TACH_D_GPIO: u8 = 38;
    pub const TACH_E_GPIO: u8 = 44;
    /// Start of the upper 32-GPIO PIO window (`GPIOBASE = 16`).
    pub const PIO_HIGH_WINDOW_BASE: u8 = 16;
}

pub struct Core1;
impl Core1 {
    /// Stack size for the Core 1 executor, sized for the ubos-mining backend.
    pub const STACK_BYTES: usize = 32_768;
}

/// Monotonic firmware version reported over the OTA API.
///
/// Sourced from the `UBOS_FW_VERSION` environment variable at build time,
/// defaulting to `1` when unset. `embassy-boot` stores no version metadata
/// in flash, so this const is the only version the running image exposes;
/// the OTA endpoint surfaces it as `running_version`.
pub const FIRMWARE_VERSION: u32 = match option_env!("UBOS_FW_VERSION") {
    // `parse` is not const; the helper in ubos-ota folds the decimal digits
    // at compile time so the value is fixed with no runtime cost.
    Some(raw) => ubos_ota::parse_version(raw),
    None => 1,
};

/// On-board QSPI flash parameters for the ubos-config record store.
///
/// `SIZE` is the 2 MiB driver window exposed to the flash HAL; the physical
/// device is 8 MiB but only the lower 2 MiB is mapped by this driver
/// instance. `CONFIG_RANGE` claims the final two 4 KiB erase sectors of
/// that window. The application binary must not grow past the start of the
/// range — the linker script does not enforce this today, so a binary
/// creeping past ~2040 KiB will silently overwrite the config region.
pub struct Flash;
impl Flash {
    pub const SIZE: usize = 2 * 1024 * 1024;
    pub const CONFIG_RANGE: core::ops::Range<u32> = 0x001F_E000..0x0020_0000;
}

/// OTA upload limit, reported through `OtaTarget::max_image_bytes`. Equals
/// the bootloader ACTIVE slot in `memory.x` (952 KiB): an image larger than
/// this could never swap into ACTIVE, so the handler rejects it up front.
pub struct Ota;
impl Ota {
    pub const MAX_IMAGE_BYTES: u64 = 952 * 1024;
}
