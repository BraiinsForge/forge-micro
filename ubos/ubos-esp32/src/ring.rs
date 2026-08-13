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

//! Persistent event-ring wrapper carrying esp-hal's `Persistable` marker.

/// Wraps the event-ring backing so it can carry the esp-hal `Persistable`
/// marker that `#[ram(persistent)]` requires. `RetainedRing` is `repr(C)` over
/// `u32` and `u8` fields, so every bit pattern is a valid value.
#[repr(transparent)]
pub struct PersistentRing<const N: usize>(pub ubos_events::RetainedRing<N>);

// Safety: every bit pattern of `PersistentRing` is a valid value and it needs
// no initialisation beyond the validity header `init` checks, so persisting it
// uninitialised across a reset is sound.
unsafe impl<const N: usize> esp_hal::Persistable for PersistentRing<N> {}
