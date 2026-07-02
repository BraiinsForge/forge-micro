//! Hold-to-trigger button detection.
//!
//! [`wait_for_hold`] resolves once a momentary button has been held pressed
//! continuously for a configured duration. It is pure mechanism — sampling,
//! debounce, and edge bookkeeping over an `embedded_hal` [`InputPin`] — with
//! no notion of what the hold means. The caller owns the action: the redacted
//! platform board loops on it and sends `ubos_config::Message::FactoryReset`.
//! Keeping the action in the caller is deliberate — `ubos-config` already
//! depends on this crate, so importing it here would be a cycle, mirroring how
//! [`crate::reboot`] stays a neutral signal between the API and the board.
//!
//! Level polling, not an edge interrupt: a multi-second hold is its own
//! debounce (contact bounce cannot accumulate seconds of unbroken press), and
//! polling needs no per-board interrupt wiring.

use embassy_time::{Duration, Instant, Ticker};
use embedded_hal::digital::InputPin;

/// Timing and polarity for [`wait_for_hold`].
#[derive(Clone, Copy)]
pub struct HoldConfig {
    /// Interval between button samples.
    pub poll: Duration,
    /// Unbroken hold required before [`wait_for_hold`] resolves.
    pub hold: Duration,
    /// `true` when a pressed button reads LOW — the usual pulled-up momentary
    /// switch. `false` for an active-high button.
    pub active_low: bool,
}

impl HoldConfig {
    /// Sample the pin and map its level to pressed/released. A read error
    /// counts as released so a flaky pin can never trigger the action.
    fn is_pressed<P: InputPin>(&self, pin: &mut P) -> bool {
        matches!(pin.is_low(), Ok(low) if low == self.active_low)
    }
}

/// Resolve once `pin` has been held pressed continuously for
/// [`HoldConfig::hold`].
///
/// A press already in progress on entry is ignored until the button is first
/// seen released. Called in a loop, this both debounces re-arming — a button
/// still held after the action ran will not immediately retrigger — and, on
/// the first call after boot, stops a line stuck LOW (or held through the
/// reboot a reset triggers) from firing the action again.
pub async fn wait_for_hold<P: InputPin>(pin: &mut P, cfg: HoldConfig) {
    let mut ticker = Ticker::every(cfg.poll);

    // Wait for a released state before arming.
    while cfg.is_pressed(pin) {
        ticker.next().await;
    }

    loop {
        // Arm on a fresh press.
        while !cfg.is_pressed(pin) {
            ticker.next().await;
        }
        let pressed_at = Instant::now();

        // The press must stay unbroken until the threshold elapses; a release
        // before then abandons it and re-arms.
        loop {
            ticker.next().await;
            if !cfg.is_pressed(pin) {
                break;
            }
            if pressed_at.elapsed() >= cfg.hold {
                return;
            }
        }
    }
}
