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

//! The task shells that belong to no single subsystem crate.
//!
//! Every board spawns the same set of background tasks, and for most of
//! them the shell is one line delegating to a `ubos-*` crate. The shells
//! cannot be plain generic functions: `#[embassy_executor::task]` expands
//! a typed `TaskPool` static, so it rejects generic task functions. That
//! is a known and still-open embassy limitation, and the workaround its
//! maintainers point at is macro-generated shells.
//!
//! **A shell lives in the crate that owns the loop it calls**, beside
//! that function, so the two move together. `ubos_core::watchdog_task!`
//! sits next to `ubos_core::watchdog::run_watchdog`, and a board reaches
//! each shell through the crate whose work it does. This module holds
//! only the three that have no such crate.
//!
//! Each macro expands at the call site, so `#[embassy_executor::task]`
//! resolves against the board's own `embassy-executor` and the spawned
//! task type belongs to the board's executor. A board that pinned a
//! different executor version, or enables different platform features as
//! these boards do, still gets a shell built against its own.
//!
//! Expansions name every path in full rather than relying on the board's
//! imports, so a board needs only the dependency the body reaches into,
//! not a matching `use`.
//!
//! A board invokes only the macros for tasks it actually runs. There is
//! deliberately no single macro emitting the whole set: the boards run
//! different subsets, and a board without a mining loop or without an
//! OTA target must not be handed shells for them.
//!
//! Shells whose signature names a HAL type take it as a parameter. The
//! ones that stay hand-written in the board crate are `boot`, which is
//! the composition root itself, `cpu_load_task`, whose core count is a
//! per-board const, `mdns_task`, which names per-board consts, and
//! `stratum_client_task`, which needs the board's own session type and
//! pool size.

/// Publishes per-slot stratum session metrics. Takes the board's pool
/// slot count, which is a per-board const.
///
/// This shell stays here rather than moving to `stratum-client`, because
/// the publisher is reached through `$crate` rather than named directly,
/// so a board spawning it needs no `stratum-client` dependency of its
/// own. It was the only thing the boards used that crate for, and once
/// this shell became a macro they had no source reference left to it, so
/// `cargo-machete` was correct to call the dependency unused. Moving the
/// macro to `stratum-client` would put that dependency back on eight
/// boards.
#[macro_export]
macro_rules! stratum_metrics_task {
    ($pool_slots:expr, $fee:expr) => {
        #[embassy_executor::task]
        async fn stratum_metrics_task() -> ! {
            $crate::run_stratum_metrics_publisher($pool_slots, ubos_config::FeeSlot::index(&$fee))
                .await
        }
    };
}

/// Drives the embassy-net stack. Takes the board's `embassy-net` driver
/// type; the `Runner` around it is the same on every board.
///
/// This shell and [`ethernet_task`] call `embassy_net::Runner::run`
/// directly, so no `ubos` crate owns either of them.
#[macro_export]
macro_rules! net_task {
    ($device:ty) => {
        #[embassy_executor::task]
        async fn net_task(mut runner: embassy_net::Runner<'static, $device>) -> ! {
            runner.run().await
        }
    };
}

/// Drives the Ethernet controller. Takes the board's runner type, which
/// differs with the PHY and the bus it hangs off.
#[macro_export]
macro_rules! ethernet_task {
    ($runner:ty) => {
        #[embassy_executor::task]
        async fn ethernet_task(runner: $runner) -> ! {
            runner.run().await
        }
    };
}
