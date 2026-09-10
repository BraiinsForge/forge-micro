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

//! Breathing status color on an addressable RGB ring or string.
//!
//! The animation shows a fixed breathing color rather than the mining
//! state, so it reads as alive without depending on the mining actor.
//! The board owns the LED count, the driver and the color; the platform
//! crate adapts its driver to [`RgbFrame`]; this module owns the math
//! and the loop.

use embassy_time::{Duration, Ticker};
use smart_leds::RGB8;

/// A driver that latches one frame of `N` colors onto the string.
//
// async_fn_in_trait: implementations run on a cooperative
// single-executor; Send bounds on returned futures are neither
// needed nor possible in no_std. Mirrors `MiningClient`'s rationale.
#[allow(async_fn_in_trait)]
pub trait RgbFrame<const N: usize> {
    /// Write the frame and wait for the string to take it.
    async fn write(&mut self, frame: &[RGB8; N]);
}

/// Base brightness numerator over 255. Sized for a status indicator on a
/// string of up to sixteen LEDs fed from a 5 V header: full white on
/// sixteen would draw about an ampere, and a quarter of that is plenty to
/// read as alive. A board with a larger or brighter string revisits this
/// budget here rather than at its own call site.
const BRIGHTNESS_BASE: u16 = 64;

/// Peak deviation of the breathing pulse from the base, giving a
/// subtle swing rather than an attention-grabbing blink.
const BRIGHTNESS_SWING: u16 = 32;

/// Half-period of the breathing triangle in frames. With the frame
/// interval below, one full breath takes about three seconds.
const PULSE_HALF_STEPS: u16 = 32;

/// Cadence of brightness frames.
const FRAME_INTERVAL: Duration = Duration::from_millis(47);

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

/// Breathe `color` on `ring` forever. The board's `#[embassy_executor::task]`
/// shell, built through [`rgb_task`], calls this.
pub async fn run_rgb<W: RgbFrame<N>, const N: usize>(mut ring: W, color: RGB8) -> ! {
    let mut ticker = Ticker::every(FRAME_INTERVAL);
    let mut step: u16 = 0;
    loop {
        let brightness = pulse_brightness(step);
        let frame = [scale(color, brightness); N];
        ring.write(&frame).await;
        ticker.next().await;
        step = (step + 1) % (2 * PULSE_HALF_STEPS);
    }
}

/// Task shell around [`run_rgb`]. Takes the board's `Ring` type alias,
/// which fixes the driver and the LED count.
///
/// The shell cannot be a plain generic function, because
/// `#[embassy_executor::task]` expands a typed `TaskPool` static and so
/// rejects a generic task. `ubos-firmware`'s `tasks.rs` explains that
/// barrier once for every shell that works around it.
#[macro_export]
macro_rules! rgb_task {
    ($ring:ty) => {
        #[embassy_executor::task]
        pub async fn rgb_task(ring: $ring, color: smart_leds::RGB8) -> ! {
            ubos_led::rgb::run_rgb(ring, color).await
        }
    };
}
