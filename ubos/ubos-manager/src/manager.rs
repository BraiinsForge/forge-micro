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

/// Map an asserting `Message` to the `PauseReason` it sets, or `None`
/// for clearing messages.
///
/// Used to capture the triggering reason before `PauseReasons::apply`
/// consumes the message, so the halt event can carry the discriminant of
/// the newly-active reason.
///
/// Free rather than a method on `Message`, though the receiver is clear
/// and local. `message.rs` states the layering this would break: a
/// sender constructs a `Message` and never spells `PauseReason`, which
/// is manager-private vocabulary. Hanging this off `Message` would put
/// the private half on the public type and hand every sender the
/// mapping.
fn halt_reason(message: Message) -> Option<PauseReason> {
    match message {
        Message::DangerousTemperature => Some(PauseReason::DangerousTemperature),
        Message::BrokenFan => Some(PauseReason::BrokenFan),
        Message::PoolUnreachable => Some(PauseReason::PoolUnreachable),
        Message::TunerFault => Some(PauseReason::TunerFault),
        Message::OtaBegin => Some(PauseReason::OtaUpdate),
        Message::Stop => Some(PauseReason::UserRequest),
        Message::TemperatureNormal
        | Message::FanRestored
        | Message::PoolReachable
        | Message::TunerRecovered
        | Message::OtaEnd
        | Message::Start => None,
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
        let triggering_reason = halt_reason(message);
        reasons = reasons.apply(message);
        log_transition(
            was_danger,
            reasons.contains(PauseReason::DangerousTemperature),
        );
        publisher.send(reasons);
        let is_empty = reasons.is_empty();
        if was_empty && !is_empty {
            if let Some(reason) = triggering_reason {
                match reason {
                    PauseReason::DangerousTemperature => {
                        crate::events::mining_halted_dangerous_temperature();
                    }
                    PauseReason::BrokenFan => crate::events::mining_halted_broken_fan(),
                    PauseReason::UserRequest => crate::events::mining_halted_user_request(),
                    PauseReason::PoolUnreachable => crate::events::mining_halted_pool_unreachable(),
                    PauseReason::OtaUpdate => crate::events::mining_halted_ota_update(),
                    PauseReason::TunerFault => crate::events::mining_halted_tuner_fault(),
                }
            }
            mining.halt().await;
        } else if !was_empty && is_empty {
            crate::events::mining_resumed();
            // Restart only when the last pause reason has cleared.
            mining.resume().await;
        }
    }
}

/// Task shell around [`run_manager`], the supervisor command router that
/// forwards halt and resume to the mining, cooling and thermal handles.
#[macro_export]
macro_rules! manager_task {
    () => {
        #[embassy_executor::task]
        async fn manager_task(mining: ubos_mining::Handle) -> ! {
            ubos_manager::run_manager(mining).await
        }
    };
}
