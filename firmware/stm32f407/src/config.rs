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

//! Clock tree and peripheral extraction for the `DevEBox` STM32F407.
//! `stm32_config` runs the core at 168 MHz from an 8 MHz HSE for the
//! ethernet MAC and RMII reference clock.
//!
//! Wiring/pinout: `../../../docs/SETUP.md`

pub use ubos_machine::MINIMINER_STM32F407 as MACHINE;

use embassy_stm32::Peri;
use embassy_stm32::peripherals::{
    ADC1, DMA1_CH0, DMA1_CH1, DMA1_CH3, DMA1_CH6, DMA2_CH0, ETH, ETH_SMA, FLASH, I2C1, IWDG, PA1,
    PA2, PA6, PA7, PB6, PB7, PB8, PB9, PB11, PB12, PB13, PC1, PC4, PC5, PD8, PD9, PE0, PE1, PE14,
    RNG, TIM3, TIM4, USART3,
};
#[cfg(feature = "display")]
use embassy_stm32::peripherals::{DMA1_CH5, PC10, PC12, PD1, PD3, PD5, SPI3};
use embassy_stm32::time::Hertz;

pub struct Watchdog;
impl Watchdog {
    /// IWDG window in microseconds. 2 s keeps a comfortable margin above the
    /// 500 ms feed period while still triggering reset well before any stall
    /// stretches long enough to affect availability.
    pub const TIMEOUT_US: u32 = 2_000_000;
}

/// HSE 8 MHz crystal, PLL to 168 MHz system clock.
pub fn stm32_config() -> embassy_stm32::Config {
    let mut config = embassy_stm32::Config::default();
    config.rcc.hse = Some(embassy_stm32::rcc::Hse {
        freq: Hertz(8_000_000),
        mode: embassy_stm32::rcc::HseMode::Oscillator,
    });
    config.rcc.pll_src = embassy_stm32::rcc::PllSource::HSE;
    config.rcc.pll = Some(embassy_stm32::rcc::Pll {
        prediv: embassy_stm32::rcc::PllPreDiv::DIV4,
        mul: embassy_stm32::rcc::PllMul::MUL168,
        divp: Some(embassy_stm32::rcc::PllPDiv::DIV2),
        divq: Some(embassy_stm32::rcc::PllQDiv::DIV7),
        divr: None,
    });
    config.rcc.sys = embassy_stm32::rcc::Sysclk::PLL1_P;
    config.rcc.ahb_pre = embassy_stm32::rcc::AHBPrescaler::DIV1;
    config.rcc.apb1_pre = embassy_stm32::rcc::APBPrescaler::DIV4;
    config.rcc.apb2_pre = embassy_stm32::rcc::APBPrescaler::DIV2;
    config
}

/// Every peripheral singleton the board consumes, extracted in one place.
#[allow(dead_code)]
pub struct Board {
    pub iwdg: Peri<'static, IWDG>,
    pub rng: Peri<'static, RNG>,
    pub eth: Peri<'static, ETH>,
    pub eth_sma: Peri<'static, ETH_SMA>,
    pub adc: Peri<'static, ADC1>,
    pub adc_dma: Peri<'static, DMA2_CH0>,
    pub temp_trigger_timer: Peri<'static, TIM3>,
    pub led: Peri<'static, PA6>,
    pub osc_enable: Peri<'static, PE14>,
    pub ref_clk: Peri<'static, PA1>,
    pub crs_dv: Peri<'static, PA7>,
    pub rxd0: Peri<'static, PC4>,
    pub rxd1: Peri<'static, PC5>,
    pub txd0: Peri<'static, PB12>,
    pub txd1: Peri<'static, PB13>,
    pub tx_en: Peri<'static, PB11>,
    pub mdio: Peri<'static, PA2>,
    pub mdc: Peri<'static, PC1>,
    /// USART3 peripheral for the BM1362 chip serial link.
    pub usart3: Peri<'static, USART3>,
    /// USART3 TX pin (PD8, AF7).
    pub usart3_tx: Peri<'static, PD8>,
    /// USART3 RX pin (PD9, AF7).
    pub usart3_rx: Peri<'static, PD9>,
    /// DMA1 Stream1 Channel4 — `USART3_RX` per RM0090 table 42.
    pub usart3_rx_dma: Peri<'static, DMA1_CH1>,
    /// DMA1 Stream3 Channel4 — `USART3_TX` per RM0090 table 42.
    pub usart3_tx_dma: Peri<'static, DMA1_CH3>,
    /// I2C1 peripheral for the BMM101 PSU and LM75A temperature sensors.
    pub i2c: Peri<'static, I2C1>,
    /// I2C1 SCL pin (PB6, AF4).
    pub scl: Peri<'static, PB6>,
    /// I2C1 SDA pin (PB7, AF4).
    pub sda: Peri<'static, PB7>,
    /// DMA1 Stream0 — `I2C1_RX` per RM0090 table 42.
    pub i2c_rx_dma: Peri<'static, DMA1_CH0>,
    /// DMA1 Stream6 — `I2C1_TX` per RM0090 table 42.
    pub i2c_tx_dma: Peri<'static, DMA1_CH6>,
    /// Hashboard reset line (active-low GPIO output, PE0).
    pub hb_rst: Peri<'static, PE0>,
    /// Hashboard plug-detect line (GPIO input with pull-up, PE1).
    pub hb_plug: Peri<'static, PE1>,
    /// TIM4 peripheral shared between PWM (CH3/PB8) and tach (CH4/PB9).
    pub tim4: Peri<'static, TIM4>,
    /// Fan PWM output pin (PB8, `TIM4_CH3` AF2).
    pub fan_pwm_pin: Peri<'static, PB8>,
    /// Fan tachometer input pin (PB9, `TIM4_CH4` AF2).
    pub fan_tach_pin: Peri<'static, PB9>,
    /// FLASH peripheral for the on-board 1 MiB flash; ubos-config persists
    /// the runtime config record in the last two 128 KiB sectors of this device.
    pub flash: Peri<'static, FLASH>,
    #[cfg(feature = "display")]
    pub display_spi: Peri<'static, SPI3>,
    #[cfg(feature = "display")]
    pub display_sck: Peri<'static, PC10>,
    #[cfg(feature = "display")]
    pub display_mosi: Peri<'static, PC12>,
    #[cfg(feature = "display")]
    pub display_cs: Peri<'static, PD5>,
    #[cfg(feature = "display")]
    pub display_dc: Peri<'static, PD3>,
    #[cfg(feature = "display")]
    pub display_rst: Peri<'static, PD1>,
    #[cfg(feature = "display")]
    pub display_tx_dma: Peri<'static, DMA1_CH5>,
}

#[cfg(feature = "display")]
pub struct DisplaySpi;
#[cfg(feature = "display")]
impl DisplaySpi {
    pub const FREQUENCY: Hertz = Hertz(10_000_000);
}

/// On-board flash parameters for the ubos-config record store.
///
/// Claims the last two 128 KiB sectors of bank 1 (sectors 10-11) so
/// the config store has the two-page minimum sequential-storage
/// requires. The application binary must not grow past the start of
/// the range — the linker does not enforce this today, so a binary
/// creeping past 768 KiB would silently overwrite the config region.
pub struct Flash;
impl Flash {
    pub const CONFIG_RANGE: core::ops::Range<u32> = 0x000C_0000..0x0010_0000;
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
    /// DNS-safe base hostname for mDNS. See `ubos_mdns::compose_hostname`
    /// for the per-unit suffix and RFC 6762 §9 conflict-resolution semantics.
    pub const MDNS_HOSTNAME_BASE: &str = "stm32f407";

    pub fn take(peripherals: embassy_stm32::Peripherals) -> Self {
        Self {
            iwdg: peripherals.IWDG,
            rng: peripherals.RNG,
            eth: peripherals.ETH,
            eth_sma: peripherals.ETH_SMA,
            adc: peripherals.ADC1,
            adc_dma: peripherals.DMA2_CH0,
            temp_trigger_timer: peripherals.TIM3,
            led: peripherals.PA6,
            osc_enable: peripherals.PE14,
            ref_clk: peripherals.PA1,
            crs_dv: peripherals.PA7,
            rxd0: peripherals.PC4,
            rxd1: peripherals.PC5,
            txd0: peripherals.PB12,
            txd1: peripherals.PB13,
            tx_en: peripherals.PB11,
            mdio: peripherals.PA2,
            mdc: peripherals.PC1,
            usart3: peripherals.USART3,
            usart3_tx: peripherals.PD8,
            usart3_rx: peripherals.PD9,
            usart3_rx_dma: peripherals.DMA1_CH1,
            usart3_tx_dma: peripherals.DMA1_CH3,
            i2c: peripherals.I2C1,
            scl: peripherals.PB6,
            sda: peripherals.PB7,
            i2c_rx_dma: peripherals.DMA1_CH0,
            i2c_tx_dma: peripherals.DMA1_CH6,
            hb_rst: peripherals.PE0,
            hb_plug: peripherals.PE1,
            tim4: peripherals.TIM4,
            fan_pwm_pin: peripherals.PB8,
            fan_tach_pin: peripherals.PB9,
            flash: peripherals.FLASH,
            #[cfg(feature = "display")]
            display_spi: peripherals.SPI3,
            #[cfg(feature = "display")]
            display_sck: peripherals.PC10,
            #[cfg(feature = "display")]
            display_mosi: peripherals.PC12,
            #[cfg(feature = "display")]
            display_cs: peripherals.PD5,
            #[cfg(feature = "display")]
            display_dc: peripherals.PD3,
            #[cfg(feature = "display")]
            display_rst: peripherals.PD1,
            #[cfg(feature = "display")]
            display_tx_dma: peripherals.DMA1_CH5,
        }
    }
}
