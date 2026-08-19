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

//! Breathing status color on the 16-LED ARGB ring.
//!
//! The ring hangs off the ARGB header: data on GPIO6 (the `DISP_SDA`
//! net doubles as ARGB data), 5 V from the header. A `PIO1` state
//! machine clocks the WS2812 stream via the embassy-rp driver. The ring
//! shows a fixed breathing color rather than the mining state, so it
//! reads as alive without depending on the mining actor.

use embassy_rp::peripherals::PIO1;
use embassy_rp::pio_programs::ws2812::{Grb, PioWs2812};
use embassy_time::{Duration, Ticker};
use smart_leds::RGB8;

/// Number of LEDs on the ring.
pub const LED_COUNT: usize = 16;

/// The ring's state machine on `PIO1`. The ring cannot share `PIO0`:
/// the fan tach program is 28 instructions and the WS2812 program is 5,
/// one over the block's 32-slot instruction memory.
pub const STATE_MACHINE: usize = 0;

/// WS2812 driver instantiated for this board: PIO1 SM0, GRB order.
pub type Ring = PioWs2812<'static, PIO1, STATE_MACHINE, LED_COUNT, Grb>;

/// Base brightness numerator over 255. Full white on 16 LEDs would
/// pull ~1 A from the 5 V header; a quarter of that is plenty for a
/// status indicator. Tune on hardware.
const BRIGHTNESS_BASE: u16 = 64;

/// Peak deviation of the breathing pulse from the base, giving a
/// subtle ±6% swing rather than an attention-grabbing blink.
const BRIGHTNESS_SWING: u16 = 32;

/// Half-period of the breathing triangle in frames. With the frame
/// interval below, one full breath takes about three seconds.
const PULSE_HALF_STEPS: u16 = 32;

/// Cadence of brightness frames. 16 LEDs take ~0.5 ms of DMA per
/// repaint, so the animation cost is negligible.
const FRAME_INTERVAL: Duration = Duration::from_millis(47);

/// Braiins Forge orange.
const COLOR: RGB8 = RGB8::new(235, 99, 7);

/// Triangle-wave brightness for one animation step: rises from
/// `base - swing` to `base + swing` over the first half-period, falls
/// back over the second.
fn pulse_brightness(step: u16) -> u16 {
    let phase = if step < PULSE_HALF_STEPS {
        step
    } else {
        2 * PULSE_HALF_STEPS - step
    };
    BRIGHTNESS_BASE - BRIGHTNESS_SWING + (2 * BRIGHTNESS_SWING * phase) / PULSE_HALF_STEPS
}

/// Scale a full-scale color to the given brightness numerator (of 255).
// The quotient is at most 255 * brightness / 255 = brightness < 256,
// so the u8 casts never truncate.
#[allow(clippy::cast_possible_truncation)]
const fn scale(color: RGB8, brightness: u16) -> RGB8 {
    RGB8::new(
        ((color.r as u16 * brightness) / 255) as u8,
        ((color.g as u16 * brightness) / 255) as u8,
        ((color.b as u16 * brightness) / 255) as u8,
    )
}

/// Breathe the status color forever.
#[embassy_executor::task]
pub async fn rgb_task(mut ring: Ring) -> ! {
    let mut ticker = Ticker::every(FRAME_INTERVAL);
    let mut step: u16 = 0;
    loop {
        let brightness = pulse_brightness(step);
        let frame = [scale(COLOR, brightness); LED_COUNT];
        ring.write(&frame).await;
        ticker.next().await;
        step = (step + 1) % (2 * PULSE_HALF_STEPS);
    }
}
