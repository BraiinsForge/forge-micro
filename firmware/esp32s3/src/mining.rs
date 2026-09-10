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

//! BM1362 chip-side wiring: hashboard reset pin and the two long-running
//! tasks the ubos-mining backend needs.
//!
//! `serial_task` owns the UART and drives `ubos_mining::serial::SERIAL` for
//! the lifetime of the firmware; `miner_task` runs the bring-up / teardown
//! state machine in `ubos_mining::Miner::run`. Neither ever returns under normal
//! operation.
//!
//! The ubos-mining `serial_task` runs on Core 1 (the `APP_CPU` embassy executor)
//! while embassy-net and the mining state machine run on Core 0, mirroring
//! the rp2350/bmm-pico dual-core split. `ASIC_SESSION` uses a
//! `CriticalSectionRawMutex`, so the cross-core share needs no extra pipe.

/// EXIO5 (P4) on the carrier's TCA9554 drives the hashboard reset line.
/// The EXIO labels are 1-based on the carrier silkscreen while the TCA9554
/// pins are 0-based, so EXIO5 maps to pin P4. Confirm this offset against
/// the carrier schematic if the expander wiring ever changes.
pub const RESET_BIT: u8 = 1 << 4;

ubos_mining::serial_task!(
    ubos_esp32::uart::MinerUart,
    ubos_firmware::AsicSession<{ crate::POOL_SLOTS }>,
    crate::config::BOARD,
    ubos_asic::Bm13xxDriver
);

ubos_mining::miner_task!(
    crate::psu::BoardPsu,
    ubos_esp32::expander::ExpanderResetPin,
    crate::config::BOARD
);
