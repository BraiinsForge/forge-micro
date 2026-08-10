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

//! Board constants and peripheral extraction for the Braiins Axe, a
//! Raspberry Pi Pico 2 (RP2350A) with a W5500 driving a four-chip
//! single-domain BM1370 hashboard and a TPS53647-based `BraiinsAxePsu`.
//! `Board::take` consumes the RP2350 `Peripherals` once and returns
//! every singleton the board's schematic claims, keeping pin
//! assignments in one place rather than scattered through `main`.
//!
//! Pin source: `BRAIINS_AXE.kicad_sch` rev 1.00 and the `HASHNODEx4`
//! to BRAIINS AXE firmware GPIO diff.

pub use ubos_machine::BRAIINS_AXE as MACHINE;

use embassy_rp::peripherals::{
    CORE1, DMA_CH0, DMA_CH1, DMA_CH3, FLASH, I2C0, PIN_0, PIN_1, PIN_16, PIN_17, PIN_18, PIN_19,
    PIN_2, PIN_20, PIN_21, PIN_22, PIN_26, PIN_27, PIN_28, PIN_3, PIN_4, PIN_5, PIN_6, PIN_8,
    PIN_9, PIO0, PIO1, PWM_SLICE4, SPI0, TRNG, UART1, WATCHDOG,
};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, PIN_10, PIN_11, PIN_12, PIN_13, PIN_14, SPI1};
use embassy_rp::Peri;
use embassy_time::Duration;

/// Every peripheral singleton the board consumes, extracted in one place.
#[allow(dead_code)]
pub struct Board {
    pub watchdog: Peri<'static, WATCHDOG>,
    pub spi: Peri<'static, SPI0>,
    /// W5500 SPI0: SCLK=GPIO18, MOSI=GPIO19, MISO=GPIO16.
    pub sck: Peri<'static, PIN_18>,
    pub mosi: Peri<'static, PIN_19>,
    pub miso: Peri<'static, PIN_16>,
    /// W5500 chip-select (GPIO17, active-low; driven as GPIO Output).
    pub cs: Peri<'static, PIN_17>,
    /// W5500 interrupt line (GPIO22, active-low input).
    pub phy_int: Peri<'static, PIN_22>,
    /// W5500 reset line (GPIO26, active-low output).
    pub phy_reset: Peri<'static, PIN_26>,
    pub rx_dma: Peri<'static, DMA_CH0>,
    pub tx_dma: Peri<'static, DMA_CH1>,
    /// UART1 drives the BM1370 chip serial: TX=GPIO4, RX=GPIO5.
    pub uart: Peri<'static, UART1>,
    pub uart_tx: Peri<'static, PIN_4>,
    pub uart_rx: Peri<'static, PIN_5>,
    /// Power I2C0 bus: SDA=GPIO20, SCL=GPIO21. Devices: TPS53647
    /// `PMBus`, TMP102 ×2.
    pub i2c: Peri<'static, I2C0>,
    pub sda: Peri<'static, PIN_20>,
    pub scl: Peri<'static, PIN_21>,
    /// Hashboard reset (GPIO2). HIGH = enabled / out-of-reset;
    /// LOW = held in reset. Drive LOW at boot until the miner task
    /// is ready to bring up the hashboard.
    pub hb_rst: Peri<'static, PIN_2>,
    /// TPS53647 ENABLE (GPIO0, active-high output). Held low at boot;
    /// the PSU wrapper raises it during miner bring-up.
    pub pwr_en: Peri<'static, PIN_0>,
    /// 0V8/1V2 LDO enable (GPIO1, active-high output). Raised together
    /// with `PWR_EN` during miner bring-up.
    pub ldo_en: Peri<'static, PIN_1>,
    /// TPS53647 `VR_RDY` power-good (GPIO3, input). Logged after enable;
    /// no gating logic until hardware behavior is known.
    pub vr_rdy: Peri<'static, PIN_3>,
    /// Fan PWM outputs (25 kHz). `PWM_SLICE4` channel A = GPIO8,
    /// channel B = GPIO9. Both run at the same duty so the two fans
    /// track together.
    pub fan_pwm_slice: Peri<'static, PWM_SLICE4>,
    pub fan_pwm_pin: Peri<'static, PIN_8>,
    pub fan_pwm_pin_b: Peri<'static, PIN_9>,
    /// Fan tachometer inputs: GPIO27 (fan 1), GPIO28 (fan 2). Neither
    /// maps to a `PWM` `ChannelB`, so each is measured by a `PIO0`
    /// state machine.
    pub fan_tach_pin_a: Peri<'static, PIN_27>,
    pub fan_tach_pin_b: Peri<'static, PIN_28>,
    /// PIO block driving the two tachometer period counters.
    pub pio: Peri<'static, PIO0>,
    /// PIO block clocking the WS2812 status ring. The ring cannot share
    /// PIO0: the tach program plus the WS2812 program overflow the
    /// 32-instruction memory by one.
    pub pio1: Peri<'static, PIO1>,
    /// ARGB header data line (GPIO6; the `DISP_SDA` net doubles as the
    /// ring's WS2812 data input).
    pub rgb_data: Peri<'static, PIN_6>,
    /// DMA channel feeding the WS2812 PIO FIFO.
    pub rgb_dma: Peri<'static, DMA_CH3>,
    /// FLASH peripheral for the on-board QSPI; ubos-config persists the
    /// runtime config record in the last two sectors of this device.
    pub flash: Peri<'static, FLASH>,
    #[cfg(feature = "display")]
    pub display_spi: Peri<'static, SPI1>,
    /// Display SPI1: SCK=GPIO10, MOSI=GPIO11.
    #[cfg(feature = "display")]
    pub display_sck: Peri<'static, PIN_10>,
    #[cfg(feature = "display")]
    pub display_mosi: Peri<'static, PIN_11>,
    /// Display data/command select (GPIO12).
    #[cfg(feature = "display")]
    pub display_dc: Peri<'static, PIN_12>,
    /// Display SPI CS (GPIO13, active-low, driven as GPIO Output).
    #[cfg(feature = "display")]
    pub display_cs: Peri<'static, PIN_13>,
    /// Display reset (GPIO14, active-low output).
    #[cfg(feature = "display")]
    pub display_rst: Peri<'static, PIN_14>,
    #[cfg(feature = "display")]
    pub display_tx_dma: Peri<'static, DMA_CH2>,
    pub core1: Peri<'static, CORE1>,
    /// Hardware TRNG peripheral. Used at boot to seed the session
    /// CSPRNG in `ubos_api`.
    pub trng: Peri<'static, TRNG>,
}

/// The board descriptor this firmware targets: the four-chip
/// single-domain BM1370 Braiins Axe. Every crate that needs board
/// facts takes this explicitly rather than selecting a board behind a
/// cargo feature.
pub const BOARD: &ubos_asic::HashboardDescriptor = &MACHINE.hashboard;

/// The Bitmain chip model this board's hashboard carries; injected into
/// the mining actor as the concrete chain driver.
pub const CHIP: ubos_asic::Chip = ubos_asic::Chip::Bm1370;

/// Accepted tuning ranges for [`BOARD`]; the API validates against these.
pub const BOUNDS: ubos_config::MiningBounds = ubos_config::bounds(BOARD);

/// Static mining capability metadata, built from [`BOARD`].
pub const MINING_INFO: ubos_config::MiningInfo = ubos_config::mining_info(BOARD, &BOUNDS);

impl Board {
    /// DNS-safe base hostname for mDNS. See `ubos_mdns::compose_hostname`
    /// for the per-unit suffix and RFC 6762 §9 conflict-resolution semantics.
    pub const MDNS_HOSTNAME_BASE: &str = "braiins-axe";

    pub fn take(peripherals: embassy_rp::Peripherals) -> Self {
        Self {
            watchdog: peripherals.WATCHDOG,
            spi: peripherals.SPI0,
            sck: peripherals.PIN_18,
            mosi: peripherals.PIN_19,
            miso: peripherals.PIN_16,
            cs: peripherals.PIN_17,
            phy_int: peripherals.PIN_22,
            phy_reset: peripherals.PIN_26,
            rx_dma: peripherals.DMA_CH0,
            tx_dma: peripherals.DMA_CH1,
            uart: peripherals.UART1,
            uart_tx: peripherals.PIN_4,
            uart_rx: peripherals.PIN_5,
            i2c: peripherals.I2C0,
            sda: peripherals.PIN_20,
            scl: peripherals.PIN_21,
            hb_rst: peripherals.PIN_2,
            pwr_en: peripherals.PIN_0,
            ldo_en: peripherals.PIN_1,
            vr_rdy: peripherals.PIN_3,
            fan_pwm_slice: peripherals.PWM_SLICE4,
            fan_pwm_pin: peripherals.PIN_8,
            fan_pwm_pin_b: peripherals.PIN_9,
            fan_tach_pin_a: peripherals.PIN_27,
            fan_tach_pin_b: peripherals.PIN_28,
            pio: peripherals.PIO0,
            pio1: peripherals.PIO1,
            rgb_data: peripherals.PIN_6,
            rgb_dma: peripherals.DMA_CH3,
            flash: peripherals.FLASH,
            #[cfg(feature = "display")]
            display_spi: peripherals.SPI1,
            #[cfg(feature = "display")]
            display_sck: peripherals.PIN_10,
            #[cfg(feature = "display")]
            display_mosi: peripherals.PIN_11,
            #[cfg(feature = "display")]
            display_dc: peripherals.PIN_12,
            #[cfg(feature = "display")]
            display_cs: peripherals.PIN_13,
            #[cfg(feature = "display")]
            display_rst: peripherals.PIN_14,
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
/// device is larger but only the lower 2 MiB is mapped by this driver
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
