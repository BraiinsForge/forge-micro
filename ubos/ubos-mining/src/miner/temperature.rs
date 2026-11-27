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

//! The miner's view of the dangerous temperature flag that thermal
//! publishes.

use ubos_thermal::Handle;

/// Wait for the dangerous temperature flag to be set.
pub(super) async fn danger_raised(thermal: Handle) {
    let mut flag = thermal.subscribe_dangerous();
    flag.get_and(|set| *set).await;
}

/// Wait for the dangerous temperature flag to be clear. A flag that is
/// clear already resolves at once.
pub(super) async fn danger_cleared(thermal: Handle) {
    let mut flag = thermal.subscribe_dangerous();
    flag.get_and(|set| !*set).await;
}
