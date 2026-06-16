//! Board constants and peripheral extraction for the Hashnode, an
//! RP2350 with a W5500 running the ubos-mining backend and a `SIC45x`-based
//! `HashnodePsu`.
//! `Board::take` consumes the RP2350 `Peripherals` once and returns every
//! singleton the board's schematic claims, keeping pin assignments in one
//! place rather than scattered through `main`.
//!
//! Wiring/pinout: `../../../docs/SETUP.md`

use embassy_rp::peripherals::{
    CORE1, DMA_CH0, DMA_CH1, FLASH, I2C1, PIN_10, PIN_11, PIN_16, PIN_17, PIN_18, PIN_19, PIN_2,
    PIN_20, PIN_21, PIN_23, PIN_24, PIN_25, PIN_3, PIN_30, PIN_32, PIN_4, PIN_5, PIN_6, PWM_SLICE5,
    SPI0, TRNG, UART1, WATCHDOG,
};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, PIN_15, PIN_26, PIN_27, PIN_28, PIN_29, SPI1};
use embassy_rp::Peri;
use embassy_time::Duration;

/// Every peripheral singleton the board consumes, extracted in one place.
#[allow(dead_code)]
pub struct Board {
    pub watchdog: Peri<'static, WATCHDOG>,
    /// Status LED on GPIO6, active-low (10k pull-up to 3V3; DS-LED off when
    /// driven high). The blink task toggles this pin, so the initial level
    /// only sets the phase of the blink pattern.
    pub led: Peri<'static, PIN_6>,
    pub spi: Peri<'static, SPI0>,
    /// W5500 SPI0: SCLK=GPIO18, MOSI=GPIO19, MISO=GPIO16.
    pub sck: Peri<'static, PIN_18>,
    pub mosi: Peri<'static, PIN_19>,
    pub miso: Peri<'static, PIN_16>,
    /// W5500 chip-select (GPIO17, active-low; driven as GPIO Output).
    pub cs: Peri<'static, PIN_17>,
    /// W5500 interrupt line (GPIO20, active-low input).
    pub w5500_int: Peri<'static, PIN_20>,
    /// W5500 reset line (GPIO21, active-low output).
    pub w5500_reset: Peri<'static, PIN_21>,
    pub rx_dma: Peri<'static, DMA_CH0>,
    pub tx_dma: Peri<'static, DMA_CH1>,
    /// UART1 TX pin, drives BM1370 chip serial.
    pub uart: Peri<'static, UART1>,
    pub uart_tx: Peri<'static, PIN_4>,
    pub uart_rx: Peri<'static, PIN_5>,
    /// I2C1 bus: SDA=GPIO2, SCL=GPIO3. Sensors: INA230 0x43, TMP102 ×2,
    /// TMP461, `SIC45x` 0x10.
    pub i2c1: Peri<'static, I2C1>,
    pub sda: Peri<'static, PIN_2>,
    pub scl: Peri<'static, PIN_3>,
    /// Hashboard reset (GPIO32). HIGH = enabled / out-of-reset;
    /// LOW = held in reset. Drive LOW at boot until the miner task
    /// is ready to bring up the hashboard.
    pub hb_rst: Peri<'static, PIN_32>,
    /// Fan PWM outputs (25 kHz). `PWM_SLICE5` channel A = GPIO10 (`PWM_A`,
    /// two connectors), channel B = GPIO11 (`PWM_B`, the third). Both run at
    /// the same duty so all three fans track together.
    pub fan_pwm_slice: Peri<'static, PWM_SLICE5>,
    pub fan_pwm_pin: Peri<'static, PIN_10>,
    pub fan_pwm_pin_b: Peri<'static, PIN_11>,
    /// Fan tachometer inputs, one per connector: GPIO24 (`RP_TACH_A`),
    /// GPIO25 (`RP_TACH_B`), GPIO23 (`RP_TACH_C`). None map to a `PWM`
    /// `ChannelB`, so `Pwm::new_input` is unusable; pulses are counted via
    /// the async GPIO wait API instead.
    pub fan_tach_pin_a: Peri<'static, PIN_24>,
    pub fan_tach_pin_b: Peri<'static, PIN_25>,
    pub fan_tach_pin_c: Peri<'static, PIN_23>,
    /// FLASH peripheral for the on-board QSPI; ubos-config persists the
    /// runtime config record in the last two sectors of this device.
    pub flash: Peri<'static, FLASH>,
    /// `SIC45x` enable (GPIO30, active-high). A 10k pull-down holds it off at
    /// power-on; DS3 LED lights when driven high. Drive HIGH before
    /// miner/psu bring-up to gate the `SIC45x` power stage. There is no
    /// MCU-gated logic rail on Hashnode — the logic DC/DC is always-on from 12V.
    pub sic_en: Peri<'static, PIN_30>,
    #[cfg(feature = "display")]
    pub display_spi: Peri<'static, SPI1>,
    /// Display SPI1: SCK=GPIO26, MOSI=GPIO27.
    #[cfg(feature = "display")]
    pub display_sck: Peri<'static, PIN_26>,
    #[cfg(feature = "display")]
    pub display_mosi: Peri<'static, PIN_27>,
    /// Display SPI CS (GPIO29, active-low, driven as GPIO Output).
    #[cfg(feature = "display")]
    pub display_cs: Peri<'static, PIN_29>,
    /// Display data/command select (GPIO28).
    #[cfg(feature = "display")]
    pub display_dc: Peri<'static, PIN_28>,
    /// Display reset (GPIO15, active-low output).
    #[cfg(feature = "display")]
    pub display_rst: Peri<'static, PIN_15>,
    #[cfg(feature = "display")]
    pub display_tx_dma: Peri<'static, DMA_CH2>,
    pub core1: Peri<'static, CORE1>,
    /// Hardware TRNG peripheral. Used at boot to seed the session
    /// CSPRNG in `ubos_api`. RP2350-specific; the RP2040 has no
    /// equivalent and seeds from `RoscRng` instead.
    pub trng: Peri<'static, TRNG>,
}

/// The board descriptor this firmware targets: the 4-chip BM1370 hashnode.
/// Every crate that needs board facts takes this explicitly rather than
/// selecting a board behind a cargo feature.
pub const BOARD: &ubos_asic::BoardDescriptor = &ubos_asic::HASHNODE;

/// Accepted tuning ranges for [`BOARD`]; the API validates against these.
pub const BOUNDS: ubos_config::MiningBounds = ubos_config::bounds(BOARD);

/// Static mining capability metadata, built from [`BOARD`].
pub const MINING_INFO: ubos_config::MiningInfo = ubos_config::mining_info(BOARD, &BOUNDS);

impl Board {
    // Single-sourced from the mining metadata so the product name has one home.
    pub const NAME: &str = MINING_INFO.device_model;
    /// DNS-safe base hostname for mDNS. See `ubos_mdns::compose_hostname`
    /// for the per-unit suffix and RFC 6762 §9 conflict-resolution semantics.
    pub const MDNS_HOSTNAME_BASE: &str = "hashnode";
    /// MCU identifier exposed in the mDNS TXT record as `mcu=<MCU>`.
    pub const MCU: &str = "rp2350";
    /// Locally-administered MAC address. The LAA bit, bit 1 of the first
    /// octet, marks it as local. Octet [4] is 0x00 for the non-BMM family;
    /// octet [5] is 0x06, the next free value after rp2350=01, esp32s3=02,
    /// stm32f407=03, rp2040=04, stm32f439=05.
    pub const MAC_ADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x06];
    /// Concurrent TCP connections the web-UI server (:80) accepts.
    pub const WEBUI_HTTP_POOL: usize = 5;
    /// Concurrent TCP connections the REST API server (:8080) accepts.
    pub const API_HTTP_POOL: usize = 5;
    /// Accepted-connection pool depth for the Prometheus metrics listener.
    pub const METRICS_HTTP_POOL: usize = 5;

    pub fn take(peripherals: embassy_rp::Peripherals) -> Self {
        Self {
            watchdog: peripherals.WATCHDOG,
            led: peripherals.PIN_6,
            spi: peripherals.SPI0,
            sck: peripherals.PIN_18,
            mosi: peripherals.PIN_19,
            miso: peripherals.PIN_16,
            cs: peripherals.PIN_17,
            w5500_int: peripherals.PIN_20,
            w5500_reset: peripherals.PIN_21,
            rx_dma: peripherals.DMA_CH0,
            tx_dma: peripherals.DMA_CH1,
            uart: peripherals.UART1,
            uart_tx: peripherals.PIN_4,
            uart_rx: peripherals.PIN_5,
            i2c1: peripherals.I2C1,
            sda: peripherals.PIN_2,
            scl: peripherals.PIN_3,
            hb_rst: peripherals.PIN_32,
            fan_pwm_slice: peripherals.PWM_SLICE5,
            fan_pwm_pin: peripherals.PIN_10,
            fan_pwm_pin_b: peripherals.PIN_11,
            fan_tach_pin_a: peripherals.PIN_24,
            fan_tach_pin_b: peripherals.PIN_25,
            fan_tach_pin_c: peripherals.PIN_23,
            flash: peripherals.FLASH,
            sic_en: peripherals.PIN_30,
            #[cfg(feature = "display")]
            display_spi: peripherals.SPI1,
            #[cfg(feature = "display")]
            display_sck: peripherals.PIN_26,
            #[cfg(feature = "display")]
            display_mosi: peripherals.PIN_27,
            #[cfg(feature = "display")]
            display_cs: peripherals.PIN_29,
            #[cfg(feature = "display")]
            display_dc: peripherals.PIN_28,
            #[cfg(feature = "display")]
            display_rst: peripherals.PIN_15,
            #[cfg(feature = "display")]
            display_tx_dma: peripherals.DMA_CH2,
            core1: peripherals.CORE1,
            trng: peripherals.TRNG,
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

/// Fan PWM and tachometer parameters. Default `clk_sys` on the RP2350 is
/// 150 MHz, so `(PWM_TOP + 1) × divider = 6000` cycles per period puts
/// the PWM frequency at exactly 25 kHz (the Intel 4-wire fan spec).
pub struct Fan;
impl Fan {
    pub const PWM_TOP: u16 = 5999;
    pub const TACH_WINDOW: Duration = Duration::from_secs(1);
    pub const TACH_PULSES_PER_REV: u64 = 2;
}

pub struct Core1;
impl Core1 {
    /// Stack size for the Core 1 executor, sized for the ubos-mining backend.
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
/// `SIZE` is the 2 MiB driver window exposed to the flash HAL; the physical
/// device is 8 MiB but only the lower 2 MiB is mapped by this driver
/// instance. `CONFIG_RANGE` claims the final two 4 KiB erase sectors of
/// that window. The application binary must not grow past the start of the
/// range — the linker script does not enforce this today, so a binary
/// creeping past ~2040 KiB will silently overwrite the config region.
pub struct Flash;
impl Flash {
    pub const SIZE: usize = 2 * 1024 * 1024;
    pub const CONFIG_RANGE: core::ops::Range<u32> = 0x001F_E000..0x0020_0000;
}
