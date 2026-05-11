//! Board constants and peripheral extraction for the Pico. `Board::take`
//! consumes the RP2040 `Peripherals` once and returns the exact set
//! this board uses: SPI0, six GPIO pins, watchdog, and a DMA channel.
//! This keeps pin assignments in one place rather than scattered
//! through `main`.

use embassy_rp::peripherals::{
    ADC, ADC_TEMP_SENSOR, CORE1, DMA_CH0, DMA_CH1, PIN_16, PIN_17, PIN_18, PIN_19, PIN_20, PIN_21,
    PIN_25, SPI0, WATCHDOG,
};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, PIN_10, PIN_11, PIN_12, PIN_13, PIN_14, SPI1};
#[cfg(feature = "backend-uart")]
use embassy_rp::peripherals::{PIN_4, PIN_5, UART1};
use embassy_rp::Peri;
use embassy_time::Duration;

/// Every peripheral singleton the board consumes, extracted in one place.
pub struct Board {
    pub watchdog: Peri<'static, WATCHDOG>,
    pub led: Peri<'static, PIN_25>,
    pub adc: Peri<'static, ADC>,
    pub adc_temp_sensor: Peri<'static, ADC_TEMP_SENSOR>,
    pub spi: Peri<'static, SPI0>,
    pub sck: Peri<'static, PIN_18>,
    pub mosi: Peri<'static, PIN_19>,
    pub miso: Peri<'static, PIN_16>,
    pub cs: Peri<'static, PIN_17>,
    pub w5500_int: Peri<'static, PIN_21>,
    pub w5500_reset: Peri<'static, PIN_20>,
    pub rx_dma: Peri<'static, DMA_CH0>,
    pub tx_dma: Peri<'static, DMA_CH1>,
    pub core1: Peri<'static, CORE1>,
    #[cfg(feature = "backend-uart")]
    pub uart: Peri<'static, UART1>,
    #[cfg(feature = "backend-uart")]
    pub uart_tx: Peri<'static, PIN_4>,
    #[cfg(feature = "backend-uart")]
    pub uart_rx: Peri<'static, PIN_5>,
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
}

impl Board {
    pub const NAME: &str = "Pico W5500 HAT";
    /// Locally-administered MAC address. The LAA bit, bit 1 of the first octet, marks it as local.
    pub const MAC_ADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x04];

    /// Extracts every peripheral this board uses from the singleton.
    pub fn take(peripherals: embassy_rp::Peripherals) -> Self {
        Self {
            watchdog: peripherals.WATCHDOG,
            led: peripherals.PIN_25,
            adc: peripherals.ADC,
            adc_temp_sensor: peripherals.ADC_TEMP_SENSOR,
            spi: peripherals.SPI0,
            sck: peripherals.PIN_18,
            mosi: peripherals.PIN_19,
            miso: peripherals.PIN_16,
            cs: peripherals.PIN_17,
            w5500_int: peripherals.PIN_21,
            w5500_reset: peripherals.PIN_20,
            rx_dma: peripherals.DMA_CH0,
            tx_dma: peripherals.DMA_CH1,
            core1: peripherals.CORE1,
            #[cfg(feature = "backend-uart")]
            uart: peripherals.UART1,
            #[cfg(feature = "backend-uart")]
            uart_tx: peripherals.PIN_4,
            #[cfg(feature = "backend-uart")]
            uart_rx: peripherals.PIN_5,
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
        }
    }
}

/// SPI clock frequency for the W5500 Ethernet chip.
pub struct Spi;
impl Spi {
    pub const FREQUENCY: u32 = 33_000_000;
}

/// SPI clock frequency for the SH1106 OLED display.
#[cfg(feature = "display")]
pub struct DisplaySpi;
#[cfg(feature = "display")]
impl DisplaySpi {
    pub const FREQUENCY: u32 = 10_000_000;
}

/// Hardware watchdog timeout.
pub struct Watchdog;
impl Watchdog {
    pub const TIMEOUT: Duration = Duration::from_secs(2);
}

/// Core 1 stack size for the worker executor.
pub struct Core1;
impl Core1 {
    /// Stack size in bytes for the Core 1 executor.
    pub const STACK_BYTES: usize = 4096;
}

/// Stratum pool connection parameters.
#[cfg(any(feature = "backend-cpu", feature = "backend-uart",))]
pub struct Pool;
#[cfg(any(feature = "backend-cpu", feature = "backend-uart",))]
impl Pool {
    pub const HOST_IP: [u8; 4] = [192, 168, 89, 100];
    pub const PORT: u16 = 3333;
    pub const USER: &'static str = "bcrt1qsdtjq8zjhefamsumwcfc6yh4gd98c900v8r4e8.rp2040";
    pub const PASS: &'static str = "x";
    pub const RECONNECT_DELAY: Duration = Duration::from_secs(5);
}
