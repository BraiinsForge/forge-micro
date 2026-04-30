//! Board constants, clock tree, and peripheral extraction for the
//! `DevEBox` STM32F407. `stm32_config` builds the `embassy_stm32::Config`
//! that drives the core at 168 MHz from an 8 MHz HSE, which the
//! ethernet MAC and RMII reference clock depend on. `Board::take`
//! consumes the peripherals main.rs needs: ETH, PHY management pins,
//! seven RMII pins, watchdog, and LED.

use embassy_stm32::peripherals::{
    ADC1, DMA2_CH0, ETH, ETH_SMA, IWDG, PA1, PA2, PA6, PA7, PB11, PB12, PB13, PC1, PC4, PC5, PE14,
    RNG, TIM3,
};
#[cfg(feature = "display")]
use embassy_stm32::peripherals::{DMA1_CH5, PC10, PC12, PD1, PD3, PD5, SPI3};
#[cfg(feature = "dummy-asic")]
use embassy_stm32::peripherals::{PD8, PD9, USART3};
use embassy_stm32::time::Hertz;
use embassy_stm32::Peri;

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
    #[cfg(feature = "dummy-asic")]
    pub usart3: Peri<'static, USART3>,
    #[cfg(feature = "dummy-asic")]
    pub usart3_tx: Peri<'static, PD8>,
    #[cfg(feature = "dummy-asic")]
    pub usart3_rx: Peri<'static, PD9>,
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

/// Stratum pool connection parameters.
#[cfg(any(feature = "cpu-mining", feature = "dummy-asic"))]
pub struct Pool;
#[cfg(any(feature = "cpu-mining", feature = "dummy-asic"))]
impl Pool {
    pub const HOST_IP: [u8; 4] = [192, 168, 89, 100];
    pub const PORT: u16 = 3333;
    pub const USER: &'static str = "bcrt1qsdtjq8zjhefamsumwcfc6yh4gd98c900v8r4e8.stm32f407";
    pub const PASS: &'static str = "x";
    pub const RECONNECT_DELAY: embassy_time::Duration = embassy_time::Duration::from_secs(5);
}

impl Board {
    pub const NAME: &str = "DevEBox STM32F407 LAN8720";
    /// Locally-administered MAC address. The LAA bit, bit 1 of the first octet, marks it as local.
    pub const MAC_ADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x03];

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
            #[cfg(feature = "dummy-asic")]
            usart3: peripherals.USART3,
            #[cfg(feature = "dummy-asic")]
            usart3_tx: peripherals.PD8,
            #[cfg(feature = "dummy-asic")]
            usart3_rx: peripherals.PD9,
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
