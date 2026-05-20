//! Clock tree and peripheral extraction for the Nucleo-F439ZI.
//!
//! Wiring/pinout: `../../../docs/SETUP.md`.

use embassy_stm32::peripherals::{
    ADC1, DMA2_CH0, ETH, ETH_SMA, IWDG, PA1, PA2, PA7, PB0, PB13, PC1, PC4, PC5, PG11, PG13, RNG,
    TIM3,
};
#[cfg(feature = "backend-uart")]
use embassy_stm32::peripherals::{DMA1_CH1, DMA1_CH3, PD8, PD9, USART3};
#[cfg(feature = "display")]
use embassy_stm32::peripherals::{DMA2_CH3, PB3, PB5, PE7, PE8, PE9, SPI1};
use embassy_stm32::time::Hertz;
use embassy_stm32::Peri;

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
    #[cfg(feature = "backend-uart")]
    pub usart3: Peri<'static, USART3>,
    #[cfg(feature = "backend-uart")]
    pub usart3_tx: Peri<'static, PD8>,
    #[cfg(feature = "backend-uart")]
    pub usart3_rx: Peri<'static, PD9>,
    /// DMA1 Stream1 Channel4 — `USART3_RX` per RM0090 table 42.
    #[cfg(feature = "backend-uart")]
    pub usart3_rx_dma: Peri<'static, DMA1_CH1>,
    /// DMA1 Stream3 Channel4 — `USART3_TX` per RM0090 table 42.
    #[cfg(feature = "backend-uart")]
    pub usart3_tx_dma: Peri<'static, DMA1_CH3>,
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

/// Default pool user for this board. Override at compile time with
/// `POOL_USER=...`.
pub const DEFAULT_POOL_USER: &str = "braiinstest.stm32f439";

impl Board {
    pub const NAME: &str = "Nucleo-F439ZI LAN8742A";
    /// Locally-administered MAC address. The LAA bit, bit 1 of the first octet, marks it as local.
    pub const MAC_ADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x05];

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
            #[cfg(feature = "backend-uart")]
            usart3: peripherals.USART3,
            #[cfg(feature = "backend-uart")]
            usart3_tx: peripherals.PD8,
            #[cfg(feature = "backend-uart")]
            usart3_rx: peripherals.PD9,
            #[cfg(feature = "backend-uart")]
            usart3_rx_dma: peripherals.DMA1_CH1,
            #[cfg(feature = "backend-uart")]
            usart3_tx_dma: peripherals.DMA1_CH3,
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
