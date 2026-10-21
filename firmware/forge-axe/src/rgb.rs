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
//! machine clocks the WS2812 stream via the embassy-rp driver. The
//! breathing loop and its math live in `ubos_led::rgb`; this board only
//! fixes the ring's shape and its color.

use embassy_rp::peripherals::PIO1;
use smart_leds::RGB8;
use ubos_rp::rgb::Ws2812Ring;

/// Number of LEDs on the ring.
pub const LED_COUNT: usize = 16;

/// The ring's state machine on `PIO1`.
pub const STATE_MACHINE: usize = 0;

/// WS2812 driver instantiated for this board: PIO1 SM0, GRB order.
pub type Ring = Ws2812Ring<'static, PIO1, STATE_MACHINE, LED_COUNT>;

/// Braiins Forge orange.
pub const COLOR: RGB8 = RGB8::new(235, 99, 7);

ubos_led::rgb_task!(Ring);
