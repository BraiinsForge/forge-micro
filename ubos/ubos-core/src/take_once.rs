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

//! A flag that lets a handle be taken once.
//!
//! A module that has a task of its own keeps a [`TakeOnce`] beside its private
//! statics and calls [`TakeOnce::take`] from `Handle::take`. The firmware takes
//! each handle in `main` and passes it to its tasks, so a second call means
//! that two places claim one handle, and the boot stops at the second call.

use portable_atomic::{AtomicBool, Ordering};

/// The record that a handle was taken, with the name the panic reports. It
/// is a `portable-atomic` flag because some boards have no native atomic
/// swap.
pub struct TakeOnce {
    taken: AtomicBool,
    name: &'static str,
}

impl TakeOnce {
    /// A flag that has not been taken, for the handle called `name`.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            taken: AtomicBool::new(false),
            name,
        }
    }

    /// Record the take. The panic names the handle and the call that came
    /// second.
    ///
    /// # Panics
    ///
    /// Panics when an earlier call recorded a take.
    #[track_caller]
    pub fn take(&self) {
        assert!(
            !self.taken.swap(true, Ordering::AcqRel),
            "BUG: the {} handle was already taken",
            self.name
        );
    }
}

#[cfg(test)]
#[path = "tests/take_once.rs"]
mod tests;
