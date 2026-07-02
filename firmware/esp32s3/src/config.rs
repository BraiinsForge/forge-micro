//! Peripheral extraction for the ESP32-S3 `DevKit`. SPI frequency is pinned
//! below the W5100S datasheet limit because higher rates corrupt frames
//! on this wiring.
//!
//! Wiring/pinout: `../../../docs/SETUP.md`

use esp_hal::peripherals::{
    CPU_CTRL, FLASH, GPIO1, GPIO2, GPIO5, GPIO7, GPIO10, GPIO11, GPIO15, GPIO39, GPIO40, GPIO41,
    GPIO42, GPIO43, GPIO44, I2C0, LEDC, PCNT, SENS, SPI2, SW_INTERRUPT, TIMG0, TIMG1, UART0,
};
#[cfg(feature = "display")]
use esp_hal::peripherals::{GPIO6, GPIO12, GPIO13, GPIO16, GPIO17, SPI3};
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

/// On-board flash parameters for the ubos-config record store.
///
/// The board runs the OTA partition layout in `partitions.csv`. The store
/// takes the first two 4 KiB sectors of the `config` data partition, which
/// begins at `0x61_0000` above the `ota_1` app slot. Keep this in sync with
/// `partitions.csv`; the offset is absolute from the flash base, not relative
/// to the partition.
pub struct Flash;
impl Flash {
    pub const CONFIG_RANGE: core::ops::Range<u32> = 0x0061_0000..0x0061_2000;
}

/// OTA upload limit, reported through `OtaTarget::max_image_bytes`. Equals
/// the `ota_N` app slot in `partitions.csv` (3 MiB): the image is written
/// into the inactive slot, so an image larger than the slot is rejected up
/// front.
pub struct Ota;
impl Ota {
    pub const MAX_IMAGE_BYTES: u64 = 3 * 1024 * 1024;
}

pub struct Watchdog;
impl Watchdog {
    pub const TIMEOUT: esp_hal::time::Duration = esp_hal::time::Duration::from_millis(2000);
}

/// Firmware version compiled into the running image, reported over the OTA
/// API and used to label updates. Set at build time via `UBOS_FW_VERSION`
/// (the flake image build passes it); defaults to 1 for local builds.
pub const FIRMWARE_VERSION: u32 = match option_env!("UBOS_FW_VERSION") {
    Some(raw) => ubos_ota::parse_version(raw),
    None => 1,
};

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
    /// I2C0 SDA pin (GPIO15, routed via the GPIO matrix). Shares the bus
    /// with the carrier's TCA9554 I/O expander and touch controller.
    pub i2c_sda: GPIO15<'d>,
    /// I2C0 SCL pin (GPIO7, routed via the GPIO matrix). Shares the bus
    /// with the carrier's TCA9554 I/O expander and touch controller.
    pub i2c_scl: GPIO7<'d>,
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
    /// On-board SPI flash for the ubos-config record store.
    pub flash: FLASH<'d>,
    #[cfg(feature = "display")]
    pub display_spi: SPI3<'d>,
    #[cfg(feature = "display")]
    pub display_sck: GPIO6<'d>,
    #[cfg(feature = "display")]
    pub display_mosi: GPIO13<'d>,
    #[cfg(feature = "display")]
    pub display_cs: GPIO12<'d>,
    #[cfg(feature = "display")]
    pub display_dc: GPIO16<'d>,
    #[cfg(feature = "display")]
    pub display_rst: GPIO17<'d>,
    #[cfg(feature = "display")]
    pub display_dma: esp_hal::peripherals::DMA_CH1<'d>,
}

/// The board descriptor this firmware targets: the 4-chip BM1362 BMM100.
/// Every crate that needs board facts takes this explicitly rather than
/// selecting a board behind a cargo feature.
pub const BOARD: &ubos_asic::BoardDescriptor = &ubos_asic::BMM100;

/// Accepted tuning ranges for [`BOARD`]; the API validates against these.
pub const BOUNDS: ubos_config::MiningBounds = ubos_config::board_bounds(BOARD);

/// Static mining capability metadata, built from [`BOARD`].
pub const MINING_INFO: ubos_config::MiningInfo = ubos_config::mining_info(BOARD, &BOUNDS);

impl Board<'static> {
    pub const NAME: &str = "ESP32-S3 W5100S";
    /// DNS-safe base hostname for mDNS. See `ubos_mdns::compose_hostname`
    /// for the per-unit suffix and RFC 6762 §9 conflict-resolution semantics.
    pub const MDNS_HOSTNAME_BASE: &str = "esp32s3";
    /// MCU identifier exposed in the mDNS TXT record as `mcu=<MCU>`.
    pub const MCU: &str = "esp32s3";
    /// Board-family byte placed in the derived MAC address. The runtime
    /// folds the ESP32-S3 factory eFuse MAC into the remaining octets via
    /// [`ubos_net::mac::derive_mac`], so each unit self-assigns a stable,
    /// distinct MAC with no per-board build flag. Families 0x01/0x02/0x03
    /// are taken by rp2040/rp2350/bmm-pico, 0x04 by hashnode; esp32s3 uses 0x05.
    pub const MAC_FAMILY: u8 = 0x05;
    /// Concurrent TCP connections the web-UI server (:80) accepts. A browser
    /// opens several parallel connections per page load; this sizes the
    /// worker/buffer pool. Held at 3 rather than the 5 the RP boards use:
    /// each worker's buffers land in `.bss`, and on this MCU `.bss` shares
    /// `dram_seg` with the core-0 stack, so oversized pools starve the boot
    /// stack and trip the esp-hal stack-guard during `embassy_net::new`. The
    /// `build.rs` link-time ASSERT enforces the resulting stack floor.
    pub const WEBUI_HTTP_POOL: usize = 3;
    /// Concurrent TCP connections the REST API server (:8080) accepts.
    pub const API_HTTP_POOL: usize = 3;
    /// Accepted-connection pool depth for the Prometheus metrics
    /// listener. All workers share one exposition body buffer held
    /// across the socket write, so depth is accept-queue slack for a
    /// second concurrent scraper, not parallel rendering; scrapes still
    /// serialize on that buffer. Matches the webui and api pools.
    pub const METRICS_HTTP_POOL: usize = 3;
    /// Capacity of the user-facing event ring served by `ubos-events`.
    pub const EVENT_LOG_CAPACITY: usize = 128;

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
            i2c_sda: peripherals.GPIO15,
            i2c_scl: peripherals.GPIO7,
            hb_plug: peripherals.GPIO5,
            ledc: peripherals.LEDC,
            fan_pwm_pin: peripherals.GPIO10,
            pcnt: peripherals.PCNT,
            fan_tach_pin: peripherals.GPIO11,
            flash: peripherals.FLASH,
            #[cfg(feature = "display")]
            display_spi: peripherals.SPI3,
            #[cfg(feature = "display")]
            display_sck: peripherals.GPIO6,
            #[cfg(feature = "display")]
            display_mosi: peripherals.GPIO13,
            #[cfg(feature = "display")]
            display_cs: peripherals.GPIO12,
            #[cfg(feature = "display")]
            display_dc: peripherals.GPIO16,
            #[cfg(feature = "display")]
            display_rst: peripherals.GPIO17,
            #[cfg(feature = "display")]
            display_dma: peripherals.DMA_CH1,
        }
    }
}
