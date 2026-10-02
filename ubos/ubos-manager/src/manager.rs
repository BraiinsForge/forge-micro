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

use ubos_logging::info;
use ubos_mining::Handle as MiningHandle;

use crate::Runner;
use crate::message::Message;
use crate::pause::{PauseReason, PauseReasons, effect};

/// Log a `DangerousTemperature` bit transition.
///
/// Only this reason carries a dedicated log line, because a thermal halt
/// originates inside the manager from a sensor reading and the operator
/// has no other surface to see it; every other reason is asserted by a
/// caller that already logs its own context. The clearing edge is an
/// event rather than a trace, because it reports the release whether or
/// not another reason still holds mining, which `mining_resumed` cannot.
/// Extracted from the loop body so each log call site is unique and
/// `clippy::if_same_then_else` does not fire.
fn log_transition(was_halted: bool, now_halted: bool) {
    if !was_halted && now_halted {
        info!("manager: halt (DangerousTemperature)");
    } else if was_halted && !now_halted {
        crate::events::mining_dangerous_temperature_cleared();
    }
}

/// The `PauseReason` an asserting `Message` sets, or `None` for a
/// clearing one, so the halt event can carry the discriminant of the
/// newly-active reason. Read off [`effect`], the one table that
/// interprets a message, so the two cannot disagree.
///
/// Free rather than a method on `Message`: hanging it off the public
/// type would hand every sender the mapping, and `message.rs` states why
/// a sender must never spell a `PauseReason`.
fn halt_reason(message: Message) -> Option<PauseReason> {
    match effect(message) {
        (reason, true) => Some(reason),
        (_, false) => None,
    }
}

/// Run the manager loop forever.
///
/// Drains the command channel fed by `Handle::send`, interprets each
/// [`Message`] into a `PauseReason` bit transition on the live
/// [`PauseReasons`] mask, publishes the updated mask, and calls
/// `mining.halt()`/`resume()` on the empty/non-empty edge. A `Start`
/// that leaves the mask empty resumes as well, because a miner that
/// parked itself clears no reason and could otherwise be released only
/// through a pause and a resume.
pub async fn run_manager(runner: &mut Runner, mining: MiningHandle) -> ! {
    let publisher = runner.publisher();
    let mut reasons = PauseReasons::default();

    info!("manager: started");

    loop {
        let message = runner.next_message().await;
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
                    PauseReason::OtaUpdate => crate::events::mining_halted_ota_update(),
                    PauseReason::TunerFault => crate::events::mining_halted_tuner_fault(),
                    PauseReason::TemperatureUnreadable => {
                        crate::events::mining_halted_temperature_unreadable();
                    }
                }
            }
            mining.halt().await;
        } else if !was_empty && is_empty {
            crate::events::mining_resumed();
            // Restart only when the last pause reason has cleared.
            mining.resume().await;
        } else if is_empty && message == Message::Start {
            mining.resume().await;
        }
    }
}

/// Task shell around [`run_manager`].
#[macro_export]
macro_rules! manager_task {
    () => {
        #[embassy_executor::task]
        async fn manager_task(mut runner: ubos_manager::Runner, mining: ubos_mining::Handle) -> ! {
            ubos_manager::run_manager(&mut runner, mining).await
        }
    };
}
