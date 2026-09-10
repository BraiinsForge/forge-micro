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

//! The WS2812 PIO driver as a [`ubos_led::rgb::RgbFrame`] writer.
//!
//! A PIO state machine clocks the WS2812 stream via the embassy-rp
//! driver. The breathing animation is `ubos_led::rgb`; this module only
//! adapts the driver, so a board spawns `ubos_led::rgb_task!` over its
//! [`Ws2812Ring`] alias. The newtype exists because the trait and the
//! driver are both foreign to this crate.

use embassy_rp::pio::Instance;
use embassy_rp::pio_programs::ws2812::{Grb, PioWs2812};
use smart_leds::RGB8;
use ubos_led::rgb::RgbFrame;

/// A WS2812 string of `N` LEDs on state machine `SM` of `PIO`, GRB order.
pub struct Ws2812Ring<'d, PIO: Instance, const SM: usize, const N: usize>(
    pub PioWs2812<'d, PIO, SM, N, Grb>,
);

impl<PIO: Instance, const SM: usize, const N: usize> RgbFrame<N> for Ws2812Ring<'_, PIO, SM, N> {
    async fn write(&mut self, frame: &[RGB8; N]) {
        self.0.write(frame).await;
    }
}
