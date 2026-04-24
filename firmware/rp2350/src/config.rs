//! Board constants and peripheral extraction for the Pico 2. `Board::take`
//! consumes the RP2350 `Peripherals` once and returns the exact set
//! this board uses: SPI0, seven GPIO pins, watchdog, and two DMA channels.
//! This keeps pin assignments in one place rather than scattered
//! through `main`.

use embassy_rp::peripherals::{
    ADC, ADC_TEMP_SENSOR, CORE1, DMA_CH0, DMA_CH1, PIN_16, PIN_17, PIN_18, PIN_19, PIN_20, PIN_21,
    PIN_25, SPI0, WATCHDOG,
};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, PIN_10, PIN_11, PIN_12, PIN_8, PIN_9, SPI1};
#[cfg(feature = "uart-echo")]
use embassy_rp::peripherals::{PIN_4, PIN_5, UART1};
use embassy_rp::Peri;
use embassy_time::Duration;

/// Every peripheral singleton the board consumes, extracted in one place.
#[allow(dead_code)]
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
    pub w6100_int: Peri<'static, PIN_21>,
    pub w6100_reset: Peri<'static, PIN_20>,
    pub rx_dma: Peri<'static, DMA_CH0>,
    pub tx_dma: Peri<'static, DMA_CH1>,
    #[cfg(feature = "uart-echo")]
    pub uart: Peri<'static, UART1>,
    #[cfg(feature = "uart-echo")]
    pub uart_tx: Peri<'static, PIN_4>,
    #[cfg(feature = "uart-echo")]
    pub uart_rx: Peri<'static, PIN_5>,
    #[cfg(feature = "display")]
    pub display_spi: Peri<'static, SPI1>,
    #[cfg(feature = "display")]
    pub display_sck: Peri<'static, PIN_10>,
    #[cfg(feature = "display")]
    pub display_mosi: Peri<'static, PIN_11>,
    #[cfg(feature = "display")]
    pub display_cs: Peri<'static, PIN_12>,
    #[cfg(feature = "display")]
    pub display_dc: Peri<'static, PIN_8>,
    #[cfg(feature = "display")]
    pub display_rst: Peri<'static, PIN_9>,
    #[cfg(feature = "display")]
    pub display_tx_dma: Peri<'static, DMA_CH2>,
    pub core1: Peri<'static, CORE1>,
}

impl Board {
    pub const NAME: &str = "Pico2 W6100";
    /// Locally-administered MAC address. The LAA bit, bit 1 of the first octet, marks it as local.
    pub const MAC_ADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];

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
            w6100_int: peripherals.PIN_21,
            w6100_reset: peripherals.PIN_20,
            rx_dma: peripherals.DMA_CH0,
            tx_dma: peripherals.DMA_CH1,
            #[cfg(feature = "uart-echo")]
            uart: peripherals.UART1,
            #[cfg(feature = "uart-echo")]
            uart_tx: peripherals.PIN_4,
            #[cfg(feature = "uart-echo")]
            uart_rx: peripherals.PIN_5,
            #[cfg(feature = "display")]
            display_spi: peripherals.SPI1,
            #[cfg(feature = "display")]
            display_sck: peripherals.PIN_10,
            #[cfg(feature = "display")]
            display_mosi: peripherals.PIN_11,
            #[cfg(feature = "display")]
            display_cs: peripherals.PIN_12,
            #[cfg(feature = "display")]
            display_dc: peripherals.PIN_8,
            #[cfg(feature = "display")]
            display_rst: peripherals.PIN_9,
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

#[cfg(feature = "cpu-mining")]
pub struct Core1;
#[cfg(feature = "cpu-mining")]
impl Core1 {
    pub const STACK_BYTES: usize = 4096;
}

// Pool presets kept so switching pool or user is a one-line edit on
// `Pool::HOST_IP`/`Pool::USER` below. The unused ones stay behind
// `allow(dead_code)` because only one pair is active at a time.
#[cfg(feature = "cpu-mining")]
#[allow(dead_code)]
const RPI_POOL: [u8; 4] = [192, 168, 89, 100];
#[cfg(feature = "cpu-mining")]
#[allow(dead_code)]
const REGTEST_SOLO_USER: &str = "bcrt1qsdtjq8zjhefamsumwcfc6yh4gd98c900v8r4e8";
#[cfg(feature = "cpu-mining")]
#[allow(dead_code)]
const BRAIINS_POOL: [u8; 4] = [172, 65, 65, 63];
#[cfg(feature = "cpu-mining")]
#[allow(dead_code)]
const BRAIINS_USER: &str = "braiinstest";

#[cfg(feature = "cpu-mining")]
pub struct Pool;
#[cfg(feature = "cpu-mining")]
impl Pool {
    pub const HOST_IP: [u8; 4] = RPI_POOL;
    pub const PORT: u16 = 3333;
    pub const USER: &'static str = "bcrt1qsdtjq8zjhefamsumwcfc6yh4gd98c900v8r4e8.rp2350";
    pub const PASS: &'static str = "x";
    pub const RECONNECT_DELAY: Duration = Duration::from_secs(5);
}
