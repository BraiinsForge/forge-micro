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

/// Build script that configures linker scripts and error handling.
///
/// The local `memory.x` is copied into `$OUT_DIR` and that directory is
/// added to the linker search path so cortex-m-rt's `link.x` picks up
/// our extended layout instead of the one embassy-stm32 would generate.
/// We carry our own memory.x to declare the CCM RAM region; the
/// `memory-x` feature on the `embassy-stm32` dependency is therefore
/// disabled.
fn main() {
    ubos_build::cortex_m_board();
}
