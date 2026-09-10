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

//! Tests for `bus_clear` over a fake open-drain line.

use crate::i2c::{BusClearError, OpenDrainPin, bus_clear};
use embedded_hal::delay::DelayNs;

/// A fake open-drain line. `permanently_stuck` models a wiring fault
/// that no clocking clears. `low_polls_remaining` models a target that
/// releases the line after a fixed number of level reads, standing in
/// for a target walking through its shift register one clock edge at a
/// time.
struct FakeLine {
    permanently_stuck: bool,
    driven_low: bool,
    low_polls_remaining: u32,
}

impl FakeLine {
    fn released_high(low_polls_remaining: u32) -> Self {
        Self {
            permanently_stuck: false,
            driven_low: false,
            low_polls_remaining,
        }
    }

    fn stuck_low() -> Self {
        Self {
            permanently_stuck: true,
            driven_low: false,
            low_polls_remaining: 0,
        }
    }
}

impl OpenDrainPin for FakeLine {
    fn release(&mut self) {
        self.driven_low = false;
    }

    fn drive_low(&mut self) {
        self.driven_low = true;
    }

    fn is_high(&mut self) -> bool {
        if self.permanently_stuck || self.driven_low {
            return false;
        }
        if self.low_polls_remaining > 0 {
            self.low_polls_remaining -= 1;
            return false;
        }
        true
    }
}

struct FakeDelay;

impl DelayNs for FakeDelay {
    fn delay_ns(&mut self, _ns: u32) {}
}

#[test]
fn returns_at_once_when_sda_already_high() {
    let mut scl = FakeLine::released_high(0);
    let mut sda = FakeLine::released_high(0);
    let mut delay = FakeDelay;
    assert_eq!(bus_clear(&mut scl, &mut sda, &mut delay), Ok(()));
}

#[test]
fn fails_without_clocking_when_scl_stuck_low() {
    let mut scl = FakeLine::stuck_low();
    let mut sda = FakeLine::stuck_low();
    let mut delay = FakeDelay;
    assert_eq!(
        bus_clear(&mut scl, &mut sda, &mut delay),
        Err(BusClearError::SclStuckLow)
    );
}

#[test]
fn clocks_a_stuck_sda_free() {
    let mut scl = FakeLine::released_high(0);
    let mut sda = FakeLine::released_high(3);
    let mut delay = FakeDelay;
    assert_eq!(bus_clear(&mut scl, &mut sda, &mut delay), Ok(()));
}

#[test]
fn reports_failure_when_sda_never_releases() {
    let mut scl = FakeLine::released_high(0);
    let mut sda = FakeLine::released_high(100);
    let mut delay = FakeDelay;
    assert_eq!(
        bus_clear(&mut scl, &mut sda, &mut delay),
        Err(BusClearError::SdaStillLow)
    );
}
