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
/// For RP2350, we need memory.x copied to the linker search path plus link.x
/// and defmt.x linker scripts. Embassy-rp is built with `imagedef-none`, and
/// this application links into the ACTIVE partition rather than offset 0, so
/// it emits no image-def of its own. The reused `rp2350-bootloader` owns the
/// offset-0 image-def block the bootrom inspects and chains to ACTIVE.
/// link-rp.x (which defines `.boot2`) is not needed for RP2350.
fn main() {
    ubos_build::cortex_m_board();
    ubos_build::open_board_build_id();
}
