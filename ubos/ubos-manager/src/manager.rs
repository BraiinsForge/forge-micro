//! Critical-path manager task.
//!
//! `run_manager` drains [`COMMAND_CHANNEL`] fed by `Handle::send`,
//! interprets each [`Message`] into a `PauseReason` bit
//! transition on the live [`PauseReasons`] mask, publishes the updated
//! mask on [`STATE_WATCH`], and drives `MiningHandle::halt`/`resume` on
//! the empty/non-empty edge.

use ubos_logging::{info, warn};
use ubos_mining::Handle as MiningHandle;

use crate::message::Message;
use crate::pause::{PauseReason, PauseReasons};
use crate::{COMMAND_CHANNEL, STATE_WATCH};

/// Log a `DangerousTemperature` bit transition.
///
/// Extracted from the loop body so that each log call site is
/// unique and does not trigger `clippy::if_same_then_else`. Only
/// `DangerousTemperature` carries a dedicated log line because
/// thermal halts originate inside the manager from a sensor
/// reading; the operator has no other surface to see them. The
/// other reasons (`BrokenFan`, `PoolUnreachable`, `TunerFault`,
/// `UserRequest`) are asserted by callers that already log their
/// own context, and the resulting halt/resume edge is visible in
/// `mining.halt()` / `resume()` traffic.
fn log_transition(was_halted: bool, now_halted: bool) {
    if !was_halted && now_halted {
        info!("manager: halt (DangerousTemperature)");
    } else if was_halted && !now_halted {
        warn!("manager: resume (DangerousTemperature cleared)");
    }
}

/// Apply a `Message` to the live mask, returning the new mask.
fn apply(reasons: PauseReasons, message: Message) -> PauseReasons {
    match message {
        Message::DangerousTemperature => reasons.assert(PauseReason::DangerousTemperature),
        Message::TemperatureNormal => reasons.clear(PauseReason::DangerousTemperature),
        Message::BrokenFan => reasons.assert(PauseReason::BrokenFan),
        Message::FanRestored => reasons.clear(PauseReason::BrokenFan),
        Message::PoolUnreachable => reasons.assert(PauseReason::PoolUnreachable),
        Message::PoolReachable => reasons.clear(PauseReason::PoolUnreachable),
        Message::TunerFault => reasons.assert(PauseReason::TunerFault),
        Message::TunerRecovered => reasons.clear(PauseReason::TunerFault),
        Message::Stop => reasons.assert(PauseReason::UserRequest),
        Message::Start => reasons.clear(PauseReason::UserRequest),
    }
}

/// Run the manager loop forever.
///
/// Drains [`COMMAND_CHANNEL`] fed by `Handle::send`, interprets
/// each [`Message`] into a `PauseReason` bit transition on the
/// live [`PauseReasons`] mask, publishes the updated mask on
/// [`STATE_WATCH`], and calls `mining.halt()`/`resume()` on the
/// empty/non-empty edge. Manager is the sole caller of
/// `MiningHandle::halt`/`resume`; senders construct domain messages
/// and never name a `PauseReason`.
pub async fn run_manager(mining: MiningHandle) -> ! {
    let publisher = STATE_WATCH.sender();
    let mut reasons = PauseReasons::default();

    info!("manager: started");

    loop {
        let message = COMMAND_CHANNEL.receive().await;
        let was_empty = reasons.is_empty();
        let was_danger = reasons.contains(PauseReason::DangerousTemperature);
        reasons = apply(reasons, message);
        log_transition(
            was_danger,
            reasons.contains(PauseReason::DangerousTemperature),
        );
        publisher.send(reasons);
        let is_empty = reasons.is_empty();
        if was_empty && !is_empty {
            mining.halt().await;
        } else if !was_empty && is_empty {
            mining.resume().await;
        }
    }
}
