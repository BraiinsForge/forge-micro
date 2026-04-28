//! Shared OLED display rendering and task loop. Draws IP address, UTC
//! time, BTC price, and hashrate onto the SH1106. Board crates call
//! [`run`] from a one-line `#[embassy_executor::task]` wrapper.
//!
//! Three values come from shared atomics (`LOCAL_IPV4`,
//! `NTP_EPOCH_OFFSET`, `BTC_PRICE_CENTS`); the hashrate is computed
//! on demand from the [`hashrate::hashrate`] sliding window.  The task
//! signature carries only the display handle; any producer can publish
//! without rewiring the display's spawn path.

use embassy_net::Ipv4Address;
use embassy_time::{Duration, Timer};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;
use embedded_hal::digital::OutputPin;
use portable_atomic::Ordering;
use profont::PROFONT_12_POINT;

// `hashrate::hashrate` needs the difficulty target to convert the share
// delta into Hz; the dummy_asic test is the only share-producing path
// here, so its target constant is the correct argument. The cpu-mining
// path bypasses this conversion via `record_external_rate`, which is
// why the import is unconditional rather than feature-gated.
use crate::dummy_asic::TEST_TARGET_LEADING_ZERO_BYTES;
use crate::hashrate;
use crate::net::{BTC_PRICE_CENTS, LOCAL_IPV4};
use crate::ntp;
use crate::sh1106::Sh1106;

/// Render IP, UTC time, BTC price, and hashrate onto the display.
///
/// The caller must call `display.flush()` afterwards to push the
/// framebuffer to the hardware. `time_buffer` is scratch space for
/// formatting the UTC time string.  All four values are read from
/// shared atomics; the first three render a short placeholder when the
/// underlying atomic is still at its zero sentinel (`no ip`, `no sync`,
/// `BTC ---`).  Hashrate has no sentinel — zero is rendered as `0 H/s`.
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

    // Line 3: BTC price in whole dollars, or "BTC ---" if not yet fetched.
    let cents = BTC_PRICE_CENTS.load(Ordering::Relaxed);
    if cents == 0 {
        Text::new("BTC ---", Point::new(0, 45), style).draw(display)?;
    } else {
        let dollars = cents / 100;
        // "BTC $" prefix plus up to 8 digits; 18 bytes covers u32 cents.
        let mut price_buffer = [0u8; 18];
        if let Ok(text) = format_no_std::show(&mut price_buffer, format_args!("BTC ${dollars}")) {
            Text::new(text, Point::new(0, 45), style).draw(display)?;
        }
    }

    // Line 4: current hashrate in H/s.  Difficulty target is hard-coded to
    // TEST_TARGET_LEADING_ZERO_BYTES because dummy_asic::build_work_frame
    // always issues Work at that target; if a future caller pushes a
    // different target the displayed rate will silently misreport.
    let hashrate = hashrate::hashrate(TEST_TARGET_LEADING_ZERO_BYTES);
    let mut hashrate_buffer = [0u8; 14];
    if let Ok(text) = format_no_std::show(&mut hashrate_buffer, format_args!("{hashrate} H/s")) {
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
