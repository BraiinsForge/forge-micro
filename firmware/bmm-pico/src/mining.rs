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

//! Hashboard reset pin plus the two long-running tasks the ubos-mining backend
//! spawns: the chip-serial actor and the bring-up/teardown state machine.
//!
//! Backend architecture: `../../../docs/MINING.md`.

/// The board's single hashboard chain.
pub static CHAINS: [ubos_mining::serial::Serial; crate::config::CHAINS] =
    ubos_mining::serial::Serial::table();

ubos_mining::serial_task!(
    chains = CHAINS,
    ubos_rp::uart::MinerUart,
    ubos_firmware::AsicSession<{ crate::POOL_SLOTS }>,
    crate::config::BOARD,
    ubos_asic::Bm13xxDriver,
    scheduler = ubos_mining::scheduler::FeeScheduler::new(crate::config::FEE_POOL.permille)
);

/// The reset line is one push-pull output with no timings, so the shared
/// newtype drives it.
pub type BoardResetPin = ubos_mining::reset::OutputResetPin<embassy_rp::gpio::Output<'static>>;

ubos_mining::miner_task!(
    BoardResetPin,
    crate::config::BOARD,
    crate::config::CHAINS,
    crate::config::TunerSet
);
