//! Shared OLED display rendering and task loop for the SH1106 128x64 display.
//!
//! Architecture: `../../../docs/OVERVIEW.md`.

use embassy_net::Ipv4Address;
use embassy_time::{Duration, Timer};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;
use embedded_hal::digital::OutputPin;
use portable_atomic::Ordering;
use profont::PROFONT_12_POINT;

use ubos_core::fan_state::{FAN_PWM_PERCENT, FAN_RPM};
use ubos_metrics::hashrate;
use ubos_net::net::LOCAL_IPV4;
use ubos_net::ntp;

use crate::sh1106::Sh1106;

/// Render IP, UTC time, fan PWM and RPM where a fan is present, and
/// hashrate onto the display.
///
/// The caller must call `display.flush()` afterwards to push the
/// framebuffer to the hardware. `time_buffer` is scratch space for
/// formatting the UTC time string. All values are read from shared
/// atomics; the first two render a short placeholder when the
/// underlying atomic is still at its zero sentinel (`no ip`, `no sync`).
/// Hashrate has no sentinel — zero is rendered as `0 H/s`.
///
/// Line 3 shows fan PWM and RPM when the board has a fan; boards
/// without a fan leave line 3 blank.
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
    let ntp_offset = ubos_net::net::NTP_EPOCH_OFFSET.load(Ordering::Relaxed);
    if ntp_offset == 0 {
        Text::new("no sync", Point::new(0, 29), style).draw(display)?;
    } else {
        let unix_secs = ubos_net::wall_clock_secs();
        let length = ntp::format_utc_time(time_buffer, unix_secs);
        if length > 0 {
            // format_utc_time writes valid ASCII into the buffer.
            if let Ok(text) = core::str::from_utf8(&time_buffer[..length]) {
                Text::new(text, Point::new(0, 29), style).draw(display)?;
            }
        }
    }

    // Line 3: fan PWM and RPM when the board publishes them. `FAN_PWM_PERCENT
    // == u8::MAX` is the "no fan" sentinel; any board with a fan task clears
    // it on startup. The raw atomic is read deliberately: `fan_state::load_pwm()`
    // collapses the sentinel to `FanPwm::ZERO` and would erase this branch.
    let pwm_percent = FAN_PWM_PERCENT.load(Ordering::Relaxed);
    if pwm_percent != u8::MAX {
        let rpm = FAN_RPM.load(Ordering::Relaxed);
        let mut fan_buffer = [0u8; 24];
        if let Ok(text) =
            format_no_std::show(&mut fan_buffer, format_args!("{pwm_percent}% {rpm} RPM"))
        {
            Text::new(text, Point::new(0, 45), style).draw(display)?;
        }
    }

    // Line 4: current hashrate. Read from the shared EWMA fed by the
    // ubos-mining BM1362 path (and by the host-runner CPU mock when used).
    // HashrateDisplay switches to GH/s at 1 GH/s, so 24 bytes covers
    // both representations.
    let hashrate = hashrate::HashrateDisplay(hashrate::hashrate());
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
