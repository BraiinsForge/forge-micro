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

//! Board constants, clock tree, and peripheral extraction for the
//! Nucleo-F439ZI. `stm32_config` builds the `embassy_stm32::Config`
//! that drives the core at 180 MHz from an 8 MHz HSE in bypass mode
//! (ST-LINK MCO), which the ethernet MAC and RMII reference clock
//! depend on. `Board::take` consumes the peripherals main.rs needs:
//! ETH, PHY management pins, seven RMII pins, watchdog, LED, USART3,
//! I2C1, TIM4, and the hashboard control pins.
//! RMII TX pins are PG11/PG13, matching the MB1137 default solder
//! bridges SB183 and SB182 that wire them to the onboard LAN8742A.
//!
//! Wiring/pinout: `../../../docs/SETUP.md`.

pub use ubos_machine::MINIMINER_STM32F439 as MACHINE;

/// How many hashboard chains this board drives. Every array sized over
/// the chains takes its length from here, so a machine that gains a
/// chain fails to build until each is wired.
pub const CHAINS: usize = MACHINE.chains;

use embassy_stm32::Peri;
use embassy_stm32::peripherals::{
    ADC1, DMA1_CH0, DMA1_CH1, DMA1_CH3, DMA1_CH6, DMA2_CH0, ETH, ETH_SMA, FLASH, I2C1, IWDG, PA1,
    PA2, PA7, PB0, PB6, PB7, PB8, PB9, PB13, PC1, PC4, PC5, PD8, PD9, PE0, PE1, PG11, PG13, RNG,
    TIM3, TIM4, USART3,
};
#[cfg(feature = "display")]
use embassy_stm32::peripherals::{DMA2_CH3, PB3, PB5, PE7, PE8, PE9, SPI1};
use embassy_stm32::time::Hertz;

pub struct Watchdog;
impl Watchdog {
    /// IWDG window in microseconds. 2 s keeps a comfortable margin above the
    /// 500 ms feed period while still triggering reset well before any stall
    /// stretches long enough to affect availability.
    pub const TIMEOUT_US: u32 = 2_000_000;
}

/// HSE 8 MHz bypass (ST-LINK MCO), PLL to 180 MHz system clock.
pub fn stm32_config() -> embassy_stm32::Config {
    let mut config = embassy_stm32::Config::default();
    config.rcc.hse = Some(embassy_stm32::rcc::Hse {
        freq: Hertz(8_000_000),
        mode: embassy_stm32::rcc::HseMode::Bypass,
    });
    config.rcc.pll_src = embassy_stm32::rcc::PllSource::HSE;
    config.rcc.pll = Some(embassy_stm32::rcc::Pll {
        prediv: embassy_stm32::rcc::PllPreDiv::DIV4,
        mul: embassy_stm32::rcc::PllMul::MUL180,
        divp: Some(embassy_stm32::rcc::PllPDiv::DIV2),
        divq: Some(embassy_stm32::rcc::PllQDiv::DIV8),
        divr: None,
    });
    config.rcc.sys = embassy_stm32::rcc::Sysclk::PLL1_P;
    config.rcc.ahb_pre = embassy_stm32::rcc::AHBPrescaler::DIV1;
    config.rcc.apb1_pre = embassy_stm32::rcc::APBPrescaler::DIV4;
    config.rcc.apb2_pre = embassy_stm32::rcc::APBPrescaler::DIV2;
    config
}

/// Every peripheral singleton the board consumes, extracted in one place.
pub struct Board {
    pub iwdg: Peri<'static, IWDG>,
    pub rng: Peri<'static, RNG>,
    pub eth: Peri<'static, ETH>,
    pub eth_sma: Peri<'static, ETH_SMA>,
    pub adc: Peri<'static, ADC1>,
    pub adc_dma: Peri<'static, DMA2_CH0>,
    pub temp_trigger_timer: Peri<'static, TIM3>,
    /// LD1 green LED (PB0, active-high).
    pub led: Peri<'static, PB0>,
    pub ref_clk: Peri<'static, PA1>,
    pub crs_dv: Peri<'static, PA7>,
    pub rxd0: Peri<'static, PC4>,
    pub rxd1: Peri<'static, PC5>,
    pub txd0: Peri<'static, PG13>,
    pub txd1: Peri<'static, PB13>,
    pub tx_en: Peri<'static, PG11>,
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
    /// FLASH peripheral for the on-board flash; ubos-config persists the
    /// runtime config record in the last two 128 KiB sectors of bank 1.
    pub flash: Peri<'static, FLASH>,
    #[cfg(feature = "display")]
    pub display_spi: Peri<'static, SPI1>,
    #[cfg(feature = "display")]
    pub display_sck: Peri<'static, PB3>,
    #[cfg(feature = "display")]
    pub display_mosi: Peri<'static, PB5>,
    #[cfg(feature = "display")]
    pub display_cs: Peri<'static, PE7>,
    #[cfg(feature = "display")]
    pub display_dc: Peri<'static, PE8>,
    #[cfg(feature = "display")]
    pub display_rst: Peri<'static, PE9>,
    #[cfg(feature = "display")]
    pub display_tx_dma: Peri<'static, DMA2_CH3>,
}

#[cfg(feature = "display")]
pub struct DisplaySpi;
#[cfg(feature = "display")]
impl DisplaySpi {
    pub const FREQUENCY: Hertz = Hertz(10_000_000);
}

/// The board descriptor this firmware targets: the 4-chip BM1362 BMM101.
/// Every crate that needs board facts takes this explicitly rather than
/// selecting a board behind a cargo feature.
pub const BOARD: &ubos_asic::HashboardDescriptor = &MACHINE.hashboard;

/// The Bitmain chip model this board's hashboard carries; injected into
/// the mining actor as the concrete chain driver.
pub const CHIP: ubos_asic::Chip = ubos_asic::Chip::Bm1362;

/// The tuners this board runs. The miner builds them, and [`BOUNDS`]
/// lists them for the config and the mining form.
pub type TunerSet = ubos_mining::tuner::Tuners;

/// Accepted tuning ranges and tuners for [`BOARD`]; the API validates
/// against these.
pub const BOUNDS: ubos_config::MiningBounds = ubos_config::bounds(
    BOARD,
    <TunerSet as ubos_mining::tuner::MachineTuner<CHAINS>>::TUNERS,
);

/// Static mining capability metadata, built from [`BOARD`].
pub const MINING_INFO: ubos_config::MiningInfo =
    ubos_config::mining_info(BOARD, &BOUNDS, ubos_config::DefaultTuning::Fixed);

/// Distinct series the Prometheus exposition map holds. The map is keyed
/// by the label-expanded series, and a series that does not fit is
/// dropped with a warning. Must be a power of two, which the map requires.
pub const MAX_METRICS: usize = 64;

/// Bytes of the buffer one scrape is rendered into. It holds every series
/// in the map at once, so it moves with [`MAX_METRICS`].
pub const METRICS_RESPONSE_BYTES: usize = 4096;

impl Board {
    /// DNS-safe base hostname for mDNS. See `ubos_mdns::compose_hostname`
    /// for the per-unit suffix and RFC 6762 §9 conflict-resolution semantics.
    pub const MDNS_HOSTNAME_BASE: &str = "stm32f439";

    pub fn take(peripherals: embassy_stm32::Peripherals) -> Self {
        Self {
            iwdg: peripherals.IWDG,
            rng: peripherals.RNG,
            eth: peripherals.ETH,
            eth_sma: peripherals.ETH_SMA,
            adc: peripherals.ADC1,
            adc_dma: peripherals.DMA2_CH0,
            temp_trigger_timer: peripherals.TIM3,
            led: peripherals.PB0,
            ref_clk: peripherals.PA1,
            crs_dv: peripherals.PA7,
            rxd0: peripherals.PC4,
            rxd1: peripherals.PC5,
            txd0: peripherals.PG13,
            txd1: peripherals.PB13,
            tx_en: peripherals.PG11,
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
            display_spi: peripherals.SPI1,
            #[cfg(feature = "display")]
            display_sck: peripherals.PB3,
            #[cfg(feature = "display")]
            display_mosi: peripherals.PB5,
            #[cfg(feature = "display")]
            display_cs: peripherals.PE7,
            #[cfg(feature = "display")]
            display_dc: peripherals.PE8,
            #[cfg(feature = "display")]
            display_rst: peripherals.PE9,
            #[cfg(feature = "display")]
            display_tx_dma: peripherals.DMA2_CH3,
        }
    }
}

/// On-board flash parameters for the ubos-config record store.
///
/// Claims the last two 128 KiB sectors of bank 1 (sectors 10-11) so
/// the config store has the two-page minimum sequential-storage
/// requires. The same range as the F407 because bank 1's sector
/// geometry is identical; bank 2 stays unused for now. The
/// application binary must not grow past the start of the range —
/// the linker does not enforce this today, so a binary creeping past
/// 768 KiB would silently overwrite the config region.
pub struct Flash;
impl Flash {
    pub const CONFIG_RANGE: core::ops::Range<u32> = 0x000C_0000..0x0010_0000;
}
