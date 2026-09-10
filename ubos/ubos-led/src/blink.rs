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

//! Status LED blink loop. Toggles any `StatefulOutputPin` at 500 ms intervals.

use embassy_time::{Duration, Ticker};
use embedded_hal::digital::StatefulOutputPin;

/// Blink an LED at a fixed 500 ms interval. Runs forever.
pub async fn run_led_blink<P: StatefulOutputPin>(mut led: P) -> ! {
    let mut ticker = Ticker::every(Duration::from_millis(500));
    loop {
        let _ = led.toggle();
        ticker.next().await;
    }
}

/// Task shell around [`run_led_blink`]. Takes the board's HAL output
/// type, which is the only thing that differs between boards.
#[macro_export]
macro_rules! led_blink_task {
    ($led:ty) => {
        #[embassy_executor::task]
        async fn led_blink_task(led: $led) -> ! {
            ubos_led::blink::run_led_blink(led).await
        }
    };
}
