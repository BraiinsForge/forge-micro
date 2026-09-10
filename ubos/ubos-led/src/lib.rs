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

//! The board's indicator LEDs: the status blink on a plain output pin and
//! the breathing colour on an addressable RGB string.
//!
//! Every board has at least one of the two, and neither depends on
//! anything but a pin or a frame writer and a clock, so the loops and
//! their task shells live here rather than in the framework core. A
//! platform crate adapts its LED driver to [`rgb::RgbFrame`]; a board
//! names the pin or ring type and the colour and spawns the shell.

#![no_std]

pub mod blink;
pub mod rgb;
