//! Periodic hardware watchdog feeder for the eth-examples boards. Feeds
//! at a fixed interval unconditionally. A cooperative task-level
//! watchdog where each task publishes a heartbeat is out of scope for
//! these example boards.

use embassy_time::Timer;

/// Hardware watchdog abstraction for the shared feed loop.
pub trait Watchdog {
    /// Start or enable the watchdog timer.
    fn init(&mut self);
    /// Feed the watchdog to prevent reset.
    fn feed(&mut self);
}

/// Periodically feed a hardware watchdog. Runs forever.
pub async fn run_watchdog<W: Watchdog>(mut wdt: W) -> ! {
    wdt.init();
    loop {
        wdt.feed();
        Timer::after(crate::consts::Watchdog::FEED_PERIOD).await;
    }
}
