//! Shared OLED display rendering and task loop. Draws IP address, UTC
//! time, BTC price or fan PWM/RPM, and hashrate onto the SH1106. Board
//! crates call [`run`] from a one-line `#[embassy_executor::task]` wrapper.
//!
//! Values come from shared atomics: `LOCAL_IPV4`, `NTP_EPOCH_OFFSET`,
//! `BTC_PRICE_CENTS`, [`FAN_PWM_PERCENT`], [`FAN_RPM`], and the EWMA
//! inside `crate::hashrate`. The task signature carries only the
//! display handle; any producer can publish without rewiring the
//! display's spawn path.
//!
//! Line 3 shows fan PWM and RPM on boards that have cleared the
//! [`FAN_PWM_PERCENT`] sentinel; on boards without a fan the same line
//! falls back to the BTC price.

use embassy_net::Ipv4Address;
use embassy_time::{Duration, Timer};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;
use embedded_hal::digital::OutputPin;
use portable_atomic::{AtomicU32, AtomicU8, Ordering};
use profont::PROFONT_12_POINT;

use crate::net::{BTC_PRICE_CENTS, LOCAL_IPV4};
use crate::ntp;
use crate::sh1106::Sh1106;

/// Last measured fan tach in RPM. Zero on boards without a fan, or
/// before the first tach sample completes on boards with one.
pub static FAN_RPM: AtomicU32 = AtomicU32::new(0);

/// Current fan PWM duty as an integer percent in the range 0 to 100
/// inclusive. `u8::MAX` is the sentinel for "this board has no fan";
/// the display uses it to decide whether to show fan info or fall back
/// to the BTC price.
pub static FAN_PWM_PERCENT: AtomicU8 = AtomicU8::new(u8::MAX);

/// Render IP, UTC time, BTC price or fan PWM/RPM, and hashrate onto
/// the display.
///
/// The caller must call `display.flush()` afterwards to push the
/// framebuffer to the hardware. `time_buffer` is scratch space for
/// formatting the UTC time string. All values are read from shared
/// atomics; the first three render a short placeholder when the
/// underlying atomic is still at its zero sentinel (`no ip`, `no sync`,
/// `BTC ---`). Hashrate has no sentinel — zero is rendered as `0 H/s`.
///
/// The 16-pixel line pitch fits four baselines in the SH1106's
/// 64-pixel height; any larger pitch clips the last line.
///
/// # Errors
///
/// Returns the display's draw error if any drawing operation fails.
pub fn render<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    time_buffer: &mut [u8; 16],
) -> Result<(), D::Error> {
    display.clear(BinaryColor::Off)?;

    let style = MonoTextStyle::new(&PROFONT_12_POINT, BinaryColor::On);

    // Line 1: IP address from DHCP, or "no ip" if DHCP has not assigned yet.
    let ip_bits = LOCAL_IPV4.load(Ordering::Relaxed);
    if ip_bits == 0 {
        Text::new("no ip", Point::new(0, 13), style).draw(display)?;
    } else {
        let octets = Ipv4Address::from_bits(ip_bits).octets();
        let mut ip_buffer = [0u8; 16];
        if let Ok(text) = format_no_std::show(
            &mut ip_buffer,
            format_args!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]),
        ) {
            Text::new(text, Point::new(0, 13), style).draw(display)?;
        }
    }

    // Line 2: UTC time from NTP, or "no sync" if NTP has not completed.
    let ntp_offset = crate::net::NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
    if ntp_offset == 0 {
        Text::new("no sync", Point::new(0, 29), style).draw(display)?;
    } else {
        let unix_secs = crate::wall_clock_secs();
        let length = ntp::format_utc_time(time_buffer, unix_secs);
        if length > 0 {
            // format_utc_time writes valid ASCII into the buffer.
            if let Ok(text) = core::str::from_utf8(&time_buffer[..length]) {
                Text::new(text, Point::new(0, 29), style).draw(display)?;
            }
        }
    }

    // Line 3: fan PWM and RPM when the board publishes them, BTC
    // price otherwise. `FAN_PWM_PERCENT == u8::MAX` is the "no fan"
    // sentinel; any board with a fan task clears it on startup.
    let pwm_percent = FAN_PWM_PERCENT.load(Ordering::Relaxed);
    if pwm_percent == u8::MAX {
        let cents = BTC_PRICE_CENTS.load(Ordering::Relaxed);
        if cents == 0 {
            Text::new("BTC ---", Point::new(0, 45), style).draw(display)?;
        } else {
            let dollars = cents / 100;
            // "BTC $" prefix plus up to 8 digits; 18 bytes covers u32 cents.
            let mut price_buffer = [0u8; 18];
            if let Ok(text) = format_no_std::show(&mut price_buffer, format_args!("BTC ${dollars}"))
            {
                Text::new(text, Point::new(0, 45), style).draw(display)?;
            }
        }
    } else {
        let rpm = FAN_RPM.load(Ordering::Relaxed);
        let mut fan_buffer = [0u8; 24];
        if let Ok(text) =
            format_no_std::show(&mut fan_buffer, format_args!("{pwm_percent}% {rpm} RPM"))
        {
            Text::new(text, Point::new(0, 45), style).draw(display)?;
        }
    }

    // Line 4: current hashrate. Read from the shared EWMA so the
    // displayed rate reflects whatever backend is wired (cpu mock,
    // UART dummy-asic, future ASIC). HashrateDisplay switches to GH/s
    // at 1 GH/s, so 24 bytes covers both representations.
    let hashrate = crate::hashrate::HashrateDisplay(crate::hashrate::hashrate());
    let mut hashrate_buffer = [0u8; 24];
    if let Ok(text) = format_no_std::show(&mut hashrate_buffer, format_args!("{hashrate}")) {
        Text::new(text, Point::new(0, 61), style).draw(display)?;
    }

    Ok(())
}

/// Run the display loop. Renders the screen once per second and flushes
/// via async SPI with DMA.
pub async fn run<SPI, DC>(display: &mut Sh1106<SPI, DC>) -> !
where
    SPI: embedded_hal_async::spi::SpiDevice,
    DC: OutputPin,
{
    let mut time_buffer = [0u8; 16];
    loop {
        let _ = render(display, &mut time_buffer);
        let _ = display.flush_async().await;
        Timer::after(Duration::from_secs(1)).await;
    }
}
