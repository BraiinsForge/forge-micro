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

//! BM1362 chip-side wiring for the ubos-mining backend.
//!
//! `serial_task` runs on the `APP_CPU` executor while embassy-net and the
//! mining state machine run on Core 0. `ASIC_SESSION` uses a
//! `CriticalSectionRawMutex`, so that cross-core share needs no extra pipe.

/// The board's single hashboard chain.
pub static CHAINS: [ubos_mining::serial::Serial; crate::config::CHAINS] =
    ubos_mining::serial::Serial::table();

/// EXIO5 on the carrier's TCA9554 drives the hashboard reset line. The EXIO
/// labels are 1-based on the silkscreen and the TCA9554 pins are 0-based,
/// so EXIO5 is pin P4.
pub const RESET_BIT: u8 = 1 << 4;

ubos_mining::serial_task!(
    chains = CHAINS,
    ubos_esp32::uart::MinerUart,
    ubos_firmware::AsicSession<{ crate::POOL_SLOTS }>,
    crate::config::BOARD,
    ubos_asic::Bm13xxDriver,
    scheduler = ubos_mining::scheduler::FeeScheduler::new(crate::config::FEE_POOL.permille)
);

ubos_mining::miner_task!(
    ubos_esp32::expander::ExpanderResetPin,
    crate::config::BOARD,
    crate::config::CHAINS,
    crate::config::TunerSet
);
