//! Shared OLED display rendering and task loop. Draws IP address, UTC
//! time, and BTC price onto the SH1106. Board crates call [`run_async`]
//! from a one-line `#[embassy_executor::task]` wrapper.

use embassy_time::{Duration, Timer};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;
use embedded_hal::digital::OutputPin;
use portable_atomic::Ordering;
use profont::PROFONT_12_POINT;

use crate::net::BTC_PRICE_CENTS;
use crate::ntp;
use crate::sh1106::Sh1106;

/// Render IP address, UTC time, and BTC price onto the display.
///
/// The caller must call `display.flush()` afterwards to push the
/// framebuffer to the hardware. `time_buffer` is scratch space for
/// formatting the UTC time string.
///
/// # Errors
///
/// Returns the display's draw error if any drawing operation fails.
pub fn render<D: DrawTarget<Color = BinaryColor>>(
    display: &mut D,
    ip: embassy_net::Ipv4Address,
    time_buffer: &mut [u8; 16],
) -> Result<(), D::Error> {
    display.clear(BinaryColor::Off)?;

    let style = MonoTextStyle::new(&PROFONT_12_POINT, BinaryColor::On);

    // Line 1: IP address from DHCP.
    let octets = ip.octets();
    // Worst case is "255.255.255.255" (15 bytes); 16 is sufficient.
    let mut ip_buffer = [0u8; 16];
    if let Ok(text) = format_no_std::show(
        &mut ip_buffer,
        format_args!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]),
    ) {
        Text::new(text, Point::new(0, 13), style).draw(display)?;
    }

    // Line 2: UTC time from NTP, or "no sync" if NTP has not completed.
    let ntp_offset = crate::net::NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
    if ntp_offset == 0 {
        Text::new("no sync", Point::new(0, 35), style).draw(display)?;
    } else {
        let unix_secs = crate::wall_clock_secs();
        let length = ntp::format_utc_time(time_buffer, unix_secs);
        if length > 0 {
            // format_utc_time writes valid ASCII into the buffer.
            if let Ok(text) = core::str::from_utf8(&time_buffer[..length]) {
                Text::new(text, Point::new(0, 35), style).draw(display)?;
            }
        }
    }

    // Line 3: BTC price in whole dollars, or "BTC ---" if not yet fetched.
    let cents = BTC_PRICE_CENTS.load(Ordering::Relaxed);
    if cents == 0 {
        Text::new("BTC ---", Point::new(0, 57), style).draw(display)?;
    } else {
        let dollars = cents / 100;
        // "BTC $" prefix plus up to 8 digits; 18 bytes covers u32 cents.
        let mut price_buffer = [0u8; 18];
        if let Ok(text) = format_no_std::show(&mut price_buffer, format_args!("BTC ${dollars}")) {
            Text::new(text, Point::new(0, 57), style).draw(display)?;
        }
    }

    Ok(())
}

/// Run the display loop. Renders the screen once per second and flushes
/// via async SPI with DMA.
pub async fn run_async<SPI, DC>(display: &mut Sh1106<SPI, DC>, ip: embassy_net::Ipv4Address) -> !
where
    SPI: embedded_hal_async::spi::SpiDevice,
    DC: OutputPin,
{
    let mut time_buffer = [0u8; 16];
    loop {
        let _ = render(display, ip, &mut time_buffer);
        let _ = display.flush_async().await;
        Timer::after(Duration::from_secs(1)).await;
    }
}
