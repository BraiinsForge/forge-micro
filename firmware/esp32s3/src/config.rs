//! Peripheral extraction for the ESP32-S3 `DevKit`. SPI frequency is pinned
//! below the W5100S datasheet limit because higher rates corrupt frames
//! on this wiring.
//!
//! Wiring/pinout: `../../../docs/SETUP.md`

use esp_hal::peripherals::{
    CPU_CTRL, GPIO1, GPIO10, GPIO11, GPIO2, GPIO39, GPIO4, GPIO40, GPIO41, GPIO42, GPIO43, GPIO44,
    GPIO5, GPIO8, GPIO9, I2C0, LEDC, PCNT, SENS, SPI2, SW_INTERRUPT, TIMG0, TIMG1, UART0,
};
#[cfg(feature = "display")]
use esp_hal::peripherals::{GPIO15, GPIO16, GPIO17, GPIO6, GPIO7, SPI3};
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

/// Default pool user for this board. Override at compile time with
/// `POOL_USER=...`.
pub const DEFAULT_POOL_USER: &str = "braiinstest.esp32s3";

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
    pub sens: SENS<'d>,
    pub sw_interrupt: SW_INTERRUPT<'d>,
    pub cpu_ctrl: CPU_CTRL<'d>,
    /// UART0 peripheral for the BM1362 chip serial link.
    pub uart0: UART0<'d>,
    /// UART0 TX pin (GPIO43, default ROM UART TX).
    pub uart_tx: GPIO43<'d>,
    /// UART0 RX pin (GPIO44, default ROM UART RX).
    pub uart_rx: GPIO44<'d>,
    /// I2C0 peripheral for the BMM100 PSU and LM75A temperature sensors.
    pub i2c0: I2C0<'d>,
    /// I2C0 SDA pin (GPIO8, conventional ESP32-S3 I2C0 SDA).
    pub i2c_sda: GPIO8<'d>,
    /// I2C0 SCL pin (GPIO9, conventional ESP32-S3 I2C0 SCL).
    pub i2c_scl: GPIO9<'d>,
    /// Hashboard reset pin (GPIO4, active-low, held low until miner task releases).
    pub hb_rst: GPIO4<'d>,
    /// Hashboard plug-detect pin (GPIO5, input pull-up).
    pub hb_plug: GPIO5<'d>,
    /// LEDC peripheral for 25 kHz fan PWM on GPIO10.
    pub ledc: LEDC<'d>,
    /// Fan PWM output pin (GPIO10).
    pub fan_pwm_pin: GPIO10<'d>,
    /// PCNT peripheral for fan tach pulse counting on GPIO11.
    pub pcnt: PCNT<'d>,
    /// Fan tach input pin (GPIO11).
    pub fan_tach_pin: GPIO11<'d>,
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
    /// DNS-safe base hostname for mDNS. See `ubos_net::mdns::compose_hostname`
    /// for the per-unit suffix and RFC 6762 §9 conflict-resolution semantics.
    pub const MDNS_HOSTNAME_BASE: &str = "esp32s3";
    /// MCU identifier exposed in the mDNS TXT record as `mcu=<MCU>`.
    pub const MCU: &str = "esp32s3";
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
            sens: peripherals.SENS,
            sw_interrupt: peripherals.SW_INTERRUPT,
            cpu_ctrl: peripherals.CPU_CTRL,
            uart0: peripherals.UART0,
            uart_tx: peripherals.GPIO43,
            uart_rx: peripherals.GPIO44,
            i2c0: peripherals.I2C0,
            i2c_sda: peripherals.GPIO8,
            i2c_scl: peripherals.GPIO9,
            hb_rst: peripherals.GPIO4,
            hb_plug: peripherals.GPIO5,
            ledc: peripherals.LEDC,
            fan_pwm_pin: peripherals.GPIO10,
            pcnt: peripherals.PCNT,
            fan_tach_pin: peripherals.GPIO11,
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
