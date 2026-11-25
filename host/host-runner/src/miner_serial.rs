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

//! Brings up the ubos-mining serial actor against a Linux tty.
//!
//! A device that will not open parks the task instead of crashing the
//! runner, so the network, stratum and metrics tasks keep working with
//! no hashboard wired in.

use embassy_time::{Duration, Timer};
use embedded_serial_unix::Serial;
use log::{error, info};
use ubos_mining::consts::INITIAL_BAUDRATE;

use crate::ASIC_SESSION;
use crate::config::{CHIP, MINER_SERIAL_PATH};

/// The count is a literal because the runner has no machine descriptor to
/// read it from: it drives the one serial device `MINER_SERIAL_PATH` names
/// and stands for no product.
pub static CHAINS: [ubos_mining::serial::Serial; 1] = ubos_mining::serial::Serial::table();

#[embassy_executor::task]
pub async fn miner_serial_task(park: &'static ubos_mining::Park) -> ! {
    let serial = match Serial::open(MINER_SERIAL_PATH, INITIAL_BAUDRATE) {
        Ok(serial) => serial,
        Err(error) => {
            error!(
                "host-runner: failed to open {MINER_SERIAL_PATH}: {error}; miner serial disabled"
            );
            loop {
                Timer::after(Duration::from_secs(60)).await;
            }
        }
    };
    info!("host-runner: miner serial bringup on {MINER_SERIAL_PATH}");
    // One slot, so the scheduler always serves slot 0 when it has work
    // and `new`'s fallback default needs no reweight.
    let scheduler = ubos_mining::scheduler::RatioScheduler::new();
    match CHAINS[0]
        .run(
            park,
            serial,
            &ASIC_SESSION,
            crate::BOARD,
            ubos_asic::Bm13xxDriver::new(CHIP),
            scheduler,
            ubos_metrics::chain::handle().chain(ubos_core::index::HashboardIndex(0)),
        )
        .await
    {
        Ok(()) => info!("host-runner: miner serial actor terminated cleanly"),
        Err(kind) => error!("host-runner: miner serial actor exited: {kind:?}"),
    }
    // Re-opening would need fresh peripheral state that the host has no
    // clean trigger for, so the task idles here and the rest of the
    // runner keeps going.
    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}
