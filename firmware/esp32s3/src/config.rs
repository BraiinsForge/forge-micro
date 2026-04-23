//! Board constants and peripheral extraction for the ESP32-S3 `DevKit`.
//! `Board::take` consumes the esp-hal `Peripherals` and returns the SPI2
//! peripheral, four GPIO pins, two DMA channels, RNG, and timer group
//! that main.rs needs. SPI frequency is pinned below the W5100S
//! datasheet limit because higher rates corrupt frames on this wiring.

use esp_hal::peripherals::{GPIO1, GPIO2, GPIO39, GPIO40, GPIO41, GPIO42, SPI2, TIMG0, TIMG1};
#[cfg(feature = "display")]
use esp_hal::peripherals::{GPIO15, GPIO16, GPIO17, GPIO6, GPIO7, SPI3};
#[cfg(feature = "uart-echo")]
use esp_hal::peripherals::{GPIO43, GPIO44, UART0};
use esp_hal::time::Rate;

pub struct Spi;
impl Spi {
    /// W5100S datasheet allows up to 33.3 MHz, but on this board 25 MHz and above
    /// produces corrupted RX frame lengths. 20 MHz is the highest stable speed.
    pub const FREQUENCY: Rate = Rate::from_mhz(20);
}

#[cfg(feature = "display")]
pub struct DisplaySpi;
#[cfg(feature = "display")]
impl DisplaySpi {
    pub const FREQUENCY: Rate = Rate::from_mhz(10);
}

pub struct Watchdog;
impl Watchdog {
    pub const TIMEOUT: esp_hal::time::Duration = esp_hal::time::Duration::from_millis(2000);
}

/// Every peripheral singleton the board consumes, extracted in one place.
pub struct Board<'d> {
    pub timg0: TIMG0<'d>,
    pub timg1: TIMG1<'d>,
    pub spi: SPI2<'d>,
    pub sck: GPIO1<'d>,
    pub mosi: GPIO2<'d>,
    pub miso: GPIO42<'d>,
    pub cs: GPIO39<'d>,
    pub w5500_int: GPIO41<'d>,
    pub w5500_reset: GPIO40<'d>,
    pub dma: esp_hal::peripherals::DMA_CH0<'d>,
    #[cfg(feature = "uart-echo")]
    pub uart0: UART0<'d>,
    #[cfg(feature = "uart-echo")]
    pub uart_tx: GPIO43<'d>,
    #[cfg(feature = "uart-echo")]
    pub uart_rx: GPIO44<'d>,
    #[cfg(feature = "display")]
    pub display_spi: SPI3<'d>,
    #[cfg(feature = "display")]
    pub display_sck: GPIO6<'d>,
    #[cfg(feature = "display")]
    pub display_mosi: GPIO7<'d>,
    #[cfg(feature = "display")]
    pub display_cs: GPIO15<'d>,
    #[cfg(feature = "display")]
    pub display_dc: GPIO16<'d>,
    #[cfg(feature = "display")]
    pub display_rst: GPIO17<'d>,
    #[cfg(feature = "display")]
    pub display_dma: esp_hal::peripherals::DMA_CH1<'d>,
}

impl Board<'static> {
    pub const NAME: &str = "ESP32-S3 W5100S";
    /// Locally-administered MAC address. The LAA bit, bit 1 of the first octet, marks it as local.
    pub const MAC_ADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];

    pub fn take(peripherals: esp_hal::peripherals::Peripherals) -> Self {
        Self {
            timg0: peripherals.TIMG0,
            timg1: peripherals.TIMG1,
            spi: peripherals.SPI2,
            sck: peripherals.GPIO1,
            mosi: peripherals.GPIO2,
            miso: peripherals.GPIO42,
            cs: peripherals.GPIO39,
            w5500_int: peripherals.GPIO41,
            w5500_reset: peripherals.GPIO40,
            dma: peripherals.DMA_CH0,
            #[cfg(feature = "uart-echo")]
            uart0: peripherals.UART0,
            #[cfg(feature = "uart-echo")]
            uart_tx: peripherals.GPIO43,
            #[cfg(feature = "uart-echo")]
            uart_rx: peripherals.GPIO44,
            #[cfg(feature = "display")]
            display_spi: peripherals.SPI3,
            #[cfg(feature = "display")]
            display_sck: peripherals.GPIO6,
            #[cfg(feature = "display")]
            display_mosi: peripherals.GPIO7,
            #[cfg(feature = "display")]
            display_cs: peripherals.GPIO15,
            #[cfg(feature = "display")]
            display_dc: peripherals.GPIO16,
            #[cfg(feature = "display")]
            display_rst: peripherals.GPIO17,
            #[cfg(feature = "display")]
            display_dma: peripherals.DMA_CH1,
        }
    }
}
