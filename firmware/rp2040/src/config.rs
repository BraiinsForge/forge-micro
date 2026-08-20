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

//! Board constants and peripheral extraction for the W5500-EVB-Pico
//! (RP2040) running the ubos-mining backend. `Board::take` consumes the RP2040
//! `Peripherals` once and returns every singleton the board's schematic
//! claims, keeping pin assignments in one place rather than scattered
//! through `main`.
//!
//! Wiring/pinout: `../../../docs/SETUP.md`

pub use ubos_machine::MINIMINER_RP2040 as MACHINE;

use embassy_rp::Peri;
use embassy_rp::peripherals::{
    CORE1, DMA_CH0, DMA_CH1, FLASH, I2C1, PIN_2, PIN_3, PIN_4, PIN_5, PIN_6, PIN_7, PIN_8, PIN_15,
    PIN_16, PIN_17, PIN_18, PIN_19, PIN_20, PIN_21, PIN_25, PWM_SLICE4, PWM_SLICE7, SPI0, UART1,
    WATCHDOG,
};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, PIN_10, PIN_11, PIN_12, PIN_13, PIN_14, SPI1};
use embassy_time::Duration;

/// Every peripheral singleton the board consumes, extracted in one place.
pub struct Board {
    pub watchdog: Peri<'static, WATCHDOG>,
    pub led: Peri<'static, PIN_25>,
    pub spi: Peri<'static, SPI0>,
    pub sck: Peri<'static, PIN_18>,
    pub mosi: Peri<'static, PIN_19>,
    pub miso: Peri<'static, PIN_16>,
    pub cs: Peri<'static, PIN_17>,
    pub phy_int: Peri<'static, PIN_21>,
    pub phy_reset: Peri<'static, PIN_20>,
    pub rx_dma: Peri<'static, DMA_CH0>,
    pub tx_dma: Peri<'static, DMA_CH1>,
    /// UART1 TX pin, drives BM1362 chip serial.
    pub uart: Peri<'static, UART1>,
    pub uart_tx: Peri<'static, PIN_4>,
    pub uart_rx: Peri<'static, PIN_5>,
    /// I2C1 bus for BMM101 PSU and LM75A temperature sensors.
    pub i2c: Peri<'static, I2C1>,
    pub sda: Peri<'static, PIN_6>,
    pub scl: Peri<'static, PIN_7>,
    /// Hashboard reset line (active-low output).
    pub hb_rst: Peri<'static, PIN_2>,
    /// Hashboard plug-detect line (input, pull-up).
    pub hb_plug: Peri<'static, PIN_3>,
    /// Fan PWM output (25 kHz via `PWM_SLICE4` channel A).
    pub fan_pwm_slice: Peri<'static, PWM_SLICE4>,
    pub fan_pwm_pin: Peri<'static, PIN_8>,
    /// Fan tachometer input (rising-edge counter via `PWM_SLICE7`).
    pub fan_tach_slice: Peri<'static, PWM_SLICE7>,
    pub fan_tach_pin: Peri<'static, PIN_15>,
    /// FLASH peripheral for the on-board QSPI; ubos-config persists the
    /// runtime config record in the last two sectors of this device.
    pub flash: Peri<'static, FLASH>,
    #[cfg(feature = "display")]
    pub display_spi: Peri<'static, SPI1>,
    #[cfg(feature = "display")]
    pub display_sck: Peri<'static, PIN_10>,
    #[cfg(feature = "display")]
    pub display_mosi: Peri<'static, PIN_11>,
    #[cfg(feature = "display")]
    pub display_cs: Peri<'static, PIN_14>,
    #[cfg(feature = "display")]
    pub display_dc: Peri<'static, PIN_13>,
    #[cfg(feature = "display")]
    pub display_rst: Peri<'static, PIN_12>,
    #[cfg(feature = "display")]
    pub display_tx_dma: Peri<'static, DMA_CH2>,
    pub core1: Peri<'static, CORE1>,
}

/// The board descriptor this firmware targets: the 4-chip BM1362 BMM101.
/// Every crate that needs board facts takes this explicitly rather than
/// selecting a board behind a cargo feature.
pub const BOARD: &ubos_asic::HashboardDescriptor = &MACHINE.hashboard;

/// The Bitmain chip model this board's hashboard carries; injected into
/// the mining actor as the concrete chain driver.
pub const CHIP: ubos_asic::Chip = ubos_asic::Chip::Bm1362;

/// Accepted tuning ranges for [`BOARD`]; the API validates against these.
pub const BOUNDS: ubos_config::MiningBounds = ubos_config::bounds(BOARD);

/// Static mining capability metadata, built from [`BOARD`].
pub const MINING_INFO: ubos_config::MiningInfo = ubos_config::mining_info(BOARD, &BOUNDS);

impl Board {
    /// DNS-safe base hostname for mDNS. The runtime appends a per-unit
    /// suffix derived from the last octet of the derived MAC so multiple
    /// rp2040 boards on the same LAN do not collide on
    /// `_prometheus._tcp.local.` and trigger RFC 6762 §9
    /// conflict-resolution renames on every boot. `MACHINE.control_board.name`
    /// is human-readable and contains spaces, so it cannot be used directly
    /// in a DNS label.
    pub const MDNS_HOSTNAME_BASE: &str = "rp2040";

    pub fn take(peripherals: embassy_rp::Peripherals) -> Self {
        Self {
            watchdog: peripherals.WATCHDOG,
            led: peripherals.PIN_25,
            spi: peripherals.SPI0,
            sck: peripherals.PIN_18,
            mosi: peripherals.PIN_19,
            miso: peripherals.PIN_16,
            cs: peripherals.PIN_17,
            phy_int: peripherals.PIN_21,
            phy_reset: peripherals.PIN_20,
            rx_dma: peripherals.DMA_CH0,
            tx_dma: peripherals.DMA_CH1,
            uart: peripherals.UART1,
            uart_tx: peripherals.PIN_4,
            uart_rx: peripherals.PIN_5,
            i2c: peripherals.I2C1,
            sda: peripherals.PIN_6,
            scl: peripherals.PIN_7,
            hb_rst: peripherals.PIN_2,
            hb_plug: peripherals.PIN_3,
            fan_pwm_slice: peripherals.PWM_SLICE4,
            fan_pwm_pin: peripherals.PIN_8,
            fan_tach_slice: peripherals.PWM_SLICE7,
            fan_tach_pin: peripherals.PIN_15,
            flash: peripherals.FLASH,
            #[cfg(feature = "display")]
            display_spi: peripherals.SPI1,
            #[cfg(feature = "display")]
            display_sck: peripherals.PIN_10,
            #[cfg(feature = "display")]
            display_mosi: peripherals.PIN_11,
            #[cfg(feature = "display")]
            display_cs: peripherals.PIN_14,
            #[cfg(feature = "display")]
            display_dc: peripherals.PIN_13,
            #[cfg(feature = "display")]
            display_rst: peripherals.PIN_12,
            #[cfg(feature = "display")]
            display_tx_dma: peripherals.DMA_CH2,
            core1: peripherals.CORE1,
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

/// Fan PWM and tachometer parameters. Default `clk_sys` on the RP2040 is
/// 125 MHz; `(PWM_TOP + 1) × divider = 5000` cycles per period puts the
/// PWM frequency at exactly 25 kHz, matching the Intel 4-wire fan spec.
/// The rp2350 port uses `PWM_TOP = 5999` at 150 MHz `clk_sys` to land at
/// the same 25 kHz.
pub struct Fan;
impl Fan {
    pub const PWM_TOP: u16 = 4999;
    pub const TACH_PULSES_PER_REV: u64 = 2;
}

pub struct Core1;
impl Core1 {
    /// Stack for the Core 1 executor, which runs only the ubos-mining
    /// BM1362 serial worker: the `BufferedUart` driver frames, the
    /// `ASIC_SESSION` critical sections, and `defmt` formatting on the
    /// send/receive path. 32 KiB is a generous default, not a measured
    /// high-water mark; trimming it safely would need on-target stack
    /// painting to find the true peak.
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
/// `SIZE` covers the full 2 MiB physical flash; the `CONFIG_RANGE`
/// claims the final two 4 KiB erase sectors at the end of flash,
/// matching the CONFIG region in the partitioned memory layout.
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
