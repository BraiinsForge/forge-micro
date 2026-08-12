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
/// For RP2040 with embassy-boot, we need memory.x on the linker search path,
/// plus link.x and defmt.x. The link-rp.x script (from embassy-rp) is omitted
/// because it only places the `.boot2` section into the `BOOT2` memory region;
/// the application lives in the ACTIVE partition at 0x10021000, has no BOOT2
/// region, and the `boot2-none` cargo feature suppresses the symbol so the
/// script's section is empty. The bootloader carries boot2.
fn main() {
    ubos_build::cortex_m_board();
}
