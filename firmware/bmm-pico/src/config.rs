//! Board constants and peripheral extraction for the BMM Adapter (RP2350 +
//! W5500). `Board::take` consumes the RP2350 `Peripherals` once and returns
//! every singleton the board's schematic claims.
//!
//! Wiring/pinout: `../../../docs/SETUP.md`

use embassy_rp::peripherals::{
    ADC, ADC_TEMP_SENSOR, CORE1, DMA_CH0, DMA_CH1, FLASH, I2C1, PIN_0, PIN_1, PIN_15, PIN_16,
    PIN_17, PIN_18, PIN_19, PIN_2, PIN_20, PIN_21, PIN_25, PIN_3, PIN_4, PIN_5, PIN_6, PIN_7,
    PIN_8, PWM_SLICE4, PWM_SLICE7, SPI0, TRNG, UART0, UART1, WATCHDOG,
};
#[cfg(feature = "display")]
use embassy_rp::peripherals::{DMA_CH2, PIN_10, PIN_11, PIN_12, PIN_13, PIN_14, SPI1};
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
    pub w5500_int: Peri<'static, PIN_21>,
    pub w5500_reset: Peri<'static, PIN_20>,
    pub rx_dma: Peri<'static, DMA_CH0>,
    pub tx_dma: Peri<'static, DMA_CH1>,
    pub probe_uart: Peri<'static, UART0>,
    pub probe_uart_tx: Peri<'static, PIN_0>,
    pub probe_uart_rx: Peri<'static, PIN_1>,
    pub hb_rst: Peri<'static, PIN_2>,
    pub hb_plug: Peri<'static, PIN_3>,
    pub i2c: Peri<'static, I2C1>,
    pub i2c_sda: Peri<'static, PIN_6>,
    pub i2c_scl: Peri<'static, PIN_7>,
    pub fan_pwm_slice: Peri<'static, PWM_SLICE4>,
    pub fan_pwm: Peri<'static, PIN_8>,
    pub fan_rpm_slice: Peri<'static, PWM_SLICE7>,
    pub fan_rpm: Peri<'static, PIN_15>,
    /// FLASH peripheral for the on-board QSPI; ubos-config persists the
    /// runtime config record in the last two sectors of this device.
    pub flash: Peri<'static, FLASH>,
    pub uart: Peri<'static, UART1>,
    pub uart_tx: Peri<'static, PIN_4>,
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
    pub core1: Peri<'static, CORE1>,
    /// Hardware TRNG peripheral. Used at boot to seed the session
    /// CSPRNG in `ubos_api`. RP2350-specific; the RP2040 has no
    /// equivalent and seeds from `RoscRng` instead.
    pub trng: Peri<'static, TRNG>,
}

impl Board {
    pub const NAME: &str = "BMM Adapter W5500";
    /// DNS-safe base hostname for mDNS. See `ubos_mdns::compose_hostname`
    /// for the per-unit suffix and RFC 6762 §9 conflict-resolution semantics.
    pub const MDNS_HOSTNAME_BASE: &str = "bmm-pico";
    /// MCU identifier exposed in the mDNS TXT record as `mcu=<MCU>`.
    pub const MCU: &str = "rp2350";
    /// Board-family byte placed in the derived MAC address. The runtime
    /// folds the RP2350 OTP chip id into the remaining octets via
    /// [`ubos_net::mac::derive_mac`], so each unit self-assigns a stable,
    /// distinct MAC with no per-board build flag.
    pub const MAC_FAMILY: u8 = 0x03;

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
            probe_uart: peripherals.UART0,
            probe_uart_tx: peripherals.PIN_0,
            probe_uart_rx: peripherals.PIN_1,
            hb_rst: peripherals.PIN_2,
            hb_plug: peripherals.PIN_3,
            i2c: peripherals.I2C1,
            i2c_sda: peripherals.PIN_6,
            i2c_scl: peripherals.PIN_7,
            fan_pwm_slice: peripherals.PWM_SLICE4,
            fan_pwm: peripherals.PIN_8,
            fan_rpm_slice: peripherals.PWM_SLICE7,
            fan_rpm: peripherals.PIN_15,
            flash: peripherals.FLASH,
            uart: peripherals.UART1,
            uart_tx: peripherals.PIN_4,
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

/// On-board QSPI flash parameters for the ubos-config record store.
///
/// `SIZE` covers the full 2 MiB physical flash; the `CONFIG_RANGE`
/// claims the final two 4 KiB erase sectors. The application binary
/// must not grow past the start of the range — the linker script does
/// not enforce this today, so a binary creeping past ~2040 KiB will
/// silently overwrite the config region.
pub struct Flash;
impl Flash {
    pub const SIZE: usize = 2 * 1024 * 1024;
    pub const CONFIG_RANGE: core::ops::Range<u32> = 0x001F_E000..0x0020_0000;
}
