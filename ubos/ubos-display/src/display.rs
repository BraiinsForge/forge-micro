// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Shared OLED display rendering and task loop for the SH1106 128x64 display.
//!
//! Architecture: `../../../docs/OVERVIEW.md`.

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;
use profont::PROFONT_12_POINT;

use ubos_metrics::hashrate;
use ubos_net::net::handle as net_handle;
use ubos_net::ntp;

/// Render IP, UTC time, fan PWM and RPM where a fan is present, and
/// hashrate onto the display.
///
/// The caller must call `display.flush()` afterwards to push the
/// framebuffer to the hardware. All values are read from shared
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
pub fn render<D: DrawTarget<Color = BinaryColor>>(display: &mut D) -> Result<(), D::Error> {
    display.clear(BinaryColor::Off)?;

    let style = MonoTextStyle::new(&PROFONT_12_POINT, BinaryColor::On);

    // Line 1: IP address from DHCP, or "no ip" if DHCP has not assigned yet.
    if let Some(ipv4) = net_handle().current().and_then(|state| state.ipv4) {
        let octets = ipv4.octets();
        let mut ip_buffer = [0u8; 16];
        if let Ok(text) = format_no_std::show(
            &mut ip_buffer,
            format_args!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]),
        ) {
            Text::new(text, Point::new(0, 13), style).draw(display)?;
        }
    } else {
        Text::new("no ip", Point::new(0, 13), style).draw(display)?;
    }

    // Line 2: UTC time from NTP, or "no sync" if NTP has not completed.
    let ntp_offset = net_handle().ntp_epoch_offset();
    if ntp_offset == 0 {
        Text::new("no sync", Point::new(0, 29), style).draw(display)?;
    } else {
        let unix_secs = ubos_net::wall_clock_secs();
        let time = ntp::format_utc_time(unix_secs);
        Text::new(time.as_str(), Point::new(0, 29), style).draw(display)?;
    }

    // Line 3: fan PWM and RPM, published by the cooling task. `current()`
    // is `None` on boards without a fan, so line 3 stays blank there.
    if let Some(fan) = ubos_cooling::handle().current() {
        let mut fan_buffer = [0u8; 24];
        if let Ok(text) = format_no_std::show(
            &mut fan_buffer,
            format_args!("{}% {} RPM", fan.pwm.percent(), fan.rpm.rpm()),
        ) {
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

/// Task shell around [`crate::sh1106::Sh1106::run`], which renders the OLED
/// display.
///
/// The reset pin is moved in and held for the task's lifetime so the
/// panel's RESET line stays driven; dropping it would release the pad and
/// blank the panel. That is why the binding is unused rather than absent.
#[macro_export]
macro_rules! display_task {
    ($display:ty, $reset:ty) => {
        #[embassy_executor::task]
        async fn display_task(
            mut display: $display,
            #[allow(clippy::used_underscore_binding)] _reset_pin: $reset,
        ) -> ! {
            display.run().await
        }
    };
}
