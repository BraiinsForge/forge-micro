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
//! A shell cannot be a plain generic function: `#[embassy_executor::task]`
//! expands a typed `TaskPool` static, so it rejects generic task
//! functions. Each macro expands at the call site, so the attribute
//! resolves against the board's own `embassy-executor` and the spawned
//! task type belongs to that board's executor.
//!
//! **A shell lives in the crate that owns the loop it calls**, beside
//! that function, so the two move together. This module holds only the
//! two that have no such crate.
//!
//! Expansions name every path in full rather than relying on the board's
//! imports, so a board needs only the dependency the body reaches into,
//! not a matching `use`.

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
