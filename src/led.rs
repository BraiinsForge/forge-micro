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
